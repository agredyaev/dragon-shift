use axum::{
    Router,
    http::HeaderValue,
    http::header::CACHE_CONTROL,
    response::IntoResponse,
    routing::{get, post},
};
use domain::WorkshopSession;
use persistence::{InMemorySessionStore, PostgresSessionStore, SessionStore};
use realtime::SessionRegistry;
use security::{
    DEFAULT_RUST_SESSION_CODE_PREFIX, FixedWindowRateLimiter, OriginPolicy, OriginPolicyOptions,
    create_origin_policy,
};
use std::{
    collections::BTreeMap, env, net::SocketAddr, path::PathBuf, str::FromStr, sync::Arc,
    time::Duration,
};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use tokio::{sync::{Mutex, mpsc}, task::JoinHandle};
use tower_http::{
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};

use crate::http::{
    create_workshop, join_workshop, live, ready, workshop_command, workshop_judge_bundle,
};
use crate::ws::{WsOutbound, workshop_ws};

#[derive(Debug, Clone)]
pub(crate) struct AppConfig {
    pub(crate) bind_addr: SocketAddr,
    pub(crate) rust_session_code_prefix: String,
    pub(crate) trust_forwarded_for: bool,
    pub(crate) create_rate_limit: u32,
    pub(crate) join_rate_limit: u32,
    pub(crate) command_rate_limit: u32,
    pub(crate) websocket_rate_limit: u32,
    pub(crate) reconnect_token_ttl: Duration,
    pub(crate) database_pool_size: u32,
    pub(crate) origin_policy: OriginPolicy,
    pub(crate) static_assets_dir: PathBuf,
    pub(crate) database_url: Option<String>,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Arc<AppConfig>,
    pub(crate) replica_id: String,
    pub(crate) store: Arc<dyn SessionStore>,
    pub(crate) sessions: Arc<Mutex<BTreeMap<String, WorkshopSession>>>,
    pub(crate) session_cache_load_locks: Arc<Mutex<BTreeMap<String, Arc<Mutex<()>>>>>,
    pub(crate) session_write_locks: Arc<Mutex<BTreeMap<String, Arc<Mutex<()>>>>>,
    pub(crate) create_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    pub(crate) join_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    pub(crate) command_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    pub(crate) websocket_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    pub(crate) realtime: Arc<Mutex<SessionRegistry>>,
    pub(crate) realtime_senders: Arc<Mutex<BTreeMap<String, mpsc::UnboundedSender<WsOutbound>>>>,
    pub(crate) realtime_heartbeats: Arc<Mutex<BTreeMap<String, JoinHandle<()>>>>,
    pub(crate) retired_realtime_connections: Arc<Mutex<BTreeMap<String, ()>>>,
    pub(crate) recent_session_notifications: Arc<Mutex<BTreeMap<String, String>>>,
    #[cfg(test)]
    pub(crate) fail_next_initial_state_send: Arc<AtomicBool>,
}

impl AppState {
    pub(crate) fn new(config: Arc<AppConfig>, store: Arc<dyn SessionStore>) -> Self {
        let create_rate_limit = config.create_rate_limit;
        let join_rate_limit = config.join_rate_limit;
        let command_rate_limit = config.command_rate_limit;
        let websocket_rate_limit = config.websocket_rate_limit;
        Self {
            config,
            replica_id: uuid::Uuid::new_v4().to_string(),
            store,
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            session_cache_load_locks: Arc::new(Mutex::new(BTreeMap::new())),
            session_write_locks: Arc::new(Mutex::new(BTreeMap::new())),
            create_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(
                create_rate_limit,
                60_000,
            ))),
            join_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(
                join_rate_limit,
                60_000,
            ))),
            command_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(
                command_rate_limit,
                60_000,
            ))),
            websocket_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(
                websocket_rate_limit,
                60_000,
            ))),
            realtime: Arc::new(Mutex::new(SessionRegistry::new())),
            realtime_senders: Arc::new(Mutex::new(BTreeMap::new())),
            realtime_heartbeats: Arc::new(Mutex::new(BTreeMap::new())),
            retired_realtime_connections: Arc::new(Mutex::new(BTreeMap::new())),
            recent_session_notifications: Arc::new(Mutex::new(BTreeMap::new())),
            #[cfg(test)]
            fail_next_initial_state_send: Arc::new(AtomicBool::new(false)),
        }
    }
}

pub(crate) fn build_app(state: AppState) -> Router {
    let static_assets_dir = state.config.static_assets_dir.clone();
    let index_file = static_assets_dir.join("index.html");

    let api_routes = Router::new()
        .route("/workshops", post(create_workshop))
        .route("/workshops/join", post(join_workshop))
        .route("/workshops/command", post(workshop_command))
        .route("/workshops/ws", get(workshop_ws))
        .route("/workshops/judge-bundle", post(workshop_judge_bundle))
        .route("/live", get(live))
        .route("/ready", get(ready))
        .fallback(api_not_found);

    Router::new()
        .nest("/api", api_routes)
        .fallback_service(
            ServeDir::new(static_assets_dir).not_found_service(ServeFile::new(index_file)),
        )
        .layer(SetResponseHeaderLayer::overriding(
            CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn api_not_found() -> impl IntoResponse {
    axum::http::StatusCode::NOT_FOUND
}

pub(crate) async fn build_session_store(
    config: &AppConfig,
) -> Result<Arc<dyn SessionStore>, String> {
    if let Some(database_url) = config.database_url.as_deref() {
        let store = PostgresSessionStore::connect(database_url, config.database_pool_size)
            .await
            .map_err(|error| format!("connect postgres session store: {error}"))?;
        Ok(Arc::new(store))
    } else {
        Ok(Arc::new(InMemorySessionStore::new()))
    }
}

pub(crate) fn load_config() -> Result<AppConfig, String> {
    let bind_addr =
        env::var("APP_SERVER_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:4100".to_string());
    let bind_addr = SocketAddr::from_str(&bind_addr)
        .map_err(|error| format!("invalid APP_SERVER_BIND_ADDR: {error}"))?;

    let is_production = env::var("NODE_ENV")
        .map(|value| value == "production")
        .unwrap_or(false);
    let rust_session_code_prefix = env::var("RUST_SESSION_CODE_PREFIX")
        .ok()
        .filter(|value| value.len() == 1 && value.chars().all(|ch| ch.is_ascii_digit()))
        .unwrap_or_else(|| DEFAULT_RUST_SESSION_CODE_PREFIX.to_string());
    let trust_forwarded_for = env::var("TRUST_X_FORWARDED_FOR")
        .ok()
        .map(|value| value.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let create_rate_limit = load_rate_limit_env("CREATE_RATE_LIMIT_MAX", 20)?;
    let join_rate_limit = load_rate_limit_env("JOIN_RATE_LIMIT_MAX", 40)?;
    let command_rate_limit = load_rate_limit_env("COMMAND_RATE_LIMIT_MAX", 120)?;
    let websocket_rate_limit = load_rate_limit_env("WEBSOCKET_RATE_LIMIT_MAX", 300)?;
    let reconnect_token_ttl = Duration::from_secs(load_duration_env(
        "RECONNECT_TOKEN_TTL_SECONDS",
        60 * 60 * 12,
    )?);
    let database_pool_size = load_rate_limit_env("DATABASE_POOL_SIZE", 10)?;
    let origin_policy = create_origin_policy(OriginPolicyOptions {
        allowed_origins: env::var("ALLOWED_ORIGINS").ok().as_deref(),
        app_origin: env::var("VITE_APP_URL").ok().as_deref(),
        is_production,
    })
    .map_err(|error| error.to_string())?;
    let static_assets_dir = env::var("APP_SERVER_STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("workspace root")
                .join("app-web/dist")
        });
    let database_url = env::var("DATABASE_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if is_production && database_url.is_none() {
        return Err("DATABASE_URL is required when NODE_ENV=production".to_string());
    }
    Ok(AppConfig {
        bind_addr,
        rust_session_code_prefix,
        trust_forwarded_for,
        create_rate_limit,
        join_rate_limit,
        command_rate_limit,
        websocket_rate_limit,
        reconnect_token_ttl,
        database_pool_size,
        origin_policy,
        static_assets_dir,
        database_url,
    })
}

fn load_rate_limit_env(key: &str, default: u32) -> Result<u32, String> {
    match env::var(key) {
        Ok(value) => {
            let parsed = value
                .trim()
                .parse::<u32>()
                .map_err(|error| format!("invalid {key}: {error}"))?;
            if parsed == 0 {
                Err(format!("{key} must be greater than zero"))
            } else {
                Ok(parsed)
            }
        }
        Err(_) => Ok(default),
    }
}

fn load_duration_env(key: &str, default_seconds: u64) -> Result<u64, String> {
    match env::var(key) {
        Ok(value) => {
            let parsed = value
                .trim()
                .parse::<u64>()
                .map_err(|error| format!("invalid {key}: {error}"))?;
            if parsed == 0 {
                Err(format!("{key} must be greater than zero"))
            } else {
                Ok(parsed)
            }
        }
        Err(_) => Ok(default_seconds),
    }
}
