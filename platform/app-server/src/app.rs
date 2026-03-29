use axum::{
    Router,
    http::HeaderValue,
    http::header::CACHE_CONTROL,
    routing::{get, post},
};
use domain::WorkshopSession;
use persistence::{InMemorySessionStore, PostgresSessionStore, SessionStore};
use protocol::ServerWsMessage;
use realtime::SessionRegistry;
use security::{
    DEFAULT_RUST_SESSION_CODE_PREFIX, FixedWindowRateLimiter, OriginPolicy, OriginPolicyOptions,
    create_origin_policy,
};
use serde::Serialize;
use std::{collections::BTreeMap, env, net::SocketAddr, path::PathBuf, str::FromStr, sync::Arc};
use tokio::sync::{Mutex, mpsc};
use tower_http::{
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};

use crate::http::{
    create_workshop, join_workshop, live, ready, runtime_snapshot, workshop_command,
    workshop_judge_bundle,
};
use crate::ws::workshop_ws;

#[derive(Debug, Clone)]
pub(crate) struct AppConfig {
    pub(crate) bind_addr: SocketAddr,
    pub(crate) is_production: bool,
    pub(crate) rust_session_code_prefix: String,
    pub(crate) origin_policy: OriginPolicy,
    pub(crate) static_assets_dir: PathBuf,
    pub(crate) database_url: Option<String>,
    pub(crate) persistence_backend: String,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Arc<AppConfig>,
    pub(crate) store: Arc<dyn SessionStore>,
    pub(crate) sessions: Arc<Mutex<BTreeMap<String, WorkshopSession>>>,
    pub(crate) session_cache_load_locks: Arc<Mutex<BTreeMap<String, Arc<Mutex<()>>>>>,
    pub(crate) session_write_locks: Arc<Mutex<BTreeMap<String, Arc<Mutex<()>>>>>,
    pub(crate) create_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    pub(crate) join_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    pub(crate) realtime: Arc<Mutex<SessionRegistry>>,
    pub(crate) realtime_senders:
        Arc<Mutex<BTreeMap<String, mpsc::UnboundedSender<ServerWsMessage>>>>,
}

impl AppState {
    pub(crate) fn new(
        config: Arc<AppConfig>,
        store: Arc<dyn SessionStore>,
        create_limit: u32,
        join_limit: u32,
    ) -> Self {
        Self {
            config,
            store,
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            session_cache_load_locks: Arc::new(Mutex::new(BTreeMap::new())),
            session_write_locks: Arc::new(Mutex::new(BTreeMap::new())),
            create_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(
                create_limit,
                60_000,
            ))),
            join_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(join_limit, 60_000))),
            realtime: Arc::new(Mutex::new(SessionRegistry::new())),
            realtime_senders: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct RuntimeSnapshot {
    pub(crate) bind_addr: String,
    pub(crate) is_production: bool,
    pub(crate) rust_session_code_prefix: String,
    pub(crate) persistence_backend: String,
    pub(crate) allow_any_origin: bool,
    pub(crate) require_origin: bool,
    pub(crate) allowed_origins: Vec<String>,
    pub(crate) active_realtime_sessions: usize,
}

pub(crate) fn build_app(state: AppState) -> Router {
    let static_assets_dir = state.config.static_assets_dir.clone();
    let index_file = static_assets_dir.join("index.html");

    Router::new()
        .route("/api/workshops", post(create_workshop))
        .route("/api/workshops/join", post(join_workshop))
        .route("/api/workshops/command", post(workshop_command))
        .route("/api/workshops/ws", get(workshop_ws))
        .route("/api/workshops/judge-bundle", post(workshop_judge_bundle))
        .route("/api/live", get(live))
        .route("/api/ready", get(ready))
        .route("/api/runtime", get(runtime_snapshot))
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

pub(crate) async fn build_session_store(
    config: &AppConfig,
) -> Result<Arc<dyn SessionStore>, String> {
    if let Some(database_url) = config.database_url.as_deref() {
        let store = PostgresSessionStore::connect(database_url)
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
    let persistence_backend = if database_url.is_some() {
        "postgres".to_string()
    } else {
        "memory".to_string()
    };

    Ok(AppConfig {
        bind_addr,
        is_production,
        rust_session_code_prefix,
        origin_policy,
        static_assets_dir,
        database_url,
        persistence_backend,
    })
}
