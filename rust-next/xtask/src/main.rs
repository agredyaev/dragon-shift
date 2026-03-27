use futures_util::{SinkExt, StreamExt};
use protocol::{
    ClientWsMessage, CreateWorkshopRequest, JudgeBundle, JoinWorkshopRequest, Phase,
    ServerWsMessage, SessionCommand, SessionEnvelope, WorkshopCommandRequest,
    WorkshopCommandResult, WorkshopJudgeBundleRequest, WorkshopJudgeBundleResult,
    WorkshopJoinResult, WorkshopJoinSuccess,
};
use serde_json::json;
use std::env;
use std::future::Future;
use std::path::PathBuf;
use std::process::Command;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message as WsMessage},
    MaybeTlsStream, WebSocketStream,
};

type SmokeWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let Some((command, rest)) = args.split_first() else {
        print_help();
        return Ok(());
    };

    let forwarded = normalize_forwarded_args(rest.to_vec());

    match command.as_str() {
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        "check" => run_tool("cargo", &["check", "--workspace", "--all-targets"]),
        "test" => run_tool("cargo", &["test", "--workspace"]),
        "fmt" => run_tool("cargo", &["fmt", "--all"]),
        "clippy" => run_tool("cargo", &["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]),
        "app-web-test" => run_tool("cargo", &["test", "-p", "app-web"]),
        "app-server-test" => run_tool("cargo", &["test", "-p", "app-server"]),
        "server" | "dev-server" => run_tool_owned("cargo", cargo_run_package_args("app-server", &forwarded)),
        "web" | "dev-web" => run_dx(dx_serve_app_web_args(&forwarded)),
        "smoke-phase1" => run_async(smoke_phase1(smoke_base_url(&forwarded)?)),
        "smoke-judge-bundle" => run_async(smoke_judge_bundle(smoke_base_url(&forwarded)?)),
        "smoke-offline-failover" => run_async(smoke_offline_failover(smoke_base_url(&forwarded)?)),
        unknown => Err(format!("unknown xtask command: {unknown}")),
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask workspace root")
        .to_path_buf()
}

fn run_async<F>(future: F) -> Result<(), String>
where
    F: Future<Output = Result<(), String>>,
{
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|error| format!("failed to create tokio runtime: {error}"))?;
    runtime.block_on(future)
}

fn normalize_forwarded_args(args: Vec<String>) -> Vec<String> {
    match args.first().map(String::as_str) {
        Some("--") => args.into_iter().skip(1).collect(),
        _ => args,
    }
}

fn normalize_base_url(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        "http://127.0.0.1:4100".to_string()
    } else {
        trimmed.to_string()
    }
}

fn smoke_base_url(forwarded: &[String]) -> Result<String, String> {
    let mut base_url = env::var("XTASK_SMOKE_BASE_URL")
        .or_else(|_| env::var("SMOKE_TEST_BASE_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:4100".to_string());
    let mut args = forwarded.iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--base-url" => {
                base_url = args
                    .next()
                    .ok_or_else(|| "missing value for --base-url".to_string())?
                    .clone();
            }
            unknown => return Err(format!("unknown smoke arg: {unknown}")),
        }
    }

    Ok(normalize_base_url(&base_url))
}

fn smoke_ws_url(base_url: &str) -> String {
    let normalized = normalize_base_url(base_url);
    let ws_base = if let Some(rest) = normalized.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = normalized.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        normalized
    };

    format!("{ws_base}/api/workshops/ws")
}

fn cargo_run_package_args(package: &str, forwarded: &[String]) -> Vec<String> {
    let mut args = vec!["run".to_string(), "-p".to_string(), package.to_string()];
    if !forwarded.is_empty() {
        args.push("--".to_string());
        args.extend(forwarded.iter().cloned());
    }
    args
}

fn dx_serve_app_web_args(forwarded: &[String]) -> Vec<String> {
    let mut args = vec![
        "serve".to_string(),
        "--package".to_string(),
        "app-web".to_string(),
        "--platform".to_string(),
        "web".to_string(),
    ];
    args.extend(forwarded.iter().cloned());
    args
}

fn preferred_dx_program(explicit_cli: Option<&str>, home_dir: Option<&str>) -> String {
    if let Some(explicit_cli) = explicit_cli
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return explicit_cli.to_string();
    }

    if let Some(home_dir) = home_dir.map(str::trim).filter(|value| !value.is_empty()) {
        let cargo_dx = PathBuf::from(home_dir).join(".cargo/bin/dx");
        if cargo_dx.exists() {
            return cargo_dx.to_string_lossy().into_owned();
        }
    }

    "dx".to_string()
}

fn run_tool(program: &str, args: &[&str]) -> Result<(), String> {
    run_tool_owned(program, args.iter().map(|value| value.to_string()).collect())
}

fn run_dx(args: Vec<String>) -> Result<(), String> {
    let program = preferred_dx_program(
        env::var("DIOXUS_CLI").ok().as_deref(),
        env::var("HOME").ok().as_deref(),
    );
    run_tool_owned(&program, args).map_err(|error| {
        format!(
            "{error}. Install the Dioxus CLI to use `cargo xtask web`, or set DIOXUS_CLI=/path/to/dx if another `dx` shadows it in PATH."
        )
    })
}

fn run_tool_owned(program: &str, args: Vec<String>) -> Result<(), String> {
    let status = Command::new(program)
        .args(&args)
        .current_dir(workspace_root())
        .status()
        .map_err(|error| format!("failed to run `{program}`: {error}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("`{program} {}` exited with status {status}", args.join(" ")))
    }
}

fn command_request(
    identity: &WorkshopJoinSuccess,
    command: SessionCommand,
    payload: Option<serde_json::Value>,
) -> WorkshopCommandRequest {
    WorkshopCommandRequest {
        session_code: identity.session_code.clone(),
        reconnect_token: identity.reconnect_token.clone(),
        coordinator_type: Some(identity.coordinator_type),
        command,
        payload,
    }
}

fn assigned_dragon_id(session: &WorkshopJoinSuccess) -> Result<String, String> {
    session
        .state
        .players
        .get(&session.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .ok_or_else(|| format!("player {} has no assigned dragon", session.player_id))
}

fn ensure_phase(session: &WorkshopJoinSuccess, expected: Phase, label: &str) -> Result<(), String> {
    if session.state.phase == expected {
        Ok(())
    } else {
        Err(format!("{label}: expected {:?}, got {:?}", expected, session.state.phase))
    }
}

fn print_json(value: serde_json::Value) -> Result<(), String> {
    let encoded = serde_json::to_string_pretty(&value)
        .map_err(|error| format!("failed to encode xtask output: {error}"))?;
    println!("{encoded}");
    Ok(())
}

async fn smoke_phase1(base_url: String) -> Result<(), String> {
    let client = reqwest::Client::new();
    let host = create_workshop(&client, &base_url, "XtaskSmokeHost").await?;

    send_command(
        &client,
        &base_url,
        command_request(&host, SessionCommand::StartPhase1, None),
    )
    .await?;

    let reconnected = reconnect_workshop(&client, &base_url, &host).await?;
    ensure_phase(&reconnected, Phase::Phase1, "smoke-phase1 reconnect")?;
    if reconnected.state.current_player_id.as_deref() != Some(host.player_id.as_str()) {
        return Err("reconnect returned a different current player than the host".to_string());
    }
    if assigned_dragon_id(&reconnected).is_err() {
        return Err("expected the single-player host to receive a dragon in Phase 1".to_string());
    }

    print_json(json!({
        "ok": true,
        "baseUrl": base_url,
        "sessionCode": host.session_code,
        "phase": "phase1",
    }))
}

async fn smoke_judge_bundle(base_url: String) -> Result<(), String> {
    let client = reqwest::Client::new();
    let host = create_workshop(&client, &base_url, "XtaskJudgeHost").await?;
    let guest = join_workshop(&client, &base_url, &host.session_code, "XtaskJudgeGuest").await?;

    send_command(&client, &base_url, command_request(&host, SessionCommand::StartPhase1, None)).await?;
    let host_phase1 = reconnect_workshop(&client, &base_url, &host).await?;
    let guest_phase1 = reconnect_workshop(&client, &base_url, &guest).await?;
    ensure_phase(&host_phase1, Phase::Phase1, "host phase1")?;
    ensure_phase(&guest_phase1, Phase::Phase1, "guest phase1")?;

    send_command(
        &client,
        &base_url,
        command_request(&host, SessionCommand::SubmitObservation, Some(json!({ "text": "Calms down at dusk" }))),
    )
    .await?;
    send_command(
        &client,
        &base_url,
        command_request(&guest, SessionCommand::SubmitObservation, Some(json!({ "text": "Rejects fruit at night" }))),
    )
    .await?;
    send_command(&client, &base_url, command_request(&host, SessionCommand::StartHandover, None)).await?;
    send_command(
        &client,
        &base_url,
        command_request(&host, SessionCommand::SubmitTags, Some(json!(["Rule 1", "Rule 2", "Rule 3"]))),
    )
    .await?;
    send_command(
        &client,
        &base_url,
        command_request(&guest, SessionCommand::SubmitTags, Some(json!(["Rule A", "Rule B", "Rule C"]))),
    )
    .await?;
    send_command(&client, &base_url, command_request(&host, SessionCommand::StartPhase2, None)).await?;

    let host_phase2 = reconnect_workshop(&client, &base_url, &host).await?;
    ensure_phase(&host_phase2, Phase::Phase2, "host phase2")?;
    send_command(
        &client,
        &base_url,
        command_request(&host, SessionCommand::Action, Some(json!({ "type": "sleep" }))),
    )
    .await?;
    send_command(&client, &base_url, command_request(&host, SessionCommand::EndGame, None)).await?;

    let host_voting = reconnect_workshop(&client, &base_url, &host).await?;
    let guest_voting = reconnect_workshop(&client, &base_url, &guest).await?;
    ensure_phase(&host_voting, Phase::Voting, "host voting")?;
    ensure_phase(&guest_voting, Phase::Voting, "guest voting")?;
    let host_vote_target = assigned_dragon_id(&guest_voting)?;
    let guest_vote_target = assigned_dragon_id(&host_voting)?;

    send_command(
        &client,
        &base_url,
        command_request(&host, SessionCommand::SubmitVote, Some(json!({ "dragonId": host_vote_target }))),
    )
    .await?;
    send_command(
        &client,
        &base_url,
        command_request(&guest, SessionCommand::SubmitVote, Some(json!({ "dragonId": guest_vote_target }))),
    )
    .await?;
    send_command(
        &client,
        &base_url,
        command_request(&host, SessionCommand::RevealVotingResults, None),
    )
    .await?;

    let host_end = reconnect_workshop(&client, &base_url, &host).await?;
    ensure_phase(&host_end, Phase::End, "host end")?;
    let bundle = fetch_judge_bundle(&client, &base_url, &host).await?;
    if bundle.artifact_count <= 0 {
        return Err("judge bundle did not include artifacts".to_string());
    }
    if !bundle.dragons.iter().any(|dragon| !dragon.handover_chain.creator_instructions.is_empty()) {
        return Err("judge bundle is missing creator instructions".to_string());
    }
    if !bundle.dragons.iter().any(|dragon| !dragon.handover_chain.discovery_observations.is_empty()) {
        return Err("judge bundle is missing discovery observations".to_string());
    }
    if !bundle.dragons.iter().any(|dragon| dragon.handover_chain.handover_tags.len() == 3) {
        return Err("judge bundle is missing completed handover tags".to_string());
    }
    if !bundle.dragons.iter().any(|dragon| !dragon.phase2_actions.is_empty()) {
        return Err("judge bundle is missing captured phase2 actions".to_string());
    }

    print_json(json!({
        "ok": true,
        "baseUrl": base_url,
        "sessionCode": host.session_code,
        "phase": "end",
        "dragons": bundle.dragons.len(),
        "artifactCount": bundle.artifact_count,
    }))
}

async fn smoke_offline_failover(base_url: String) -> Result<(), String> {
    let client = reqwest::Client::new();
    let host = create_workshop(&client, &base_url, "XtaskFailoverHost").await?;
    let guest = join_workshop(&client, &base_url, &host.session_code, "XtaskFailoverGuest").await?;

    let mut host_socket = attach_ws_session(&base_url, &host).await?;
    host_socket
        .close(None)
        .await
        .map_err(|error| format!("failed to close smoke websocket: {error}"))?;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let guest_after_failover = reconnect_workshop(&client, &base_url, &guest).await?;
    let guest_player = guest_after_failover
        .state
        .players
        .get(&guest.player_id)
        .ok_or_else(|| "guest player missing after failover".to_string())?;
    let host_player = guest_after_failover
        .state
        .players
        .get(&host.player_id)
        .ok_or_else(|| "host player missing after failover".to_string())?;
    if !guest_player.is_host {
        return Err("guest did not become host after websocket disconnect".to_string());
    }
    if host_player.is_connected {
        return Err("host still appears connected after websocket disconnect".to_string());
    }

    send_command(
        &client,
        &base_url,
        command_request(&guest, SessionCommand::StartPhase1, None),
    )
    .await?;
    let guest_phase1 = reconnect_workshop(&client, &base_url, &guest).await?;
    ensure_phase(&guest_phase1, Phase::Phase1, "guest phase1 after failover")?;

    let host_reconnected = reconnect_workshop(&client, &base_url, &host).await?;
    let host_player_after_reconnect = host_reconnected
        .state
        .players
        .get(&host.player_id)
        .ok_or_else(|| "host player missing after reconnect".to_string())?;
    if !host_player_after_reconnect.is_connected {
        return Err("host did not recover connectivity after HTTP reconnect".to_string());
    }
    if host_player_after_reconnect.is_host {
        return Err("host unexpectedly reclaimed host role after guest failover".to_string());
    }

    send_command(
        &client,
        &base_url,
        command_request(&guest, SessionCommand::ResetGame, None),
    )
    .await?;
    let guest_lobby = reconnect_workshop(&client, &base_url, &guest).await?;
    ensure_phase(&guest_lobby, Phase::Lobby, "guest lobby after reset")?;

    print_json(json!({
        "ok": true,
        "baseUrl": base_url,
        "sessionCode": host.session_code,
        "hostAfterFailover": guest.player_id,
        "phaseAfterReset": "lobby",
        "reconnectedHostConnected": host_player_after_reconnect.is_connected,
    }))
}

async fn create_workshop(
    client: &reqwest::Client,
    base_url: &str,
    name: &str,
) -> Result<WorkshopJoinSuccess, String> {
    let response = client
        .post(format!("{base_url}/api/workshops"))
        .header("Origin", base_url)
        .json(&CreateWorkshopRequest {
            name: name.to_string(),
        })
        .send()
        .await
        .map_err(|error| format!("failed to reach backend: {error}"))?;

    parse_join_response(response).await
}

async fn attach_ws_session(base_url: &str, identity: &WorkshopJoinSuccess) -> Result<SmokeWebSocket, String> {
    let mut request = smoke_ws_url(base_url)
        .into_client_request()
        .map_err(|error| format!("failed to build smoke websocket request: {error}"))?;
    request
        .headers_mut()
        .insert("origin", HeaderValue::from_str(base_url).map_err(|error| format!("invalid websocket origin: {error}"))?);

    let (mut socket, _) = connect_async(request)
        .await
        .map_err(|error| format!("failed to connect smoke websocket: {error}"))?;
    let attach_payload = serde_json::to_string(&ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: identity.session_code.clone(),
        player_id: identity.player_id.clone(),
        reconnect_token: identity.reconnect_token.clone(),
        coordinator_type: Some(identity.coordinator_type),
    }))
    .map_err(|error| format!("failed to encode smoke websocket attach payload: {error}"))?;
    socket
        .send(WsMessage::Text(attach_payload.into()))
        .await
        .map_err(|error| format!("failed to send smoke websocket attach payload: {error}"))?;

    let message = socket
        .next()
        .await
        .ok_or_else(|| "smoke websocket closed before sending initial state".to_string())
        .and_then(|message| message.map_err(|error| format!("failed to read smoke websocket frame: {error}")))?;
    let payload = match message {
        WsMessage::Text(text) => text,
        other => return Err(format!("unexpected smoke websocket frame: {other:?}")),
    };
    let server_message = serde_json::from_str::<ServerWsMessage>(&payload)
        .map_err(|error| format!("failed to decode smoke websocket payload: {error}"))?;
    match server_message {
        ServerWsMessage::StateUpdate(_) => Ok(socket),
        ServerWsMessage::Error { message } => Err(message),
        other => Err(format!("unexpected smoke websocket payload: {other:?}")),
    }
}

async fn join_workshop(
    client: &reqwest::Client,
    base_url: &str,
    session_code: &str,
    name: &str,
) -> Result<WorkshopJoinSuccess, String> {
    let response = client
        .post(format!("{base_url}/api/workshops/join"))
        .header("Origin", base_url)
        .json(&JoinWorkshopRequest {
            session_code: session_code.to_string(),
            name: Some(name.to_string()),
            reconnect_token: None,
        })
        .send()
        .await
        .map_err(|error| format!("failed to reach backend: {error}"))?;

    parse_join_response(response).await
}

async fn reconnect_workshop(
    client: &reqwest::Client,
    base_url: &str,
    identity: &WorkshopJoinSuccess,
) -> Result<WorkshopJoinSuccess, String> {
    let response = client
        .post(format!("{base_url}/api/workshops/join"))
        .header("Origin", base_url)
        .json(&JoinWorkshopRequest {
            session_code: identity.session_code.clone(),
            name: None,
            reconnect_token: Some(identity.reconnect_token.clone()),
        })
        .send()
        .await
        .map_err(|error| format!("failed to reach backend: {error}"))?;

    parse_join_response(response).await
}

async fn send_command(
    client: &reqwest::Client,
    base_url: &str,
    request: WorkshopCommandRequest,
) -> Result<(), String> {
    let response = client
        .post(format!("{base_url}/api/workshops/command"))
        .header("Origin", base_url)
        .json(&request)
        .send()
        .await
        .map_err(|error| format!("failed to reach backend: {error}"))?;

    let payload = response
        .json::<WorkshopCommandResult>()
        .await
        .map_err(|error| format!("failed to parse backend response: {error}"))?;

    match payload {
        WorkshopCommandResult::Success(_) => Ok(()),
        WorkshopCommandResult::Error(error) => Err(error.error),
    }
}

async fn fetch_judge_bundle(
    client: &reqwest::Client,
    base_url: &str,
    identity: &WorkshopJoinSuccess,
) -> Result<JudgeBundle, String> {
    let response = client
        .post(format!("{base_url}/api/workshops/judge-bundle"))
        .header("Origin", base_url)
        .json(&WorkshopJudgeBundleRequest {
            session_code: identity.session_code.clone(),
            reconnect_token: identity.reconnect_token.clone(),
            coordinator_type: Some(identity.coordinator_type),
        })
        .send()
        .await
        .map_err(|error| format!("failed to reach backend: {error}"))?;

    let payload = response
        .json::<WorkshopJudgeBundleResult>()
        .await
        .map_err(|error| format!("failed to parse backend response: {error}"))?;

    match payload {
        WorkshopJudgeBundleResult::Success(success) => Ok(success.bundle),
        WorkshopJudgeBundleResult::Error(error) => Err(error.error),
    }
}

async fn parse_join_response(response: reqwest::Response) -> Result<WorkshopJoinSuccess, String> {
    let payload = response
        .json::<WorkshopJoinResult>()
        .await
        .map_err(|error| format!("failed to parse backend response: {error}"))?;

    match payload {
        WorkshopJoinResult::Success(success) => Ok(success),
        WorkshopJoinResult::Error(error) => Err(error.error),
    }
}

fn print_help() {
    println!(
        "xtask commands:
  cargo xtask check
  cargo xtask test
  cargo xtask fmt
  cargo xtask clippy
  cargo xtask app-web-test
  cargo xtask app-server-test
  cargo xtask server -- [app-server args]
  cargo xtask web -- [dx serve args]
  cargo xtask smoke-phase1 -- [--base-url http://127.0.0.1:4100]
  cargo xtask smoke-judge-bundle -- [--base-url http://127.0.0.1:4100]
  cargo xtask smoke-offline-failover -- [--base-url http://127.0.0.1:4100]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_forwarded_args_drops_separator() {
        assert_eq!(
            normalize_forwarded_args(vec!["--".to_string(), "--port".to_string(), "4100".to_string()]),
            vec!["--port".to_string(), "4100".to_string()]
        );
    }

    #[test]
    fn normalize_base_url_trims_slashes_and_whitespace() {
        assert_eq!(normalize_base_url(" http://127.0.0.1:4100/ "), "http://127.0.0.1:4100");
        assert_eq!(normalize_base_url("   "), "http://127.0.0.1:4100");
    }

    #[test]
    fn smoke_base_url_uses_forwarded_flag() {
        assert_eq!(
            smoke_base_url(&["--base-url".to_string(), "http://localhost:4200/".to_string()]).expect("base url"),
            "http://localhost:4200"
        );
    }

    #[test]
    fn smoke_ws_url_converts_http_and_https_schemes() {
        assert_eq!(smoke_ws_url("http://127.0.0.1:4100/"), "ws://127.0.0.1:4100/api/workshops/ws");
        assert_eq!(smoke_ws_url("https://dragon-switch.dev"), "wss://dragon-switch.dev/api/workshops/ws");
    }

    #[test]
    fn cargo_run_package_args_include_separator_only_when_needed() {
        assert_eq!(
            cargo_run_package_args("app-server", &[]),
            vec!["run".to_string(), "-p".to_string(), "app-server".to_string()]
        );
        assert_eq!(
            cargo_run_package_args("app-server", &["--port".to_string(), "4100".to_string()]),
            vec![
                "run".to_string(),
                "-p".to_string(),
                "app-server".to_string(),
                "--".to_string(),
                "--port".to_string(),
                "4100".to_string(),
            ]
        );
    }

    #[test]
    fn dx_args_target_app_web_platform() {
        assert_eq!(
            dx_serve_app_web_args(&["--release".to_string()]),
            vec![
                "serve".to_string(),
                "--package".to_string(),
                "app-web".to_string(),
                "--platform".to_string(),
                "web".to_string(),
                "--release".to_string(),
            ]
        );
    }

    #[test]
    fn preferred_dx_program_uses_explicit_env_override() {
        assert_eq!(
            preferred_dx_program(Some("/tmp/custom-dx"), Some("/Users/fingerbib")),
            "/tmp/custom-dx".to_string()
        );
    }

    #[test]
    fn preferred_dx_program_falls_back_to_plain_dx_without_override_or_installed_cargo_binary() {
        assert_eq!(
            preferred_dx_program(None, Some("/definitely/missing/home")),
            "dx".to_string()
        );
    }
}
