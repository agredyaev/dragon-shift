use futures_util::{SinkExt, StreamExt};
use sqlx::PgPool;
use protocol::{
    ClientWsMessage, CreateWorkshopRequest, JudgeBundle, JoinWorkshopRequest, Phase,
    ServerWsMessage, SessionCommand, SessionEnvelope, WorkshopCommandRequest,
    WorkshopCommandResult, WorkshopJudgeBundleRequest, WorkshopJudgeBundleResult,
    WorkshopJoinResult, WorkshopJoinSuccess,
};
use serde_json::json;
use std::fs;
use std::env;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message as WsMessage},
    MaybeTlsStream, WebSocketStream,
};

type SmokeWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PersistenceSmokeConfig {
    base_url: String,
    database_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WebBuildConfig {
    out_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JoinLoadConfig {
    base_url: String,
    clients: usize,
}

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
        "build-web" => build_web_bundle(build_web_config(&forwarded)?),
        "server" | "dev-server" => run_tool_owned("cargo", cargo_run_package_args("app-server", &forwarded)),
        "smoke-phase1" => run_async(smoke_phase1(smoke_base_url(&forwarded)?)),
        "smoke-join-load" => run_async(smoke_join_load(join_load_config(&forwarded)?)),
        "smoke-judge-bundle" => run_async(smoke_judge_bundle(smoke_base_url(&forwarded)?)),
        "smoke-offline-failover" => run_async(smoke_offline_failover(smoke_base_url(&forwarded)?)),
        "smoke-persistence" => run_async(smoke_persistence(persistence_smoke_config(&forwarded)?)),
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

fn join_load_config(forwarded: &[String]) -> Result<JoinLoadConfig, String> {
    let mut base_url = env::var("XTASK_SMOKE_BASE_URL")
        .or_else(|_| env::var("SMOKE_TEST_BASE_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:4100".to_string());
    let mut clients = 30usize;
    let mut args = forwarded.iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--base-url" => {
                base_url = args
                    .next()
                    .ok_or_else(|| "missing value for --base-url".to_string())?
                    .clone();
            }
            "--clients" => {
                clients = args
                    .next()
                    .ok_or_else(|| "missing value for --clients".to_string())?
                    .parse::<usize>()
                    .map_err(|error| format!("invalid value for --clients: {error}"))?;
            }
            unknown => return Err(format!("unknown join-load arg: {unknown}")),
        }
    }

    if clients == 0 {
        return Err("--clients must be greater than 0".to_string());
    }

    Ok(JoinLoadConfig {
        base_url: normalize_base_url(&base_url),
        clients,
    })
}

fn persistence_smoke_config(forwarded: &[String]) -> Result<PersistenceSmokeConfig, String> {
    let mut base_url = env::var("XTASK_SMOKE_BASE_URL")
        .or_else(|_| env::var("SMOKE_TEST_BASE_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:4100".to_string());
    let mut database_url = env::var("XTASK_PERSISTENCE_DATABASE_URL")
        .or_else(|_| env::var("DATABASE_URL"))
        .ok();
    let mut args = forwarded.iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--base-url" => {
                base_url = args
                    .next()
                    .ok_or_else(|| "missing value for --base-url".to_string())?
                    .clone();
            }
            "--database-url" => {
                database_url = Some(
                    args.next()
                        .ok_or_else(|| "missing value for --database-url".to_string())?
                        .clone(),
                );
            }
            unknown => return Err(format!("unknown persistence smoke arg: {unknown}")),
        }
    }

    let database_url = database_url
        .ok_or_else(|| "smoke-persistence requires --database-url or XTASK_PERSISTENCE_DATABASE_URL or DATABASE_URL".to_string())?;

    Ok(PersistenceSmokeConfig {
        base_url: normalize_base_url(&base_url),
        database_url: database_url.trim().to_string(),
    })
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

fn build_web_config(forwarded: &[String]) -> Result<WebBuildConfig, String> {
    let mut out_dir = workspace_root().join("app-web/dist");
    let mut args = forwarded.iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --out-dir".to_string())?;
                let path = PathBuf::from(value);
                out_dir = if path.is_absolute() {
                    path
                } else {
                    workspace_root().join(path)
                };
            }
            unknown => return Err(format!("unknown build-web arg: {unknown}")),
        }
    }

    Ok(WebBuildConfig { out_dir })
}

fn run_tool(program: &str, args: &[&str]) -> Result<(), String> {
    run_tool_owned(program, args.iter().map(|value| value.to_string()).collect())
}

fn build_web_bundle(config: WebBuildConfig) -> Result<(), String> {
    fs::create_dir_all(&config.out_dir)
        .map_err(|error| format!("failed to create app-web output dir {}: {error}", config.out_dir.display()))?;

    run_tool(
        "cargo",
        &["build", "--locked", "--release", "-p", "app-web", "--target", "wasm32-unknown-unknown"],
    )?;

    let wasm_input = workspace_root().join("target/wasm32-unknown-unknown/release/app-web.wasm");
    let wasm_bindgen_args = vec![
        "--target".to_string(),
        "web".to_string(),
        "--no-typescript".to_string(),
        "--out-name".to_string(),
        "app-web".to_string(),
        "--out-dir".to_string(),
        config.out_dir.to_string_lossy().into_owned(),
        wasm_input.to_string_lossy().into_owned(),
    ];
    run_tool_owned("wasm-bindgen", wasm_bindgen_args).map_err(|error| {
        format!(
            "{error}. Install wasm-bindgen-cli with `cargo install wasm-bindgen-cli --version 0.2.114 --locked` to use `cargo xtask build-web`."
        )
    })?;

    write_app_web_index_html(&config.out_dir)?;

    print_json(json!({
        "ok": true,
        "outDir": config.out_dir.display().to_string(),
        "entryHtml": config.out_dir.join("index.html").display().to_string(),
        "entryJs": config.out_dir.join("app-web.js").display().to_string(),
        "entryWasm": config.out_dir.join("app-web_bg.wasm").display().to_string(),
    }))
}

fn write_app_web_index_html(out_dir: &Path) -> Result<(), String> {
    let index_path = out_dir.join("index.html");
    fs::write(
        &index_path,
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Dragon Shift</title>
    <link rel="icon" href="data:," />
  </head>
  <body>
    <main id="main"></main>
    <script type="module">
      import init from "./app-web.js";
      init();
    </script>
  </body>
</html>
"#,
    )
    .map_err(|error| format!("failed to write {}: {error}", index_path.display()))
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

async fn smoke_join_load(config: JoinLoadConfig) -> Result<(), String> {
    let client = reqwest::Client::new();
    let host = create_workshop(&client, &config.base_url, "XtaskLoadHost").await?;
    let requested_clients = config.clients;
    let join_results = futures_util::future::join_all((0..requested_clients).map(|index| {
        let client = client.clone();
        let base_url = config.base_url.clone();
        let session_code = host.session_code.clone();
        let player_name = format!("LoadPlayer{:02}", index + 1);

        async move { join_workshop(&client, &base_url, &session_code, &player_name).await }
    }))
    .await;

    for join_result in join_results {
        join_result?;
    }

    let host_after_load = reconnect_workshop(&client, &config.base_url, &host).await?;
    let total_players = host_after_load.state.players.len();
    let connected_players = host_after_load
        .state
        .players
        .values()
        .filter(|player| player.is_connected)
        .count();
    let expected_total_players = requested_clients + 1;

    if total_players != expected_total_players {
        return Err(format!(
            "expected {expected_total_players} total players after load join, got {total_players}"
        ));
    }
    if connected_players != expected_total_players {
        return Err(format!(
            "expected {expected_total_players} connected players after load join, got {connected_players}"
        ));
    }

    print_json(json!({
        "ok": true,
        "baseUrl": config.base_url,
        "sessionCode": host.session_code,
        "hostPlayerId": host.player_id,
        "joinedClients": requested_clients,
        "totalPlayers": total_players,
        "connectedPlayers": connected_players,
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

async fn smoke_persistence(config: PersistenceSmokeConfig) -> Result<(), String> {
    let client = reqwest::Client::new();
    let host = create_workshop(&client, &config.base_url, "XtaskPersistenceHost").await?;

    send_command(
        &client,
        &config.base_url,
        command_request(&host, SessionCommand::StartPhase1, None),
    )
    .await?;

    let report = query_persistence_report(&config.database_url, &host.session_code, &host.reconnect_token)
        .await?;
    if report.persisted_phase != "phase1" {
        return Err(format!(
            "expected persisted workshop phase to be phase1, got {}",
            report.persisted_phase
        ));
    }
    if report.artifact_count < 2 {
        return Err(format!(
            "expected at least 2 persisted artifacts, got {}",
            report.artifact_count
        ));
    }
    if report.identity_player_id != host.player_id {
        return Err(format!(
            "persisted identity player mismatch: expected {}, got {}",
            host.player_id, report.identity_player_id
        ));
    }

    print_json(json!({
        "ok": true,
        "baseUrl": config.base_url,
        "sessionCode": host.session_code,
        "persistedPhase": report.persisted_phase,
        "artifactCount": report.artifact_count,
        "identityPlayerId": report.identity_player_id,
        "tablesChecked": ["workshop_sessions", "session_artifacts", "player_identities"],
    }))
}

struct PersistenceReport {
    persisted_phase: String,
    artifact_count: i64,
    identity_player_id: String,
}

async fn query_persistence_report(
    database_url: &str,
    session_code: &str,
    reconnect_token: &str,
) -> Result<PersistenceReport, String> {
    let pool = PgPool::connect(database_url)
        .await
        .map_err(|error| format!("failed to connect to persistence database: {error}"))?;

    let (session_id, payload): (String, serde_json::Value) = sqlx::query_as(
            "SELECT session_id, payload FROM workshop_sessions WHERE session_code = $1",
        )
        .bind(session_code)
        .fetch_optional(&pool)
        .await
        .map_err(|error| format!("failed to query workshop_sessions: {error}"))?
        .ok_or_else(|| format!("no persisted workshop_sessions row found for session code {session_code}"))?;
    let persisted_phase = payload
        .get("phase")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("persisted workshop payload is missing string phase: {payload}"))?
        .to_string();

    let (artifact_count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM session_artifacts WHERE session_id = $1",
        )
        .bind(&session_id)
        .fetch_one(&pool)
        .await
        .map_err(|error| format!("failed to query session_artifacts: {error}"))?;

    let (identity_player_id,): (String,) = sqlx::query_as(
            "SELECT player_id FROM player_identities WHERE reconnect_token = $1 AND session_id = $2",
        )
        .bind(reconnect_token)
        .bind(&session_id)
        .fetch_optional(&pool)
        .await
        .map_err(|error| format!("failed to query player_identities: {error}"))?
        .ok_or_else(|| format!("no persisted player_identities row found for reconnect token {reconnect_token}"))?;

    Ok(PersistenceReport {
        persisted_phase,
        artifact_count,
        identity_player_id,
    })
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
  cargo xtask build-web -- [--out-dir app-web/dist]
  cargo xtask server -- [app-server args]
  cargo xtask smoke-phase1 -- [--base-url http://127.0.0.1:4100]
  cargo xtask smoke-join-load -- [--base-url http://127.0.0.1:4100] [--clients 30]
  cargo xtask smoke-judge-bundle -- [--base-url http://127.0.0.1:4100]
  cargo xtask smoke-offline-failover -- [--base-url http://127.0.0.1:4100]
  cargo xtask smoke-persistence -- [--base-url http://127.0.0.1:4100] [--database-url postgres://...]"
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
    fn join_load_config_defaults_to_thirty_clients() {
        let config = join_load_config(&[]).expect("join load config");
        assert_eq!(config.base_url, "http://127.0.0.1:4100");
        assert_eq!(config.clients, 30);
    }

    #[test]
    fn join_load_config_uses_forwarded_flags() {
        let config = join_load_config(&[
            "--base-url".to_string(),
            "http://localhost:4300/".to_string(),
            "--clients".to_string(),
            "12".to_string(),
        ])
        .expect("join load config with flags");
        assert_eq!(config.base_url, "http://localhost:4300");
        assert_eq!(config.clients, 12);
    }

    #[test]
    fn smoke_ws_url_converts_http_and_https_schemes() {
        assert_eq!(smoke_ws_url("http://127.0.0.1:4100/"), "ws://127.0.0.1:4100/api/workshops/ws");
        assert_eq!(smoke_ws_url("https://dragon-switch.dev"), "wss://dragon-switch.dev/api/workshops/ws");
    }

    #[test]
    fn persistence_smoke_config_uses_forwarded_flag() {
        let previous = env::var("XTASK_PERSISTENCE_DATABASE_URL").ok();
        unsafe {
            env::set_var("XTASK_PERSISTENCE_DATABASE_URL", "postgres://env-user:env-pass@env-host:5432/env-db");
        }

        let config = persistence_smoke_config(&[
            "--base-url".to_string(),
            "http://localhost:4300/".to_string(),
            "--database-url".to_string(),
            "postgres://cli-user:cli-pass@cli-host:5432/cli-db".to_string(),
        ])
        .expect("persistence config");

        assert_eq!(config.base_url, "http://localhost:4300");
        assert_eq!(config.database_url, "postgres://cli-user:cli-pass@cli-host:5432/cli-db");

        unsafe {
            if let Some(value) = previous {
                env::set_var("XTASK_PERSISTENCE_DATABASE_URL", value);
            } else {
                env::remove_var("XTASK_PERSISTENCE_DATABASE_URL");
            }
        }
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
    fn build_web_config_defaults_to_workspace_dist() {
        let config = build_web_config(&[]).expect("default build web config");
        assert_eq!(config.out_dir, workspace_root().join("app-web/dist"));
    }

    #[test]
    fn build_web_config_uses_forwarded_out_dir() {
        let config = build_web_config(&["--out-dir".to_string(), "custom-dist".to_string()])
            .expect("forwarded build web config");
        assert_eq!(
            config.out_dir,
            workspace_root().join("custom-dist")
        );
    }
}
