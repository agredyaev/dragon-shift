use axum::{
    Router,
    extract::FromRef,
    http::HeaderValue,
    http::header::CACHE_CONTROL,
    response::IntoResponse,
    routing::{delete, get, post},
};
use axum_extra::extract::cookie::Key;
use base64::Engine;
use domain::WorkshopSession;
use persistence::{
    InMemorySessionStore, PostgresSessionStore, SessionStore, TIMEOUT_COMPANION_SPRITE_KEY,
    timeout_companion_defaults,
};
use protocol::SpriteSet;
use realtime::SessionRegistry;
use security::{
    DEFAULT_RUST_SESSION_CODE_PREFIX, FixedWindowRateLimiter, OriginPolicy, OriginPolicyOptions,
    create_origin_policy,
};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::{
    collections::BTreeMap, env, net::SocketAddr, path::PathBuf, str::FromStr, sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{Mutex, Semaphore, mpsc},
    task::JoinHandle,
};
use tower_http::{
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};

use crate::auth::{accounts_me, logout as auth_logout, signin as auth_signin};
use crate::http::{
    create_character, create_workshop, create_workshop_lobby, delete_character, delete_workshop,
    eligible_characters, generate_character_sprite_preview, generate_character_sprite_sheet,
    generate_sprite_sheet, get_character_sprite, join_workshop, list_character_catalog,
    list_my_characters, list_open_workshops, live, llm_generate_image, llm_judge, ready,
    update_character, update_workshop, workshop_command, workshop_judge_bundle,
};
use crate::llm::{LlmClient, LlmPoolConfig, load_llm_pool_config};
use crate::ws::{WsOutbound, workshop_ws};

/// Bridge so `SignedCookieJar<Key>` extractors can pull the signing key out of
/// `AppState`. Required by `axum_extra`'s signed-cookie machinery.
impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.config.cookie_key.clone()
    }
}

#[derive(Clone)]
pub(crate) struct AppConfig {
    pub(crate) bind_addr: SocketAddr,
    pub(crate) rust_session_code_prefix: String,
    pub(crate) trust_forwarded_for: bool,
    pub(crate) create_rate_limit: u32,
    pub(crate) join_rate_limit: u32,
    pub(crate) command_rate_limit: u32,
    pub(crate) websocket_rate_limit: u32,
    pub(crate) signup_rate_limit: u32,
    pub(crate) login_rate_limit: u32,
    pub(crate) character_create_rate_limit: u32,
    pub(crate) reconnect_token_ttl: Duration,
    pub(crate) database_pool_size: u32,
    pub(crate) origin_policy: OriginPolicy,
    pub(crate) static_assets_dir: PathBuf,
    pub(crate) database_url: Option<String>,
    pub(crate) llm_pool: LlmPoolConfig,
    pub(crate) sprite_queue_timeout: Duration,
    pub(crate) image_job_max_concurrency: usize,
    pub(crate) is_production: bool,
    /// Signing key for session cookies. Never logged.
    pub(crate) cookie_key: Key,
}

impl std::fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppConfig")
            .field("bind_addr", &self.bind_addr)
            .field("rust_session_code_prefix", &self.rust_session_code_prefix)
            .field("trust_forwarded_for", &self.trust_forwarded_for)
            .field("create_rate_limit", &self.create_rate_limit)
            .field("join_rate_limit", &self.join_rate_limit)
            .field("command_rate_limit", &self.command_rate_limit)
            .field("websocket_rate_limit", &self.websocket_rate_limit)
            .field("signup_rate_limit", &self.signup_rate_limit)
            .field("login_rate_limit", &self.login_rate_limit)
            .field(
                "character_create_rate_limit",
                &self.character_create_rate_limit,
            )
            .field("reconnect_token_ttl", &self.reconnect_token_ttl)
            .field("database_pool_size", &self.database_pool_size)
            .field("origin_policy", &self.origin_policy)
            .field("static_assets_dir", &self.static_assets_dir)
            .field("database_url", &self.database_url)
            .field("llm_pool", &self.llm_pool)
            .field("sprite_queue_timeout", &self.sprite_queue_timeout)
            .field("image_job_max_concurrency", &self.image_job_max_concurrency)
            .field("is_production", &self.is_production)
            .field("cookie_key", &"<redacted>")
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config: Arc<AppConfig>,
    pub(crate) replica_id: String,
    pub(crate) store: Arc<dyn SessionStore>,
    pub(crate) llm_client: Arc<LlmClient>,
    pub(crate) fallback_companion_sprites: Arc<SpriteSet>,
    pub(crate) image_job_queue: Arc<Semaphore>,
    pub(crate) sessions: Arc<Mutex<BTreeMap<String, WorkshopSession>>>,
    pub(crate) session_cache_load_locks: Arc<Mutex<BTreeMap<String, Arc<Mutex<()>>>>>,
    pub(crate) session_write_locks: Arc<Mutex<BTreeMap<String, Arc<Mutex<()>>>>>,
    pub(crate) create_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    pub(crate) join_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    pub(crate) command_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    pub(crate) websocket_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    pub(crate) signup_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    pub(crate) login_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    pub(crate) character_create_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    pub(crate) realtime: Arc<Mutex<SessionRegistry>>,
    pub(crate) realtime_senders: Arc<Mutex<BTreeMap<String, mpsc::UnboundedSender<WsOutbound>>>>,
    pub(crate) realtime_heartbeats: Arc<Mutex<BTreeMap<String, JoinHandle<()>>>>,
    pub(crate) retired_realtime_connections: Arc<Mutex<BTreeMap<String, ()>>>,
    pub(crate) recent_session_notifications: Arc<Mutex<BTreeMap<String, String>>>,
    #[cfg(test)]
    pub(crate) fail_next_initial_state_send: Arc<AtomicBool>,
}

impl AppState {
    pub(crate) fn new(
        config: Arc<AppConfig>,
        store: Arc<dyn SessionStore>,
        fallback_companion_sprites: SpriteSet,
    ) -> Self {
        let create_rate_limit = config.create_rate_limit;
        let join_rate_limit = config.join_rate_limit;
        let command_rate_limit = config.command_rate_limit;
        let websocket_rate_limit = config.websocket_rate_limit;
        let signup_rate_limit = config.signup_rate_limit;
        let login_rate_limit = config.login_rate_limit;
        let character_create_rate_limit = config.character_create_rate_limit;
        let image_job_max_concurrency = config.image_job_max_concurrency;
        let llm_client = Arc::new(LlmClient::new(config.llm_pool.clone()));
        Self {
            config,
            replica_id: uuid::Uuid::new_v4().to_string(),
            store,
            llm_client,
            fallback_companion_sprites: Arc::new(fallback_companion_sprites),
            image_job_queue: Arc::new(Semaphore::new(image_job_max_concurrency)),
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
            signup_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(
                signup_rate_limit,
                60_000,
            ))),
            login_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(
                login_rate_limit,
                60_000,
            ))),
            character_create_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(
                character_create_rate_limit,
                3_600_000, // 1-hour window (20/hr/account)
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

pub(crate) async fn load_fallback_companion_sprites(
    store: &Arc<dyn SessionStore>,
) -> Result<SpriteSet, String> {
    let defaults = store
        .load_app_sprite_defaults(TIMEOUT_COMPANION_SPRITE_KEY)
        .await
        .map_err(|error| format!("load fallback companion sprites: {error}"))?
        .unwrap_or_else(timeout_companion_defaults);

    Ok(defaults.sprites)
}

pub(crate) fn build_app(state: AppState) -> Router {
    let static_assets_dir = state.config.static_assets_dir.clone();
    let index_file = static_assets_dir.join("index.html");

    let api_routes = Router::new()
        .route("/auth/signin", post(auth_signin))
        .route("/auth/logout", post(auth_logout))
        .route("/accounts/me", get(accounts_me))
        .route("/characters", post(create_character))
        .route("/characters/mine", get(list_my_characters))
        .route(
            "/characters/preview-sprites",
            post(generate_character_sprite_preview),
        )
        .route(
            "/characters/{id}",
            delete(delete_character).patch(update_character),
        )
        .route(
            "/characters/{id}/sprites/{emotion}",
            get(get_character_sprite),
        )
        .route("/workshops", post(create_workshop))
        .route("/workshops/lobby", post(create_workshop_lobby))
        .route("/workshops/open", get(list_open_workshops))
        .route(
            "/workshops/{code}",
            delete(delete_workshop).patch(update_workshop),
        )
        .route("/workshops/join", post(join_workshop))
        .route("/workshops/command", post(workshop_command))
        .route("/workshops/ws", get(workshop_ws))
        .route("/workshops/judge-bundle", post(workshop_judge_bundle))
        .route("/workshops/sprite-sheet", post(generate_sprite_sheet))
        .route(
            "/workshops/{code}/eligible-characters",
            get(eligible_characters),
        )
        .route("/characters/catalog", post(list_character_catalog))
        .route(
            "/characters/sprite-sheet",
            post(generate_character_sprite_sheet),
        )
        .route("/llm/judge", post(llm_judge))
        .route("/llm/images", post(llm_generate_image))
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
    let signup_rate_limit = load_rate_limit_env("SIGNUP_RATE_LIMIT_MAX", 5)?;
    let login_rate_limit = load_rate_limit_env("LOGIN_RATE_LIMIT_MAX", 10)?;
    let character_create_rate_limit = load_rate_limit_env("CHARACTER_CREATE_RATE_LIMIT_MAX", 20)?;
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
    let llm_pool = load_llm_pool_config()?;
    let database_url = load_database_url()?;
    let sprite_queue_timeout =
        Duration::from_secs(load_duration_env("SPRITE_QUEUE_TIMEOUT_SECONDS", 20 * 60)?);
    let image_job_max_concurrency = load_queue_concurrency_env("IMAGE_JOB_MAX_CONCURRENCY", 2)?;
    if is_production && database_url.is_none() {
        return Err("DATABASE_URL is required when NODE_ENV=production".to_string());
    }
    let cookie_key = load_cookie_key(is_production)?;
    Ok(AppConfig {
        bind_addr,
        rust_session_code_prefix,
        trust_forwarded_for,
        create_rate_limit,
        join_rate_limit,
        command_rate_limit,
        websocket_rate_limit,
        signup_rate_limit,
        login_rate_limit,
        character_create_rate_limit,
        reconnect_token_ttl,
        database_pool_size,
        origin_policy,
        static_assets_dir,
        database_url,
        llm_pool,
        sprite_queue_timeout,
        image_job_max_concurrency,
        is_production,
        cookie_key,
    })
}

/// Parse `SESSION_COOKIE_KEY` env (base64-encoded, min 64 raw bytes).
///
/// - Production: required; missing or short key is a hard error.
/// - Development: when unset, generate a random key and emit a WARN log so
///   developers know sessions will not survive a restart.
fn load_cookie_key(is_production: bool) -> Result<Key, String> {
    match env::var("SESSION_COOKIE_KEY") {
        Ok(raw) => {
            let normalized: String = raw.chars().filter(|ch| !ch.is_ascii_whitespace()).collect();
            if normalized.is_empty() {
                if is_production {
                    return Err("SESSION_COOKIE_KEY is required when NODE_ENV=production".into());
                }
                return Ok(random_cookie_key_with_warning());
            }
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(&normalized)
                .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(&normalized))
                .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&normalized))
                .map_err(|error| {
                    format!("invalid SESSION_COOKIE_KEY (expected base64): {error}")
                })?;
            if decoded.len() < 64 {
                return Err(format!(
                    "SESSION_COOKIE_KEY must decode to at least 64 bytes (got {})",
                    decoded.len()
                ));
            }
            Ok(Key::from(&decoded))
        }
        Err(_) => {
            if is_production {
                Err("SESSION_COOKIE_KEY is required when NODE_ENV=production".into())
            } else {
                Ok(random_cookie_key_with_warning())
            }
        }
    }
}

fn random_cookie_key_with_warning() -> Key {
    tracing::warn!(
        "SESSION_COOKIE_KEY not set; generating an ephemeral key. Sessions will not survive a \
         server restart. Set SESSION_COOKIE_KEY (base64, >=64 bytes) for persistent sessions."
    );
    Key::generate()
}

fn load_database_url() -> Result<Option<String>, String> {
    if let Some(value) = env::var("DATABASE_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(Some(value));
    }

    Ok(None)
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

fn load_queue_concurrency_env(key: &str, default: usize) -> Result<usize, String> {
    match env::var(key) {
        Ok(value) => {
            let parsed = value
                .trim()
                .parse::<usize>()
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
