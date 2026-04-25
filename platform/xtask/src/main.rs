use futures_util::{SinkExt, StreamExt};
use protocol::{
    AuthRequest, ClientWsMessage, CreateWorkshopRequest, JoinWorkshopRequest, JudgeBundle, Phase,
    ServerWsMessage, SessionCommand, SessionEnvelope, SpriteSheetRequest, SpriteSheetResult,
    WorkshopCommandRequest, WorkshopCommandResult, WorkshopJoinResult, WorkshopJoinSuccess,
    WorkshopJudgeBundleRequest, WorkshopJudgeBundleResult,
};
use serde_json::json;
use sqlx::PgPool;
use std::env;
use std::fs;
use std::future::Future;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message as WsMessage, client::IntoClientRequest, http::HeaderValue},
};

type SmokeWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PersistenceSmokeConfig {
    base_url: String,
    database_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RestoreSmokeConfig {
    base_url: String,
    database_url: String,
    restart_timeout_seconds: u64,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpriteLoadConfig {
    base_url: String,
    workers: usize,
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
        "clippy" => run_tool(
            "cargo",
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ),
        "app-web-test" => run_tool("cargo", &["test", "-p", "app-web"]),
        "app-server-test" => run_tool("cargo", &["test", "-p", "app-server"]),
        "build-web" => build_web_bundle(build_web_config(&forwarded)?),
        "server" | "dev-server" => {
            run_tool_owned("cargo", cargo_run_package_args("app-server", &forwarded))
        }
        "smoke-phase1" => run_async(smoke_phase1(smoke_base_url(&forwarded)?)),
        "smoke-join-load" => run_async(smoke_join_load(join_load_config(&forwarded)?)),
        "smoke-sprite-load" => run_async(smoke_sprite_load(sprite_load_config(&forwarded)?)),
        "smoke-judge-bundle" => run_async(smoke_judge_bundle(smoke_base_url(&forwarded)?)),
        "smoke-offline-failover" => run_async(smoke_offline_failover(smoke_base_url(&forwarded)?)),
        "smoke-persistence" => run_async(smoke_persistence(persistence_smoke_config(&forwarded)?)),
        "smoke-persistence-restart" => run_async(smoke_persistence_restart(
            persistence_smoke_config(&forwarded)?,
        )),
        "smoke-restore-reconnect" => {
            run_async(smoke_restore_reconnect(restore_smoke_config(&forwarded)?))
        }
        unknown => Err(format!("unknown xtask command: {unknown}")),
    }
}

struct AppServerProcess {
    child: Option<Child>,
}

impl AppServerProcess {
    fn spawn(base_url: &str, database_url: &str) -> Result<Self, String> {
        ensure_default_app_web_bundle()?;

        let child = Command::new(app_server_binary_path())
            .current_dir(workspace_root())
            .env("APP_SERVER_BIND_ADDR", app_server_bind_addr(base_url)?)
            .env("DATABASE_URL", database_url)
            .env("APP_SERVER_STATIC_DIR", app_web_dist_dir())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("failed to start app-server: {error}"))?;

        Ok(Self { child: Some(child) })
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>, String> {
        self.child
            .as_mut()
            .expect("app-server child process")
            .try_wait()
            .map_err(|error| format!("failed to check app-server status: {error}"))
    }

    fn shutdown(&mut self) -> Result<(), String> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };

        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(error) => {
                return Err(format!(
                    "failed to check app-server status before shutdown: {error}"
                ));
            }
        }

        child
            .kill()
            .map_err(|error| format!("failed to stop app-server: {error}"))?;
        child
            .wait()
            .map_err(|error| format!("failed to wait for app-server shutdown: {error}"))?;
        Ok(())
    }
}

impl Drop for AppServerProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                Err(_) => {}
            }
        }
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask workspace root")
        .to_path_buf()
}

fn app_web_static_dir() -> PathBuf {
    workspace_root().join("app-web/static")
}

fn app_web_dist_dir() -> PathBuf {
    workspace_root().join("app-web/dist")
}

fn ensure_default_app_web_bundle() -> Result<(), String> {
    let out_dir = app_web_dist_dir();
    let required_paths = [
        out_dir.join("index.html"),
        out_dir.join("app-web.js"),
        out_dir.join("app-web_bg.wasm"),
        out_dir.join("style.css"),
        out_dir.join("fonts"),
    ];

    if required_paths.iter().all(|path| path.exists()) {
        return Ok(());
    }

    build_web_bundle(WebBuildConfig { out_dir })
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
    let mut clients = 4usize;
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

fn sprite_load_config(forwarded: &[String]) -> Result<SpriteLoadConfig, String> {
    let mut base_url = env::var("XTASK_SMOKE_BASE_URL")
        .or_else(|_| env::var("SMOKE_TEST_BASE_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:4100".to_string());
    let mut workers = 40usize;
    let mut args = forwarded.iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--base-url" => {
                base_url = args
                    .next()
                    .ok_or_else(|| "missing value for --base-url".to_string())?
                    .clone();
            }
            "--workers" => {
                workers = args
                    .next()
                    .ok_or_else(|| "missing value for --workers".to_string())?
                    .parse::<usize>()
                    .map_err(|error| format!("invalid value for --workers: {error}"))?;
            }
            unknown => return Err(format!("unknown sprite-load arg: {unknown}")),
        }
    }

    if workers == 0 {
        return Err("--workers must be greater than 0".to_string());
    }

    Ok(SpriteLoadConfig {
        base_url: normalize_base_url(&base_url),
        workers,
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

fn restore_smoke_config(forwarded: &[String]) -> Result<RestoreSmokeConfig, String> {
    let mut base_url = env::var("XTASK_SMOKE_BASE_URL")
        .or_else(|_| env::var("SMOKE_TEST_BASE_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:4100".to_string());
    let mut database_url = env::var("XTASK_PERSISTENCE_DATABASE_URL")
        .or_else(|_| env::var("DATABASE_URL"))
        .ok();
    let mut restart_timeout_seconds = 300u64;
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
            "--restart-timeout-seconds" => {
                restart_timeout_seconds = args
                    .next()
                    .ok_or_else(|| "missing value for --restart-timeout-seconds".to_string())?
                    .parse::<u64>()
                    .map_err(|error| {
                        format!("invalid value for --restart-timeout-seconds: {error}")
                    })?;
            }
            unknown => return Err(format!("unknown restore smoke arg: {unknown}")),
        }
    }

    if restart_timeout_seconds == 0 {
        return Err("--restart-timeout-seconds must be greater than 0".to_string());
    }

    let database_url = database_url
        .ok_or_else(|| "smoke-restore-reconnect requires --database-url or XTASK_PERSISTENCE_DATABASE_URL or DATABASE_URL".to_string())?;

    Ok(RestoreSmokeConfig {
        base_url: normalize_base_url(&base_url),
        database_url: database_url.trim().to_string(),
        restart_timeout_seconds,
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

fn app_server_binary_path() -> PathBuf {
    workspace_root().join("target/debug/app-server")
}

fn app_server_bind_addr(base_url: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(base_url)
        .map_err(|error| format!("invalid base url for managed app-server: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("managed app-server requires an http or https base url".to_string());
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err("managed app-server base url must not include a path".to_string());
    }

    let host = url
        .host_str()
        .ok_or_else(|| "managed app-server base url is missing a host".to_string())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "managed app-server base url is missing a port".to_string())?;
    let bind_host = if host.starts_with('[') || !host.contains(':') {
        host.to_string()
    } else {
        format!("[{host}]")
    };

    Ok(format!("{bind_host}:{port}"))
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
    run_tool_owned(
        program,
        args.iter().map(|value| value.to_string()).collect(),
    )
}

fn build_web_bundle(config: WebBuildConfig) -> Result<(), String> {
    let parent_dir = config
        .out_dir
        .parent()
        .ok_or_else(|| {
            format!(
                "app-web output dir has no parent: {}",
                config.out_dir.display()
            )
        })?
        .to_path_buf();
    let staging_dir = parent_dir.join(format!(
        ".{}-staging",
        config
            .out_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("app-web-dist")
    ));
    let backup_dir = parent_dir.join(format!(
        ".{}-backup",
        config
            .out_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("app-web-dist")
    ));
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir).map_err(|error| {
            format!(
                "failed to clear temporary app-web output dir {}: {error}",
                staging_dir.display()
            )
        })?;
    }

    fs::create_dir_all(&staging_dir).map_err(|error| {
        format!(
            "failed to create temporary app-web output dir {}: {error}",
            staging_dir.display()
        )
    })?;

    run_tool(
        "cargo",
        &[
            "build",
            "--locked",
            "--profile",
            "wasm-release",
            "-p",
            "app-web",
            "--target",
            "wasm32-unknown-unknown",
        ],
    )?;

    let wasm_input =
        workspace_root().join("target/wasm32-unknown-unknown/wasm-release/app-web.wasm");
    let wasm_bindgen_args = vec![
        "--target".to_string(),
        "web".to_string(),
        "--no-typescript".to_string(),
        "--out-name".to_string(),
        "app-web".to_string(),
        "--out-dir".to_string(),
        staging_dir.to_string_lossy().into_owned(),
        wasm_input.to_string_lossy().into_owned(),
    ];
    run_tool_owned("wasm-bindgen", wasm_bindgen_args).map_err(|error| {
        format!(
            "{error}. Install wasm-bindgen-cli with `cargo install wasm-bindgen-cli --version 0.2.115 --locked` to use `cargo xtask build-web`."
        )
    })?;

    let wasm_bg = staging_dir.join("app-web_bg.wasm");
    let raw_wasm_bytes = fs::metadata(&wasm_input).map(|m| m.len()).unwrap_or(0);
    let bindgen_wasm_bytes = fs::metadata(&wasm_bg).map(|m| m.len()).unwrap_or(0);
    let skip_wasm_opt = env_flag_enabled("XTASK_SKIP_WASM_OPT");
    let post_opt_bytes = if skip_wasm_opt {
        eprintln!("wasm-opt skipped because XTASK_SKIP_WASM_OPT is set");
        bindgen_wasm_bytes
    } else {
        run_tool_owned(
            "wasm-opt",
            vec![
                "-Oz".to_string(),
                "--enable-bulk-memory".to_string(),
                "--enable-nontrapping-float-to-int".to_string(),
                "--strip-debug".to_string(),
                "--strip-producers".to_string(),
                "-o".to_string(),
                wasm_bg.to_string_lossy().into_owned(),
                wasm_bg.to_string_lossy().into_owned(),
            ],
        )
        .map_err(|error| {
            format!(
                "{error}. Install binaryen with `brew install binaryen` (or your system package manager) to enable wasm-opt, or set XTASK_SKIP_WASM_OPT=1 to skip the optimization step."
            )
        })?;
        let post_opt_bytes = fs::metadata(&wasm_bg).map(|m| m.len()).unwrap_or(0);
        eprintln!(
            "wasm-opt: {} KB -> {} KB",
            bindgen_wasm_bytes / 1024,
            post_opt_bytes / 1024,
        );
        post_opt_bytes
    };

    let bindgen_saved_pct = saved_percent(raw_wasm_bytes, bindgen_wasm_bytes);
    let opt_saved_pct = saved_percent(bindgen_wasm_bytes, post_opt_bytes);
    let total_saved_pct = saved_percent(raw_wasm_bytes, post_opt_bytes);

    copy_app_web_static_assets(&staging_dir)?;
    write_app_web_index_html(&staging_dir)?;

    // ── WASM bundle size gate ──────────────────────────────────────────
    let wasm_output = staging_dir.join("app-web_bg.wasm");
    let wasm_size_bytes = fs::metadata(&wasm_output).map(|m| m.len()).unwrap_or(0);
    let wasm_size_kb = wasm_size_bytes / 1024;
    const WASM_SIZE_WARN_KB: u64 = 2048; // 2 MB — warn threshold
    const WASM_SIZE_FAIL_KB: u64 = 3072; // 3 MB — hard fail threshold
    if wasm_size_kb > WASM_SIZE_FAIL_KB {
        return Err(format!(
            "WASM bundle size regression: {wasm_size_kb} KB exceeds hard limit of {WASM_SIZE_FAIL_KB} KB"
        ));
    }
    let wasm_size_warning = if wasm_size_kb > WASM_SIZE_WARN_KB {
        Some(format!(
            "WASM bundle is {wasm_size_kb} KB — approaching {WASM_SIZE_FAIL_KB} KB limit"
        ))
    } else {
        None
    };

    let js_output = staging_dir.join("app-web.js");
    let js_size_bytes = fs::metadata(&js_output).map(|m| m.len()).unwrap_or(0);

    if backup_dir.exists() {
        fs::remove_dir_all(&backup_dir).map_err(|error| {
            format!(
                "failed to clear app-web backup dir {}: {error}",
                backup_dir.display()
            )
        })?;
    }

    fs::create_dir_all(&parent_dir).map_err(|error| {
        format!(
            "failed to create app-web output parent dir {}: {error}",
            parent_dir.display()
        )
    })?;

    let had_previous_out_dir = config.out_dir.exists();
    if had_previous_out_dir {
        fs::rename(&config.out_dir, &backup_dir).map_err(|error| {
            format!(
                "failed to move existing app-web bundle out of the way {} -> {}: {error}",
                config.out_dir.display(),
                backup_dir.display()
            )
        })?;
    }

    if let Err(error) = fs::rename(&staging_dir, &config.out_dir) {
        if had_previous_out_dir {
            let restore_result = fs::rename(&backup_dir, &config.out_dir);
            if let Err(restore_error) = restore_result {
                return Err(format!(
                    "failed to move built app-web bundle into {}: {error}; also failed to restore previous bundle: {restore_error}",
                    config.out_dir.display()
                ));
            }
        } else if error.kind() != ErrorKind::NotFound && backup_dir.exists() {
            let _ = fs::remove_dir_all(&backup_dir);
        }
        return Err(format!(
            "failed to move built app-web bundle into {}: {error}",
            config.out_dir.display()
        ));
    }

    if backup_dir.exists() {
        fs::remove_dir_all(&backup_dir).map_err(|error| {
            format!(
                "failed to remove app-web backup dir {}: {error}",
                backup_dir.display()
            )
        })?;
    }

    let mut result = json!({
        "ok": true,
        "outDir": config.out_dir.display().to_string(),
        "entryHtml": config.out_dir.join("index.html").display().to_string(),
        "entryJs": config.out_dir.join("app-web.js").display().to_string(),
        "entryWasm": config.out_dir.join("app-web_bg.wasm").display().to_string(),
        "wasmRawKb": raw_wasm_bytes / 1024,
        "wasmBindgenKb": bindgen_wasm_bytes / 1024,
        "wasmSizeKb": wasm_size_kb,
        "wasmBindgenSavedPct": bindgen_saved_pct,
        "wasmOptSavedPct": opt_saved_pct,
        "wasmTotalSavedPct": total_saved_pct,
        "wasmOptSkipped": skip_wasm_opt,
        "jsSizeKb": js_size_bytes / 1024,
    });
    if let Some(warning) = wasm_size_warning {
        result["wasmSizeWarning"] = json!(warning);
    }

    print_json(result)
}

fn saved_percent(before: u64, after: u64) -> u64 {
    if before == 0 {
        0
    } else {
        ((before as f64 - after as f64) / before as f64 * 100.0) as u64
    }
}

fn env_flag_enabled(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn write_app_web_index_html(out_dir: &Path) -> Result<(), String> {
    let index_path = out_dir.join("index.html");
    let cache_bust = cache_bust_token(out_dir)?;
    fs::write(
        &index_path,
        format!(
            r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Dragon Shift</title>
    <link rel="icon" href="data:," />
    <link rel="preload" href="fonts/silkscreen-400-latin.woff2" as="font" type="font/woff2" crossorigin />
    <link rel="preload" href="fonts/silkscreen-700-latin.woff2" as="font" type="font/woff2" crossorigin />
    <link rel="stylesheet" href="style.css?v={cache_bust}" />
  </head>
  <body>
    <div id="main"></div>
    <script type="module">
      import init from "./app-web.js?v={cache_bust}";
      init(new URL("./app-web_bg.wasm?v={cache_bust}", import.meta.url));
    </script>
  </body>
</html>
"#
        ),
    )
    .map_err(|error| format!("failed to write {}: {error}", index_path.display()))
}

fn cache_bust_token(out_dir: &Path) -> Result<String, String> {
    let js_metadata = fs::metadata(out_dir.join("app-web.js"))
        .map_err(|error| format!("failed to stat app-web.js for cache busting: {error}"))?;
    let wasm_metadata = fs::metadata(out_dir.join("app-web_bg.wasm"))
        .map_err(|error| format!("failed to stat app-web_bg.wasm for cache busting: {error}"))?;
    let css_metadata = fs::metadata(out_dir.join("style.css"))
        .map_err(|error| format!("failed to stat style.css for cache busting: {error}"))?;

    Ok(format!(
        "{}-{}-{}",
        js_metadata.len(),
        wasm_metadata.len(),
        css_metadata.len()
    ))
}

fn copy_app_web_static_assets(out_dir: &Path) -> Result<(), String> {
    let static_dir = app_web_static_dir();
    if !static_dir.is_dir() {
        return Err(format!(
            "app-web static assets directory is missing: {}",
            static_dir.display()
        ));
    }

    copy_dir_contents(&static_dir, out_dir)
}

fn copy_dir_contents(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| {
        format!(
            "failed to create static asset dir {}: {error}",
            destination.display()
        )
    })?;

    for entry in fs::read_dir(source)
        .map_err(|error| format!("failed to read {}: {error}", source.display()))?
    {
        let entry = entry
            .map_err(|error| format!("failed to read entry under {}: {error}", source.display()))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());

        if entry
            .file_type()
            .map_err(|error| format!("failed to inspect {}: {error}", source_path.display()))?
            .is_dir()
        {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else {
            copy_file(&source_path, &destination_path)?;
        }
    }

    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), String> {
    fs::copy(source, destination).map_err(|error| {
        format!(
            "failed to copy {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })?;

    Ok(())
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| {
        format!(
            "failed to create static asset dir {}: {error}",
            destination.display()
        )
    })?;

    for entry in fs::read_dir(source)
        .map_err(|error| format!("failed to read {}: {error}", source.display()))?
    {
        let entry = entry
            .map_err(|error| format!("failed to read entry under {}: {error}", source.display()))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());

        if entry
            .file_type()
            .map_err(|error| format!("failed to inspect {}: {error}", source_path.display()))?
            .is_dir()
        {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else {
            copy_file(&source_path, &destination_path)?;
        }
    }

    Ok(())
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
        Err(format!(
            "`{program} {}` exited with status {status}",
            args.join(" ")
        ))
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
        Err(format!(
            "{label}: expected {:?}, got {:?}",
            expected, session.state.phase
        ))
    }
}

fn print_json(value: serde_json::Value) -> Result<(), String> {
    let encoded = serde_json::to_string_pretty(&value)
        .map_err(|error| format!("failed to encode xtask output: {error}"))?;
    println!("{encoded}");
    Ok(())
}

async fn smoke_phase1(base_url: String) -> Result<(), String> {
    let smoke_start = std::time::Instant::now();
    let client = smoke_http_client()?;

    let t0 = std::time::Instant::now();
    let host = create_workshop(&client, &base_url, "XtaskSmokeHost").await?;
    let create_ms = t0.elapsed().as_millis();

    let t1 = std::time::Instant::now();
    send_command(
        &client,
        &base_url,
        command_request(&host, SessionCommand::StartPhase1, None),
    )
    .await?;
    let command_ms = t1.elapsed().as_millis();

    let t2 = std::time::Instant::now();
    let reconnected = reconnect_workshop(&client, &base_url, &host).await?;
    let reconnect_ms = t2.elapsed().as_millis();

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
        "timing": {
            "totalMs": smoke_start.elapsed().as_millis() as u64,
            "createWorkshopMs": create_ms as u64,
            "startPhase1CommandMs": command_ms as u64,
            "reconnectMs": reconnect_ms as u64,
        },
    }))
}

async fn smoke_join_load(config: JoinLoadConfig) -> Result<(), String> {
    if config.clients > 4 {
        return Err(
            "smoke-join-load currently supports at most 4 guest clients under cookie-auth rate limits"
                .to_string(),
        );
    }

    let client = smoke_http_client()?;
    let host = create_workshop(&client, &config.base_url, "XtaskLoadHost").await?;
    let requested_clients = config.clients;
    let join_results = futures_util::future::join_all((0..requested_clients).map(|index| {
        let base_url = config.base_url.clone();
        let session_code = host.session_code.clone();
        let player_name = format!("LoadPlayer{:02}", index + 1);

        async move {
            let client = smoke_http_client()?;
            signin_account(&client, &base_url, &player_name).await?;
            join_workshop(&client, &base_url, &session_code, &player_name).await
        }
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

async fn smoke_sprite_load(config: SpriteLoadConfig) -> Result<(), String> {
    let total_start = std::time::Instant::now();
    let client = smoke_http_client()?;
    let workers = config.workers;

    // Reuse a small signed-in account pool so the smoke can stay within the
    // public signup rate limit while still exercising the create-lobby and
    // explicit-join flow.
    let account_pool_size = workers.min(5);
    let mut account_clients = Vec::with_capacity(account_pool_size);
    for index in 0..account_pool_size {
        let name = format!("SpriteWorkerAccount{:02}", index + 1);
        account_clients.push(sign_in_smoke_client(&config.base_url, &name).await?);
    }

    // Full account flow: signed-in account → create empty lobby → explicit join.
    let mut sessions: Vec<WorkshopJoinSuccess> = Vec::with_capacity(workers);
    for index in 0..workers {
        let account_client = &account_clients[index % account_clients.len()];
        let host = create_workshop_on_client(account_client, &config.base_url).await?;
        sessions.push(host);
        // Pace create calls to stay under the public create limiter.
        tokio::time::sleep(std::time::Duration::from_millis(3_100)).await;
    }
    let setup_ms = total_start.elapsed().as_millis();

    // Phase 2: fire all sprite-sheet requests concurrently.

    let gen_start = std::time::Instant::now();
    let sprite_results =
        futures_util::future::join_all(sessions.iter().enumerate().map(|(index, session)| {
            let client = client.clone();
            let base_url = config.base_url.clone();
            let description = format!("Smoke test dragon {}", index + 1);
            let session_code = session.session_code.clone();
            let reconnect_token = session.reconnect_token.clone();

            async move {
                let t0 = std::time::Instant::now();
                let result = client
                    .post(format!("{base_url}/api/workshops/sprite-sheet"))
                    .header("Origin", &base_url)
                    .json(&SpriteSheetRequest {
                        session_code,
                        reconnect_token,
                        description,
                    })
                    .send()
                    .await;

                let elapsed_ms = t0.elapsed().as_millis();
                match result {
                    Ok(response) => {
                        let status = response.status().as_u16();
                        let body = response.json::<SpriteSheetResult>().await;
                        match body {
                            Ok(SpriteSheetResult::Success(success)) => {
                                let sprite_sizes: Vec<usize> = [
                                    &success.sprites.neutral,
                                    &success.sprites.happy,
                                    &success.sprites.angry,
                                    &success.sprites.sleepy,
                                ]
                                .iter()
                                .map(|s| s.len())
                                .collect();
                                (index, elapsed_ms, status, Ok(sprite_sizes))
                            }
                            Ok(SpriteSheetResult::Error(err)) => {
                                (index, elapsed_ms, status, Err(err.error))
                            }
                            Err(parse_err) => (
                                index,
                                elapsed_ms,
                                status,
                                Err(format!("response parse error: {parse_err}")),
                            ),
                        }
                    }
                    Err(req_err) => (
                        index,
                        elapsed_ms,
                        0,
                        Err(format!("request error: {req_err}")),
                    ),
                }
            }
        }))
        .await;

    let gen_ms = gen_start.elapsed().as_millis();

    // Phase 3: summarize results.
    let mut successes = 0u64;
    let mut failures = 0u64;
    let mut success_times: Vec<u128> = Vec::new();
    let mut failure_details: Vec<serde_json::Value> = Vec::new();

    for (index, elapsed_ms, status, result) in &sprite_results {
        match result {
            Ok(_sizes) => {
                successes += 1;
                success_times.push(*elapsed_ms);
            }
            Err(error) => {
                failures += 1;
                failure_details.push(json!({
                    "worker": index,
                    "status": status,
                    "elapsedMs": elapsed_ms,
                    "error": error,
                }));
            }
        }
    }

    success_times.sort();
    let p50 = success_times
        .get(success_times.len() / 2)
        .copied()
        .unwrap_or(0);
    let p95 = success_times
        .get((success_times.len() as f64 * 0.95) as usize)
        .copied()
        .unwrap_or(0);
    let p99 = success_times
        .get((success_times.len() as f64 * 0.99) as usize)
        .copied()
        .unwrap_or(0);
    let max = success_times.last().copied().unwrap_or(0);
    let min = success_times.first().copied().unwrap_or(0);

    let total_ms = total_start.elapsed().as_millis();

    print_json(json!({
        "ok": failures == 0,
        "baseUrl": config.base_url,
        "workers": workers,
        "setupMs": setup_ms,
        "generationMs": gen_ms,
        "totalMs": total_ms,
        "successes": successes,
        "failures": failures,
        "latency": {
            "minMs": min,
            "p50Ms": p50,
            "p95Ms": p95,
            "p99Ms": p99,
            "maxMs": max,
        },
        "failureDetails": failure_details,
    }))
}

async fn smoke_judge_bundle(base_url: String) -> Result<(), String> {
    let smoke_start = std::time::Instant::now();
    let client = smoke_http_client()?;
    let mut host = create_workshop(&client, &base_url, "XtaskJudgeHost").await?;
    let mut guest =
        join_workshop(&client, &base_url, &host.session_code, "XtaskJudgeGuest").await?;

    send_command(
        &client,
        &base_url,
        command_request(&host, SessionCommand::StartPhase1, None),
    )
    .await?;
    guest = reconnect_workshop(&client, &base_url, &guest).await?;
    host = reconnect_workshop(&client, &base_url, &host).await?;
    ensure_phase(&host, Phase::Phase1, "host phase1")?;
    ensure_phase(&guest, Phase::Phase1, "guest phase1")?;

    send_command(
        &client,
        &base_url,
        command_request(
            &host,
            SessionCommand::SubmitObservation,
            Some(json!({ "text": "Calms down at dusk" })),
        ),
    )
    .await?;
    send_command(
        &client,
        &base_url,
        command_request(
            &guest,
            SessionCommand::SubmitObservation,
            Some(json!({ "text": "Rejects fruit at night" })),
        ),
    )
    .await?;
    send_command(
        &client,
        &base_url,
        command_request(&host, SessionCommand::StartHandover, None),
    )
    .await?;
    send_command(
        &client,
        &base_url,
        command_request(
            &host,
            SessionCommand::SubmitTags,
            Some(json!(["Rule 1", "Rule 2", "Rule 3"])),
        ),
    )
    .await?;
    send_command(
        &client,
        &base_url,
        command_request(
            &guest,
            SessionCommand::SubmitTags,
            Some(json!(["Rule A", "Rule B", "Rule C"])),
        ),
    )
    .await?;
    send_command(
        &client,
        &base_url,
        command_request(&host, SessionCommand::StartPhase2, None),
    )
    .await?;

    host = reconnect_workshop(&client, &base_url, &host).await?;
    ensure_phase(&host, Phase::Phase2, "host phase2")?;
    send_command(
        &client,
        &base_url,
        command_request(
            &host,
            SessionCommand::Action,
            Some(json!({ "type": "sleep" })),
        ),
    )
    .await?;
    send_command(
        &client,
        &base_url,
        command_request(&host, SessionCommand::EndGame, None),
    )
    .await?;

    guest = reconnect_workshop(&client, &base_url, &guest).await?;
    host = reconnect_workshop(&client, &base_url, &host).await?;
    ensure_phase(&host, Phase::Voting, "host voting")?;
    ensure_phase(&guest, Phase::Voting, "guest voting")?;
    let host_vote_target = assigned_dragon_id(&guest)?;
    let guest_vote_target = assigned_dragon_id(&host)?;

    send_command(
        &client,
        &base_url,
        command_request(
            &host,
            SessionCommand::SubmitVote,
            Some(json!({ "dragonId": host_vote_target })),
        ),
    )
    .await?;
    send_command(
        &client,
        &base_url,
        command_request(
            &guest,
            SessionCommand::SubmitVote,
            Some(json!({ "dragonId": guest_vote_target })),
        ),
    )
    .await?;
    send_command(
        &client,
        &base_url,
        command_request(&host, SessionCommand::RevealVotingResults, None),
    )
    .await?;

    host = reconnect_workshop(&client, &base_url, &host).await?;
    ensure_phase(&host, Phase::End, "host end")?;
    let t_bundle = std::time::Instant::now();
    let bundle = fetch_judge_bundle(&client, &base_url, &host).await?;
    let fetch_bundle_ms = t_bundle.elapsed().as_millis();
    if bundle.artifact_count <= 0 {
        return Err("judge bundle did not include artifacts".to_string());
    }
    if !bundle
        .dragons
        .iter()
        .any(|dragon| !dragon.handover_chain.creator_instructions.is_empty())
    {
        return Err("judge bundle is missing creator instructions".to_string());
    }
    if !bundle
        .dragons
        .iter()
        .any(|dragon| !dragon.handover_chain.discovery_observations.is_empty())
    {
        return Err("judge bundle is missing discovery observations".to_string());
    }
    if !bundle
        .dragons
        .iter()
        .any(|dragon| dragon.handover_chain.handover_tags.len() == 3)
    {
        return Err("judge bundle is missing completed handover tags".to_string());
    }
    if !bundle
        .dragons
        .iter()
        .any(|dragon| !dragon.phase2_actions.is_empty())
    {
        return Err("judge bundle is missing captured phase2 actions".to_string());
    }

    print_json(json!({
        "ok": true,
        "baseUrl": base_url,
        "sessionCode": host.session_code,
        "phase": "end",
        "dragons": bundle.dragons.len(),
        "artifactCount": bundle.artifact_count,
        "timing": {
            "totalMs": smoke_start.elapsed().as_millis() as u64,
            "fetchJudgeBundleMs": fetch_bundle_ms as u64,
        },
    }))
}

async fn smoke_offline_failover(base_url: String) -> Result<(), String> {
    let smoke_start = std::time::Instant::now();
    let client = smoke_http_client()?;
    let host = create_workshop(&client, &base_url, "XtaskFailoverHost").await?;
    let mut guest =
        join_workshop(&client, &base_url, &host.session_code, "XtaskFailoverGuest").await?;

    let mut host_socket = attach_ws_session(&base_url, &host).await?;
    host_socket
        .close(None)
        .await
        .map_err(|error| format!("failed to close smoke websocket: {error}"))?;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    guest = reconnect_workshop(&client, &base_url, &guest).await?;
    let guest_player = guest
        .state
        .players
        .get(&guest.player_id)
        .ok_or_else(|| "guest player missing after failover".to_string())?;
    let host_player = guest
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
    guest = reconnect_workshop(&client, &base_url, &guest).await?;
    ensure_phase(&guest, Phase::Phase1, "guest phase1 after failover")?;

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
    guest = reconnect_workshop(&client, &base_url, &guest).await?;
    ensure_phase(&guest, Phase::Lobby, "guest lobby after reset")?;

    print_json(json!({
        "ok": true,
        "baseUrl": base_url,
        "sessionCode": host.session_code,
        "hostAfterFailover": guest.player_id,
        "phaseAfterReset": "lobby",
        "reconnectedHostConnected": host_player_after_reconnect.is_connected,
        "timing": {
            "totalMs": smoke_start.elapsed().as_millis() as u64,
        },
    }))
}

async fn smoke_persistence(config: PersistenceSmokeConfig) -> Result<(), String> {
    let client = smoke_http_client()?;
    let host = create_workshop(&client, &config.base_url, "XtaskPersistenceHost").await?;

    send_command(
        &client,
        &config.base_url,
        command_request(&host, SessionCommand::StartPhase1, None),
    )
    .await?;

    let report = query_persistence_report(
        &config.database_url,
        &host.session_code,
        &host.reconnect_token,
    )
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

async fn smoke_persistence_restart(config: PersistenceSmokeConfig) -> Result<(), String> {
    run_tool("cargo", &["build", "-p", "app-server"])?;

    let client = smoke_http_client()?;
    let smoke_start = std::time::Instant::now();
    let mut server = AppServerProcess::spawn(&config.base_url, &config.database_url)?;
    wait_for_server_ready(&client, &config.base_url, &mut server).await?;

    let host = create_workshop(&client, &config.base_url, "XtaskPersistenceRestartHost").await?;
    send_command(
        &client,
        &config.base_url,
        command_request(&host, SessionCommand::StartPhase1, None),
    )
    .await?;

    let initial_report = query_persistence_report(
        &config.database_url,
        &host.session_code,
        &host.reconnect_token,
    )
    .await?;
    if initial_report.persisted_phase != "phase1" {
        return Err(format!(
            "expected persisted workshop phase before restart to be phase1, got {}",
            initial_report.persisted_phase
        ));
    }
    if initial_report.identity_player_id != host.player_id {
        return Err(format!(
            "persisted identity player mismatch before restart: expected {}, got {}",
            host.player_id, initial_report.identity_player_id
        ));
    }

    server.shutdown()?;
    let mut restarted = AppServerProcess::spawn(&config.base_url, &config.database_url)?;
    wait_for_server_ready(&client, &config.base_url, &mut restarted).await?;

    let recovered = reconnect_workshop(&client, &config.base_url, &host).await?;
    ensure_phase(
        &recovered,
        Phase::Phase1,
        "smoke-persistence-restart reconnect",
    )?;
    if recovered.player_id != host.player_id {
        return Err(format!(
            "restart reconnect returned player {}, expected {}",
            recovered.player_id, host.player_id
        ));
    }
    if recovered.state.session.id != host.state.session.id {
        return Err(format!(
            "restart reconnect returned session {}, expected {}",
            recovered.state.session.id, host.state.session.id
        ));
    }
    if recovered.state.current_player_id.as_deref() != Some(host.player_id.as_str()) {
        return Err(
            "restart reconnect returned a different current player than the host".to_string(),
        );
    }

    let recovered_dragon_id = assigned_dragon_id(&recovered)?;
    let observation_text = "Recovered after restart";
    send_command(
        &client,
        &config.base_url,
        command_request(
            &host,
            SessionCommand::SubmitObservation,
            Some(json!({ "text": observation_text })),
        ),
    )
    .await?;

    let continued = reconnect_workshop(&client, &config.base_url, &host).await?;
    ensure_phase(
        &continued,
        Phase::Phase1,
        "smoke-persistence-restart continuity reconnect",
    )?;
    let continued_dragon = continued
        .state
        .dragons
        .get(&recovered_dragon_id)
        .ok_or_else(|| {
            format!("recovered dragon {recovered_dragon_id} missing after restart continuity check")
        })?;
    if !continued_dragon
        .discovery_observations
        .iter()
        .any(|observation| observation == observation_text)
    {
        return Err("post-restart observation was not present after reconnect".to_string());
    }

    let continued_report = query_persistence_report(
        &config.database_url,
        &host.session_code,
        &host.reconnect_token,
    )
    .await?;
    if continued_report.artifact_count <= initial_report.artifact_count {
        return Err(format!(
            "expected post-restart persistence artifact count to grow beyond {}, got {}",
            initial_report.artifact_count, continued_report.artifact_count
        ));
    }

    restarted.shutdown()?;

    print_json(json!({
        "ok": true,
        "baseUrl": config.base_url,
        "sessionCode": host.session_code,
        "persistedPhaseAfterRestart": continued_report.persisted_phase,
        "identityPlayerId": continued_report.identity_player_id,
        "artifactCountBeforeRestart": initial_report.artifact_count,
        "artifactCountAfterRestart": continued_report.artifact_count,
        "continuityObservation": observation_text,
        "tablesChecked": ["workshop_sessions", "session_artifacts", "player_identities"],
        "timing": {
            "totalMs": smoke_start.elapsed().as_millis() as u64,
        },
    }))
}

async fn smoke_restore_reconnect(config: RestoreSmokeConfig) -> Result<(), String> {
    let client = smoke_http_client()?;
    let smoke_start = std::time::Instant::now();
    let host = create_workshop(&client, &config.base_url, "XtaskRestoreHost").await?;
    send_command(
        &client,
        &config.base_url,
        command_request(&host, SessionCommand::StartPhase1, None),
    )
    .await?;

    let initial_report = query_persistence_report(
        &config.database_url,
        &host.session_code,
        &host.reconnect_token,
    )
    .await?;
    if initial_report.persisted_phase != "phase1" {
        return Err(format!(
            "expected persisted workshop phase before restore to be phase1, got {}",
            initial_report.persisted_phase
        ));
    }
    if initial_report.artifact_count < 2 {
        return Err(format!(
            "expected at least 2 persisted artifacts before restore, got {}",
            initial_report.artifact_count
        ));
    }
    if initial_report.identity_player_id != host.player_id {
        return Err(format!(
            "persisted identity player mismatch before restore: expected {}, got {}",
            host.player_id, initial_report.identity_player_id
        ));
    }

    eprintln!(
        "staging restore checkpoint ready for session {}; trigger database restore and app restart now (waiting up to {} seconds for /api/ready outage and recovery)",
        host.session_code, config.restart_timeout_seconds
    );

    wait_for_observed_restart_and_ready(
        &client,
        &config.base_url,
        std::time::Duration::from_secs(config.restart_timeout_seconds),
    )
    .await?;
    assert_health_endpoint(&client, &config.base_url, "/api/live").await?;
    assert_health_endpoint(&client, &config.base_url, "/api/ready").await?;

    let recovered = reconnect_workshop(&client, &config.base_url, &host).await?;
    ensure_phase(
        &recovered,
        Phase::Phase1,
        "smoke-restore-reconnect reconnect",
    )?;
    if recovered.player_id != host.player_id {
        return Err(format!(
            "restore reconnect returned player {}, expected {}",
            recovered.player_id, host.player_id
        ));
    }
    if recovered.state.session.id != host.state.session.id {
        return Err(format!(
            "restore reconnect returned session {}, expected {}",
            recovered.state.session.id, host.state.session.id
        ));
    }
    if recovered.state.current_player_id.as_deref() != Some(host.player_id.as_str()) {
        return Err(
            "restore reconnect returned a different current player than the host".to_string(),
        );
    }

    let mut restored_socket = attach_ws_session(&config.base_url, &recovered).await?;
    restored_socket
        .close(None)
        .await
        .map_err(|error| format!("failed to close restore validation websocket: {error}"))?;

    let recovered_dragon_id = assigned_dragon_id(&recovered)?;
    let observation_text = "Recovered after staging restore";
    send_command(
        &client,
        &config.base_url,
        command_request(
            &recovered,
            SessionCommand::SubmitObservation,
            Some(json!({ "text": observation_text })),
        ),
    )
    .await?;

    let continued = reconnect_workshop(&client, &config.base_url, &recovered).await?;
    ensure_phase(
        &continued,
        Phase::Phase1,
        "smoke-restore-reconnect continuity reconnect",
    )?;
    if continued.player_id != host.player_id {
        return Err(format!(
            "continuity reconnect returned player {}, expected {}",
            continued.player_id, host.player_id
        ));
    }
    if continued.state.session.id != host.state.session.id {
        return Err(format!(
            "continuity reconnect returned session {}, expected {}",
            continued.state.session.id, host.state.session.id
        ));
    }
    let continued_dragon = continued
        .state
        .dragons
        .get(&recovered_dragon_id)
        .ok_or_else(|| {
            format!("recovered dragon {recovered_dragon_id} missing after restore continuity check")
        })?;
    if !continued_dragon
        .discovery_observations
        .iter()
        .any(|observation| observation == observation_text)
    {
        return Err("post-restore observation was not present after reconnect".to_string());
    }

    let continued_report = query_persistence_report(
        &config.database_url,
        &host.session_code,
        &continued.reconnect_token,
    )
    .await?;
    if continued_report.persisted_phase != "phase1" {
        return Err(format!(
            "expected persisted workshop phase after restore to remain phase1, got {}",
            continued_report.persisted_phase
        ));
    }
    if continued_report.artifact_count <= initial_report.artifact_count {
        return Err(format!(
            "expected post-restore persistence artifact count to grow beyond {}, got {}",
            initial_report.artifact_count, continued_report.artifact_count
        ));
    }
    if continued_report.identity_player_id != host.player_id {
        return Err(format!(
            "persisted identity player mismatch after restore: expected {}, got {}",
            host.player_id, continued_report.identity_player_id
        ));
    }

    print_json(json!({
        "ok": true,
        "baseUrl": config.base_url,
        "sessionCode": host.session_code,
        "persistedPhaseAfterRestore": continued_report.persisted_phase,
        "identityPlayerId": continued_report.identity_player_id,
        "artifactCountBeforeRestore": initial_report.artifact_count,
        "artifactCountAfterRestore": continued_report.artifact_count,
        "continuityObservation": observation_text,
        "websocketReattachVerified": true,
        "tablesChecked": ["workshop_sessions", "session_artifacts", "player_identities"],
        "timing": {
            "totalMs": smoke_start.elapsed().as_millis() as u64,
        },
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

    let (session_id, payload): (String, serde_json::Value) =
        sqlx::query_as("SELECT session_id, payload FROM workshop_sessions WHERE session_code = $1")
            .bind(session_code)
            .fetch_optional(&pool)
            .await
            .map_err(|error| format!("failed to query workshop_sessions: {error}"))?
            .ok_or_else(|| {
                format!("no persisted workshop_sessions row found for session code {session_code}")
            })?;
    let persisted_phase = payload
        .get("phase")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("persisted workshop payload is missing string phase: {payload}"))?
        .to_string();

    let (artifact_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM session_artifacts WHERE session_id = $1")
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
    .ok_or_else(|| {
        format!("no persisted player_identities row found for reconnect token {reconnect_token}")
    })?;

    Ok(PersistenceReport {
        persisted_phase,
        artifact_count,
        identity_player_id,
    })
}

async fn wait_for_server_ready(
    client: &reqwest::Client,
    base_url: &str,
    server: &mut AppServerProcess,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);

    loop {
        if let Some(status) = server.try_wait()? {
            return Err(format!("app-server exited before becoming ready: {status}"));
        }

        if let Ok(response) = client
            .get(format!("{base_url}/api/ready"))
            .header("Origin", base_url)
            .send()
            .await
            && response.status().is_success()
        {
            return Ok(());
        }

        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for app-server readiness at {base_url}/api/ready"
            ));
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

async fn wait_for_observed_restart_and_ready(
    client: &reqwest::Client,
    base_url: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut saw_outage = false;

    loop {
        let ready = client
            .get(format!("{base_url}/api/ready"))
            .header("Origin", base_url)
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false);

        if ready {
            if saw_outage {
                return Ok(());
            }
        } else {
            saw_outage = true;
        }

        if std::time::Instant::now() >= deadline {
            return Err(if saw_outage {
                format!(
                    "observed app outage during restore at {base_url}, but readiness did not recover within {} seconds",
                    timeout.as_secs()
                )
            } else {
                format!(
                    "did not observe a readiness outage at {base_url} within {} seconds; rerun the restore with an explicit app restart or longer timeout",
                    timeout.as_secs()
                )
            });
        }

        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

async fn assert_health_endpoint(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
) -> Result<(), String> {
    let response = client
        .get(format!("{base_url}{path}"))
        .header("Origin", base_url)
        .send()
        .await
        .map_err(|error| format!("failed to reach {path}: {error}"))?;

    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "expected {path} to succeed after restore, got {}",
            response.status()
        ))
    }
}

async fn create_workshop(
    client: &reqwest::Client,
    base_url: &str,
    name: &str,
) -> Result<WorkshopJoinSuccess, String> {
    signin_account(client, base_url, name).await?;
    create_workshop_on_client(client, base_url).await
}

async fn create_workshop_on_client(
    client: &reqwest::Client,
    base_url: &str,
) -> Result<WorkshopJoinSuccess, String> {
    let session_code = create_workshop_lobby(client, base_url).await?;
    join_workshop_authenticated(client, base_url, &session_code).await
}

async fn signin_account(
    client: &reqwest::Client,
    base_url: &str,
    name: &str,
) -> Result<(), String> {
    let response = client
        .post(format!("{base_url}/api/auth/signin"))
        .header("Origin", base_url)
        .json(&AuthRequest {
            hero: name.to_string(),
            name: name.to_string(),
            password: "smoketest-password-1234".to_string(),
        })
        .send()
        .await
        .map_err(|error| format!("signin failed: {error}"))?;

    let status = response.status().as_u16();
    if status != 200 && status != 201 {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<unreadable>".to_string());
        return Err(format!("signin returned {status}: {body}"));
    }

    Ok(())
}

async fn sign_in_smoke_client(base_url: &str, name: &str) -> Result<reqwest::Client, String> {
    let client = smoke_http_client()?;
    signin_account(&client, base_url, name).await?;
    Ok(client)
}

async fn create_workshop_lobby(client: &reqwest::Client, base_url: &str) -> Result<String, String> {
    let response = client
        .post(format!("{base_url}/api/workshops/lobby"))
        .header("Origin", base_url)
        .json(&CreateWorkshopRequest {
            name: None,
            config: Some(protocol::WorkshopCreateConfig {
                phase0_minutes: 5,
                phase1_minutes: 10,
                phase2_minutes: 10,
            }),
            character_id: None,
        })
        .send()
        .await
        .map_err(|error| format!("create workshop lobby failed: {error}"))?;

    let status = response.status().as_u16();
    if status != 201 {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<unreadable>".to_string());
        return Err(format!("create workshop lobby returned {status}: {body}"));
    }

    let payload = response
        .json::<protocol::WorkshopCreateResult>()
        .await
        .map_err(|error| format!("failed to parse create workshop response: {error}"))?;

    match payload {
        protocol::WorkshopCreateResult::Success(success) => Ok(success.session_code),
        protocol::WorkshopCreateResult::Error(error) => Err(error.error),
    }
}

async fn attach_ws_session(
    base_url: &str,
    identity: &WorkshopJoinSuccess,
) -> Result<SmokeWebSocket, String> {
    let mut request = smoke_ws_url(base_url)
        .into_client_request()
        .map_err(|error| format!("failed to build smoke websocket request: {error}"))?;
    request.headers_mut().insert(
        "origin",
        HeaderValue::from_str(base_url)
            .map_err(|error| format!("invalid websocket origin: {error}"))?,
    );

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
        .send(WsMessage::Text(attach_payload))
        .await
        .map_err(|error| format!("failed to send smoke websocket attach payload: {error}"))?;

    let message = socket
        .next()
        .await
        .ok_or_else(|| "smoke websocket closed before sending initial state".to_string())
        .and_then(|message| {
            message.map_err(|error| format!("failed to read smoke websocket frame: {error}"))
        })?;
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
    signin_account(client, base_url, name).await?;
    join_workshop_authenticated(client, base_url, session_code).await
}

async fn join_workshop_authenticated(
    client: &reqwest::Client,
    base_url: &str,
    session_code: &str,
) -> Result<WorkshopJoinSuccess, String> {
    let response = client
        .post(format!("{base_url}/api/workshops/join"))
        .header("Origin", base_url)
        .json(&JoinWorkshopRequest {
            session_code: session_code.to_string(),
            name: None,
            character_id: None,
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
            character_id: None,
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

fn smoke_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .map_err(|error| format!("failed to build smoke HTTP client: {error}"))
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
  cargo xtask smoke-join-load -- [--base-url http://127.0.0.1:4100] [--clients 4]
  cargo xtask smoke-sprite-load -- [--base-url http://127.0.0.1:4100] [--workers 40]
  cargo xtask smoke-judge-bundle -- [--base-url http://127.0.0.1:4100]
  cargo xtask smoke-offline-failover -- [--base-url http://127.0.0.1:4100]
  cargo xtask smoke-persistence -- [--base-url http://127.0.0.1:4100] [--database-url postgres://...]
  cargo xtask smoke-persistence-restart -- [--base-url http://127.0.0.1:4100] [--database-url postgres://...]
  cargo xtask smoke-restore-reconnect -- [--base-url http://127.0.0.1:4100] [--database-url postgres://...] [--restart-timeout-seconds 300]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_forwarded_args_drops_separator() {
        assert_eq!(
            normalize_forwarded_args(vec![
                "--".to_string(),
                "--port".to_string(),
                "4100".to_string()
            ]),
            vec!["--port".to_string(), "4100".to_string()]
        );
    }

    #[test]
    fn normalize_base_url_trims_slashes_and_whitespace() {
        assert_eq!(
            normalize_base_url(" http://127.0.0.1:4100/ "),
            "http://127.0.0.1:4100"
        );
        assert_eq!(normalize_base_url("   "), "http://127.0.0.1:4100");
    }

    #[test]
    fn smoke_base_url_uses_forwarded_flag() {
        assert_eq!(
            smoke_base_url(&[
                "--base-url".to_string(),
                "http://localhost:4200/".to_string()
            ])
            .expect("base url"),
            "http://localhost:4200"
        );
    }

    #[test]
    fn join_load_config_defaults_to_four_clients() {
        let config = join_load_config(&[]).expect("join load config");
        assert_eq!(config.base_url, "http://127.0.0.1:4100");
        assert_eq!(config.clients, 4);
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
    fn sprite_load_config_defaults_to_forty_workers() {
        let config = sprite_load_config(&[]).expect("sprite load config");
        assert_eq!(config.base_url, "http://127.0.0.1:4100");
        assert_eq!(config.workers, 40);
    }

    #[test]
    fn sprite_load_config_uses_forwarded_flags() {
        let config = sprite_load_config(&[
            "--base-url".to_string(),
            "http://localhost:4300/".to_string(),
            "--workers".to_string(),
            "10".to_string(),
        ])
        .expect("sprite load config with flags");
        assert_eq!(config.base_url, "http://localhost:4300");
        assert_eq!(config.workers, 10);
    }

    #[test]
    fn smoke_ws_url_converts_http_and_https_schemes() {
        assert_eq!(
            smoke_ws_url("http://127.0.0.1:4100/"),
            "ws://127.0.0.1:4100/api/workshops/ws"
        );
        assert_eq!(
            smoke_ws_url("https://dragon-switch.dev"),
            "wss://dragon-switch.dev/api/workshops/ws"
        );
    }

    #[test]
    fn app_server_bind_addr_uses_base_url_host_and_port() {
        assert_eq!(
            app_server_bind_addr("http://127.0.0.1:4300/").expect("bind addr"),
            "127.0.0.1:4300"
        );
        assert_eq!(
            app_server_bind_addr("https://[::1]:4400").expect("ipv6 bind addr"),
            "[::1]:4400"
        );
    }

    #[test]
    fn persistence_smoke_config_uses_forwarded_flag() {
        let previous = env::var("XTASK_PERSISTENCE_DATABASE_URL").ok();
        unsafe {
            env::set_var(
                "XTASK_PERSISTENCE_DATABASE_URL",
                "postgres://env-user:env-pass@env-host:5432/env-db",
            );
        }

        let config = persistence_smoke_config(&[
            "--base-url".to_string(),
            "http://localhost:4300/".to_string(),
            "--database-url".to_string(),
            "postgres://cli-user:cli-pass@cli-host:5432/cli-db".to_string(),
        ])
        .expect("persistence config");

        assert_eq!(config.base_url, "http://localhost:4300");
        assert_eq!(
            config.database_url,
            "postgres://cli-user:cli-pass@cli-host:5432/cli-db"
        );

        unsafe {
            if let Some(value) = previous {
                env::set_var("XTASK_PERSISTENCE_DATABASE_URL", value);
            } else {
                env::remove_var("XTASK_PERSISTENCE_DATABASE_URL");
            }
        }
    }

    #[test]
    fn restore_smoke_config_uses_forwarded_flags() {
        let previous = env::var("XTASK_PERSISTENCE_DATABASE_URL").ok();
        unsafe {
            env::set_var(
                "XTASK_PERSISTENCE_DATABASE_URL",
                "postgres://env-user:env-pass@env-host:5432/env-db",
            );
        }

        let config = restore_smoke_config(&[
            "--base-url".to_string(),
            "http://localhost:4400/".to_string(),
            "--database-url".to_string(),
            "postgres://cli-user:cli-pass@cli-host:5432/cli-db".to_string(),
            "--restart-timeout-seconds".to_string(),
            "420".to_string(),
        ])
        .expect("restore config");

        assert_eq!(config.base_url, "http://localhost:4400");
        assert_eq!(
            config.database_url,
            "postgres://cli-user:cli-pass@cli-host:5432/cli-db"
        );
        assert_eq!(config.restart_timeout_seconds, 420);

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
            vec![
                "run".to_string(),
                "-p".to_string(),
                "app-server".to_string()
            ]
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
        assert_eq!(config.out_dir, workspace_root().join("custom-dist"));
    }

    #[test]
    fn app_web_static_dir_points_to_source_assets() {
        assert_eq!(
            app_web_static_dir(),
            workspace_root().join("app-web/static")
        );
    }
}
