#![allow(clippy::bool_assert_comparison, clippy::useless_conversion)]

use crate::{
    app::{AppConfig, AppState, build_app},
    cache::ensure_session_cached,
    handle_session_update_notification,
    helpers::{build_judge_action_traces, to_client_game_state},
    http::allocate_session_code,
    llm::{LlmClient, LlmPoolConfig, ResolvedProvider},
    parse_session_update_notification,
    ws::{advance_game_ticks, emit_phase_warning_notices},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderValue, Request, StatusCode},
};
use chrono::{Duration as ChronoDuration, Utc};
use domain::{MAX_CHARACTERS_PER_ACCOUNT, SessionCode, SessionPlayer, WorkshopSession};
use futures_util::{SinkExt, StreamExt};
use persistence::{
    AccountRecord, AppSpriteDefaults, InMemorySessionStore, PersistenceError, PlayerIdentityMatch,
    PostgresSessionStore, RealtimeConnectionClaim, RealtimeConnectionRegistration,
    RealtimeConnectionRestore, SessionStore, SessionUpdateNotification, timeout_companion_defaults,
};
use protocol::{
    ClientWsMessage, CoordinatorType, DragonStats, JoinWorkshopRequest, LlmJudgeResult,
    LlmProviderKind, NoticeLevel, ServerWsMessage, SessionArtifactKind, SessionArtifactRecord,
    SessionCommand, SessionEnvelope, SpriteSheetResult, WorkshopCommandRequest,
    WorkshopCommandResult, WorkshopCreateResult, WorkshopJoinResult, WorkshopJudgeBundleResult,
};
use security::{DEFAULT_RUST_SESSION_CODE_PREFIX, OriginPolicyOptions, create_origin_policy};
use sqlx::PgPool;
use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    process::Command,
    sync::{
        Arc, LazyLock, Mutex as StdMutex, MutexGuard,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message as WsMessage, client::IntoClientRequest},
};
use tower::util::ServiceExt;
use uuid::Uuid;

fn postgres_test_database_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL").ok()
}

fn postgres_test_schema_prefix() -> String {
    std::env::var("TEST_DATABASE_SCHEMA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "app_server_itest".to_string())
}

fn sanitize_identifier(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();

    if sanitized.is_empty() {
        "itest".to_string()
    } else {
        sanitized
    }
}

fn postgres_test_schema_name(test_name: &str) -> String {
    let prefix = sanitize_identifier(&postgres_test_schema_prefix());
    let test_name = sanitize_identifier(test_name);
    let suffix = Uuid::new_v4().simple().to_string();
    let mut schema = format!("{}_{}_{}", prefix, test_name, &suffix[..12]);
    schema.truncate(63);
    schema
}

fn scoped_database_url(base_url: &str, schema: &str) -> String {
    let separator = if base_url.contains('?') { '&' } else { '?' };
    format!("{base_url}{separator}options=-csearch_path%3D{schema}")
}

async fn create_schema(base_url: &str, schema: &str) {
    let pool = PgPool::connect(base_url)
        .await
        .expect("connect admin pool for schema creation");
    sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS {schema}"))
        .execute(&pool)
        .await
        .expect("create isolated test schema");
    pool.close().await;
}

async fn drop_schema(base_url: &str, schema: &str) {
    let pool = PgPool::connect(base_url)
        .await
        .expect("connect admin pool for schema cleanup");
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(&pool)
        .await
        .expect("drop isolated test schema");
    pool.close().await;
}

struct PostgresAppTestStore {
    container_name: Option<String>,
    base_url: String,
    schema: String,
    store: Arc<PostgresSessionStore>,
    _ephemeral_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

static EPHEMERAL_POSTGRES_TEST_MUTEX: LazyLock<Arc<tokio::sync::Mutex<()>>> =
    LazyLock::new(|| Arc::new(tokio::sync::Mutex::new(())));

impl PostgresAppTestStore {
    async fn new(test_name: &str) -> Self {
        let (container_name, url, ephemeral_guard) = if let Some(url) = postgres_test_database_url()
        {
            (None, url, None)
        } else {
            let ephemeral_guard = EPHEMERAL_POSTGRES_TEST_MUTEX.clone().lock_owned().await;
            let container_name = format!("dragon-shift-pg-{}", Uuid::new_v4().simple());
            let status = Command::new("docker")
                .args([
                    "run",
                    "--rm",
                    "-d",
                    "--name",
                    &container_name,
                    "-e",
                    "POSTGRES_PASSWORD=postgres",
                    "-e",
                    "POSTGRES_USER=postgres",
                    "-e",
                    "POSTGRES_DB=dragon_shift_test",
                    "-p",
                    "5432",
                    "postgres:16-alpine",
                ])
                .status()
                .expect("start ephemeral Postgres container");
            assert!(
                status.success(),
                "docker run for Postgres test container failed"
            );

            let host_port = docker_host_port(&container_name, "5432/tcp").await;

            let url = format!(
                "postgres://postgres:postgres@127.0.0.1:{}/dragon_shift_test",
                host_port
            );
            wait_for_postgres(&url).await;
            (Some(container_name), url, Some(ephemeral_guard))
        };
        let schema = postgres_test_schema_name(test_name);
        create_schema(&url, &schema).await;
        let scoped_url = scoped_database_url(&url, &schema);
        let store = Arc::new(
            PostgresSessionStore::connect(&scoped_url, 10)
                .await
                .expect("connect postgres session store"),
        );
        store.init().await.expect("init postgres schema");
        Self {
            container_name,
            base_url: url,
            schema,
            store,
            _ephemeral_guard: ephemeral_guard,
        }
    }

    async fn reconnect(&self) -> Arc<PostgresSessionStore> {
        let scoped_url = scoped_database_url(&self.base_url, &self.schema);
        let store = Arc::new(
            PostgresSessionStore::connect(&scoped_url, 10)
                .await
                .expect("reconnect postgres session store"),
        );
        store.init().await.expect("re-init postgres schema");
        store
    }

    fn scoped_database_url(&self) -> String {
        scoped_database_url(&self.base_url, &self.schema)
    }

    async fn cleanup(self) {
        let PostgresAppTestStore {
            container_name,
            base_url,
            schema,
            store,
            _ephemeral_guard,
        } = self;
        drop(store);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop_schema(&base_url, &schema).await;
        if let Some(container_name) = container_name {
            let status = Command::new("docker")
                .args(["stop", &container_name])
                .status()
                .expect("stop ephemeral Postgres container");
            assert!(
                status.success(),
                "docker stop for Postgres test container failed"
            );
        }
    }
}

async fn docker_host_port(container_name: &str, container_port: &str) -> u16 {
    for _ in 0..20 {
        let output = Command::new("docker")
            .args(["port", container_name, container_port])
            .output()
            .expect("read ephemeral Postgres port mapping");

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(port) = stdout
                .lines()
                .find_map(|line| line.rsplit(':').next()?.trim().parse().ok())
            {
                return port;
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    panic!("timed out reading mapped docker port for ephemeral postgres")
}

async fn wait_for_postgres(database_url: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Ok(pool)) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            PgPool::connect(database_url),
        )
        .await
        {
            pool.close().await;
            return;
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    panic!("timed out waiting for ephemeral postgres");
}

struct ScopedEnvVar {
    key: &'static str,
    original: Option<String>,
}

static ENV_TEST_MUTEX: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

fn lock_env() -> MutexGuard<'static, ()> {
    ENV_TEST_MUTEX.lock().expect("lock env test mutex")
}

impl ScopedEnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }

    /// Remove the variable for the duration of the guard, restoring its
    /// original value (if any) on drop. Use when a test needs to assert
    /// behaviour specifically triggered by an *unset* env var.
    fn unset(key: &'static str) -> Self {
        let original = std::env::var(key).ok();
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, original }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        if let Some(original) = &self.original {
            unsafe {
                std::env::set_var(self.key, original);
            }
        } else {
            unsafe {
                std::env::remove_var(self.key);
            }
        }
    }
}

#[derive(Default)]
struct FaultyStore {
    inner: InMemorySessionStore,
    fail_health_check: AtomicBool,
    unhealthy_health_check: AtomicBool,
    fail_load_session_by_code: AtomicBool,
    fail_touch_player_identity: AtomicBool,
    fail_create_player_identity: AtomicBool,
    fail_append_session_artifact: AtomicBool,
    fail_save_session_with_artifact: AtomicBool,
    fail_grouped_session_artifact_persist: AtomicBool,
    fail_save_session_with_identity_and_artifact: AtomicBool,
    fail_replace_player_identity_and_save_session_with_artifact: AtomicBool,
    fail_renew_session_lease: AtomicBool,
    fail_claim_realtime_connection: AtomicBool,
    load_session_by_code_calls: AtomicUsize,
    save_session_calls: AtomicUsize,
}

impl FaultyStore {
    fn new() -> Self {
        Self {
            inner: InMemorySessionStore::new(),
            ..Self::default()
        }
    }

    fn fail_loads(&self) {
        self.fail_load_session_by_code.store(true, Ordering::SeqCst);
    }

    fn fail_health_checks(&self) {
        self.fail_health_check.store(true, Ordering::SeqCst);
    }

    fn return_unhealthy_health_checks(&self) {
        self.unhealthy_health_check.store(true, Ordering::SeqCst);
    }

    fn fail_save_with_artifact(&self) {
        self.fail_save_session_with_artifact
            .store(true, Ordering::SeqCst);
    }

    fn fail_grouped_session_artifact_persist(&self) {
        self.fail_grouped_session_artifact_persist
            .store(true, Ordering::SeqCst);
    }

    fn fail_save_with_identity_and_artifact(&self) {
        self.fail_save_session_with_identity_and_artifact
            .store(true, Ordering::SeqCst);
    }

    fn fail_replace_identity_and_save_with_artifact(&self) {
        self.fail_replace_player_identity_and_save_session_with_artifact
            .store(true, Ordering::SeqCst);
    }

    fn fail_lease_renewal(&self) {
        self.fail_renew_session_lease.store(true, Ordering::SeqCst);
    }

    fn fail_realtime_claims(&self) {
        self.fail_claim_realtime_connection
            .store(true, Ordering::SeqCst);
    }

    fn load_calls(&self) -> usize {
        self.load_session_by_code_calls.load(Ordering::SeqCst)
    }

    fn save_session_calls(&self) -> usize {
        self.save_session_calls.load(Ordering::SeqCst)
    }
}

impl SessionStore for FaultyStore {
    fn init(&self) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        self.inner.init()
    }

    fn health_check(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        if self.fail_health_check.load(Ordering::SeqCst) {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        if self.unhealthy_health_check.load(Ordering::SeqCst) {
            return Box::pin(async { Ok(false) });
        }
        self.inner.health_check()
    }

    fn load_session_by_code(
        &self,
        session_code: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<WorkshopSession>, PersistenceError>> + Send + '_>>
    {
        self.load_session_by_code_calls
            .fetch_add(1, Ordering::SeqCst);
        if self.fail_load_session_by_code.load(Ordering::SeqCst) {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner.load_session_by_code(session_code)
    }

    fn save_session(
        &self,
        session: &WorkshopSession,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        self.save_session_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.save_session(session)
    }

    fn append_session_artifact(
        &self,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        if self.fail_append_session_artifact.load(Ordering::SeqCst) {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner.append_session_artifact(artifact)
    }

    fn list_session_artifacts(
        &self,
        session_id: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Vec<SessionArtifactRecord>, PersistenceError>> + Send + '_>,
    > {
        self.inner.list_session_artifacts(session_id)
    }

    fn create_player_identity(
        &self,
        identity: &persistence::PlayerIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        if self.fail_create_player_identity.load(Ordering::SeqCst) {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner.create_player_identity(identity)
    }

    fn find_player_identity(
        &self,
        session_code: &str,
        reconnect_token: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<PlayerIdentityMatch>, PersistenceError>> + Send + '_>,
    > {
        self.inner
            .find_player_identity(session_code, reconnect_token)
    }

    fn touch_player_identity(
        &self,
        reconnect_token: &str,
        last_seen_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        if self.fail_touch_player_identity.load(Ordering::SeqCst) {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner
            .touch_player_identity(reconnect_token, last_seen_at)
    }

    fn revoke_player_identity(
        &self,
        reconnect_token: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        self.inner.revoke_player_identity(reconnect_token)
    }

    fn save_session_with_artifact(
        &self,
        session: &WorkshopSession,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        if self.fail_save_session_with_artifact.load(Ordering::SeqCst)
            || self
                .fail_grouped_session_artifact_persist
                .load(Ordering::SeqCst)
        {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner.save_session_with_artifact(session, artifact)
    }

    fn save_session_with_identity_and_artifact(
        &self,
        session: &WorkshopSession,
        identity: &persistence::PlayerIdentity,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        if self
            .fail_save_session_with_identity_and_artifact
            .load(Ordering::SeqCst)
        {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner
            .save_session_with_identity_and_artifact(session, identity, artifact)
    }

    fn replace_player_identity_and_save_session_with_artifact(
        &self,
        previous_reconnect_token: &str,
        next_identity: &persistence::PlayerIdentity,
        session: &WorkshopSession,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        if self
            .fail_replace_player_identity_and_save_session_with_artifact
            .load(Ordering::SeqCst)
        {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner
            .replace_player_identity_and_save_session_with_artifact(
                previous_reconnect_token,
                next_identity,
                session,
                artifact,
            )
    }

    fn acquire_session_lease(
        &self,
        session_code: &str,
        lease_id: &str,
        expires_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        self.inner
            .acquire_session_lease(session_code, lease_id, expires_at)
    }

    fn release_session_lease(
        &self,
        session_code: &str,
        lease_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        self.inner.release_session_lease(session_code, lease_id)
    }

    fn renew_session_lease(
        &self,
        session_code: &str,
        lease_id: &str,
        expires_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        if self.fail_renew_session_lease.load(Ordering::SeqCst) {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner
            .renew_session_lease(session_code, lease_id, expires_at)
    }

    fn renew_realtime_connection(
        &self,
        connection_id: &str,
        replica_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        self.inner
            .renew_realtime_connection(connection_id, replica_id)
    }

    fn claim_realtime_connection(
        &self,
        registration: &RealtimeConnectionRegistration,
    ) -> Pin<Box<dyn Future<Output = Result<RealtimeConnectionClaim, PersistenceError>> + Send + '_>>
    {
        if self.fail_claim_realtime_connection.load(Ordering::SeqCst) {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner.claim_realtime_connection(registration)
    }

    fn restore_realtime_connection(
        &self,
        registration: &RealtimeConnectionRegistration,
    ) -> Pin<
        Box<dyn Future<Output = Result<RealtimeConnectionRestore, PersistenceError>> + Send + '_>,
    > {
        self.inner.restore_realtime_connection(registration)
    }

    fn release_realtime_connection(
        &self,
        connection_id: &str,
        replica_id: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        self.inner
            .release_realtime_connection(connection_id, replica_id)
    }

    fn take_retired_realtime_connection(
        &self,
        connection_id: &str,
        replica_id: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        self.inner
            .take_retired_realtime_connection(connection_id, replica_id)
    }

    fn list_realtime_connections(
        &self,
        session_code: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        self.inner.list_realtime_connections(session_code)
    }

    fn load_app_sprite_defaults(
        &self,
        key: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<AppSpriteDefaults>, PersistenceError>> + Send + '_>,
    > {
        self.inner.load_app_sprite_defaults(key)
    }

    fn load_character(
        &self,
        character_id: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<persistence::CharacterRecord>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        self.inner.load_character(character_id)
    }

    fn list_characters(
        &self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<persistence::CharacterRecord>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        self.inner.list_characters()
    }

    fn save_character(
        &self,
        character: &persistence::CharacterRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        self.inner.save_character(character)
    }

    fn save_character_enforcing_cap(
        &self,
        character: &persistence::CharacterRecord,
        max: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        self.inner.save_character_enforcing_cap(character, max)
    }

    fn publish_session_notification(
        &self,
        notification: &SessionUpdateNotification,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        self.inner.publish_session_notification(notification)
    }

    fn insert_account(
        &self,
        account: &AccountRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        self.inner.insert_account(account)
    }

    fn find_account_by_name_lower(
        &self,
        name: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<AccountRecord>, PersistenceError>> + Send + '_>>
    {
        self.inner.find_account_by_name_lower(name)
    }

    fn find_account_by_id(
        &self,
        account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<AccountRecord>, PersistenceError>> + Send + '_>>
    {
        self.inner.find_account_by_id(account_id)
    }

    fn touch_last_login(
        &self,
        account_id: &str,
        now: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        self.inner.touch_last_login(account_id, now)
    }

    fn list_characters_by_owner(
        &self,
        owner_account_id: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<persistence::CharacterRecord>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        self.inner.list_characters_by_owner(owner_account_id)
    }

    fn count_characters_by_owner(
        &self,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<u32, PersistenceError>> + Send + '_>> {
        self.inner.count_characters_by_owner(owner_account_id)
    }

    fn delete_character_by_owner(
        &self,
        character_id: &str,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        self.inner
            .delete_character_by_owner(character_id, owner_account_id)
    }

    fn update_character_name_by_owner(
        &self,
        character_id: &str,
        owner_account_id: &str,
        name: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        self.inner
            .update_character_name_by_owner(character_id, owner_account_id, name)
    }

    fn delete_lobby_workshop_by_owner(
        &self,
        session_code: &str,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        self.inner
            .delete_lobby_workshop_by_owner(session_code, owner_account_id)
    }

    fn delete_realtime_connections_for_session(
        &self,
        session_code: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        self.inner
            .delete_realtime_connections_for_session(session_code)
    }

    fn list_open_workshops(
        &self,
        paging: persistence::OpenWorkshopsPaging,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<persistence::OpenWorkshopsPage, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        self.inner.list_open_workshops(paging)
    }
}

fn session_player_with_state(
    id: &str,
    name: &str,
    joined_at: chrono::DateTime<Utc>,
    is_host: bool,
    is_connected: bool,
) -> SessionPlayer {
    SessionPlayer {
        id: id.to_string(),
        name: name.to_string(),
        account_id: None,
        character_id: None,
        selected_character: None,
        is_host,
        is_connected,
        is_ready: false,
        score: 0,
        current_dragon_id: None,
        achievements: Vec::new(),
        joined_at,
    }
}

fn session_player(id: &str, name: &str, joined_at_seconds: i64) -> SessionPlayer {
    session_player_with_state(
        id,
        name,
        chrono::DateTime::from_timestamp(joined_at_seconds, 0).expect("valid timestamp"),
        false,
        true,
    )
}

fn test_character_profile(
    id: &str,
    description: &str,
    sprites: protocol::SpriteSet,
    remaining_sprite_regenerations: u8,
) -> protocol::CharacterProfile {
    protocol::CharacterProfile {
        id: id.to_string(),
        name: None,
        description: description.to_string(),
        sprites,
        remaining_sprite_regenerations,
    }
}

/// Sign in a test account and return the cookie header value.
/// Each unique `name` creates a separate account.
async fn test_auth_cookie(app: &Router, name: &str) -> String {
    signin_and_get_cookie(app, "knight", name, "testpassword123").await
}

fn create_workshop_body(name: &str) -> String {
    serde_json::json!({
        "name": name,
        "config": {
            "phase0Minutes": 5,
            "phase1Minutes": 10,
            "phase2Minutes": 10
        }
    })
    .to_string()
}

fn setup_phase1_body(session_code: &str, reconnect_token: &str) -> String {
    format!(
        r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
        session_code, reconnect_token
    )
}

/// Sync counterpart of `seed_selected_characters` for tests that operate on a
/// `WorkshopSession` by value (no AppState / cache involved). Mutates the
/// session in place so every player has a `selected_character` and is ready,
/// satisfying `begin_phase1`'s strict guard.
fn seed_selected_characters_on_session(session: &mut WorkshopSession) {
    let mut index: u32 = 0;
    for player in session.players.values_mut() {
        index += 1;
        if player.selected_character.is_none() {
            let character_id = format!("character-{}", index);
            player.character_id = Some(character_id.clone());
            player.selected_character = Some(test_character_profile(
                &character_id,
                "Seeded test character",
                protocol::SpriteSet {
                    neutral: "neutral_b64".into(),
                    happy: "happy_b64".into(),
                    angry: "angry_b64".into(),
                    sleepy: "sleepy_b64".into(),
                },
                1,
            ));
        }
        player.is_ready = true;
    }
}

/// Seed every player in the cached session with a `selected_character` and
/// mark them ready. After the refactor, `begin_phase1` strictly requires a
/// selected_character on every player, and character creation is no longer
/// part of the workshop flow. In tests this helper lets us advance Lobby
/// → Phase1 via the real HTTP path without the ceremony of seeding the
/// character catalog and issuing SelectCharacter commands for every player.
async fn seed_selected_characters(state: &AppState, session_code: &str) {
    let mut sessions = state.sessions.lock().await;
    let session = sessions
        .get_mut(session_code)
        .expect("session exists in cache for seeding");
    let mut index: u32 = 0;
    for player in session.players.values_mut() {
        index += 1;
        if player.selected_character.is_none() {
            let character_id = format!("character-{}", index);
            player.character_id = Some(character_id.clone());
            player.selected_character = Some(test_character_profile(
                &character_id,
                "Seeded test character",
                protocol::SpriteSet {
                    neutral: "neutral_b64".into(),
                    happy: "happy_b64".into(),
                    angry: "angry_b64".into(),
                    sleepy: "sleepy_b64".into(),
                },
                1,
            ));
        }
        player.is_ready = true;
    }
}

async fn create_workshop_success(
    app: &Router,
    cookie: &str,
    name: &str,
) -> protocol::WorkshopJoinSuccess {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, cookie)
                .body(Body::from(create_workshop_body(name)))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    match serde_json::from_slice::<WorkshopJoinResult>(&body).expect("parse create result") {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    }
}

async fn join_workshop_success(
    app: &Router,
    cookie: &str,
    session_code: &str,
    name: &str,
) -> protocol::WorkshopJoinSuccess {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, cookie)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"{}"}}"#,
                    session_code, name
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    match serde_json::from_slice::<WorkshopJoinResult>(&body).expect("parse join result") {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    }
}

async fn send_command_ok(app: &Router, request_body: String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(request_body))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");
    assert_eq!(response.status(), StatusCode::OK);
}

async fn progress_workshop_to_end(
    app: &Router,
    state: &AppState,
    host: &protocol::WorkshopJoinSuccess,
    guest: &protocol::WorkshopJoinSuccess,
) {
    let session_code = host.session_code.clone();
    seed_selected_characters(state, &session_code).await;
    for request_body in [
        setup_phase1_body(&session_code, &host.reconnect_token),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, host.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, host.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, guest.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, host.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, host.reconnect_token
        ),
    ] {
        send_command_ok(app, request_body).await;
    }

    let (host_vote_target, guest_vote_target) = {
        let sessions = state.sessions.lock().await;
        let session = sessions
            .get(&session_code)
            .expect("session exists after endGame");
        let host_vote_target = session
            .players
            .get(&host.player_id)
            .and_then(|player| player.current_dragon_id.clone())
            .expect("host current dragon id");
        let guest_vote_target = session
            .players
            .get(&guest.player_id)
            .and_then(|player| player.current_dragon_id.clone())
            .expect("guest current dragon id");
        (host_vote_target, guest_vote_target)
    };

    for request_body in [
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#,
            session_code, host.reconnect_token, host_vote_target
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#,
            session_code, guest.reconnect_token, guest_vote_target
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#,
            session_code, host.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endSession"}}"#,
            session_code, host.reconnect_token
        ),
    ] {
        send_command_ok(app, request_body).await;
    }
}

fn test_state() -> AppState {
    test_state_with_limits(20, 40)
}

fn test_state_with_limits(create_limit: u32, join_limit: u32) -> AppState {
    let config = Arc::new(AppConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 4100)),
        rust_session_code_prefix: DEFAULT_RUST_SESSION_CODE_PREFIX.to_string(),
        trust_forwarded_for: false,
        create_rate_limit: create_limit,
        join_rate_limit: join_limit,
        command_rate_limit: 120,
        websocket_rate_limit: 300,
        signup_rate_limit: 5,
        login_rate_limit: 10,
        character_create_rate_limit: 20,
        reconnect_token_ttl: std::time::Duration::from_secs(60 * 60 * 12),
        database_pool_size: 10,
        origin_policy: create_origin_policy(OriginPolicyOptions {
            allowed_origins: Some("http://localhost:5173"),
            app_origin: None,
            is_production: false,
        })
        .expect("create origin policy"),
        static_assets_dir: std::env::temp_dir().join("dragon-shift-test-static-missing"),
        database_url: None,
        llm_pool: LlmPoolConfig {
            google_cloud_project: None,
            google_cloud_location: None,
            judge_providers: Vec::new(),
            image_providers: Vec::new(),
        },
        sprite_queue_timeout: std::time::Duration::from_secs(20 * 60),
        image_job_max_concurrency: 2,
        is_production: false,
        cookie_key: axum_extra::extract::cookie::Key::generate(),
    });

    AppState::new(
        config,
        Arc::new(InMemorySessionStore::new()),
        timeout_companion_defaults().sprites,
    )
}

async fn spawn_test_server(app: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve test app");
    });
    (addr, handle)
}

fn ws_request(addr: SocketAddr) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    ws_request_with_cookie(addr, None)
}

fn ws_request_with_cookie(
    addr: SocketAddr,
    cookie: Option<&str>,
) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    let mut request = format!("ws://{addr}/api/workshops/ws")
        .into_client_request()
        .expect("ws client request");
    request
        .headers_mut()
        .insert("origin", HeaderValue::from_static("http://localhost:5173"));
    if let Some(cookie) = cookie {
        request.headers_mut().insert(
            axum::http::header::COOKIE,
            HeaderValue::from_str(cookie).expect("cookie header value"),
        );
    }
    request
}

fn masked_ws_text_frame(payload: &str) -> Vec<u8> {
    let payload = payload.as_bytes();
    let mask = [0x12_u8, 0x34, 0x56, 0x78];
    let mut frame = Vec::with_capacity(payload.len() + 8);
    frame.push(0x81);

    let len = payload.len();
    if len <= 125 {
        frame.push(0x80 | len as u8);
    } else if u16::try_from(len).is_ok() {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        panic!("test websocket payload is unexpectedly large");
    }

    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % mask.len()]),
    );
    frame
}

async fn connect_raw_ws(addr: SocketAddr) -> tokio::net::TcpStream {
    connect_raw_ws_with_cookie(addr, None).await
}

async fn connect_raw_ws_with_cookie(
    addr: SocketAddr,
    cookie: Option<&str>,
) -> tokio::net::TcpStream {
    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect raw ws tcp stream");
    let cookie_header = match cookie {
        Some(cookie) => format!("Cookie: {cookie}\r\n"),
        None => String::new(),
    };
    let request = format!(
        "GET /api/workshops/ws HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: http://localhost:5173\r\n{cookie_header}\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write websocket upgrade request");

    let mut response = Vec::new();
    let mut chunk = [0_u8; 256];
    loop {
        let bytes_read = stream
            .read(&mut chunk)
            .await
            .expect("read websocket upgrade response");
        assert!(bytes_read > 0, "websocket upgrade response ended early");
        response.extend_from_slice(&chunk[..bytes_read]);
        if response.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    stream
}

async fn send_raw_ws_message(stream: &mut tokio::net::TcpStream, message: &ClientWsMessage) {
    let frame =
        masked_ws_text_frame(&serde_json::to_string(message).expect("encode websocket message"));
    stream
        .write_all(&frame)
        .await
        .expect("write websocket frame");
    stream.flush().await.expect("flush websocket frame");
}

fn test_state_with_static_assets() -> AppState {
    let static_assets_dir =
        std::env::temp_dir().join(format!("dragon-shift-test-static-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&static_assets_dir).expect("create static assets dir");
    std::fs::write(
        static_assets_dir.join("index.html"),
        "<!doctype html><html><body><div id=\"root\">dragon shift</div></body></html>",
    )
    .expect("write static index");

    let mut state = test_state();
    state.config = Arc::new(AppConfig {
        static_assets_dir,
        ..state.config.as_ref().clone()
    });
    state
}

#[tokio::test]
async fn live_endpoint_returns_ok() {
    let app = build_app(test_state());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/live")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("call live endpoint");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read live body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse live json");
    assert_eq!(json["status"], "live");
    assert_eq!(json["ok"], true);
}

#[tokio::test]
async fn ready_endpoint_returns_ok_when_store_is_healthy() {
    let app = build_app(test_state());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/ready")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("call ready endpoint");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read ready body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse ready json");
    assert_eq!(json["ok"], true);
    assert_eq!(json["service"], "app-server");
    assert_eq!(json["status"], "ready");
    assert_eq!(json["checks"]["store"], true);
}

#[test]
fn load_config_parses_trust_x_forwarded_for() {
    let _env_lock = lock_env();
    let _bind = ScopedEnvVar::set("APP_SERVER_BIND_ADDR", "127.0.0.1:4100");
    let _app_url = ScopedEnvVar::set("VITE_APP_URL", "http://127.0.0.1:4100");
    let _origins = ScopedEnvVar::set("ALLOWED_ORIGINS", "http://127.0.0.1:4100");
    let _trust = ScopedEnvVar::set("TRUST_X_FORWARDED_FOR", "true");
    let _node_env = ScopedEnvVar::set("NODE_ENV", "development");
    let _database = ScopedEnvVar::set("DATABASE_URL", "postgres://user:pass@localhost:5432/db");

    let config = crate::app::load_config().expect("load config");

    assert!(config.trust_forwarded_for);
}

#[test]
fn load_config_parses_database_pool_size() {
    let _env_lock = lock_env();
    let _bind = ScopedEnvVar::set("APP_SERVER_BIND_ADDR", "127.0.0.1:4100");
    let _app_url = ScopedEnvVar::set("VITE_APP_URL", "http://127.0.0.1:4100");
    let _origins = ScopedEnvVar::set("ALLOWED_ORIGINS", "http://127.0.0.1:4100");
    let _node_env = ScopedEnvVar::set("NODE_ENV", "development");
    let _database = ScopedEnvVar::set("DATABASE_URL", "postgres://user:pass@localhost:5432/db");
    let _pool = ScopedEnvVar::set("DATABASE_POOL_SIZE", "17");

    let config = crate::app::load_config().expect("load config");

    assert_eq!(config.database_pool_size, 17);
}

#[test]
fn load_config_parses_rate_limits() {
    let _env_lock = lock_env();
    let _bind = ScopedEnvVar::set("APP_SERVER_BIND_ADDR", "127.0.0.1:4100");
    let _app_url = ScopedEnvVar::set("VITE_APP_URL", "http://127.0.0.1:4100");
    let _origins = ScopedEnvVar::set("ALLOWED_ORIGINS", "http://127.0.0.1:4100");
    let _node_env = ScopedEnvVar::set("NODE_ENV", "development");
    let _database = ScopedEnvVar::set("DATABASE_URL", "postgres://user:pass@localhost:5432/db");
    let _create_limit = ScopedEnvVar::set("CREATE_RATE_LIMIT_MAX", "11");
    let _join_limit = ScopedEnvVar::set("JOIN_RATE_LIMIT_MAX", "22");
    let _command_limit = ScopedEnvVar::set("COMMAND_RATE_LIMIT_MAX", "33");
    let _websocket_limit = ScopedEnvVar::set("WEBSOCKET_RATE_LIMIT_MAX", "44");

    let config = crate::app::load_config().expect("load config");

    assert_eq!(config.create_rate_limit, 11);
    assert_eq!(config.join_rate_limit, 22);
    assert_eq!(config.command_rate_limit, 33);
    assert_eq!(config.websocket_rate_limit, 44);
}

#[test]
fn load_config_requires_database_url_in_production() {
    let _env_lock = lock_env();
    let _bind = ScopedEnvVar::set("APP_SERVER_BIND_ADDR", "127.0.0.1:4100");
    let _app_url = ScopedEnvVar::set("VITE_APP_URL", "http://127.0.0.1:4100");
    let _origins = ScopedEnvVar::set("ALLOWED_ORIGINS", "http://127.0.0.1:4100");
    let _node_env = ScopedEnvVar::set("NODE_ENV", "production");
    let _cookie_key = ScopedEnvVar::set("SESSION_COOKIE_KEY", &fake_cookie_key_base64());
    let _database = ScopedEnvVar::set("DATABASE_URL", "");

    let result = crate::app::load_config();
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("DATABASE_URL is required"));
}

#[test]
fn load_config_requires_cookie_key_in_production() {
    let _env_lock = lock_env();
    let _bind = ScopedEnvVar::set("APP_SERVER_BIND_ADDR", "127.0.0.1:4100");
    let _app_url = ScopedEnvVar::set("VITE_APP_URL", "http://127.0.0.1:4100");
    let _origins = ScopedEnvVar::set("ALLOWED_ORIGINS", "http://127.0.0.1:4100");
    let _node_env = ScopedEnvVar::set("NODE_ENV", "production");
    let _database = ScopedEnvVar::set("DATABASE_URL", "postgres://u:p@localhost:5432/db");
    // Intentionally: no SESSION_COOKIE_KEY set.
    let _cookie_key = ScopedEnvVar::unset("SESSION_COOKIE_KEY");

    let result = crate::app::load_config();
    assert!(result.is_err());
    assert!(
        result
            .as_ref()
            .unwrap_err()
            .contains("SESSION_COOKIE_KEY is required"),
        "unexpected error: {:?}",
        result.err()
    );
}

#[test]
fn load_config_rejects_short_cookie_key() {
    let _env_lock = lock_env();
    let _bind = ScopedEnvVar::set("APP_SERVER_BIND_ADDR", "127.0.0.1:4100");
    let _app_url = ScopedEnvVar::set("VITE_APP_URL", "http://127.0.0.1:4100");
    let _origins = ScopedEnvVar::set("ALLOWED_ORIGINS", "http://127.0.0.1:4100");
    let _node_env = ScopedEnvVar::set("NODE_ENV", "development");
    // Valid base64 but only 32 bytes decoded.
    let short_key = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode([0u8; 32])
    };
    let _cookie_key = ScopedEnvVar::set("SESSION_COOKIE_KEY", &short_key);

    let result = crate::app::load_config();
    assert!(result.is_err());
    assert!(
        result.as_ref().unwrap_err().contains("at least 64 bytes"),
        "unexpected error: {:?}",
        result.err()
    );
}

#[test]
fn load_config_reads_database_url_directly() {
    let _env_lock = lock_env();
    let _bind = ScopedEnvVar::set("APP_SERVER_BIND_ADDR", "127.0.0.1:4100");
    let _app_url = ScopedEnvVar::set("VITE_APP_URL", "http://127.0.0.1:4100");
    let _origins = ScopedEnvVar::set("ALLOWED_ORIGINS", "http://127.0.0.1:4100");
    let _node_env = ScopedEnvVar::set("NODE_ENV", "production");
    let _cookie_key = ScopedEnvVar::set("SESSION_COOKIE_KEY", &fake_cookie_key_base64());
    let _database = ScopedEnvVar::set("DATABASE_URL", "postgres://inline:pass@localhost:5432/db");

    let config = crate::app::load_config().expect("load config");

    assert_eq!(
        config.database_url.as_deref(),
        Some("postgres://inline:pass@localhost:5432/db")
    );
}

#[test]
fn load_config_accepts_multiline_cookie_key() {
    let _env_lock = lock_env();
    let _bind = ScopedEnvVar::set("APP_SERVER_BIND_ADDR", "127.0.0.1:4100");
    let _app_url = ScopedEnvVar::set("VITE_APP_URL", "http://127.0.0.1:4100");
    let _origins = ScopedEnvVar::set("ALLOWED_ORIGINS", "http://127.0.0.1:4100");
    let _node_env = ScopedEnvVar::set("NODE_ENV", "production");
    let multiline_key = {
        let key = fake_cookie_key_base64();
        format!("{}\n{}", &key[..64], &key[64..])
    };
    let _cookie_key = ScopedEnvVar::set("SESSION_COOKIE_KEY", &multiline_key);
    let _database = ScopedEnvVar::set("DATABASE_URL", "postgres://inline:pass@localhost:5432/db");

    let config = crate::app::load_config().expect("load config");

    assert_eq!(
        config.database_url.as_deref(),
        Some("postgres://inline:pass@localhost:5432/db")
    );
}

/// Returns a deterministic 64-byte key encoded as standard base64. Used by
/// env-driven `load_config` tests that need a valid cookie key without the
/// developer-fallback warning path.
fn fake_cookie_key_base64() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode([0x42u8; 64])
}

#[tokio::test]
async fn ready_endpoint_returns_service_unavailable_when_store_is_degraded() {
    let store = Arc::new(FaultyStore::new());
    store.fail_health_checks();
    let app = build_app(test_state_with_store(store));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/ready")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("call ready endpoint");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read ready body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse ready json");
    assert_eq!(json["ok"], false);
    assert_eq!(json["service"], "app-server");
    assert_eq!(json["status"], "degraded");
    assert_eq!(json["checks"]["store"], false);
}

#[tokio::test]
async fn ready_endpoint_returns_service_unavailable_when_store_reports_unhealthy() {
    let store = Arc::new(FaultyStore::new());
    store.return_unhealthy_health_checks();
    let app = build_app(test_state_with_store(store));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/ready")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("call ready endpoint");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read ready body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse ready json");
    assert_eq!(json["ok"], false);
    assert_eq!(json["service"], "app-server");
    assert_eq!(json["status"], "degraded");
    assert_eq!(json["checks"]["store"], false);
}

#[tokio::test]
async fn runtime_endpoint_is_absent_for_get_and_post() {
    let get_response = build_app(test_state_with_static_assets())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/runtime")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("call runtime endpoint with GET");

    assert_eq!(get_response.status(), StatusCode::NOT_FOUND);

    let post_response = build_app(test_state_with_static_assets())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/runtime")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("call runtime endpoint with POST");

    assert_eq!(post_response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn join_workshop_returns_internal_error_when_cache_load_fails() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());

    let session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        Utc::now(),
        protocol::WorkshopCreateConfig::default(),
    );
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");
    store.fail_loads();

    let app = build_app(state);
    let cookie = test_auth_cookie(&app, "Bob").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(r#"{"sessionCode":"123456","name":"Bob"}"#))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Error(error) => assert!(error.error.contains("failed to load session")),
        WorkshopJoinResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_does_not_leave_mutated_cache_when_persisted_command_write_fails() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let session_code = "123456";
    let player_id = "player-1".to_string();
    let reconnect_token = "token-1".to_string();
    let timestamp = Utc::now();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    let host_player = session_player_with_state(&player_id, "Alice", timestamp, true, true);
    session.add_player(host_player);
    // Seed the host with a selected character before persisting, so both the
    // store and any subsequent cache reload satisfy begin_phase1's strict
    // guard. After the Phase0-removal refactor this is required before any
    // Lobby -> Phase1 transition.
    seed_selected_characters_on_session(&mut session);
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");
    store
        .inner
        .create_player_identity(&persistence::PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: player_id.clone(),
            reconnect_token: reconnect_token.clone(),
            created_at: timestamp.to_rfc3339(),
            last_seen_at: timestamp.to_rfc3339(),
        })
        .await
        .expect("seed identity");
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session.clone());

    store.fail_save_with_artifact();

    let app = build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&WorkshopCommandRequest {
                        session_code: session_code.to_string(),
                        reconnect_token: reconnect_token.clone(),
                        coordinator_type: Some(CoordinatorType::Rust),
                        command: SessionCommand::StartPhase1,
                        payload: None,
                    })
                    .expect("encode command request"),
                ))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let sessions = state.sessions.lock().await;
    let cached = sessions.get(session_code).expect("session remains cached");
    assert_eq!(cached.phase, protocol::Phase::Lobby);
}

#[tokio::test]
async fn root_path_serves_static_index_when_bundle_exists() {
    let app = build_app(test_state_with_static_assets());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("call root path");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read root body");
    let html = String::from_utf8(body.to_vec()).expect("decode html");
    assert!(html.contains("dragon shift"));
}

#[tokio::test]
async fn workshop_ws_attach_sends_current_state_for_valid_identity() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request_with_cookie(addr, Some(&cookie)))
        .await
        .expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let message = socket
        .next()
        .await
        .expect("state update frame")
        .expect("state update message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::StateUpdate(client_state) => {
            assert_eq!(client_state.session.code, create_success.session_code);
            assert_eq!(
                client_state.current_player_id.as_deref(),
                Some(create_success.player_id.as_str())
            );
        }
        other => panic!("expected state update, got {other:?}"),
    }
    assert_eq!(
        state
            .realtime
            .lock()
            .await
            .session_connection_count(&create_success.session_code),
        1
    );

    let _ = socket.close(None).await;
    server_handle.abort();
}

// ---------------------------------------------------------------------------
// Item-4 tests: WS attach binds session identity to authenticated account.
//
// `workshop_ws` extracts the signed `ds_session` cookie's account id and
// `attach_ws_session` asserts it matches `player.account_id` for
// account-owned players. Legacy anonymous players (`account_id: None`) are
// unaffected. Rejections surface as `ServerWsMessage::Error { message }`
// over the opened socket (see ws.rs ~337-347).
// ---------------------------------------------------------------------------

/// Read the next text frame from `socket` and parse it as a `ServerWsMessage`.
async fn next_server_ws_message(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> ServerWsMessage {
    let message = socket
        .next()
        .await
        .expect("server ws frame")
        .expect("server ws message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    serde_json::from_str(&payload).expect("parse ServerWsMessage")
}

#[tokio::test]
async fn ws_attach_rejects_account_mismatch_cookie() {
    // Alice signs in and creates a workshop; her player is account-owned.
    // A separate Bob account signs in and we replay Bob's cookie on the WS
    // upgrade. The attach assertion must reject the mismatch.
    let state = test_state();
    let app = build_app(state.clone());
    let alice_cookie = test_auth_cookie(&app, "AliceMismatch").await;
    let bob_cookie = test_auth_cookie(&app, "BobMismatch").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &alice_cookie)
                .body(Body::from(create_workshop_body("AliceMismatch")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_success = match serde_json::from_slice::<WorkshopJoinResult>(&create_body)
        .expect("parse create result")
    {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let (addr, server_handle) = spawn_test_server(app).await;
    // Upgrade the WS carrying Bob's cookie — mismatching the account-owned
    // player that Alice just created.
    let (mut socket, _) = connect_async(ws_request_with_cookie(addr, Some(&bob_cookie)))
        .await
        .expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    match next_server_ws_message(&mut socket).await {
        ServerWsMessage::Error { message } => {
            assert!(
                message.contains("does not match authenticated account"),
                "unexpected error message: {message}"
            );
        }
        other => panic!("expected Error rejecting account mismatch, got {other:?}"),
    }

    // Server must hard-close after rejecting the attach: the next frame is
    // either a Close frame or a stream termination. Anything else means the
    // socket was left open in a retryable state (regression lock).
    match socket.next().await {
        None => {}
        Some(Ok(WsMessage::Close(_))) => {}
        Some(Ok(other)) => panic!("expected Close or stream end after rejection, got {other:?}"),
        Some(Err(_)) => {}
    }

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn ws_attach_rejects_missing_cookie_for_account_owned_player() {
    // Same account-owned player, but the WS upgrade carries no cookie at
    // all. The assertion must fail closed — a cookieless client cannot
    // attach to an account-owned session.
    let state = test_state();
    let app = build_app(state.clone());
    let alice_cookie = test_auth_cookie(&app, "AliceMissing").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &alice_cookie)
                .body(Body::from(create_workshop_body("AliceMissing")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_success = match serde_json::from_slice::<WorkshopJoinResult>(&create_body)
        .expect("parse create result")
    {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let (addr, server_handle) = spawn_test_server(app).await;
    // Upgrade without any cookie.
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    match next_server_ws_message(&mut socket).await {
        ServerWsMessage::Error { message } => {
            assert!(
                message.contains("does not match authenticated account"),
                "unexpected error message: {message}"
            );
        }
        other => panic!("expected Error rejecting missing cookie, got {other:?}"),
    }

    // Server must hard-close after rejecting the attach: the next frame is
    // either a Close frame or a stream termination. Anything else means the
    // socket was left open in a retryable state (regression lock).
    match socket.next().await {
        None => {}
        Some(Ok(WsMessage::Close(_))) => {}
        Some(Ok(other)) => panic!("expected Close or stream end after rejection, got {other:?}"),
        Some(Err(_)) => {}
    }

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn ws_attach_allows_anonymous_player_without_cookie() {
    // Legacy no-regression case: seed a session whose player has
    // `account_id = None`. A cookieless WS attach must succeed. This
    // mirrors the pre-auth fixture pattern at
    // `workshop_command_does_not_leave_mutated_cache_when_persisted_command_write_fails`.
    let state = test_state();
    let session_code = "714141";
    let player_id = "player-anon".to_string();
    let reconnect_token = "token-anon".to_string();
    let timestamp = Utc::now();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    let host_player = session_player_with_state(&player_id, "Anon", timestamp, true, false);
    assert!(
        host_player.account_id.is_none(),
        "fixture precondition: anonymous player has no account_id"
    );
    session.add_player(host_player);
    session.host_player_id = Some(player_id.clone());
    state
        .store
        .save_session(&session)
        .await
        .expect("seed session");
    state
        .store
        .create_player_identity(&persistence::PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: player_id.clone(),
            reconnect_token: reconnect_token.clone(),
            created_at: timestamp.to_rfc3339(),
            last_seen_at: timestamp.to_rfc3339(),
        })
        .await
        .expect("seed identity");
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session);

    let app = build_app(state.clone());
    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: session_code.to_string(),
        player_id: player_id.clone(),
        reconnect_token: reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    match next_server_ws_message(&mut socket).await {
        ServerWsMessage::StateUpdate(client_state) => {
            assert_eq!(client_state.session.code, session_code);
            assert_eq!(
                client_state.current_player_id.as_deref(),
                Some(player_id.as_str())
            );
        }
        ServerWsMessage::Error { message } => {
            panic!("anonymous attach rejected unexpectedly: {message}")
        }
        other => panic!("expected StateUpdate, got {other:?}"),
    }

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn ws_attach_accepts_matching_account_cookie() {
    // Happy path: Alice signs in, creates a workshop, and replays her
    // cookie on the WS upgrade. The signed cookie's account id matches
    // `player.account_id`, so the attach succeeds and the server emits a
    // `StateUpdate` frame.
    let state = test_state();
    let app = build_app(state.clone());
    let alice_cookie = test_auth_cookie(&app, "AliceMatch").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &alice_cookie)
                .body(Body::from(create_workshop_body("AliceMatch")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_success = match serde_json::from_slice::<WorkshopJoinResult>(&create_body)
        .expect("parse create result")
    {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request_with_cookie(addr, Some(&alice_cookie)))
        .await
        .expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    match next_server_ws_message(&mut socket).await {
        ServerWsMessage::StateUpdate(client_state) => {
            assert_eq!(client_state.session.code, create_success.session_code);
            assert_eq!(
                client_state.current_player_id.as_deref(),
                Some(create_success.player_id.as_str())
            );
        }
        ServerWsMessage::Error { message } => {
            panic!("matching-cookie attach rejected unexpectedly: {message}")
        }
        other => panic!("expected StateUpdate, got {other:?}"),
    }

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn ensure_session_cached_deduplicates_concurrent_loads() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        Utc::now(),
        protocol::WorkshopCreateConfig::default(),
    );
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");

    let first = ensure_session_cached(&state, "123456");
    let second = ensure_session_cached(&state, "123456");
    let (first_result, second_result) = tokio::join!(first, second);

    assert_eq!(first_result.expect("first load"), true);
    assert_eq!(second_result.expect("second load"), true);
    assert_eq!(store.load_calls(), 1);
}

#[tokio::test]
async fn ensure_session_cached_clears_restored_transient_connectivity() {
    let state = test_state();
    let session_code = "123456";
    let timestamp = Utc::now();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        "player-1", "Alice", timestamp, true, true,
    ));
    session.host_player_id = Some("player-1".to_string());
    state
        .store
        .save_session(&session)
        .await
        .expect("seed persisted session");

    assert!(
        ensure_session_cached(&state, session_code)
            .await
            .expect("load session")
    );

    let sessions = state.sessions.lock().await;
    let restored = sessions.get(session_code).expect("restored session");
    let player = restored.players.get("player-1").expect("restored player");
    assert!(player.is_host);
    assert!(
        !player.is_connected,
        "restored cache should treat persisted connectivity as transient"
    );
}

#[tokio::test]
async fn workshop_command_pushes_state_update_to_attached_websocket() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let (addr, server_handle) = spawn_test_server(app.clone()).await;
    let (mut socket, _) = connect_async(ws_request_with_cookie(addr, Some(&cookie)))
        .await
        .expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let _ = socket
        .next()
        .await
        .expect("initial state update frame")
        .expect("initial state update message");

    // Seed the host with a selected character before triggering StartPhase1.
    seed_selected_characters(&state, &create_success.session_code).await;

    let command_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&WorkshopCommandRequest {
                        session_code: create_success.session_code.clone(),
                        reconnect_token: create_success.reconnect_token.clone(),
                        coordinator_type: Some(CoordinatorType::Rust),
                        command: SessionCommand::StartPhase1,
                        payload: None,
                    })
                    .expect("encode command request"),
                ))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");
    assert_eq!(command_response.status(), StatusCode::OK);

    let message = socket
        .next()
        .await
        .expect("broadcast state update frame")
        .expect("broadcast state update message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::StateUpdate(client_state) => {
            assert_eq!(client_state.phase, protocol::Phase::Phase1);
            assert_eq!(client_state.session.code, create_success.session_code);
        }
        other => panic!("expected state update, got {other:?}"),
    }

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn session_update_notification_skip_does_not_evict_cache_or_broadcast() {
    let state = test_state();
    let timestamp = Utc::now();
    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        "player-1", "Alice", timestamp, true, true,
    ));

    state
        .sessions
        .lock()
        .await
        .insert(session.code.0.clone(), session.clone());

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    state
        .realtime
        .lock()
        .await
        .attach(&session.code.0, "player-1", "conn-1");
    state
        .realtime_senders
        .lock()
        .await
        .insert("conn-1".to_string(), sender);

    let notification = parse_session_update_notification(
        &SessionUpdateNotification::session_state_changed(&session)
            .to_payload()
            .expect("serialize typed notification"),
    )
    .expect("parse notification");

    handle_session_update_notification(&state, &notification).await;

    assert!(
        state.sessions.lock().await.contains_key("123456"),
        "matching updated_at should preserve cached session"
    );
    assert!(
        receiver.try_recv().is_err(),
        "skip path should not broadcast"
    );
}

#[tokio::test]
async fn session_update_notification_same_timestamp_different_payload_evicts_cache() {
    let state = test_state();
    let timestamp = Utc::now();
    let mut lower_order = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    lower_order.host_player_id = Some("host-a".to_string());
    lower_order.updated_at = timestamp;

    let mut higher_order = lower_order.clone();
    higher_order.host_player_id = Some("host-z".to_string());

    state
        .store
        .save_session(&higher_order)
        .await
        .expect("seed higher-order session");
    state
        .sessions
        .lock()
        .await
        .insert(lower_order.code.0.clone(), lower_order.clone());

    let notification = parse_session_update_notification(
        &SessionUpdateNotification::session_state_changed(&higher_order)
            .to_payload()
            .expect("serialize typed notification"),
    )
    .expect("parse typed notification");

    handle_session_update_notification(&state, &notification).await;

    assert!(
        !state.sessions.lock().await.contains_key("123456"),
        "same-timestamp different payload should invalidate stale cache"
    );
}

#[tokio::test]
async fn session_update_notification_without_local_interest_does_not_reload_or_broadcast() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let notification = parse_session_update_notification("123456").expect("parse notification");

    handle_session_update_notification(&state, &notification).await;

    assert_eq!(
        store.load_calls(),
        0,
        "uninterested replica should not reload session"
    );
    assert!(
        !state.sessions.lock().await.contains_key("123456"),
        "uninterested replica should not populate cache"
    );
}

#[tokio::test]
async fn session_update_notification_typed_payload_without_local_interest_does_not_reload_or_broadcast()
 {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let notification = parse_session_update_notification(
        &serde_json::json!({
            "kind": "session_state_changed",
            "sessionCode": "123456",
            "updatedAt": Utc::now().to_rfc3339(),
        })
        .to_string(),
    )
    .expect("parse typed notification");

    handle_session_update_notification(&state, &notification).await;

    assert_eq!(
        store.load_calls(),
        0,
        "typed uninterested replica should not reload session"
    );
    assert!(
        !state.sessions.lock().await.contains_key("123456"),
        "typed uninterested replica should not populate cache"
    );
}

#[tokio::test]
async fn session_update_notification_cached_without_registrations_evicts_without_reloading() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let timestamp = Utc::now();
    let session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    state
        .sessions
        .lock()
        .await
        .insert(session.code.0.clone(), session.clone());

    let notification = parse_session_update_notification("123456").expect("parse notification");

    handle_session_update_notification(&state, &notification).await;

    assert_eq!(
        store.load_calls(),
        0,
        "cache-only replica should not reload session"
    );
    assert!(
        !state.sessions.lock().await.contains_key("123456"),
        "cache-only replica should evict stale cache without repopulating it"
    );
}

#[tokio::test]
async fn typed_notification_followed_by_legacy_notification_does_not_rebroadcast_same_update() {
    let state = test_state();
    let timestamp = Utc::now();
    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        "player-1", "Alice", timestamp, true, true,
    ));

    state
        .store
        .save_session(&session)
        .await
        .expect("seed persisted session");
    state
        .sessions
        .lock()
        .await
        .insert(session.code.0.clone(), session.clone());

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    state
        .realtime
        .lock()
        .await
        .attach(&session.code.0, "player-1", "conn-1");
    state
        .realtime_senders
        .lock()
        .await
        .insert("conn-1".to_string(), sender);

    let typed_notification = parse_session_update_notification(
        &SessionUpdateNotification::session_state_changed(&session)
            .to_payload()
            .expect("serialize typed notification"),
    )
    .expect("parse typed notification");
    let legacy_notification =
        parse_session_update_notification("123456").expect("parse legacy notification");

    handle_session_update_notification(&state, &typed_notification).await;
    handle_session_update_notification(&state, &legacy_notification).await;

    assert!(
        receiver.try_recv().is_err(),
        "duplicate legacy follow-up should not rebroadcast"
    );
    assert!(
        state.sessions.lock().await.contains_key("123456"),
        "typed notification should keep matched cache hot"
    );
}

#[tokio::test]
async fn legacy_notification_after_typed_dedupe_window_is_still_processed() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let timestamp = Utc::now();
    let session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed persisted session");
    state
        .sessions
        .lock()
        .await
        .insert(session.code.0.clone(), session.clone());

    let typed_notification = parse_session_update_notification(
        &SessionUpdateNotification::session_state_changed(&session)
            .to_payload()
            .expect("serialize typed notification"),
    )
    .expect("parse typed notification");
    let legacy_notification =
        parse_session_update_notification("123456").expect("parse legacy notification");

    handle_session_update_notification(&state, &typed_notification).await;
    handle_session_update_notification(&state, &legacy_notification).await;
    assert!(
        state.sessions.lock().await.contains_key("123456"),
        "first legacy follow-up should be deduped"
    );

    handle_session_update_notification(&state, &legacy_notification).await;

    assert!(
        !state.sessions.lock().await.contains_key("123456"),
        "later legacy-only invalidation should still evict stale cache"
    );
}

#[tokio::test]
async fn realtime_replaced_notification_clears_local_registration_without_persisting_disconnect() {
    let state = test_state();
    let timestamp = Utc::now();
    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        "player-1", "Alice", timestamp, true, true,
    ));
    state
        .store
        .save_session(&session)
        .await
        .expect("seed persisted session");
    state
        .sessions
        .lock()
        .await
        .insert(session.code.0.clone(), session.clone());

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    state
        .realtime
        .lock()
        .await
        .attach(&session.code.0, "player-1", "conn-1");
    state
        .realtime_senders
        .lock()
        .await
        .insert("conn-1".to_string(), sender);

    let notification =
        SessionUpdateNotification::realtime_connection_replaced(&RealtimeConnectionRegistration {
            session_code: session.code.0.clone(),
            player_id: "player-1".to_string(),
            connection_id: "conn-1".to_string(),
            replica_id: state.replica_id.clone(),
        });

    handle_session_update_notification(&state, &notification).await;

    let close_message = receiver
        .try_recv()
        .expect("close message sent to replaced connection");
    assert!(matches!(close_message, crate::ws::WsOutbound::Close));
    assert!(
        state
            .realtime
            .lock()
            .await
            .session_registrations(&session.code.0)
            .is_empty(),
        "replaced local registration should be cleared before socket shutdown"
    );

    super::ws::sync_ws_disconnect(&state, "conn-1").await;

    let cached = state
        .sessions
        .lock()
        .await
        .get(&session.code.0)
        .expect("cached session remains")
        .clone();
    assert!(
        cached
            .players
            .get("player-1")
            .expect("cached player exists")
            .is_connected,
        "takeover notification must not persist a false disconnect"
    );

    let persisted = state
        .store
        .load_session_by_code(&session.code.0)
        .await
        .expect("load persisted session")
        .expect("persisted session exists");
    assert!(
        !persisted
            .players
            .get("player-1")
            .expect("persisted player exists")
            .is_connected,
        "persisted sessions must still sanitize runtime-only presence"
    );
    assert_eq!(
        cached.host_player_id,
        Some("player-1".to_string()),
        "takeover notification must not reassign host ownership"
    );
    assert_eq!(
        persisted.host_player_id,
        Some("player-1".to_string()),
        "takeover notification must not durably reassign host ownership"
    );
    let artifacts = state
        .store
        .list_session_artifacts(&persisted.id.to_string())
        .await
        .expect("list artifacts");
    assert!(
        !artifacts
            .iter()
            .any(|artifact| artifact.kind == SessionArtifactKind::PlayerLeft),
        "takeover notification must not emit a PlayerLeft artifact"
    );
}

#[tokio::test]
async fn workshop_deleted_notification_closes_local_session_connections() {
    let state = test_state();
    let timestamp = Utc::now();
    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        "player-1", "Alice", timestamp, true, true,
    ));
    state
        .sessions
        .lock()
        .await
        .insert(session.code.0.clone(), session.clone());

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    state
        .realtime
        .lock()
        .await
        .attach(&session.code.0, "player-1", "conn-1");
    state
        .realtime_senders
        .lock()
        .await
        .insert("conn-1".to_string(), sender);

    handle_session_update_notification(
        &state,
        &SessionUpdateNotification::workshop_deleted(&session.code.0),
    )
    .await;

    let error_message = receiver.try_recv().expect("deleted workshop error sent");
    assert!(matches!(
        error_message,
        crate::ws::WsOutbound::Message(ServerWsMessage::Error { ref message })
            if message == "Workshop not found."
    ));
    let close_message = receiver.try_recv().expect("deleted workshop close sent");
    assert!(matches!(close_message, crate::ws::WsOutbound::Close));
    assert!(
        state
            .realtime
            .lock()
            .await
            .session_registrations(&session.code.0)
            .is_empty(),
        "deleted workshop should clear local session registrations"
    );
    assert!(
        !state.sessions.lock().await.contains_key(&session.code.0),
        "deleted workshop notification should evict cached session"
    );
}

#[tokio::test]
async fn clearing_local_realtime_before_close_prevents_false_disconnect_fallback() {
    let state = test_state();
    let timestamp = Utc::now();
    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        "player-1", "Alice", timestamp, true, true,
    ));
    state
        .store
        .save_session(&session)
        .await
        .expect("seed persisted session");
    state
        .sessions
        .lock()
        .await
        .insert(session.code.0.clone(), session.clone());

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    state
        .realtime
        .lock()
        .await
        .attach(&session.code.0, "player-1", "conn-1");
    state
        .realtime_senders
        .lock()
        .await
        .insert("conn-1".to_string(), sender);

    super::ws::clear_local_realtime_connection(&state, "conn-1").await;
    super::ws::close_local_connection(&state, "conn-1").await;

    let close_message = receiver
        .try_recv()
        .expect("close message sent to stale connection");
    assert!(matches!(close_message, crate::ws::WsOutbound::Close));

    super::ws::sync_ws_disconnect(&state, "conn-1").await;

    let cached = state
        .sessions
        .lock()
        .await
        .get(&session.code.0)
        .expect("cached session remains")
        .clone();
    assert!(
        cached
            .players
            .get("player-1")
            .expect("cached player exists")
            .is_connected,
        "fallback close path must not persist a false disconnect after local ownership is cleared"
    );
    assert_eq!(cached.host_player_id, Some("player-1".to_string()));

    let artifacts = state
        .store
        .list_session_artifacts(&session.id.to_string())
        .await
        .expect("list artifacts");
    assert!(
        !artifacts
            .iter()
            .any(|artifact| artifact.kind == SessionArtifactKind::PlayerLeft),
        "fallback close path must not emit a PlayerLeft artifact"
    );
}

#[tokio::test]
async fn retired_connection_id_cannot_reattach_before_socket_closes() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });

    let (addr, server_handle) = spawn_test_server(app).await;
    let mut stream = connect_raw_ws_with_cookie(addr, Some(&cookie)).await;
    send_raw_ws_message(&mut stream, &attach_message).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let connection_id = {
        let registrations = state
            .realtime
            .lock()
            .await
            .session_registrations(&create_success.session_code);
        assert_eq!(registrations.len(), 1, "expected one local registration");
        registrations[0].connection_id.clone()
    };

    super::ws::clear_local_realtime_connection(&state, &connection_id).await;
    send_raw_ws_message(&mut stream, &attach_message).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert!(
        state
            .realtime
            .lock()
            .await
            .session_registrations(&create_success.session_code)
            .is_empty(),
        "retired connection id must not be able to reattach before shutdown completes"
    );

    let _ = stream.shutdown().await;
    drop(stream);
    server_handle.abort();
}

#[tokio::test]
async fn same_replica_replaced_connection_is_retired_before_close_signal() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });

    let (addr, server_handle) = spawn_test_server(app).await;
    let mut first_stream = connect_raw_ws_with_cookie(addr, Some(&cookie)).await;
    send_raw_ws_message(&mut first_stream, &attach_message).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let first_connection_id = {
        let registrations = state
            .realtime
            .lock()
            .await
            .session_registrations(&create_success.session_code);
        assert_eq!(
            registrations.len(),
            1,
            "first attach should register connection"
        );
        registrations[0].connection_id.clone()
    };

    let (mut second_socket, _) = connect_async(ws_request_with_cookie(addr, Some(&cookie)))
        .await
        .expect("connect replacement ws");
    second_socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send replacement attach");
    let _ = second_socket
        .next()
        .await
        .expect("replacement state frame")
        .expect("replacement state message");

    send_raw_ws_message(&mut first_stream, &attach_message).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let registrations = state
        .realtime
        .lock()
        .await
        .session_registrations(&create_success.session_code);
    assert_eq!(
        registrations.len(),
        1,
        "replaced socket must not reclaim ownership"
    );
    assert_ne!(
        registrations[0].connection_id, first_connection_id,
        "replacement owner must remain active"
    );

    let _ = second_socket.close(None).await;
    let _ = first_stream.shutdown().await;
    drop(first_stream);
    server_handle.abort();
}

#[tokio::test]
async fn same_socket_cannot_attach_to_different_player_after_already_attached() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(
                    serde_json::to_vec(&JoinWorkshopRequest {
                        session_code: create_success.session_code.clone(),
                        name: Some("Bob".to_string()),
                        character_id: None,
                        reconnect_token: None,
                    })
                    .expect("encode join request"),
                ))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    let first_attach = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    let second_attach = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: join_success.session_code.clone(),
        player_id: join_success.player_id.clone(),
        reconnect_token: join_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });

    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request_with_cookie(addr, Some(&cookie)))
        .await
        .expect("connect ws");
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&first_attach)
                .expect("encode first attach")
                .into(),
        ))
        .await
        .expect("send first attach");
    let _ = socket
        .next()
        .await
        .expect("first state frame")
        .expect("first state message");

    socket
        .send(WsMessage::Text(
            serde_json::to_string(&second_attach)
                .expect("encode second attach")
                .into(),
        ))
        .await
        .expect("send second attach");
    let message = socket
        .next()
        .await
        .expect("second response frame")
        .expect("second response message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::Error { message } => assert_eq!(
            message,
            "WebSocket is already attached to a different player."
        ),
        other => panic!("expected close error, got {other:?}"),
    }

    let registrations = state
        .realtime
        .lock()
        .await
        .session_registrations(&create_success.session_code);
    assert_eq!(
        registrations.len(),
        1,
        "original ownership must remain intact"
    );
    assert_eq!(registrations[0].player_id, create_success.player_id);

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn workshop_command_endpoint_is_rate_limited_for_repeated_requests() {
    let mut state = test_state();
    state.config = Arc::new(AppConfig {
        command_rate_limit: 1,
        ..state.config.as_ref().clone()
    });
    state.command_limiter = Arc::new(tokio::sync::Mutex::new(
        security::FixedWindowRateLimiter::new(1, 60_000),
    ));
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    // Seed the host character so the first StartPhase1 succeeds; the second
    // hits the command rate limiter before any phase-guard runs.
    seed_selected_characters(&state, &create_success.session_code).await;

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&WorkshopCommandRequest {
                        session_code: create_success.session_code.clone(),
                        reconnect_token: create_success.reconnect_token.clone(),
                        coordinator_type: Some(CoordinatorType::Rust),
                        command: SessionCommand::StartPhase1,
                        payload: None,
                    })
                    .expect("encode first command request"),
                ))
                .expect("build first command request"),
        )
        .await
        .expect("call first command endpoint");
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&WorkshopCommandRequest {
                        session_code: create_success.session_code.clone(),
                        reconnect_token: create_success.reconnect_token.clone(),
                        coordinator_type: Some(CoordinatorType::Rust),
                        command: SessionCommand::StartPhase1,
                        payload: None,
                    })
                    .expect("encode second command request"),
                ))
                .expect("build second command request"),
        )
        .await
        .expect("call second command endpoint");

    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = to_bytes(second.into_body(), usize::MAX)
        .await
        .expect("read rate limited command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse rate limited command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(
                error.error,
                "Too many requests. Please slow down and try again."
            );
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn allocate_session_code_treats_store_errors_as_unavailable() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    store.fail_loads();

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        allocate_session_code(&state),
    )
    .await;

    assert!(
        result.is_err(),
        "allocation should keep retrying while store reads fail"
    );
}

fn test_state_with_store(store: Arc<dyn SessionStore>) -> AppState {
    let mut state = test_state();
    state.store = store;
    state
}

fn test_state_with_reconnect_ttl(ttl: std::time::Duration) -> AppState {
    let mut state = test_state();
    state.config = Arc::new(AppConfig {
        reconnect_token_ttl: ttl,
        ..state.config.as_ref().clone()
    });
    state
}

fn test_state_with_sprite_queue_timeout(timeout: std::time::Duration) -> AppState {
    let mut state = test_state();
    state.config = Arc::new(AppConfig {
        sprite_queue_timeout: timeout,
        ..state.config.as_ref().clone()
    });
    state
}

async fn overwrite_identity_last_seen_at(
    store: &dyn SessionStore,
    session_id: &str,
    player_id: &str,
    reconnect_token: &str,
    last_seen_at: chrono::DateTime<Utc>,
) {
    store
        .create_player_identity(&persistence::PlayerIdentity {
            session_id: session_id.to_string(),
            player_id: player_id.to_string(),
            reconnect_token: reconnect_token.to_string(),
            created_at: last_seen_at.to_rfc3339(),
            last_seen_at: last_seen_at.to_rfc3339(),
        })
        .await
        .expect("overwrite player identity");
}

#[tokio::test]
async fn workshop_judge_bundle_returns_bundle_for_completed_session() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &session_code).await;
    for request_body in [
        setup_phase1_body(&session_code, &create_success.reconnect_token),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_code).expect("session exists");
    let alice_current_dragon_id = session
        .players
        .get(&create_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("alice current dragon id");
    let bob_current_dragon_id = session
        .players
        .get(&join_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("bob current dragon id");
    let session_id = session.id.to_string();
    drop(sessions);

    for request_body in [
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#,
            session_code, create_success.reconnect_token, alice_current_dragon_id
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#,
            session_code, join_success.reconnect_token, bob_current_dragon_id
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build vote request"),
            )
            .await
            .expect("call submitVote command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let reveal_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build reveal results request"),
             )
              .await
              .expect("call revealVotingResults command");
    assert_eq!(reveal_response.status(), StatusCode::OK);

    let end_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endSession"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build end session request"),
        )
        .await
        .expect("call endSession command");
    assert_eq!(end_response.status(), StatusCode::OK);

    state
        .store
        .append_session_artifact(&SessionArtifactRecord {
            id: "artifact-action-1".into(),
            session_id: session_id.clone(),
            phase: protocol::Phase::Phase2,
            step: 2,
            kind: SessionArtifactKind::ActionProcessed,
            player_id: Some(join_success.player_id.clone()),
            created_at: "2026-01-01T00:00:00Z".into(),
            payload: serde_json::json!({
                "dragonId": bob_current_dragon_id,
                "actionType": "feed",
                "actionValue": "meat",
                "hunger": 88,
                "energy": 100,
                "happiness": 97
            }),
        })
        .await
        .expect("append action artifact");

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/judge-bundle")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build judge bundle request"),
        )
        .await
        .expect("call judge bundle endpoint");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read judge bundle body");
    let result: WorkshopJudgeBundleResult =
        serde_json::from_slice(&body).expect("parse judge bundle result");
    let success = match result {
        WorkshopJudgeBundleResult::Success(success) => success,
        WorkshopJudgeBundleResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    };

    assert!(success.ok);
    assert_eq!(success.bundle.session_code, session_code);
    assert_eq!(success.bundle.current_phase, protocol::Phase::End);
    assert_eq!(success.bundle.players.len(), 2);
    assert_eq!(success.bundle.dragons.len(), 2);
    let judged_dragon = success
        .bundle
        .dragons
        .iter()
        .find(|dragon| dragon.dragon_id == bob_current_dragon_id)
        .expect("judged dragon bundle");
    assert_eq!(judged_dragon.creative_vote_count, 1);
    assert_eq!(
        judged_dragon.handover_chain.discovery_observations,
        Vec::<String>::new()
    );
    assert_eq!(judged_dragon.phase2_actions.len(), 1);
    assert_eq!(judged_dragon.phase2_actions[0].player_name, "Bob");
    assert_eq!(judged_dragon.phase2_actions[0].action_type, "feed");
}

#[tokio::test]
async fn create_workshop_endpoint_returns_join_success() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build request"),
        )
        .await
        .expect("call create workshop");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Success(success) => {
            assert!(success.ok);
            assert_eq!(success.coordinator_type, CoordinatorType::Rust);
            assert_eq!(
                success.state.current_player_id.as_deref(),
                Some(success.player_id.as_str())
            );
            assert_eq!(success.state.players.len(), 1);
            let host = success
                .state
                .players
                .get(&success.player_id)
                .expect("host player in state");
            assert!(host.character_id.is_some());
            assert!(host.pet_description.is_some());
            assert!(host.is_ready);
        }
        WorkshopJoinResult::Error(error) => panic!("expected success, got error: {}", error.error),
    }
}

#[tokio::test]
async fn create_workshop_lobby_endpoint_creates_empty_reserved_lobby() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/lobby")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create lobby request"),
        )
        .await
        .expect("call create workshop lobby");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read create lobby body");
    let result: WorkshopCreateResult =
        serde_json::from_slice(&body).expect("parse create lobby result");
    let success = match result {
        WorkshopCreateResult::Success(success) => success,
        WorkshopCreateResult::Error(error) => {
            panic!("expected create lobby success, got error: {}", error.error)
        }
    };

    let session = state
        .store
        .load_session_by_code(&success.session_code)
        .await
        .expect("load created lobby")
        .expect("created lobby exists");
    assert!(session.players.is_empty());
    assert_eq!(session.host_player_id, None);
    assert_eq!(session.reserved_host_name(), Some("Alice"));
    assert!(session.reserved_host_account_id().is_some());
}

#[tokio::test]
async fn create_workshop_lobby_appears_in_open_workshops_with_zero_players() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/lobby")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create lobby request"),
        )
        .await
        .expect("call create workshop lobby");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create lobby body");
    let create_result: WorkshopCreateResult =
        serde_json::from_slice(&create_body).expect("parse create lobby result");
    let session_code = match create_result {
        WorkshopCreateResult::Success(success) => success.session_code,
        WorkshopCreateResult::Error(error) => {
            panic!("expected create lobby success, got error: {}", error.error)
        }
    };

    let (status, value) = get_open_workshops_json(&app, &cookie, "").await;
    assert_eq!(status, StatusCode::OK);
    let workshops = value["workshops"].as_array().expect("workshops array");
    let created = workshops
        .iter()
        .find(|row| row["sessionCode"].as_str() == Some(session_code.as_str()))
        .expect("created lobby appears in open workshops");
    assert_eq!(created["hostName"].as_str(), Some("Alice"));
    assert_eq!(created["playerCount"].as_u64(), Some(0));
    assert_eq!(created["canDelete"].as_bool(), Some(true));
}

#[tokio::test]
async fn open_workshops_marks_only_own_reserved_lobby_as_deletable() {
    let state = test_state();
    let app = build_app(state.clone());
    let alice_cookie = test_auth_cookie(&app, "Alice").await;
    let bob_cookie = test_auth_cookie(&app, "Bob").await;

    let alice_create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/lobby")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &alice_cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build Alice create lobby request"),
        )
        .await
        .expect("call Alice create workshop lobby");
    let alice_body = to_bytes(alice_create.into_body(), usize::MAX)
        .await
        .expect("read Alice create body");
    let alice_result: WorkshopCreateResult =
        serde_json::from_slice(&alice_body).expect("parse Alice create result");
    let alice_code = match alice_result {
        WorkshopCreateResult::Success(success) => success.session_code,
        WorkshopCreateResult::Error(error) => panic!("expected success, got {}", error.error),
    };

    let bob_create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/lobby")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &bob_cookie)
                .body(Body::from(create_workshop_body("Bob")))
                .expect("build Bob create lobby request"),
        )
        .await
        .expect("call Bob create workshop lobby");
    let bob_body = to_bytes(bob_create.into_body(), usize::MAX)
        .await
        .expect("read Bob create body");
    let bob_result: WorkshopCreateResult =
        serde_json::from_slice(&bob_body).expect("parse Bob create result");
    let bob_code = match bob_result {
        WorkshopCreateResult::Success(success) => success.session_code,
        WorkshopCreateResult::Error(error) => panic!("expected success, got {}", error.error),
    };

    let (status, alice_view) = get_open_workshops_json(&app, &alice_cookie, "").await;
    assert_eq!(status, StatusCode::OK);
    let alice_rows = alice_view["workshops"].as_array().expect("Alice rows");
    assert_eq!(
        alice_rows
            .iter()
            .find(|row| row["sessionCode"].as_str() == Some(alice_code.as_str()))
            .and_then(|row| row["canDelete"].as_bool()),
        Some(true)
    );
    assert_eq!(
        alice_rows
            .iter()
            .find(|row| row["sessionCode"].as_str() == Some(bob_code.as_str()))
            .and_then(|row| row["canDelete"].as_bool()),
        Some(false)
    );
}

#[tokio::test]
async fn open_workshops_marks_legacy_reserved_lobby_as_deletable_for_owner() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let account_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/accounts/me")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build accounts/me request"),
        )
        .await
        .expect("accounts/me response");
    assert_eq!(account_response.status(), StatusCode::OK);
    let account_body = to_bytes(account_response.into_body(), 64 * 1024)
        .await
        .expect("read accounts/me body");
    let account_value: serde_json::Value =
        serde_json::from_slice(&account_body).expect("accounts/me json");
    let account_id = account_value["id"]
        .as_str()
        .expect("account id")
        .to_string();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("654321".to_string()),
        Utc::now(),
        protocol::WorkshopCreateConfig::default(),
    );
    session.reserve_host(account_id, "Alice");
    session.owner_account_id = None;
    state
        .store
        .save_session(&session)
        .await
        .expect("save legacy lobby");

    let (status, view) = get_open_workshops_json(&app, &cookie, "").await;
    assert_eq!(status, StatusCode::OK);
    let rows = view["workshops"].as_array().expect("rows");
    assert_eq!(
        rows.iter()
            .find(|row| row["sessionCode"].as_str() == Some("654321"))
            .and_then(|row| row["canDelete"].as_bool()),
        Some(true)
    );
}

#[tokio::test]
async fn list_open_workshops_same_timestamp_orders_by_session_code_and_pages_without_gaps() {
    let state = test_state();
    let shared_created_at =
        chrono::DateTime::<Utc>::from_timestamp(2_000, 0).expect("valid timestamp");
    for code in ["200003", "200001", "200002"] {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode(code.to_string()),
            shared_created_at,
            protocol::WorkshopCreateConfig::default(),
        );
        session.reserve_host(format!("acct-{code}"), format!("Host-{code}"));
        state
            .store
            .save_session(&session)
            .await
            .expect("save tied lobby");
    }
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let (status, value) = get_open_workshops_json(&app, &cookie, "").await;
    assert_eq!(status, StatusCode::OK);
    let workshops = value["workshops"].as_array().expect("workshops array");
    let tied_codes: Vec<&str> = workshops
        .iter()
        .filter(|row| row["createdAt"].as_str() == Some("1970-01-01T00:33:20+00:00"))
        .map(|row| row["sessionCode"].as_str().expect("session code"))
        .collect();
    assert_eq!(tied_codes, vec!["200001", "200002", "200003"]);
}

#[tokio::test]
async fn create_workshop_endpoint_applies_default_config_when_omitted() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;

    // Body omits `config` entirely; the server must fall back to
    // `WorkshopCreateConfig::default()` (8/8/8 minutes).
    let body = serde_json::json!({ "name": "Alice" }).to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build request"),
        )
        .await
        .expect("call create workshop");

    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let result: WorkshopJoinResult = serde_json::from_slice(&bytes).expect("parse join result");
    let success = match result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    };

    let phases = &success.state.session.settings.phases;
    assert_eq!(
        phases
            .get(&protocol::Phase::Phase0)
            .expect("phase0 settings")
            .duration_seconds,
        8 * 60,
        "phase0 should default to 8 minutes",
    );
    assert_eq!(
        phases
            .get(&protocol::Phase::Phase1)
            .expect("phase1 settings")
            .duration_seconds,
        8 * 60,
        "phase1 should default to 8 minutes",
    );
    assert_eq!(
        phases
            .get(&protocol::Phase::Phase2)
            .expect("phase2 settings")
            .duration_seconds,
        8 * 60,
        "phase2 should default to 8 minutes",
    );
}

#[tokio::test]
async fn workshop_judge_bundle_rejects_invalid_credentials() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let session_code = match create_result {
        WorkshopJoinResult::Success(success) => success.session_code,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/judge-bundle")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"missing"}}"#,
                    session_code
                )))
                .expect("build request"),
        )
        .await
        .expect("call judge bundle endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read judge bundle body");
    let result: WorkshopJudgeBundleResult =
        serde_json::from_slice(&body).expect("parse judge bundle result");
    match result {
        WorkshopJudgeBundleResult::Error(error) => {
            assert_eq!(error.error, "Session identity is invalid or expired.");
        }
        WorkshopJudgeBundleResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_judge_bundle_rejects_expired_reconnect_token() {
    let state = test_state_with_reconnect_ttl(std::time::Duration::from_secs(60));
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    overwrite_identity_last_seen_at(
        state.store.as_ref(),
        &create_success.state.session.id,
        &create_success.player_id,
        &create_success.reconnect_token,
        Utc::now() - ChronoDuration::seconds(61),
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/judge-bundle")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build expired judge bundle request"),
        )
        .await
        .expect("call judge bundle endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read judge bundle body");
    let result: WorkshopJudgeBundleResult =
        serde_json::from_slice(&body).expect("parse judge bundle result");
    match result {
        WorkshopJudgeBundleResult::Error(error) => {
            assert_eq!(error.error, "Session identity is invalid or expired.");
        }
        WorkshopJudgeBundleResult::Success(_) => panic!("expected error response"),
    }

    let found = state
        .store
        .find_player_identity(
            &create_success.session_code,
            &create_success.reconnect_token,
        )
        .await
        .expect("find expired reconnect token after judge bundle check");
    assert_eq!(found, None);
}

#[tokio::test]
async fn workshop_judge_bundle_rejects_non_host_requests() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    create_success.session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/judge-bundle")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                    create_success.session_code, join_success.reconnect_token
                )))
                .expect("build judge bundle request"),
        )
        .await
        .expect("call judge bundle endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read judge bundle body");
    let result: WorkshopJudgeBundleResult =
        serde_json::from_slice(&body).expect("parse judge bundle result");
    match result {
        WorkshopJudgeBundleResult::Error(error) => {
            assert_eq!(error.error, "Only the host can build the workshop archive.");
        }
        WorkshopJudgeBundleResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_judge_bundle_rejects_requests_before_end_phase() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/judge-bundle")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build judge bundle request"),
        )
        .await
        .expect("call judge bundle endpoint");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read judge bundle body");
    let result: WorkshopJudgeBundleResult =
        serde_json::from_slice(&body).expect("parse judge bundle result");
    match result {
        WorkshopJudgeBundleResult::Error(error) => {
            assert_eq!(
                error.error,
                "Workshop archive can only be built after the game ends."
            );
        }
        WorkshopJudgeBundleResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn generate_sprite_sheet_times_out_to_fallback_companion() {
    let state = test_state_with_sprite_queue_timeout(std::time::Duration::from_millis(20));
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &create_success.session_code).await;
    let mut queue_permits = Vec::new();
    for _ in 0..state.config.image_job_max_concurrency {
        queue_permits.push(
            state
                .image_job_queue
                .clone()
                .try_acquire_owned()
                .expect("reserve queue permit for timeout test"),
        );
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/sprite-sheet")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","description":"A bronze dragon with bright wings"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build sprite sheet request"),
        )
        .await
        .expect("call sprite sheet endpoint");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read sprite sheet body");
    let result: SpriteSheetResult =
        serde_json::from_slice(&body).expect("parse sprite sheet result");
    let success = match result {
        SpriteSheetResult::Success(success) => success,
        SpriteSheetResult::Error(error) => panic!("expected success, got error: {}", error.error),
    };
    assert!(success.fallback_used);
    assert_eq!(success.sprites, timeout_companion_defaults().sprites);

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("cached session exists");
    let player = session
        .players
        .get(&create_success.player_id)
        .expect("player exists");
    let selected_character = player
        .selected_character
        .as_ref()
        .expect("selected character saved");
    let character_id = player.character_id.clone().expect("character id present");
    assert_eq!(
        Some(character_id.as_str()),
        Some(selected_character.id.as_str())
    );
    assert_eq!(
        selected_character.description,
        "A bronze dragon with bright wings"
    );
    assert_eq!(
        selected_character.sprites,
        timeout_companion_defaults().sprites
    );
    assert_eq!(selected_character.remaining_sprite_regenerations, 0);
    drop(sessions);

    let persisted_character = state
        .store
        .load_character(&character_id)
        .await
        .expect("load saved character")
        .expect("saved character exists");
    assert_eq!(
        persisted_character.description,
        "A bronze dragon with bright wings"
    );
    assert_eq!(
        persisted_character.sprites,
        timeout_companion_defaults().sprites
    );
    assert_eq!(persisted_character.remaining_sprite_regenerations, 0);

    let artifacts = state
        .store
        .list_session_artifacts(&create_success.state.session.id)
        .await
        .expect("list artifacts");
    let artifact = artifacts
        .iter()
        .rev()
        .find(|artifact| artifact.kind == SessionArtifactKind::PetProfileUpdated)
        .expect("pet profile artifact exists");
    assert!(artifact.payload.get("characterId").is_some());
    assert_eq!(
        artifact.payload.get("fallbackUsed"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        artifact.payload.get("remainingSpriteRegenerations"),
        Some(&serde_json::json!(0))
    );
    drop(queue_permits);
}

#[tokio::test]
async fn generate_sprite_sheet_provider_failure_uses_fallback_companion() {
    let _env_lock = lock_env();
    let _google_credentials = ScopedEnvVar::set(
        "GOOGLE_APPLICATION_CREDENTIALS",
        "/definitely/missing-test-credentials.json",
    );
    let _google_project = ScopedEnvVar::set("GOOGLE_CLOUD_PROJECT", "rna-workshop2");
    let _google_location = ScopedEnvVar::set("GOOGLE_CLOUD_LOCATION", "us-central1");

    let mut state = test_state();
    state.config = Arc::new(AppConfig {
        llm_pool: LlmPoolConfig {
            google_cloud_project: Some("rna-workshop2".to_string()),
            google_cloud_location: Some("us-central1".to_string()),
            judge_providers: Vec::new(),
            image_providers: vec![ResolvedProvider {
                kind: LlmProviderKind::VertexAi,
                model: "gemini-2.5-flash-image".to_string(),
                api_key: None,
            }],
        },
        ..state.config.as_ref().clone()
    });
    state.llm_client = Arc::new(LlmClient::new(state.config.llm_pool.clone()));

    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &create_success.session_code).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/sprite-sheet")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","description":"A bronze dragon with bright wings"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build sprite sheet request"),
        )
        .await
        .expect("call sprite sheet endpoint");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read sprite sheet body");
    let result: SpriteSheetResult =
        serde_json::from_slice(&body).expect("parse sprite sheet result");
    let success = match result {
        SpriteSheetResult::Success(success) => success,
        SpriteSheetResult::Error(error) => panic!("expected success, got error: {}", error.error),
    };
    assert!(success.fallback_used);
    assert_eq!(success.sprites, timeout_companion_defaults().sprites);

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("cached session exists");
    let player = session
        .players
        .get(&create_success.player_id)
        .expect("player exists");
    let selected_character = player
        .selected_character
        .as_ref()
        .expect("selected character saved");
    let character_id = player.character_id.clone().expect("character id present");
    assert_eq!(
        Some(character_id.as_str()),
        Some(selected_character.id.as_str())
    );
    assert_eq!(
        selected_character.description,
        "A bronze dragon with bright wings"
    );
    assert_eq!(
        selected_character.sprites,
        timeout_companion_defaults().sprites
    );
    assert_eq!(selected_character.remaining_sprite_regenerations, 0);
    drop(sessions);

    let persisted_character = state
        .store
        .load_character(&character_id)
        .await
        .expect("load saved character")
        .expect("saved character exists");
    assert_eq!(
        persisted_character.description,
        "A bronze dragon with bright wings"
    );
    assert_eq!(
        persisted_character.sprites,
        timeout_companion_defaults().sprites
    );
    assert_eq!(persisted_character.remaining_sprite_regenerations, 0);

    let artifacts = state
        .store
        .list_session_artifacts(&create_success.state.session.id)
        .await
        .expect("list artifacts");
    let artifact = artifacts
        .iter()
        .rev()
        .find(|artifact| artifact.kind == SessionArtifactKind::PetProfileUpdated)
        .expect("pet profile artifact exists");
    assert!(artifact.payload.get("characterId").is_some());
    assert_eq!(
        artifact.payload.get("fallbackUsed"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        artifact.payload.get("remainingSpriteRegenerations"),
        Some(&serde_json::json!(0))
    );
}

#[tokio::test]
async fn create_workshop_endpoint_succeeds_even_with_empty_payload_name() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("   ")))
                .expect("build request"),
        )
        .await
        .expect("call create workshop");

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Success(success) => {
            assert!(success.ok);
        }
        WorkshopJoinResult::Error(error) => panic!("expected success, got error: {}", error.error),
    }
}

#[tokio::test]
async fn generate_character_sprite_sheet_preserves_owned_character_owner_account_id() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_character_body = serde_json::json!({
        "description": "Alice owned dragon",
        "sprites": {
            "neutral": "owned-neutral",
            "happy": "owned-happy",
            "angry": "owned-angry",
            "sleepy": "owned-sleepy"
        }
    })
    .to_string();
    let create_character_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_character_body))
                .expect("build create character request"),
        )
        .await
        .expect("call create character");
    assert_eq!(create_character_response.status(), StatusCode::CREATED);
    let create_character_body = to_bytes(create_character_response.into_body(), usize::MAX)
        .await
        .expect("read create character body");
    let created_character: serde_json::Value =
        serde_json::from_slice(&create_character_body).expect("parse created character");
    let character_id = created_character["id"]
        .as_str()
        .expect("created character id")
        .to_string();

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(format!(
                    r#"{{"name":"Alice","characterId":"{}","config":{{"phase0Minutes":5,"phase1Minutes":10,"phase2Minutes":10}}}}"#,
                    character_id
                )))
                .expect("build create workshop request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create workshop body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create workshop result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!(
                "expected create workshop success, got error: {}",
                error.error
            )
        }
    };

    let session_code = create_success.session_code.clone();
    let owner_account_id = {
        let sessions = state.sessions.lock().await;
        let session = sessions.get(&session_code).expect("session exists");
        session
            .owner_account_id
            .clone()
            .expect("workshop owner account id")
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters/sprite-sheet")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","characterId":"{}","description":"A bright silver dragon with teal horns"}}"#,
                    session_code, create_success.reconnect_token, character_id
                )))
                .expect("build character sprite sheet request"),
        )
        .await
        .expect("call character sprite sheet endpoint");

    assert_eq!(response.status(), StatusCode::OK);

    let persisted_character = state
        .store
        .load_character(&character_id)
        .await
        .expect("load saved character")
        .expect("saved character exists");
    assert_eq!(persisted_character.owner_account_id, Some(owner_account_id));
}

#[tokio::test]
async fn workshop_command_rejects_duplicate_starter_selection_in_lobby() {
    let state = test_state();

    let starter = persistence::CharacterRecord {
        id: "starter_shared_001".to_string(),
        name: None,
        description: "Shared starter dragon".to_string(),
        sprites: protocol::SpriteSet {
            neutral: "starter-neutral".to_string(),
            happy: "starter-happy".to_string(),
            angry: "starter-angry".to_string(),
            sleepy: "starter-sleepy".to_string(),
        },
        remaining_sprite_regenerations: 0,
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
        owner_account_id: None,
    };
    state
        .store
        .save_character(&starter)
        .await
        .expect("seed starter");

    let app = build_app(state.clone());
    let alice_cookie = test_auth_cookie(&app, "Alice").await;
    let bob_cookie = test_auth_cookie(&app, "Bob").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/lobby")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &alice_cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create lobby request"),
        )
        .await
        .expect("call create lobby workshop");
    assert_eq!(create_response.status(), StatusCode::CREATED);
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create lobby body");
    let create_result: WorkshopCreateResult =
        serde_json::from_slice(&create_body).expect("parse create lobby result");
    let session_code = match create_result {
        WorkshopCreateResult::Success(success) => success.session_code,
        WorkshopCreateResult::Error(error) => {
            panic!("expected create lobby success, got error: {}", error.error)
        }
    };

    let host_join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &alice_cookie)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Alice"}}"#,
                    session_code
                )))
                .expect("build host join request"),
        )
        .await
        .expect("call host join");
    assert_eq!(host_join_response.status(), StatusCode::OK);
    let host_join_body = to_bytes(host_join_response.into_body(), usize::MAX)
        .await
        .expect("read host join body");
    let host_join_result: WorkshopJoinResult =
        serde_json::from_slice(&host_join_body).expect("parse host join result");
    let host_join_success = match host_join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected host join success, got error: {}", error.error)
        }
    };
    let host_starter_id = host_join_success
        .state
        .players
        .get(&host_join_success.player_id)
        .and_then(|player| player.character_id.clone())
        .expect("host should lease a starter on join");

    let guest_join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &bob_cookie)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build guest join request"),
        )
        .await
        .expect("call guest join");
    assert_eq!(guest_join_response.status(), StatusCode::OK);
    let guest_join_body = to_bytes(guest_join_response.into_body(), usize::MAX)
        .await
        .expect("read guest join body");
    let guest_join_result: WorkshopJoinResult =
        serde_json::from_slice(&guest_join_body).expect("parse guest join result");
    let guest_join_success = match guest_join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected guest join success, got error: {}", error.error)
        }
    };

    let guest_select_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"selectCharacter","payload":{{"characterId":"{}"}}}}"#,
                    session_code, guest_join_success.reconnect_token, host_starter_id
                )))
                .expect("build guest select request"),
        )
        .await
        .expect("call guest selectCharacter");

    assert_eq!(guest_select_response.status(), StatusCode::BAD_REQUEST);
    let guest_select_body = to_bytes(guest_select_response.into_body(), usize::MAX)
        .await
        .expect("read guest select body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&guest_select_body).expect("parse guest select result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(
                error.error,
                "That starter is already taken in this workshop."
            );
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn create_workshop_endpoint_rejects_forbidden_origin() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .header("origin", "https://evil.example.com")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build request"),
        )
        .await
        .expect("call create workshop");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read error body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Error(error) => assert_eq!(error.error, "Origin is not allowed."),
        WorkshopJoinResult::Success(_) => panic!("expected error response"),
    }
}

#[test]
fn load_config_reads_server_side_llm_settings() {
    let _env_lock = lock_env();
    let _bind = ScopedEnvVar::set("APP_SERVER_BIND_ADDR", "127.0.0.1:4100");
    let _app_url = ScopedEnvVar::set("VITE_APP_URL", "http://127.0.0.1:4100");
    let _origins = ScopedEnvVar::set("ALLOWED_ORIGINS", "http://127.0.0.1:4100");
    let _node_env = ScopedEnvVar::set("NODE_ENV", "development");
    let _database = ScopedEnvVar::set("DATABASE_URL", "postgres://user:pass@localhost:5432/db");
    let _project = ScopedEnvVar::set("GOOGLE_CLOUD_PROJECT", "dragon-shift-prod");
    let _location = ScopedEnvVar::set("GOOGLE_CLOUD_LOCATION", "us-central1");
    let _judge_providers = ScopedEnvVar::set(
        "LLM_JUDGE_PROVIDERS",
        r#"[{"type":"api_key","model":"gemini-2.5-flash","apiKeyEnvVar":"LLM_JUDGE_API_KEY_0"}]"#,
    );
    let _image_providers = ScopedEnvVar::set(
        "LLM_IMAGE_PROVIDERS",
        r#"[{"type":"vertex_ai","model":"gemini-2.5-flash-image"}]"#,
    );
    let _judge_key = ScopedEnvVar::set("LLM_JUDGE_API_KEY_0", "judge-key");

    let config = crate::app::load_config().expect("load config");

    assert_eq!(
        config.llm_pool.google_cloud_project.as_deref(),
        Some("dragon-shift-prod")
    );
    assert_eq!(
        config.llm_pool.google_cloud_location.as_deref(),
        Some("us-central1")
    );
    assert_eq!(config.llm_pool.judge_providers.len(), 1);
    assert_eq!(config.llm_pool.judge_providers[0].model, "gemini-2.5-flash");
    assert_eq!(config.llm_pool.image_providers.len(), 1);
    assert_eq!(
        config.llm_pool.image_providers[0].model,
        "gemini-2.5-flash-image"
    );
}

#[tokio::test]
async fn create_workshop_endpoint_is_rate_limited_for_repeated_requests() {
    let app = build_app(test_state_with_limits(1, 40));
    let cookie = test_auth_cookie(&app, "Alice").await;

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .header("x-forwarded-for", "10.0.0.1")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build first request"),
        )
        .await
        .expect("call first create workshop");
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .header("x-forwarded-for", "10.0.0.1")
                .body(Body::from(create_workshop_body("Bob")))
                .expect("build second request"),
        )
        .await
        .expect("call second create workshop");

    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = to_bytes(second.into_body(), usize::MAX)
        .await
        .expect("read rate limited body");
    let result: WorkshopJoinResult =
        serde_json::from_slice(&body).expect("parse rate limited result");
    match result {
        WorkshopJoinResult::Error(error) => {
            assert_eq!(
                error.error,
                "Too many requests. Please slow down and try again."
            );
        }
        WorkshopJoinResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn create_workshop_rate_limit_ignores_spoofed_forwarded_for_by_default() {
    let app = build_app(test_state_with_limits(1, 40));
    let cookie = test_auth_cookie(&app, "Alice").await;

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .header("x-forwarded-for", "10.0.0.1")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build first request"),
        )
        .await
        .expect("call first create workshop");
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .header("x-forwarded-for", "203.0.113.99")
                .body(Body::from(create_workshop_body("Bob")))
                .expect("build second request"),
        )
        .await
        .expect("call second create workshop");

    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn create_workshop_rate_limit_uses_forwarded_for_when_trusted() {
    let mut state = test_state_with_limits(1, 40);
    state.config = Arc::new(AppConfig {
        trust_forwarded_for: true,
        ..state.config.as_ref().clone()
    });
    let app = build_app(state);
    let cookie = test_auth_cookie(&app, "Alice").await;

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .header("x-forwarded-for", "10.0.0.1")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build first request"),
        )
        .await
        .expect("call first create workshop");
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .header("x-forwarded-for", "203.0.113.99")
                .body(Body::from(create_workshop_body("Bob")))
                .expect("build second request"),
        )
        .await
        .expect("call second create workshop");

    assert_eq!(second.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn workshop_command_rate_limit_ignores_spoofed_forwarded_for_by_default() {
    let mut state = test_state();
    state.config = Arc::new(AppConfig {
        command_rate_limit: 1,
        ..state.config.as_ref().clone()
    });
    state.command_limiter = Arc::new(tokio::sync::Mutex::new(
        security::FixedWindowRateLimiter::new(1, 60_000),
    ));
    let app = build_app(state);
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .header("x-forwarded-for", "10.0.0.1")
                .body(Body::from(
                    serde_json::to_vec(&WorkshopCommandRequest {
                        session_code: create_success.session_code.clone(),
                        reconnect_token: create_success.reconnect_token.clone(),
                        coordinator_type: Some(CoordinatorType::Rust),
                        command: SessionCommand::ResetGame,
                        payload: None,
                    })
                    .expect("encode first command request"),
                ))
                .expect("build first command request"),
        )
        .await
        .expect("call first command request");
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .header("x-forwarded-for", "203.0.113.99")
                .body(Body::from(
                    serde_json::to_vec(&WorkshopCommandRequest {
                        session_code: create_success.session_code.clone(),
                        reconnect_token: create_success.reconnect_token.clone(),
                        coordinator_type: Some(CoordinatorType::Rust),
                        command: SessionCommand::ResetGame,
                        payload: None,
                    })
                    .expect("encode second command request"),
                ))
                .expect("build second command request"),
        )
        .await
        .expect("call second command request");

    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn workshop_command_rate_limit_uses_forwarded_for_when_trusted() {
    let mut state = test_state();
    state.config = Arc::new(AppConfig {
        trust_forwarded_for: true,
        command_rate_limit: 1,
        ..state.config.as_ref().clone()
    });
    state.command_limiter = Arc::new(tokio::sync::Mutex::new(
        security::FixedWindowRateLimiter::new(1, 60_000),
    ));
    let app = build_app(state);
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .header("x-forwarded-for", "10.0.0.1")
                .body(Body::from(
                    serde_json::to_vec(&WorkshopCommandRequest {
                        session_code: create_success.session_code.clone(),
                        reconnect_token: create_success.reconnect_token.clone(),
                        coordinator_type: Some(CoordinatorType::Rust),
                        command: SessionCommand::ResetGame,
                        payload: None,
                    })
                    .expect("encode first command request"),
                ))
                .expect("build first command request"),
        )
        .await
        .expect("call first command request");
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .header("x-forwarded-for", "203.0.113.99")
                .body(Body::from(
                    serde_json::to_vec(&WorkshopCommandRequest {
                        session_code: create_success.session_code.clone(),
                        reconnect_token: create_success.reconnect_token.clone(),
                        coordinator_type: Some(CoordinatorType::Rust),
                        command: SessionCommand::ResetGame,
                        payload: None,
                    })
                    .expect("encode second command request"),
                ))
                .expect("build second command request"),
        )
        .await
        .expect("call second command request");

    assert_eq!(second.status(), StatusCode::OK);
}

#[tokio::test]
async fn join_workshop_endpoint_rejects_invalid_code() {
    let app = build_app(test_state());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"sessionCode":"12ab56","name":"Bob"}"#))
                .expect("build request"),
        )
        .await
        .expect("call join workshop");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Error(error) => {
            assert_eq!(error.error, "Workshop codes must be 6 digits.")
        }
        WorkshopJoinResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn join_workshop_endpoint_rejects_missing_session() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Bob").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(r#"{"sessionCode":"123456","name":"Bob"}"#))
                .expect("build request"),
        )
        .await
        .expect("call join workshop");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Error(error) => assert_eq!(error.error, "Workshop not found."),
        WorkshopJoinResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn join_workshop_endpoint_returns_join_success_for_lobby_session() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let session_code = match create_result {
        WorkshopJoinResult::Success(success) => success.session_code,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build request"),
        )
        .await
        .expect("call join workshop");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Success(success) => {
            assert!(success.ok);
            assert_eq!(success.coordinator_type, CoordinatorType::Rust);
            assert_eq!(success.state.players.len(), 2);
            assert_eq!(
                success.state.current_player_id.as_deref(),
                Some(success.player_id.as_str())
            );
            let joined = success
                .state
                .players
                .get(&success.player_id)
                .expect("joined player in state");
            assert!(joined.character_id.is_some());
            assert!(joined.pet_description.is_some());
            assert!(joined.is_ready);
        }
        WorkshopJoinResult::Error(error) => panic!("expected success, got error: {}", error.error),
    }
}

#[tokio::test]
async fn join_workshop_endpoint_reconnects_existing_player_without_name() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call start phase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    {
        let mut sessions = state.sessions.lock().await;
        let session = sessions
            .get_mut(&create_success.session_code)
            .expect("session exists");
        let player = session
            .players
            .get_mut(&create_success.player_id)
            .expect("player exists");
        player.is_connected = false;
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build reconnect request"),
        )
        .await
        .expect("call reconnect join workshop");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read reconnect body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse reconnect result");
    match result {
        WorkshopJoinResult::Success(success) => {
            assert!(success.ok);
            assert_eq!(success.player_id, create_success.player_id);
            assert_ne!(success.reconnect_token, create_success.reconnect_token);
            assert_eq!(success.state.phase, protocol::Phase::Phase1);
            assert_eq!(
                success.state.current_player_id.as_deref(),
                Some(create_success.player_id.as_str())
            );
            let player = success
                .state
                .players
                .get(&create_success.player_id)
                .expect("reconnected player in state");
            assert!(player.is_connected);
            assert!(player.current_dragon_id.is_some());

            let revoked = state
                .store
                .find_player_identity(
                    &create_success.session_code,
                    &create_success.reconnect_token,
                )
                .await
                .expect("find revoked reconnect token");
            assert_eq!(revoked, None);

            let rotated = state
                .store
                .find_player_identity(&create_success.session_code, &success.reconnect_token)
                .await
                .expect("find rotated reconnect token");
            assert!(rotated.is_some());
        }
        WorkshopJoinResult::Error(error) => {
            panic!("expected reconnect success, got error: {}", error.error)
        }
    }
}

#[tokio::test]
async fn http_reconnect_does_not_persist_connected_presence() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    {
        let mut sessions = state.sessions.lock().await;
        let session = sessions
            .get_mut(&create_success.session_code)
            .expect("cached session exists");
        session
            .players
            .get_mut(&create_success.player_id)
            .expect("player exists")
            .is_connected = false;
    }

    let reconnect_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&JoinWorkshopRequest {
                        session_code: create_success.session_code.clone(),
                        name: None,
                        character_id: None,
                        reconnect_token: Some(create_success.reconnect_token.clone()),
                    })
                    .expect("encode reconnect request"),
                ))
                .expect("build reconnect request"),
        )
        .await
        .expect("call reconnect join");
    assert_eq!(reconnect_response.status(), StatusCode::OK);

    let persisted = state
        .store
        .load_session_by_code(&create_success.session_code)
        .await
        .expect("load persisted session after http reconnect")
        .expect("persisted session exists");
    assert!(
        !persisted
            .players
            .get(&create_success.player_id)
            .expect("persisted player exists")
            .is_connected,
        "http reconnect must not persist durable live presence"
    );
}

#[tokio::test]
async fn restart_reload_and_reconnect_keep_presence_runtime_only() {
    let store: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::new());
    let state1 = test_state_with_store(store.clone());
    let app1 = build_app(state1.clone());
    let cookie = test_auth_cookie(&app1, "Alice").await;

    let create_response = app1
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let persisted_after_create = store
        .load_session_by_code(&create_success.session_code)
        .await
        .expect("load persisted session after create")
        .expect("persisted session exists");
    assert!(
        persisted_after_create
            .players
            .values()
            .all(|player| !player.is_connected),
        "created persisted session should not store live presence"
    );

    let state2 = test_state_with_store(store.clone());
    assert!(
        ensure_session_cached(&state2, &create_success.session_code)
            .await
            .expect("reload cached session after restart"),
        "restarted app should reload session"
    );
    {
        let sessions = state2.sessions.lock().await;
        let reloaded = sessions
            .get(&create_success.session_code)
            .expect("reloaded session exists");
        assert!(
            reloaded.players.values().all(|player| !player.is_connected),
            "reloaded cache should treat presence as runtime-only"
        );
    }

    let app2 = build_app(state2.clone());
    let reconnect_response = app2
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&JoinWorkshopRequest {
                        session_code: create_success.session_code.clone(),
                        name: None,
                        character_id: None,
                        reconnect_token: Some(create_success.reconnect_token.clone()),
                    })
                    .expect("encode reconnect request"),
                ))
                .expect("build reconnect request"),
        )
        .await
        .expect("call reconnect after restart");
    assert_eq!(reconnect_response.status(), StatusCode::OK);

    let persisted_after_reconnect = store
        .load_session_by_code(&create_success.session_code)
        .await
        .expect("load persisted session after reconnect")
        .expect("persisted session exists after reconnect");
    assert!(
        persisted_after_reconnect
            .players
            .values()
            .all(|player| !player.is_connected),
        "persisted session after reconnect should still keep presence runtime-only"
    );

    let state3 = test_state_with_store(store);
    assert!(
        ensure_session_cached(&state3, &create_success.session_code)
            .await
            .expect("reload cached session after second restart"),
        "second restarted app should reload session"
    );
    let sessions = state3.sessions.lock().await;
    let reloaded = sessions
        .get(&create_success.session_code)
        .expect("reloaded session exists after reconnect");
    assert!(
        reloaded.players.values().all(|player| !player.is_connected),
        "reloaded cache after reconnect should still clear durable presence"
    );
}

#[tokio::test]
async fn postgres_restart_reload_and_reconnect_keep_presence_runtime_only() {
    let pg = PostgresAppTestStore::new(
        "postgres_restart_reload_and_reconnect_keep_presence_runtime_only",
    )
    .await;

    let state1 = test_state_with_store(pg.store.clone() as Arc<dyn SessionStore>);
    let app1 = build_app(state1.clone());
    let cookie = test_auth_cookie(&app1, "Alice").await;

    let create_response = app1
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let persisted_after_create = pg
        .store
        .load_session_by_code(&create_success.session_code)
        .await
        .expect("load persisted session after create")
        .expect("persisted session exists");
    assert!(
        persisted_after_create
            .players
            .values()
            .all(|player| !player.is_connected),
        "created persisted session should not store live presence"
    );

    let store2 = pg.reconnect().await;
    let state2 = test_state_with_store(store2.clone() as Arc<dyn SessionStore>);
    assert!(
        ensure_session_cached(&state2, &create_success.session_code)
            .await
            .expect("reload cached session after restart"),
        "restarted app should reload session"
    );
    {
        let sessions = state2.sessions.lock().await;
        let reloaded = sessions
            .get(&create_success.session_code)
            .expect("reloaded session exists");
        assert!(
            reloaded.players.values().all(|player| !player.is_connected),
            "reloaded cache should treat presence as runtime-only"
        );
    }

    let app2 = build_app(state2.clone());
    let reconnect_response = app2
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&JoinWorkshopRequest {
                        session_code: create_success.session_code.clone(),
                        name: None,
                        character_id: None,
                        reconnect_token: Some(create_success.reconnect_token.clone()),
                    })
                    .expect("encode reconnect request"),
                ))
                .expect("build reconnect request"),
        )
        .await
        .expect("call reconnect after restart");
    let reconnect_status = reconnect_response.status();
    let reconnect_body = to_bytes(reconnect_response.into_body(), usize::MAX)
        .await
        .expect("read reconnect body");
    assert_eq!(
        reconnect_status,
        StatusCode::OK,
        "unexpected reconnect body: {}",
        String::from_utf8_lossy(&reconnect_body)
    );
    let reconnect_result: WorkshopJoinResult =
        serde_json::from_slice(&reconnect_body).expect("parse reconnect result");
    let reconnect_success = match reconnect_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected reconnect success, got error: {}", error.error)
        }
    };

    let (addr, server_handle) = spawn_test_server(app2.clone()).await;
    let cookie2 = test_auth_cookie(&app2, "Alice").await;
    let (mut socket, _) = connect_async(ws_request_with_cookie(addr, Some(&cookie2)))
        .await
        .expect("connect ws after restart");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: reconnect_success.session_code.clone(),
        player_id: reconnect_success.player_id.clone(),
        reconnect_token: reconnect_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach after restart");

    let message = socket
        .next()
        .await
        .expect("state update frame")
        .expect("state update message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::StateUpdate(client_state) => {
            assert_eq!(client_state.session.code, reconnect_success.session_code);
            assert_eq!(
                client_state.current_player_id.as_deref(),
                Some(reconnect_success.player_id.as_str())
            );
        }
        other => panic!("expected state update, got {other:?}"),
    }

    // Seed host with a selected character before triggering StartPhase1. The
    // post-refactor FSM removes Phase0 entirely, so we advance directly from
    // Lobby to Phase1 to exercise the same broadcast path.
    seed_selected_characters(&state2, &reconnect_success.session_code).await;

    let phase0_command_response = app2
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&WorkshopCommandRequest {
                        session_code: reconnect_success.session_code.clone(),
                        reconnect_token: reconnect_success.reconnect_token.clone(),
                        coordinator_type: Some(CoordinatorType::Rust),
                        command: SessionCommand::StartPhase1,
                        payload: None,
                    })
                    .expect("encode phase0 command request"),
                ))
                .expect("build phase0 command request"),
        )
        .await
        .expect("call phase0 command after websocket reconnect");
    assert_eq!(phase0_command_response.status(), StatusCode::OK);

    let message = socket
        .next()
        .await
        .expect("phase0 update frame")
        .expect("phase0 update message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse phase0 server ws message");
    match server_message {
        ServerWsMessage::StateUpdate(client_state) => {
            assert_eq!(client_state.phase, protocol::Phase::Phase1);
            assert_eq!(
                client_state.current_player_id.as_deref(),
                Some(reconnect_success.player_id.as_str())
            );
        }
        other => panic!("expected phase0 state update, got {other:?}"),
    }

    // After the Phase0-removal refactor, the original follow-up StartPhase1
    // would double-transition (Phase1 -> Phase1) and fail. The first command
    // above already confirms broadcast-after-reconnect works, so we drop the
    // redundant follow-up command here.

    let persisted_after_reconnect = store2
        .load_session_by_code(&create_success.session_code)
        .await
        .expect("load persisted session after reconnect")
        .expect("persisted session exists after reconnect");
    assert!(
        persisted_after_reconnect
            .players
            .values()
            .all(|player| !player.is_connected),
        "persisted session after reconnect should still keep presence runtime-only"
    );

    let store3 = pg.reconnect().await;
    let state3 = test_state_with_store(store3 as Arc<dyn SessionStore>);
    assert!(
        ensure_session_cached(&state3, &create_success.session_code)
            .await
            .expect("reload cached session after second restart"),
        "second restarted app should reload session"
    );
    let sessions = state3.sessions.lock().await;
    let reloaded = sessions
        .get(&create_success.session_code)
        .expect("reloaded session exists after reconnect");
    let reloaded_player = reloaded
        .players
        .get(&reconnect_success.player_id)
        .expect("reloaded player exists after reconnect");
    assert!(
        reloaded_player.is_connected,
        "restarted replica should recover live presence from distributed realtime ownership"
    );

    let _ = socket.close(None).await;
    server_handle.abort();

    pg.cleanup().await;
}

#[tokio::test]
async fn reload_cached_session_clears_stale_cached_presence_without_realtime_registration() {
    let state = test_state();
    let timestamp = Utc::now();
    let session_code = "123456";
    let player_id = "player-1".to_string();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.to_string()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        &player_id, "Alice", timestamp, true, false,
    ));
    state
        .store
        .save_session(&session)
        .await
        .expect("persist session");

    let mut stale_cached = session.clone();
    stale_cached
        .players
        .get_mut(&player_id)
        .expect("player exists")
        .is_connected = true;
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), stale_cached);

    assert!(
        crate::cache::reload_cached_session(&state, session_code)
            .await
            .expect("reload cached session")
    );

    let sessions = state.sessions.lock().await;
    let reloaded = sessions.get(session_code).expect("reloaded session exists");
    assert!(
        !reloaded
            .players
            .get(&player_id)
            .expect("reloaded player exists")
            .is_connected,
        "reload must source presence from distributed realtime ownership, not stale cache"
    );
}

#[tokio::test]
async fn postgres_reload_ignores_stale_distributed_realtime_presence() {
    let pg =
        PostgresAppTestStore::new("postgres_reload_ignores_stale_distributed_realtime_presence")
            .await;
    let timestamp = Utc::now();
    let session_code = "654321";
    let player_id = "player-1".to_string();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.to_string()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        &player_id, "Alice", timestamp, true, false,
    ));
    pg.store
        .save_session(&session)
        .await
        .expect("persist session");
    pg.store
        .claim_realtime_connection(&RealtimeConnectionRegistration {
            session_code: session_code.to_string(),
            player_id: player_id.clone(),
            connection_id: "conn-stale".to_string(),
            replica_id: "replica-dead".to_string(),
        })
        .await
        .expect("seed realtime registration");

    let scoped_pool = PgPool::connect(&pg.scoped_database_url())
        .await
        .expect("connect scoped postgres pool");
    sqlx::query(
        "UPDATE realtime_connections SET updated_at = NOW() - INTERVAL '16 seconds' WHERE connection_id = 'conn-stale'",
    )
    .execute(&scoped_pool)
    .await
    .expect("age realtime registration");
    scoped_pool.close().await;

    let reloaded_store = pg.reconnect().await;
    let state = test_state_with_store(reloaded_store as Arc<dyn SessionStore>);
    assert!(
        ensure_session_cached(&state, session_code)
            .await
            .expect("reload cached session"),
        "session should reload"
    );

    let sessions = state.sessions.lock().await;
    let reloaded = sessions.get(session_code).expect("reloaded session exists");
    let player = reloaded
        .players
        .get(&player_id)
        .expect("reloaded player exists");
    assert!(
        !player.is_connected,
        "stale distributed realtime ownership must not resurrect live presence"
    );

    pg.cleanup().await;
}

#[tokio::test]
async fn postgres_replaced_connection_cannot_reclaim_before_notification_is_processed() {
    let pg = PostgresAppTestStore::new(
        "postgres_replaced_connection_cannot_reclaim_before_notification_is_processed",
    )
    .await;
    let state = test_state_with_store(pg.store.clone() as Arc<dyn SessionStore>);
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });

    let (addr, server_handle) = spawn_test_server(app).await;
    let mut first_stream = connect_raw_ws_with_cookie(addr, Some(&cookie)).await;
    send_raw_ws_message(&mut first_stream, &attach_message).await;
    let first_connection_id = loop {
        let registrations = pg
            .store
            .list_realtime_connections(&create_success.session_code)
            .await
            .expect("list initial distributed registrations");
        if registrations.len() == 1 {
            assert_eq!(registrations[0].replica_id, state.replica_id);
            break registrations[0].connection_id.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    };

    let remote_store = pg.reconnect().await;
    let remote_replica_id = "replica-remote".to_string();
    let replacement = remote_store
        .claim_realtime_connection(&RealtimeConnectionRegistration {
            session_code: create_success.session_code.clone(),
            player_id: create_success.player_id.clone(),
            connection_id: "conn-remote".to_string(),
            replica_id: remote_replica_id.clone(),
        })
        .await
        .expect("remote replica should replace original connection");
    assert_eq!(
        replacement.replaced,
        Some(RealtimeConnectionRegistration {
            session_code: create_success.session_code.clone(),
            player_id: create_success.player_id.clone(),
            connection_id: first_connection_id.clone(),
            replica_id: state.replica_id.clone(),
        })
    );

    send_raw_ws_message(&mut first_stream, &attach_message).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let registrations = pg
        .store
        .list_realtime_connections(&create_success.session_code)
        .await
        .expect("list distributed registrations after stale reclaim attempt");
    assert_eq!(
        registrations.len(),
        1,
        "stale socket must not reclaim distributed ownership"
    );
    assert_eq!(registrations[0].connection_id, "conn-remote");
    assert_eq!(registrations[0].replica_id, remote_replica_id);

    let _ = first_stream.shutdown().await;
    drop(first_stream);
    drop(remote_store);
    server_handle.abort();
    pg.cleanup().await;
}

#[tokio::test]
async fn session_write_lease_detects_renewal_loss_before_another_writer_can_proceed() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let session_code = "123456";

    let (_, _write_guard, write_lease) =
        crate::cache::SessionWriteLease::acquire(&state, session_code)
            .await
            .expect("acquire write lease");

    store.fail_lease_renewal();
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;

    assert!(
        write_lease.ensure_active().is_err(),
        "request guard must fence itself after lease renewal stops and ownership expires"
    );

    let replacement_expires_at = (Utc::now() + ChronoDuration::seconds(5)).to_rfc3339();
    assert!(
        store
            .inner
            .acquire_session_lease(session_code, "replacement-lease", &replacement_expires_at)
            .await
            .expect("replacement writer should acquire expired lease after fence trips")
    );
}

#[tokio::test]
async fn join_workshop_endpoint_rejects_invalid_reconnect_token() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let session_code = match create_result {
        WorkshopJoinResult::Success(success) => success.session_code,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"missing"}}"#,
                    session_code
                )))
                .expect("build reconnect request"),
        )
        .await
        .expect("call reconnect join workshop");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read reconnect body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse reconnect result");
    match result {
        WorkshopJoinResult::Error(error) => {
            assert_eq!(error.error, "Session identity is invalid or expired.");
        }
        WorkshopJoinResult::Success(_) => panic!("expected reconnect error response"),
    }
}

#[tokio::test]
async fn join_workshop_endpoint_rejects_expired_reconnect_token() {
    let state = test_state_with_reconnect_ttl(std::time::Duration::from_secs(60));
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    overwrite_identity_last_seen_at(
        state.store.as_ref(),
        &create_success.state.session.id,
        &create_success.player_id,
        &create_success.reconnect_token,
        Utc::now() - ChronoDuration::seconds(61),
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build reconnect request"),
        )
        .await
        .expect("call reconnect join workshop");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read reconnect body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse reconnect result");
    match result {
        WorkshopJoinResult::Error(error) => {
            assert_eq!(error.error, "Session identity is invalid or expired.");
        }
        WorkshopJoinResult::Success(_) => panic!("expected reconnect error response"),
    }

    let found = state
        .store
        .find_player_identity(
            &create_success.session_code,
            &create_success.reconnect_token,
        )
        .await
        .expect("find identity after expiry");
    assert_eq!(found, None);
}

#[tokio::test]
async fn workshop_command_saves_observation_during_phase1() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &create_success.session_code).await;
    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(setup_phase1_body(
                    &create_success.session_code,
                    &create_success.reconnect_token,
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call start phase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(
                         r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitObservation","payload":{{"text":"Calms down at dusk"}}}}"#,
                         create_success.session_code, create_success.reconnect_token
                     )))
                     .expect("build observation request"),
             )
             .await
             .expect("call observation command");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read observation body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse observation result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected observation success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("session exists");
    let dragon_id = session
        .players
        .get(&create_success.player_id)
        .and_then(|player| player.current_dragon_id.as_ref())
        .expect("current dragon id");
    let dragon = session.dragons.get(dragon_id).expect("dragon exists");
    assert_eq!(
        dragon.discovery_observations,
        vec!["Calms down at dusk".to_string()]
    );
}

#[tokio::test]
async fn workshop_command_records_action_artifact_during_phase2() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    create_success.session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &create_success.session_code).await;
    for request_body in [
        setup_phase1_body(
            &create_success.session_code,
            &create_success.reconnect_token,
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            create_success.session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["Rule 1","Rule 2","Rule 3"]}}"#,
            create_success.session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["Rule A","Rule B","Rule C"]}}"#,
            create_success.session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            create_success.session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(
                         r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"action","payload":{{"type":"sleep"}}}}"#,
                         create_success.session_code, create_success.reconnect_token
                     )))
                     .expect("build action request"),
             )
             .await
             .expect("call action command");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read action body");
    let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse action result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected action success, got error: {}", error.error)
        }
    }

    let artifacts = state
        .store
        .list_session_artifacts(&create_success.state.session.id)
        .await
        .expect("list artifacts");
    let action_artifact = artifacts
        .iter()
        .rev()
        .find(|artifact| artifact.kind == SessionArtifactKind::ActionProcessed)
        .expect("action artifact exists");
    assert_eq!(action_artifact.phase, protocol::Phase::Phase2);
    assert_eq!(
        action_artifact
            .payload
            .get("actionType")
            .and_then(|value: &serde_json::Value| value.as_str()),
        Some("sleep")
    );
    assert!(
        action_artifact
            .payload
            .get("dragonId")
            .and_then(|value: &serde_json::Value| value.as_str())
            .is_some()
    );
}

#[tokio::test]
async fn workshop_command_rejects_invalid_credentials() {
    let app = build_app(test_state());

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"sessionCode":"123456","reconnectToken":"missing","command":"startPhase1"}"#))
                     .expect("build request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Session identity is invalid or expired.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_non_host_start_phase1() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();
    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &session_code).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    session_code, join_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Only the host can start the workshop.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_starts_phase1_from_lobby() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => {
            assert!(success.ok);
        }
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error response: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("session cached after phase1 start");
    assert_eq!(session.phase, protocol::Phase::Phase1);
    assert!(
        session
            .players
            .values()
            .all(|player| player.current_dragon_id.is_some())
    );
}

#[tokio::test]
async fn workshop_command_rejects_start_handover_outside_phase1() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Handover can only begin during Phase 1.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_non_host_start_handover() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();
    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    let host_start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(host_start_phase1_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    session_code, join_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Only the host can trigger handover.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_starts_handover_for_host_in_phase1() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &create_success.session_code).await;
    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(setup_phase1_body(
                    &create_success.session_code,
                    &create_success.reconnect_token,
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build handover request"),
        )
        .await
        .expect("call handover command");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("session exists");
    assert_eq!(session.phase, protocol::Phase::Handover);
}

#[tokio::test]
async fn workshop_command_rejects_submit_tags_outside_handover() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["a","b","c"]}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(
                error.error,
                "Handover notes can only be saved during handover."
            );
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_invalid_submit_tags_payload() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &create_success.session_code).await;
    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(setup_phase1_body(
                    &create_success.session_code,
                    &create_success.reconnect_token,
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let start_handover_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start handover request"),
        )
        .await
        .expect("call startHandover command");
    assert_eq!(start_handover_response.status(), StatusCode::OK);

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":{{"tags":["a"]}}}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Handover notes must be sent as a list.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_submit_tags_with_wrong_count() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &create_success.session_code).await;
    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(setup_phase1_body(
                    &create_success.session_code,
                    &create_success.reconnect_token,
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let start_handover_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start handover request"),
        )
        .await
        .expect("call startHandover command");
    assert_eq!(start_handover_response.status(), StatusCode::OK);

    let response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two"]}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert!(
                error.error.contains("handover notes") && error.error.contains("Exactly 3"),
                "unexpected error message: {}",
                error.error
            );
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }

    // Also reject a payload with too many tags (4).
    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three","four"]}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert!(
                error.error.contains("handover notes") && error.error.contains("Exactly 3"),
                "unexpected error message: {}",
                error.error
            );
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_saves_submit_tags_in_handover_phase() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let start_handover_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start handover request"),
        )
        .await
        .expect("call startHandover command");
    assert_eq!(start_handover_response.status(), StatusCode::OK);

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["first","second","third"]}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("session exists");
    let dragon_id = session
        .players
        .get(&create_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("current dragon id");
    let dragon = session.dragons.get(&dragon_id).expect("dragon exists");
    assert_eq!(dragon.handover_tags, vec!["first", "second", "third"]);
}

#[tokio::test]
async fn workshop_command_rejects_start_phase2_outside_handover() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Phase 2 can only begin from handover.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_non_host_start_phase2() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &session_code).await;
    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(setup_phase1_body(
                    &session_code,
                    &create_success.reconnect_token,
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let start_handover_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build start handover request"),
        )
        .await
        .expect("call startHandover command");
    assert_eq!(start_handover_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
                    session_code, join_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Only the host can begin Phase 2.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_start_phase2_when_tags_are_missing() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    state
        .store
        .claim_realtime_connection(&RealtimeConnectionRegistration {
            session_code: create_success.session_code.clone(),
            player_id: create_success.player_id.clone(),
            connection_id: "conn-host".to_string(),
            replica_id: state.replica_id.clone(),
        })
        .await
        .expect("seed host realtime registration");

    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let start_handover_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start handover request"),
        )
        .await
        .expect("call startHandover command");
    assert_eq!(start_handover_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Still waiting on: Alice.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_starts_phase2_when_handover_is_complete() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let start_handover_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start handover request"),
        )
        .await
        .expect("call startHandover command");
    assert_eq!(start_handover_response.status(), StatusCode::OK);

    let submit_tags_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["first","second","third"]}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build submit tags request"),
             )
             .await
             .expect("call submitTags command");
    assert_eq!(submit_tags_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("session exists");
    assert_eq!(session.phase, protocol::Phase::Phase2);
}

#[tokio::test]
async fn workshop_command_rejects_end_game_outside_phase2() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Design voting can only begin from Phase 2.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_non_host_end_game() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &session_code).await;
    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(setup_phase1_body(
                    &session_code,
                    &create_success.reconnect_token,
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let start_handover_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build start handover request"),
        )
        .await
        .expect("call startHandover command");
    assert_eq!(start_handover_response.status(), StatusCode::OK);

    let submit_tags_host_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#, session_code, create_success.reconnect_token)))
                     .expect("build submit tags request"),
             )
             .await
             .expect("call host submitTags command");
    assert_eq!(submit_tags_host_response.status(), StatusCode::OK);

    let submit_tags_join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#, session_code, join_success.reconnect_token)))
                     .expect("build submit tags request"),
             )
             .await
             .expect("call join submitTags command");
    assert_eq!(submit_tags_join_response.status(), StatusCode::OK);

    let start_phase2_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build start phase2 request"),
        )
        .await
        .expect("call startPhase2 command");
    assert_eq!(start_phase2_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
                    session_code, join_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Only the host can end the workshop.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_enters_voting_and_runs_judge_in_background_when_host_ends_multiplayer_phase2()
 {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &session_code).await;
    for request_body in [
        setup_phase1_body(&session_code, &create_success.reconnect_token),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build command request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call endGame command");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_code).expect("session exists");
    assert_eq!(session.phase, protocol::Phase::Voting);
    assert!(session.voting.is_some());
    let session_id = session.id.to_string();
    drop(sessions);

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let artifacts = state
        .store
        .list_session_artifacts(&session_id)
        .await
        .expect("list session artifacts after endGame");
    let judge_artifact = artifacts
        .iter()
        .find(|artifact| artifact.kind == SessionArtifactKind::JudgeBundleGenerated)
        .expect("judge artifact generated");
    let summary = judge_artifact
        .payload
        .get("llmSummary")
        .and_then(|value| value.as_str())
        .expect("judge artifact summary");
    assert!(!summary.trim().is_empty());
}

#[tokio::test]
async fn workshop_command_rejects_submit_vote_outside_voting() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"dragon-a"}}}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Voting is not active right now.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_invalid_submit_vote_payload() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragon":"dragon-a"}}}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Voting is not active right now.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_self_vote_in_voting() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &session_code).await;
    for request_body in [
        setup_phase1_body(&session_code, &create_success.reconnect_token),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_code).expect("session exists");
    let alice_current_dragon_id = session
        .players
        .get(&create_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("alice current dragon id");
    drop(sessions);

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                      .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#, session_code, join_success.reconnect_token, alice_current_dragon_id)))
                      .expect("build command request"),
              )
             .await
             .expect("call submitVote command");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "You cannot vote for your own dragon.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_accepts_valid_vote_in_voting() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &session_code).await;
    for request_body in [
        setup_phase1_body(&session_code, &create_success.reconnect_token),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_code).expect("session exists");
    let alice_current_dragon_id = session
        .players
        .get(&create_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("alice current dragon id");
    drop(sessions);

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                      .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#, session_code, create_success.reconnect_token, alice_current_dragon_id)))
                      .expect("build command request"),
              )
             .await
             .expect("call submitVote command");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }
}

#[tokio::test]
async fn workshop_command_rejects_submit_vote_after_results_are_revealed() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &session_code).await;
    for request_body in [
        setup_phase1_body(&session_code, &create_success.reconnect_token),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_code).expect("session exists");
    let alice_current_dragon_id = session
        .players
        .get(&create_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("alice current dragon id");
    drop(sessions);

    let submit_vote_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#,
                    session_code, create_success.reconnect_token, alice_current_dragon_id
                )))
                .expect("build submitVote request"),
        )
        .await
        .expect("call submitVote command");
    assert_eq!(submit_vote_response.status(), StatusCode::OK);

    let reveal_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build reveal request"),
        )
        .await
        .expect("call revealVotingResults command");
    assert_eq!(reveal_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#,
                    session_code, create_success.reconnect_token, alice_current_dragon_id
                )))
                .expect("build late submitVote request"),
        )
        .await
        .expect("call submitVote command after reveal");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Voting is already closed.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_reveal_results_outside_voting() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Results can only be revealed during voting.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_non_host_reveal_results() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &session_code).await;
    for request_body in [
        setup_phase1_body(&session_code, &create_success.reconnect_token),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, session_code, join_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Only the host can reveal voting results.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_reveal_results_while_votes_are_pending() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &session_code).await;
    for request_body in [
        setup_phase1_body(&session_code, &create_success.reconnect_token),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => assert_eq!(
            error.error,
            "Voting can only be finished after at least one eligible vote is submitted."
        ),
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_end_session_before_results_are_revealed() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &session_code).await;
    for request_body in [
        setup_phase1_body(&session_code, &create_success.reconnect_token),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endSession"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build end session request"),
        )
        .await
        .expect("call endSession command");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => assert_eq!(
            error.error,
            "Session can only be ended after voting results are revealed."
        ),
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_reveals_voting_results_after_all_votes() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &session_code).await;
    for request_body in [
        setup_phase1_body(&session_code, &create_success.reconnect_token),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_code).expect("session exists");
    let alice_current_dragon_id = session
        .players
        .get(&create_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("alice current dragon id");
    let bob_current_dragon_id = session
        .players
        .get(&join_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("bob current dragon id");
    drop(sessions);

    for request_body in [
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#,
            session_code, create_success.reconnect_token, alice_current_dragon_id
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#,
            session_code, join_success.reconnect_token, bob_current_dragon_id
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build vote request"),
            )
            .await
            .expect("call submitVote command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call revealVotingResults command");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_code).expect("session exists");
    assert_eq!(session.phase, protocol::Phase::Voting);
    assert!(
        session
            .voting
            .as_ref()
            .is_some_and(|voting| voting.results_revealed)
    );
    assert!(session.players.values().all(|player| player.score >= 0));
}

#[tokio::test]
async fn advance_game_ticks_updates_cached_session_without_persisting_each_tick() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("955555".into()),
        Utc::now(),
        protocol::WorkshopCreateConfig::default(),
    );

    let mut host = session_player("player-1", "Alice", 1);
    host.is_host = true;
    host.character_id = Some("character-1".into());
    host.selected_character = Some(test_character_profile(
        "character-1",
        "Coral dragon",
        protocol::SpriteSet {
            neutral: "neutral_b64".into(),
            happy: "happy_b64".into(),
            angry: "angry_b64".into(),
            sleepy: "sleepy_b64".into(),
        },
        1,
    ));
    host.is_ready = true;
    let mut guest = session_player("player-2", "Bob", 2);
    guest.character_id = Some("character-2".into());
    guest.selected_character = Some(test_character_profile(
        "character-2",
        "Moss dragon",
        timeout_companion_defaults().sprites,
        1,
    ));
    guest.is_ready = true;
    session.add_player(host);
    session.add_player(guest);

    session
        .begin_phase1(&[
            domain::Phase1Assignment {
                player_id: "player-1".into(),
                dragon_id: "dragon-1".into(),
            },
            domain::Phase1Assignment {
                player_id: "player-2".into(),
                dragon_id: "dragon-2".into(),
            },
        ])
        .unwrap();

    let original_time = session.time;
    state
        .store
        .save_session(&session)
        .await
        .expect("persist initial session");
    state
        .sessions
        .lock()
        .await
        .insert(session.code.0.clone(), session);

    let baseline_save_calls = store.save_session_calls();

    // Tick 1: time 16 → 17; 17 % 5 != 0 → no persist (throttled)
    advance_game_ticks(&state).await;

    let cached = state.sessions.lock().await;
    let updated = cached.get("955555").expect("cached session after tick");
    assert_eq!(updated.phase, protocol::Phase::Phase1);
    assert_eq!(updated.time, (original_time + 1) % 48);
    assert_eq!(
        store.save_session_calls(),
        baseline_save_calls,
        "non-multiple-of-5 ticks should NOT persist (throttled to reduce DB writes)"
    );
    drop(cached);

    // Advance until time is a multiple of 5 (tick 20) to verify persistence kicks in
    // time is 17 now, need 3 more ticks to reach 20
    advance_game_ticks(&state).await; // → 18
    advance_game_ticks(&state).await; // → 19
    let before_persist = store.save_session_calls();
    advance_game_ticks(&state).await; // → 20, 20 % 5 == 0 → persist

    let cached = state.sessions.lock().await;
    let updated = cached
        .get("955555")
        .expect("cached session after persist tick");
    assert_eq!(updated.time, 20);
    assert_eq!(
        store.save_session_calls(),
        before_persist + 1,
        "multiple-of-5 ticks must persist so command-handler reloads pick up decayed stats"
    );
}

#[tokio::test]
async fn workshop_command_rejects_non_host_reset_game() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let session_code = match create_result {
        WorkshopJoinResult::Success(success) => success.session_code,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"resetGame"}}"#,
                    session_code, join_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Only the host can reset the workshop.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_reset_game_returns_session_to_lobby() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let start_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_response.status(), StatusCode::OK);

    let reset_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"resetGame"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build reset request"),
        )
        .await
        .expect("call reset command");

    assert_eq!(reset_response.status(), StatusCode::OK);
    let body = to_bytes(reset_response.into_body(), usize::MAX)
        .await
        .expect("read reset body");
    let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse reset result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("session exists");
    assert_eq!(session.phase, protocol::Phase::Lobby);
    assert!(session.dragons.is_empty());
    assert!(
        session
            .players
            .values()
            .all(|player| player.current_dragon_id.is_none())
    );
}

#[tokio::test]
async fn workshop_ws_close_marks_player_offline_and_reassigns_host() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let cookie_bob = test_auth_cookie(&app, "Bob").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie_bob)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    create_success.session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request_with_cookie(addr, Some(&cookie)))
        .await
        .expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let _ = socket
        .next()
        .await
        .expect("state update frame")
        .expect("state update message");
    let (mut guest_socket, _) = connect_async(ws_request_with_cookie(addr, Some(&cookie_bob)))
        .await
        .expect("connect guest ws");
    let guest_attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: join_success.session_code.clone(),
        player_id: join_success.player_id.clone(),
        reconnect_token: join_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    guest_socket
        .send(WsMessage::Text(
            serde_json::to_string(&guest_attach_message)
                .expect("encode guest attach")
                .into(),
        ))
        .await
        .expect("send guest attach");
    let _ = guest_socket
        .next()
        .await
        .expect("guest state update frame")
        .expect("guest state update message");
    assert_eq!(
        state
            .realtime
            .lock()
            .await
            .session_connection_count(&create_success.session_code),
        2
    );

    let _ = socket.close(None).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(
        state
            .realtime
            .lock()
            .await
            .session_connection_count(&create_success.session_code),
        1
    );

    let (session_id, host_player, guest_player) = {
        let sessions = state.sessions.lock().await;
        let session = sessions
            .get(&create_success.session_code)
            .expect("session exists after disconnect");
        (
            session.id.to_string(),
            session
                .players
                .get(&create_success.player_id)
                .expect("host player exists")
                .clone(),
            session
                .players
                .get(&join_success.player_id)
                .expect("guest player exists")
                .clone(),
        )
    };

    assert!(!host_player.is_connected);
    assert!(!host_player.is_host);
    assert!(guest_player.is_host);
    assert!(guest_player.is_connected);

    let artifacts = state
        .store
        .list_session_artifacts(&session_id)
        .await
        .expect("list session artifacts");
    let disconnect_artifact = artifacts
        .iter()
        .rev()
        .find(|artifact| artifact.kind == SessionArtifactKind::PlayerLeft)
        .expect("player left artifact");
    assert_eq!(
        disconnect_artifact.player_id.as_deref(),
        Some(create_success.player_id.as_str())
    );
    assert_eq!(
        disconnect_artifact
            .payload
            .get("sessionCode")
            .and_then(|value: &serde_json::Value| value.as_str()),
        Some(create_success.session_code.as_str())
    );

    let _ = guest_socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_rejects_invalid_identity() {
    let app = build_app(test_state());
    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: "123456".to_string(),
        player_id: "player-1".to_string(),
        reconnect_token: "missing".to_string(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let message = socket
        .next()
        .await
        .expect("error frame")
        .expect("error message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::Error { message } => {
            assert_eq!(message, "Session identity is invalid or expired.");
        }
        other => panic!("expected error message, got {other:?}"),
    }

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_rejects_expired_identity() {
    let state = test_state_with_reconnect_ttl(std::time::Duration::from_secs(60));
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    overwrite_identity_last_seen_at(
        state.store.as_ref(),
        &create_success.state.session.id,
        &create_success.player_id,
        &create_success.reconnect_token,
        Utc::now() - ChronoDuration::seconds(61),
    )
    .await;

    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let message = socket
        .next()
        .await
        .expect("error frame")
        .expect("error message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::Error { message } => {
            assert_eq!(message, "Session identity is invalid or expired.");
        }
        other => panic!("expected error message, got {other:?}"),
    }

    let found = state
        .store
        .find_player_identity(
            &create_success.session_code,
            &create_success.reconnect_token,
        )
        .await
        .expect("find identity after expiry");
    assert_eq!(found, None);

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_attach_restores_cache_state_when_grouped_reconnect_persist_fails() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let session_code = "123456";
    let player_id = "player-1".to_string();
    let reconnect_token = "token-1".to_string();
    let timestamp = Utc::now();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        &player_id, "Alice", timestamp, true, false,
    ));
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");
    store
        .inner
        .create_player_identity(&persistence::PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: player_id.clone(),
            reconnect_token: reconnect_token.clone(),
            created_at: timestamp.to_rfc3339(),
            last_seen_at: timestamp.to_rfc3339(),
        })
        .await
        .expect("seed identity");
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session.clone());
    store.fail_save_with_artifact();

    let app = build_app(state.clone());
    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: session_code.to_string(),
        player_id: player_id.clone(),
        reconnect_token: reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let message = socket
        .next()
        .await
        .expect("state frame")
        .expect("state message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::StateUpdate(state_update) => {
            assert!(
                state_update
                    .players
                    .get(&player_id)
                    .expect("player in state update")
                    .is_connected,
                "initial websocket state should still reflect the optimistic reconnect"
            );
        }
        other => panic!("expected state update, got {other:?}"),
    }

    let message = socket
        .next()
        .await
        .expect("error frame")
        .expect("error message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::Error { message } => {
            assert!(message.contains("failed to persist websocket reconnect"))
        }
        other => panic!("expected error message, got {other:?}"),
    }

    let sessions = state.sessions.lock().await;
    let cached = sessions.get(session_code).expect("session remains cached");
    let player = cached.players.get(&player_id).expect("player exists");
    assert!(
        !player.is_connected,
        "cache should roll back when grouped reconnect persistence fails"
    );
    drop(sessions);

    let persisted = state
        .store
        .load_session_by_code(session_code)
        .await
        .expect("load persisted session after failed reconnect persist")
        .expect("persisted session remains");
    assert!(
        !persisted
            .players
            .get(&player_id)
            .expect("persisted player exists")
            .is_connected,
        "persisted reconnect state should roll back when grouped reconnect persistence fails"
    );

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_attach_does_not_leave_registration_when_initial_state_send_fails() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    state
        .fail_next_initial_state_send
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request_with_cookie(addr, Some(&cookie)))
        .await
        .expect("connect ws");
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&ClientWsMessage::AttachSession(SessionEnvelope {
                session_code: create_success.session_code.clone(),
                player_id: create_success.player_id.clone(),
                reconnect_token: create_success.reconnect_token.clone(),
                coordinator_type: Some(CoordinatorType::Rust),
            }))
            .expect("encode attach")
            .into(),
        ))
        .await
        .expect("send attach");

    let message = socket
        .next()
        .await
        .expect("error frame")
        .expect("error message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::Error { message } => assert_eq!(message, "connection is closed"),
        other => panic!("expected error message, got {other:?}"),
    }

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(
        state
            .realtime
            .lock()
            .await
            .session_connection_count(&create_success.session_code),
        0,
        "failed initial state send should not leave a realtime registration behind"
    );

    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_attach_restores_connected_state_when_initial_state_send_fails() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let mut persisted = state
        .store
        .load_session_by_code(&create_success.session_code)
        .await
        .expect("load persisted session")
        .expect("persisted session exists");
    persisted
        .players
        .get_mut(&create_success.player_id)
        .expect("persisted player exists")
        .is_connected = false;
    persisted.updated_at = Utc::now();
    state
        .store
        .save_session(&persisted)
        .await
        .expect("persist disconnected session");
    state
        .sessions
        .lock()
        .await
        .insert(create_success.session_code.clone(), persisted.clone());
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    state
        .fail_next_initial_state_send
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request_with_cookie(addr, Some(&cookie)))
        .await
        .expect("connect ws");
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&ClientWsMessage::AttachSession(SessionEnvelope {
                session_code: create_success.session_code.clone(),
                player_id: create_success.player_id.clone(),
                reconnect_token: create_success.reconnect_token.clone(),
                coordinator_type: Some(CoordinatorType::Rust),
            }))
            .expect("encode attach")
            .into(),
        ))
        .await
        .expect("send attach");

    let message = socket
        .next()
        .await
        .expect("error frame")
        .expect("error message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::Error { message } => assert_eq!(message, "connection is closed"),
        other => panic!("expected error message, got {other:?}"),
    }

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let cached = state
        .sessions
        .lock()
        .await
        .get(&create_success.session_code)
        .expect("cached session remains")
        .clone();
    assert!(
        !cached
            .players
            .get(&create_success.player_id)
            .expect("cached player exists")
            .is_connected,
        "failed initial state send should roll back cached reconnect state"
    );

    let persisted = state
        .store
        .load_session_by_code(&create_success.session_code)
        .await
        .expect("load persisted session after failed attach")
        .expect("persisted session remains");
    assert!(
        !persisted
            .players
            .get(&create_success.player_id)
            .expect("persisted player exists")
            .is_connected,
        "failed initial state send should roll back persisted reconnect state"
    );

    let artifacts = state
        .store
        .list_session_artifacts(&persisted.id.to_string())
        .await
        .expect("list artifacts after failed attach");
    assert!(
        !artifacts.iter().any(|artifact| {
            matches!(
                artifact.kind,
                SessionArtifactKind::PlayerReconnected | SessionArtifactKind::PlayerLeft
            )
        }),
        "failed initial state send should not persist reconnect/disconnect artifacts"
    );

    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_attach_restores_connected_state_when_realtime_claim_fails() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let mut persisted = state
        .store
        .load_session_by_code(&create_success.session_code)
        .await
        .expect("load persisted session")
        .expect("persisted session exists");
    persisted
        .players
        .get_mut(&create_success.player_id)
        .expect("persisted player exists")
        .is_connected = false;
    persisted.updated_at = Utc::now();
    state
        .store
        .save_session(&persisted)
        .await
        .expect("persist disconnected session");
    state
        .sessions
        .lock()
        .await
        .insert(create_success.session_code.clone(), persisted.clone());
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    store.fail_realtime_claims();

    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request_with_cookie(addr, Some(&cookie)))
        .await
        .expect("connect ws");
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&ClientWsMessage::AttachSession(SessionEnvelope {
                session_code: create_success.session_code.clone(),
                player_id: create_success.player_id.clone(),
                reconnect_token: create_success.reconnect_token.clone(),
                coordinator_type: Some(CoordinatorType::Rust),
            }))
            .expect("encode attach")
            .into(),
        ))
        .await
        .expect("send attach");

    let message = socket
        .next()
        .await
        .expect("error frame")
        .expect("error message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::Error { message } => {
            assert!(message.contains("failed to claim realtime connection"))
        }
        other => panic!("expected error message, got {other:?}"),
    }

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let cached = state
        .sessions
        .lock()
        .await
        .get(&create_success.session_code)
        .expect("cached session remains")
        .clone();
    assert!(
        !cached
            .players
            .get(&create_success.player_id)
            .expect("cached player exists")
            .is_connected,
        "failed realtime claim should roll back cached reconnect state"
    );

    let persisted = state
        .store
        .load_session_by_code(&create_success.session_code)
        .await
        .expect("load persisted session after failed attach")
        .expect("persisted session remains");
    assert!(
        !persisted
            .players
            .get(&create_success.player_id)
            .expect("persisted player exists")
            .is_connected,
        "failed realtime claim should not persist reconnect state"
    );

    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_failed_reattach_restores_replaced_registration() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });

    let (addr, server_handle) = spawn_test_server(app).await;
    let mut first_stream = connect_raw_ws_with_cookie(addr, Some(&cookie)).await;
    send_raw_ws_message(&mut first_stream, &attach_message).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let first_connection_id = {
        let registrations = state
            .realtime
            .lock()
            .await
            .session_registrations(&create_success.session_code);
        assert_eq!(
            registrations.len(),
            1,
            "first attach should register the raw websocket connection"
        );
        registrations[0].connection_id.clone()
    };

    let (mut second_socket, _) = connect_async(ws_request_with_cookie(addr, Some(&cookie)))
        .await
        .expect("connect replacement ws");
    second_socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send replacement attach");
    let _ = second_socket
        .next()
        .await
        .expect("replacement state frame")
        .expect("replacement state message");

    let replacement_connection_id = {
        let registrations = state
            .realtime
            .lock()
            .await
            .session_registrations(&create_success.session_code);
        assert_eq!(
            registrations.len(),
            1,
            "replacement attach should keep a single registration"
        );
        let connection_id = registrations[0].connection_id.clone();
        assert_ne!(
            connection_id, first_connection_id,
            "replacement attach should take over the player slot"
        );
        connection_id
    };

    send_raw_ws_message(&mut first_stream, &attach_message).await;
    let _ = first_stream.shutdown().await;
    drop(first_stream);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let registrations = state
        .realtime
        .lock()
        .await
        .session_registrations(&create_success.session_code);
    assert_eq!(
        registrations.len(),
        1,
        "failed re-attach should preserve the prior live registration"
    );
    assert_eq!(registrations[0].connection_id, replacement_connection_id);

    let persisted_registrations = state
        .store
        .list_realtime_connections(&create_success.session_code)
        .await
        .expect("list persisted realtime registrations");
    assert_eq!(
        persisted_registrations.len(),
        1,
        "failed re-attach should restore the prior distributed registration"
    );
    assert_eq!(
        persisted_registrations[0].connection_id, replacement_connection_id,
        "failed re-attach must not orphan the previous distributed owner"
    );

    let _ = second_socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn reconnect_join_restores_cache_state_when_grouped_reconnect_persist_fails() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let session_code = "123456";
    let player_id = "player-1".to_string();
    let reconnect_token = "token-1".to_string();
    let timestamp = Utc::now();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        &player_id, "Alice", timestamp, true, false,
    ));
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");
    store
        .inner
        .create_player_identity(&persistence::PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: player_id.clone(),
            reconnect_token: reconnect_token.clone(),
            created_at: timestamp.to_rfc3339(),
            last_seen_at: timestamp.to_rfc3339(),
        })
        .await
        .expect("seed identity");
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session.clone());
    store.fail_replace_identity_and_save_with_artifact();

    let app = build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                    session_code, reconnect_token
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let sessions = state.sessions.lock().await;
    let cached = sessions.get(session_code).expect("session remains cached");
    let player = cached.players.get(&player_id).expect("player exists");
    assert!(
        !player.is_connected,
        "cache should roll back when grouped reconnect persistence fails"
    );
}

#[tokio::test]
async fn websocket_reconnect_persistence_does_not_store_connected_presence() {
    let state = test_state();
    let session_code = "123456";
    let player_id = "player-1".to_string();
    let reconnect_token = "token-1".to_string();
    let timestamp = Utc::now();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        &player_id, "Alice", timestamp, true, false,
    ));
    state
        .store
        .save_session(&session)
        .await
        .expect("seed session");
    state
        .store
        .create_player_identity(&persistence::PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: player_id.clone(),
            reconnect_token: reconnect_token.clone(),
            created_at: timestamp.to_rfc3339(),
            last_seen_at: timestamp.to_rfc3339(),
        })
        .await
        .expect("seed identity");
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session.clone());

    let app = build_app(state.clone());
    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: session_code.to_string(),
        player_id: player_id.clone(),
        reconnect_token: reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let _ = socket
        .next()
        .await
        .expect("state frame")
        .expect("state message");

    let persisted = state
        .store
        .load_session_by_code(session_code)
        .await
        .expect("load persisted session after reconnect")
        .expect("persisted session remains");
    assert!(
        !persisted
            .players
            .get(&player_id)
            .expect("persisted player exists")
            .is_connected,
        "successful reconnect persistence must not store durable live presence"
    );

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn websocket_disconnect_restores_cache_state_when_grouped_disconnect_persist_fails() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let session_code = "123456";
    let player_id = "player-1".to_string();
    let reconnect_token = "token-1".to_string();
    let timestamp = Utc::now();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        &player_id, "Alice", timestamp, true, true,
    ));
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");
    store
        .inner
        .create_player_identity(&persistence::PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: player_id.clone(),
            reconnect_token: reconnect_token.clone(),
            created_at: timestamp.to_rfc3339(),
            last_seen_at: timestamp.to_rfc3339(),
        })
        .await
        .expect("seed identity");
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session.clone());
    state
        .realtime
        .lock()
        .await
        .attach(session_code, &player_id, "conn-1");
    store
        .inner
        .claim_realtime_connection(&RealtimeConnectionRegistration {
            session_code: session_code.to_string(),
            player_id: player_id.clone(),
            connection_id: "conn-1".to_string(),
            replica_id: state.replica_id.clone(),
        })
        .await
        .expect("seed realtime registration");
    store.fail_grouped_session_artifact_persist();

    super::ws::sync_ws_disconnect(&state, "conn-1").await;

    let sessions = state.sessions.lock().await;
    let cached = sessions.get(session_code).expect("session remains cached");
    let player = cached.players.get(&player_id).expect("player exists");
    assert!(
        player.is_connected,
        "cache should roll back when grouped disconnect persistence fails"
    );
    drop(sessions);

    let registrations = state
        .realtime
        .lock()
        .await
        .session_registrations(session_code);
    assert!(
        registrations.is_empty(),
        "disconnect should still detach runtime registration"
    );
}

#[tokio::test]
async fn replaced_connection_close_before_notification_does_not_persist_false_disconnect() {
    let pg = PostgresAppTestStore::new(
        "replaced_connection_close_before_notification_does_not_persist_false_disconnect",
    )
    .await;
    let state = test_state_with_store(pg.store.clone() as Arc<dyn SessionStore>);
    let timestamp = Utc::now();
    let session_code = "123456";
    let player_id = "player-1".to_string();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.to_string()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        &player_id, "Alice", timestamp, true, true,
    ));
    pg.store
        .save_session(&session)
        .await
        .expect("persist session");
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session.clone());
    state
        .realtime
        .lock()
        .await
        .attach(session_code, &player_id, "conn-1");
    pg.store
        .claim_realtime_connection(&RealtimeConnectionRegistration {
            session_code: session_code.to_string(),
            player_id: player_id.clone(),
            connection_id: "conn-1".to_string(),
            replica_id: state.replica_id.clone(),
        })
        .await
        .expect("claim initial distributed registration");

    let remote_store = pg.reconnect().await;
    remote_store
        .claim_realtime_connection(&RealtimeConnectionRegistration {
            session_code: session_code.to_string(),
            player_id: player_id.clone(),
            connection_id: "conn-remote".to_string(),
            replica_id: "replica-remote".to_string(),
        })
        .await
        .expect("remote replica replaces local connection");

    super::ws::sync_ws_disconnect(&state, "conn-1").await;

    let cached = state
        .sessions
        .lock()
        .await
        .get(session_code)
        .expect("cached session remains")
        .clone();
    assert!(
        cached
            .players
            .get(&player_id)
            .expect("cached player exists")
            .is_connected,
        "stale replaced close must not persist a false disconnect before notification arrives"
    );

    let artifacts = pg
        .store
        .list_session_artifacts(&session.id.to_string())
        .await
        .expect("list artifacts");
    assert!(
        !artifacts
            .iter()
            .any(|artifact| artifact.kind == SessionArtifactKind::PlayerLeft),
        "stale replaced close must not emit a PlayerLeft artifact"
    );

    let registrations = pg
        .store
        .list_realtime_connections(session_code)
        .await
        .expect("list distributed registrations after stale close");
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].connection_id, "conn-remote");

    drop(remote_store);
    pg.cleanup().await;
}

#[tokio::test]
async fn workshop_command_rejects_expired_reconnect_token() {
    let state = test_state_with_reconnect_ttl(std::time::Duration::from_secs(60));
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    overwrite_identity_last_seen_at(
        state.store.as_ref(),
        &create_success.state.session.id,
        &create_success.player_id,
        &create_success.reconnect_token,
        Utc::now() - ChronoDuration::seconds(61),
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Session identity is invalid or expired.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected command error response"),
    }
}

#[tokio::test]
async fn fresh_join_restores_cache_state_when_grouped_join_persist_fails() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let session_code = "123456";
    let timestamp = Utc::now();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(session_player_with_state(
        "host-1", "Alice", timestamp, true, true,
    ));
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session.clone());
    store.fail_save_with_identity_and_artifact();

    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Bob").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let sessions = state.sessions.lock().await;
    let cached = sessions.get(session_code).expect("session remains cached");
    assert_eq!(
        cached.players.len(),
        1,
        "cache should roll back when grouped join persistence fails"
    );
}

#[tokio::test]
async fn workshop_ws_replies_with_pong_for_ping_message() {
    let app = build_app(test_state());
    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&ClientWsMessage::Ping)
                .expect("encode ping")
                .into(),
        ))
        .await
        .expect("send ping message");

    let message = socket
        .next()
        .await
        .expect("pong frame")
        .expect("pong message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    assert_eq!(server_message, ServerWsMessage::Pong);

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_upgrade_is_rate_limited_for_repeated_connections() {
    let mut state = test_state();
    state.config = Arc::new(AppConfig {
        websocket_rate_limit: 1,
        ..state.config.as_ref().clone()
    });
    state.websocket_limiter = Arc::new(tokio::sync::Mutex::new(
        security::FixedWindowRateLimiter::new(1, 60_000),
    ));
    let app = build_app(state);
    let (addr, server_handle) = spawn_test_server(app).await;

    let (socket, _) = connect_async(ws_request(addr))
        .await
        .expect("connect first ws");
    drop(socket);

    let second = connect_async(ws_request(addr)).await;
    let error = second.expect_err("second websocket upgrade should be rate limited");
    let response = match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => response,
        other => panic!("expected websocket http error, got {other:?}"),
    };
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_upgrade_uses_forwarded_for_when_trusted() {
    let mut state = test_state();
    state.config = Arc::new(AppConfig {
        trust_forwarded_for: true,
        websocket_rate_limit: 1,
        ..state.config.as_ref().clone()
    });
    state.websocket_limiter = Arc::new(tokio::sync::Mutex::new(
        security::FixedWindowRateLimiter::new(1, 60_000),
    ));
    let app = build_app(state);
    let (addr, server_handle) = spawn_test_server(app).await;

    let mut first_request = ws_request(addr);
    first_request
        .headers_mut()
        .insert("x-forwarded-for", HeaderValue::from_static("10.0.0.1"));
    let (first_socket, _) = connect_async(first_request)
        .await
        .expect("connect first ws");
    drop(first_socket);

    let mut second_request = ws_request(addr);
    second_request
        .headers_mut()
        .insert("x-forwarded-for", HeaderValue::from_static("203.0.113.99"));
    let second = connect_async(second_request).await;
    assert!(
        second.is_ok(),
        "trusted forwarded-for should separate websocket client identity"
    );

    if let Ok((socket, _)) = second {
        drop(socket);
    }
    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_messages_are_rate_limited_after_attach() {
    let mut state = test_state();
    state.config = Arc::new(AppConfig {
        websocket_rate_limit: 3,
        ..state.config.as_ref().clone()
    });
    state.websocket_limiter = Arc::new(tokio::sync::Mutex::new(
        security::FixedWindowRateLimiter::new(3, 60_000),
    ));
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request_with_cookie(addr, Some(&cookie)))
        .await
        .expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let _ = socket
        .next()
        .await
        .expect("initial state update frame")
        .expect("initial state update message");

    socket
        .send(WsMessage::Text(
            serde_json::to_string(&ClientWsMessage::Ping)
                .expect("encode first ping")
                .into(),
        ))
        .await
        .expect("send first ping");
    let first = socket
        .next()
        .await
        .expect("first pong frame")
        .expect("first pong message");
    let first_payload = match first {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let first_server_message: ServerWsMessage =
        serde_json::from_str(&first_payload).expect("parse first server ws message");
    assert_eq!(first_server_message, ServerWsMessage::Pong);

    socket
        .send(WsMessage::Text(
            serde_json::to_string(&ClientWsMessage::Ping)
                .expect("encode second ping")
                .into(),
        ))
        .await
        .expect("send second ping");
    let second = socket
        .next()
        .await
        .expect("rate limited frame")
        .expect("rate limited message");
    let second_payload = match second {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let second_server_message: ServerWsMessage =
        serde_json::from_str(&second_payload).expect("parse rate limited server ws message");
    match second_server_message {
        ServerWsMessage::Error { message } => {
            assert_eq!(
                message,
                "Too many requests. Please slow down and try again."
            );
        }
        other => panic!("expected error message, got {other:?}"),
    }

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn phase_timer_broadcasts_warning_notice_at_thirty_seconds_remaining() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    seed_selected_characters(&state, &create_success.session_code).await;

    let command_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&WorkshopCommandRequest {
                        session_code: create_success.session_code.clone(),
                        reconnect_token: create_success.reconnect_token.clone(),
                        coordinator_type: Some(CoordinatorType::Rust),
                        command: SessionCommand::StartPhase1,
                        payload: None,
                    })
                    .expect("encode command request"),
                ))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");
    assert_eq!(command_response.status(), StatusCode::OK);

    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request_with_cookie(addr, Some(&cookie)))
        .await
        .expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");
    let _ = socket
        .next()
        .await
        .expect("initial state update frame")
        .expect("initial state update message");

    {
        let mut sessions = state.sessions.lock().await;
        let session = sessions
            .get_mut(&create_success.session_code)
            .expect("cached session");
        session.phase_started_at = Utc::now()
            - chrono::Duration::seconds((session.config.phase1_minutes as i64 * 60) - 30);
        session.warned_for_current_phase = false;
    }

    emit_phase_warning_notices(&state).await;

    let message = socket
        .next()
        .await
        .expect("warning notice frame")
        .expect("warning notice message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::Notice(notice) => {
            assert_eq!(notice.level, NoticeLevel::Warning);
            assert_eq!(notice.title, "Phase ending soon");
            assert!(notice.message.contains("Phase 1 ends in 30 seconds."));
        }
        other => panic!("expected warning notice, got {other:?}"),
    }

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[test]
fn build_judge_action_traces_groups_action_artifacts_by_dragon() {
    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        chrono::DateTime::from_timestamp(1, 0).expect("valid timestamp"),
        protocol::WorkshopCreateConfig {
            phase0_minutes: 5,
            phase1_minutes: 10,
            phase2_minutes: 10,
        },
    );
    session.add_player(session_player("p1", "Alice", 10));

    let artifacts = vec![
        SessionArtifactRecord {
            id: "artifact-1".into(),
            session_id: session.id.to_string(),
            phase: protocol::Phase::Phase2,
            step: 2,
            kind: SessionArtifactKind::ActionProcessed,
            player_id: Some("p1".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            payload: serde_json::json!({
                "dragonId": "dragon-a",
                "actionType": "feed",
                "actionValue": "meat",
                "hunger": 90,
                "energy": 100,
                "happiness": 95
            }),
        },
        SessionArtifactRecord {
            id: "artifact-2".into(),
            session_id: session.id.to_string(),
            phase: protocol::Phase::Phase2,
            step: 2,
            kind: SessionArtifactKind::ActionProcessed,
            player_id: Some("p1".into()),
            created_at: "2026-01-01T00:00:01Z".into(),
            payload: serde_json::json!({
                "dragonId": "dragon-a",
                "actionType": "play",
                "actionValue": "fetch"
            }),
        },
        SessionArtifactRecord {
            id: "artifact-3".into(),
            session_id: session.id.to_string(),
            phase: protocol::Phase::Phase2,
            step: 2,
            kind: SessionArtifactKind::ActionProcessed,
            player_id: Some("p1".into()),
            created_at: "2026-01-01T00:00:02Z".into(),
            payload: serde_json::json!({
                "dragonId": "dragon-b",
                "actionType": "sleep"
            }),
        },
    ];

    let traces = build_judge_action_traces(&session, &artifacts);

    let dragon_traces = traces.get("dragon-a").expect("dragon a traces");
    assert_eq!(dragon_traces.len(), 2);
    assert_eq!(dragon_traces[0].player_name, "Alice");
    assert_eq!(dragon_traces[0].action_type, "feed");
    assert_eq!(dragon_traces[0].action_value.as_deref(), Some("meat"));
    assert_eq!(
        dragon_traces[0].resulting_stats,
        Some(DragonStats {
            hunger: 90,
            energy: 100,
            happiness: 95
        })
    );
    assert_eq!(dragon_traces[1].action_type, "play");
    assert_eq!(dragon_traces[1].action_value.as_deref(), Some("fetch"));
    assert!(dragon_traces[1].resulting_stats.is_none());

    let second_dragon_traces = traces.get("dragon-b").expect("dragon b traces");
    assert_eq!(second_dragon_traces.len(), 1);
    assert_eq!(second_dragon_traces[0].action_type, "sleep");
}

#[test]
fn build_judge_action_traces_uses_unknown_fallbacks_for_missing_player_or_payload() {
    let session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        chrono::DateTime::from_timestamp(1, 0).expect("valid timestamp"),
        protocol::WorkshopCreateConfig::default(),
    );

    let artifacts = vec![SessionArtifactRecord {
        id: "artifact-1".into(),
        session_id: session.id.to_string(),
        phase: protocol::Phase::Phase2,
        step: 2,
        kind: SessionArtifactKind::ActionProcessed,
        player_id: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        payload: serde_json::json!({
            "dragonId": "dragon-a"
        }),
    }];

    let traces = build_judge_action_traces(&session, &artifacts);

    let dragon_traces = traces.get("dragon-a").expect("dragon a traces");
    assert_eq!(dragon_traces.len(), 1);
    assert_eq!(dragon_traces[0].player_id, "unknown");
    assert_eq!(dragon_traces[0].player_name, "Unknown");
    assert_eq!(dragon_traces[0].action_type, "unknown");
    assert_eq!(dragon_traces[0].action_value, None);
    assert_eq!(dragon_traces[0].resulting_stats, None);
}

#[test]
fn to_client_game_state_includes_dragons_and_voting_details() {
    let timestamp = chrono::DateTime::from_timestamp(1, 0).expect("valid timestamp");
    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    let mut alice = session_player("p1", "Alice", 1);
    alice.is_host = true;
    let bob = session_player("p2", "Bob", 2);
    session.add_player(alice);
    session.add_player(bob);

    let assignments = vec![
        domain::Phase1Assignment {
            player_id: "p1".into(),
            dragon_id: "dragon-p1".into(),
        },
        domain::Phase1Assignment {
            player_id: "p2".into(),
            dragon_id: "dragon-p2".into(),
        },
    ];
    seed_selected_characters_on_session(&mut session);
    session.begin_phase1(&assignments).expect("begin phase1");
    session.record_discovery_observation("p1", "Calms down at dusk");
    session.record_discovery_observation("p2", "Rejects fruit at night");
    session
        .transition_to(protocol::Phase::Handover)
        .expect("enter handover");
    session
        .save_handover_tags(
            "p1",
            vec!["Rule 1".into(), "Rule 2".into(), "Rule 3".into()],
        )
        .expect("save handover tags for p1");
    session
        .save_handover_tags(
            "p2",
            vec!["Rule A".into(), "Rule B".into(), "Rule C".into()],
        )
        .expect("save handover tags for p2");
    session.enter_phase2().expect("enter phase2");
    session
        .apply_action("p1", domain::PlayerAction::Sleep)
        .expect("apply action");
    session.enter_voting().expect("enter voting");
    let voted_dragon_id = session
        .dragons
        .values()
        .find(|dragon| dragon.original_owner_id != "p1")
        .map(|dragon| dragon.id.clone())
        .expect("vote dragon");
    session
        .submit_vote("p1", &voted_dragon_id)
        .expect("submit vote");

    let client_state = to_client_game_state(&session, "p1");

    assert_eq!(client_state.phase, protocol::Phase::Voting);
    assert_eq!(client_state.dragons.len(), 2);
    let alice_rendered_current_dragon_id = client_state
        .players
        .get("p1")
        .and_then(|player| player.current_dragon_id.as_deref())
        .expect("alice current dragon")
        .to_string();
    let alice_current_dragon = client_state
        .dragons
        .get(&alice_rendered_current_dragon_id)
        .expect("alice current dragon details");
    assert!(
        !alice_current_dragon
            .condition_hint
            .as_deref()
            .unwrap_or_default()
            .is_empty()
    );
    assert_eq!(alice_current_dragon.handover_tags.len(), 3);
    assert_eq!(
        alice_current_dragon.last_action,
        protocol::DragonAction::Sleep
    );
    assert_eq!(
        alice_current_dragon.last_emotion,
        protocol::DragonEmotion::Sleepy
    );
    let original_dragon = client_state
        .dragons
        .get("dragon-p1")
        .expect("original dragon");
    assert_eq!(original_dragon.discovery_observations.len(), 1);
    assert!(
        client_state
            .voting
            .as_ref()
            .is_some_and(|voting| voting.eligible_count == 2)
    );
    assert!(
        client_state
            .voting
            .as_ref()
            .is_some_and(|voting| voting.submitted_count == 1)
    );
    assert!(
        client_state
            .voting
            .as_ref()
            .is_some_and(|voting| voting.current_player_vote_dragon_id.as_deref()
                == Some(voted_dragon_id.as_str()))
    );

    let current_players_original_dragon = client_state
        .dragons
        .get("dragon-p1")
        .expect("current player's original dragon");
    let other_players_original_dragon = client_state
        .dragons
        .get("dragon-p2")
        .expect("other player's original dragon");
    assert_eq!(
        current_players_original_dragon.original_owner_id.as_deref(),
        Some("p1")
    );
    assert_eq!(current_players_original_dragon.current_owner_id, None);
    assert_eq!(other_players_original_dragon.original_owner_id, None);
    assert_eq!(other_players_original_dragon.current_owner_id, None);
}

#[test]
fn to_client_game_state_normalizes_white_sprite_backgrounds() {
    use base64::Engine as _;
    use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
    use std::io::Cursor;

    let timestamp = chrono::DateTime::from_timestamp(1, 0).expect("valid timestamp");
    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    let mut alice = session_player("p1", "Alice", 1);
    alice.is_host = true;

    let mut sprite = ImageBuffer::from_pixel(6, 6, Rgba([255, 255, 255, 255]));
    for y in 1..5 {
        for x in 1..5 {
            sprite.put_pixel(x, y, Rgba([16, 32, 64, 255]));
        }
    }

    let mut png = Vec::new();
    DynamicImage::ImageRgba8(sprite)
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .expect("encode sprite png");
    let base64_sprite = base64::engine::general_purpose::STANDARD.encode(png);

    alice.character_id = Some("character-p1".to_string());
    alice.selected_character = Some(test_character_profile(
        "character-p1",
        "A fiery red dragon",
        protocol::SpriteSet {
            neutral: base64_sprite.clone(),
            happy: base64_sprite.clone(),
            angry: base64_sprite.clone(),
            sleepy: base64_sprite.clone(),
        },
        1,
    ));
    let bob = session_player("p2", "Bob", 2);
    session.add_player(alice);
    session.add_player(bob);

    seed_selected_characters_on_session(&mut session);
    session
        .begin_phase1(&[
            domain::Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "dragon-p1".into(),
            },
            domain::Phase1Assignment {
                player_id: "p2".into(),
                dragon_id: "dragon-p2".into(),
            },
        ])
        .expect("begin phase1");

    let client_state = to_client_game_state(&session, "p1");
    let alice_dragon_id = client_state
        .players
        .get("p1")
        .and_then(|p| p.current_dragon_id.clone())
        .expect("alice assigned to a dragon");
    let normalized = client_state
        .dragons
        .get(&alice_dragon_id)
        .and_then(|dragon| dragon.custom_sprites.as_ref())
        .map(|sprites| sprites.neutral.clone())
        .expect("normalized neutral sprite");

    let normalized = base64::engine::general_purpose::STANDARD
        .decode(normalized)
        .expect("decode normalized sprite");
    let normalized = image::load_from_memory_with_format(&normalized, ImageFormat::Png)
        .expect("decode normalized png")
        .to_rgba8();

    assert_eq!(
        normalized.get_pixel(0, 0)[3],
        0,
        "white edge background should become transparent"
    );
    assert_eq!(normalized.get_pixel(2, 2).0, [16, 32, 64, 255]);
}

#[test]
fn assign_player_character_marks_player_ready_and_persists_selected_character() {
    let timestamp = chrono::DateTime::from_timestamp(1, 0).expect("valid timestamp");
    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    let mut alice = session_player("p1", "Alice", 1);
    alice.is_host = true;
    session.add_player(alice);

    let sprites = protocol::SpriteSet {
        neutral: "neutral_b64".to_string(),
        happy: "happy_b64".to_string(),
        angry: "angry_b64".to_string(),
        sleepy: "sleepy_b64".to_string(),
    };

    session
        .assign_player_character(
            "p1",
            test_character_profile("character-p1", "A fiery red dragon", sprites.clone(), 1),
        )
        .expect("assign selected character");

    let player = session.players.get("p1").expect("player exists");
    assert_eq!(player.character_id.as_deref(), Some("character-p1"));
    assert_eq!(
        player
            .selected_character
            .as_ref()
            .map(|character| character.description.as_str()),
        Some("A fiery red dragon")
    );
    assert_eq!(
        player
            .selected_character
            .as_ref()
            .map(|character| &character.sprites),
        Some(&sprites)
    );
    assert!(
        player.is_ready,
        "character assignment should mark the player ready"
    );
}

// ---------------------------------------------------------------------------
// Auth (session 4, checkpoint 2a): signin / logout smoke tests.
// ---------------------------------------------------------------------------

/// Extract the `ds_session` Set-Cookie header from a response, if present.
fn extract_session_cookie(response: &axum::http::Response<Body>) -> Option<String> {
    response
        .headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("ds_session="))
        .map(|value| value.to_string())
}

#[tokio::test]
async fn signin_creates_new_account_and_sets_cookie() {
    let state = test_state();
    let app = build_app(state.clone());

    let body = serde_json::json!({
        "hero": "knight",
        "name": "Alice",
        "password": "correcthorse",
    })
    .to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/signin")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("build signin request"),
        )
        .await
        .expect("signin response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = extract_session_cookie(&response).expect("session cookie set");
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));

    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read signin body");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("signin body is json");
    assert_eq!(value["created"], serde_json::Value::Bool(true));
    assert_eq!(value["account"]["hero"], "knight");
    assert_eq!(value["account"]["name"], "Alice");

    // Account should be persisted.
    let record = state
        .store
        .find_account_by_name_lower("alice")
        .await
        .expect("find account")
        .expect("account exists");
    assert_eq!(record.hero, "knight");
    assert_eq!(record.name, "Alice");
    assert_ne!(record.password_hash, "correcthorse");
}

#[tokio::test]
async fn signin_logs_in_existing_account_when_password_matches() {
    let state = test_state();
    let app = build_app(state.clone());

    // First call creates.
    let body = serde_json::json!({
        "hero": "wizard",
        "name": "Bob",
        "password": "hunter2hunter2",
    })
    .to_string();
    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/signin")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.clone()))
                .expect("build first signin request"),
        )
        .await
        .expect("first signin response");
    assert_eq!(first.status(), StatusCode::CREATED);

    // Second call with same creds should log in (not create).
    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/signin")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("build second signin request"),
        )
        .await
        .expect("second signin response");

    assert_eq!(second.status(), StatusCode::OK);
    let bytes = to_bytes(second.into_body(), 64 * 1024)
        .await
        .expect("read login body");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("login body is json");
    assert_eq!(value["created"], serde_json::Value::Bool(false));
}

#[tokio::test]
async fn signin_rejects_wrong_password_for_existing_name() {
    let state = test_state();
    let app = build_app(state.clone());

    let create = serde_json::json!({
        "hero": "ranger",
        "name": "Carol",
        "password": "originalpassword",
    })
    .to_string();
    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/signin")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(create))
                .expect("build create request"),
        )
        .await
        .expect("create response");
    assert_eq!(created.status(), StatusCode::CREATED);

    let wrong = serde_json::json!({
        "hero": "ranger",
        "name": "Carol",
        "password": "guessedwrong",
    })
    .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/signin")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(wrong))
                .expect("build wrong-password request"),
        )
        .await
        .expect("wrong-password response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // No cookie on failure.
    assert!(extract_session_cookie(&response).is_none());

    // Structured error body so the SignIn screen can render the spec copy
    // from refactor.md:50 without string-matching free text.
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read wrong-password body");
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).expect("wrong-password body is json");
    assert_eq!(value["error"], "name_taken_wrong_password");
    assert_eq!(
        value["message"],
        "That name is already registered. Enter the correct password or choose a different name."
    );
}

#[tokio::test]
async fn signin_with_unknown_name_does_not_leak_name_taken_code() {
    // Enumeration guard: unknown name must NOT surface the
    // `name_taken_wrong_password` code (which would leak "name exists").
    // Today the unknown-name branch runs the argon2 timing dummy then
    // creates the account (201). This test locks that contract: the code
    // only appears after argon2 verify fails against a real stored hash.
    let state = test_state();
    let app = build_app(state);

    let body = serde_json::json!({
        "hero": "ghost",
        "name": "NeverSeen",
        "password": "anypasswordlong",
    })
    .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/signin")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("build unknown-name request"),
        )
        .await
        .expect("unknown-name response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read unknown-name body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf8 body");
    assert!(
        !text.contains("name_taken_wrong_password"),
        "unknown-name branch must not surface name_taken_wrong_password; got {text}"
    );
}

#[tokio::test]
async fn signin_rejects_short_password() {
    let state = test_state();
    let app = build_app(state);

    let body = serde_json::json!({
        "hero": "bard",
        "name": "Dave",
        "password": "short",
    })
    .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/signin")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("build short-password request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn logout_returns_no_content_and_clears_cookie() {
    let state = test_state();
    let app = build_app(state);

    // First sign in to acquire a signed session cookie.
    let body = serde_json::json!({
        "hero": "paladin",
        "name": "Erin",
        "password": "logoutpassword",
    })
    .to_string();
    let signin = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/signin")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("build signin request"),
        )
        .await
        .expect("signin response");
    assert_eq!(signin.status(), StatusCode::CREATED);
    let set_cookie = extract_session_cookie(&signin).expect("signin set cookie");
    // Extract just the `name=value` portion for the subsequent Cookie header.
    let cookie_pair = set_cookie
        .split(';')
        .next()
        .expect("cookie pair")
        .trim()
        .to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout")
                .header(axum::http::header::COOKIE, cookie_pair)
                .body(Body::empty())
                .expect("build logout request"),
        )
        .await
        .expect("logout response");

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    // remove() should emit a Set-Cookie that expires the session cookie.
    let cookie = extract_session_cookie(&response).expect("cleared cookie header");
    // An expired cookie has Max-Age=0 or a past Expires date.
    assert!(
        cookie.contains("Max-Age=0") || cookie.contains("Expires="),
        "logout cookie should expire the session: got {cookie}"
    );
}

#[tokio::test]
async fn logout_is_idempotent_without_prior_cookie() {
    let state = test_state();
    let app = build_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout")
                .body(Body::empty())
                .expect("build logout request"),
        )
        .await
        .expect("logout response");

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn signin_is_case_insensitive_on_login() {
    // Signup uses the hero-supplied casing; subsequent logins must succeed
    // regardless of case because `find_account_by_name_lower` normalises
    // the lookup. Guards against a regression where the handler does a
    // case-sensitive compare on the raw `name` column.
    let state = test_state();
    let app = build_app(state.clone());

    let create = serde_json::json!({
        "hero": "knight",
        "name": "Alice",
        "password": "correcthorsebattery",
    })
    .to_string();
    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/signin")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(create))
                .expect("build create request"),
        )
        .await
        .expect("create response");
    assert_eq!(created.status(), StatusCode::CREATED);

    // Login with upper-cased name + same password should succeed as an
    // existing-account login, not a new create.
    let login = serde_json::json!({
        "hero": "knight",
        "name": "ALICE",
        "password": "correcthorsebattery",
    })
    .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/signin")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(login))
                .expect("build case-variant login request"),
        )
        .await
        .expect("login response");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read login body");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("login body is json");
    assert_eq!(value["created"], serde_json::Value::Bool(false));
    // Response carries the originally-stored casing, not the request's.
    assert_eq!(value["account"]["name"], "Alice");
}

// ---------------------------------------------------------------------------
// Helper: sign in and return the Cookie header value for subsequent requests.
// ---------------------------------------------------------------------------

async fn signin_and_get_cookie(app: &Router, hero: &str, name: &str, password: &str) -> String {
    let body = serde_json::json!({
        "hero": hero,
        "name": name,
        "password": password,
    })
    .to_string();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/signin")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("build signin request"),
        )
        .await
        .expect("signin response");
    assert!(
        response.status() == StatusCode::CREATED || response.status() == StatusCode::OK,
        "signin failed with {}",
        response.status()
    );
    let set_cookie = extract_session_cookie(&response).expect("session cookie set");
    // Extract just the cookie key=value pair (everything before the first ';').
    let cookie_value = set_cookie
        .split(';')
        .next()
        .expect("cookie key=value part")
        .to_string();
    cookie_value
}

// ---------------------------------------------------------------------------
// C2-b tests: accounts/me
// ---------------------------------------------------------------------------

#[tokio::test]
async fn accounts_me_returns_profile_for_authenticated_user() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/accounts/me")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build accounts/me request"),
        )
        .await
        .expect("accounts/me response");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read accounts/me body");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(value["name"], "Alice");
    assert_eq!(value["hero"], "knight");
}

#[tokio::test]
async fn accounts_me_rejects_unauthenticated() {
    let state = test_state();
    let app = build_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/accounts/me")
                .body(Body::empty())
                .expect("build accounts/me request"),
        )
        .await
        .expect("accounts/me response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn signin_rejects_tampered_cookie() {
    let state = test_state();
    let app = build_app(state);

    // Use a tampered cookie: valid format but wrong signature.
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/accounts/me")
                .header(axum::http::header::COOKIE, "ds_session=tampered-value-here")
                .body(Body::empty())
                .expect("build tampered cookie request"),
        )
        .await
        .expect("tampered response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// C2-b tests: character CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_character_succeeds() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let body = serde_json::json!({
        "description": "A fierce red dragon",
        "sprites": {
            "neutral": "base64neutral",
            "happy": "base64happy",
            "angry": "base64angry",
            "sleepy": "base64sleepy"
        }
    })
    .to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build create character request"),
        )
        .await
        .expect("create character response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read character body");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(value["description"], "A fierce red dragon");
    assert!(value["id"].as_str().unwrap().starts_with("character_"));
}

#[tokio::test]
async fn create_character_enforces_limit() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    // Create MAX_CHARACTERS_PER_ACCOUNT characters (5).
    for i in 0..5 {
        let body = serde_json::json!({
            "description": format!("Dragon #{i}"),
            "sprites": {
                "neutral": "base64neutral",
                "happy": "base64happy",
                "angry": "base64angry",
                "sleepy": "base64sleepy"
            }
        })
        .to_string();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/characters")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .header(axum::http::header::COOKIE, &cookie)
                    .body(Body::from(body))
                    .expect("build create character request"),
            )
            .await
            .expect("create character response");
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "character #{i} should succeed"
        );
    }

    // 6th should fail.
    let body = serde_json::json!({
        "description": "One too many",
        "sprites": {
            "neutral": "base64neutral",
            "happy": "base64happy",
            "angry": "base64angry",
            "sleepy": "base64sleepy"
        }
    })
    .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build 6th create request"),
        )
        .await
        .expect("6th create response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_my_characters_returns_owned() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    // Create one character.
    let body = serde_json::json!({
        "description": "My dragon",
        "sprites": {
            "neutral": "base64neutral",
            "happy": "base64happy",
            "angry": "base64angry",
            "sleepy": "base64sleepy"
        }
    })
    .to_string();
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build create request"),
        )
        .await
        .expect("create response");

    // List should return it.
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/characters/mine")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build list request"),
        )
        .await
        .expect("list response");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read list body");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(value["characters"].as_array().unwrap().len(), 1);
    assert_eq!(value["limit"], 5);
}

#[tokio::test]
async fn delete_character_removes_owned() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    // Create a character.
    let body = serde_json::json!({
        "description": "Doomed dragon",
        "sprites": {
            "neutral": "base64neutral",
            "happy": "base64happy",
            "angry": "base64angry",
            "sleepy": "base64sleepy"
        }
    })
    .to_string();
    let create_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build create request"),
        )
        .await
        .expect("create response");
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let bytes = to_bytes(create_resp.into_body(), 64 * 1024)
        .await
        .expect("read create body");
    let created: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    let character_id = created["id"].as_str().unwrap();

    // Delete it.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/characters/{character_id}"))
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build delete request"),
        )
        .await
        .expect("delete response");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // List should be empty.
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/characters/mine")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build list request"),
        )
        .await
        .expect("list response");
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read list body");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(value["characters"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn delete_character_returns_404_for_nonexistent() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/characters/nonexistent_id")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build delete request"),
        )
        .await
        .expect("delete response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_character_renames_owned() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let body = serde_json::json!({
        "description": "Rename me",
        "sprites": {
            "neutral": "base64neutral",
            "happy": "base64happy",
            "angry": "base64angry",
            "sleepy": "base64sleepy"
        }
    })
    .to_string();
    let create_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build create request"),
        )
        .await
        .expect("create response");
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let bytes = to_bytes(create_resp.into_body(), 64 * 1024)
        .await
        .expect("read create body");
    let created: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    let character_id = created["id"].as_str().unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/characters/{character_id}"))
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(
                    serde_json::json!({ "name": "Sparky" }).to_string(),
                ))
                .expect("build update request"),
        )
        .await
        .expect("update response");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read update body");
    let updated: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(updated["name"], "Sparky");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/characters/mine")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build list request"),
        )
        .await
        .expect("list response");
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read list body");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(value["characters"][0]["name"], "Sparky");
}

#[tokio::test]
async fn update_character_rejects_blank_name() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/characters/character_1")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(serde_json::json!({ "name": "   " }).to_string()))
                .expect("build update request"),
        )
        .await
        .expect("update response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_character_rejects_wrong_owner() {
    let state = test_state();
    let app = build_app(state);
    let alice_cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;
    let bob_cookie = signin_and_get_cookie(&app, "knight", "Bob", "correcthorse").await;

    let body = serde_json::json!({
        "description": "Alice's dragon",
        "sprites": {
            "neutral": "base64neutral",
            "happy": "base64happy",
            "angry": "base64angry",
            "sleepy": "base64sleepy"
        }
    })
    .to_string();
    let create_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &alice_cookie)
                .body(Body::from(body))
                .expect("build create request"),
        )
        .await
        .expect("create response");
    let bytes = to_bytes(create_resp.into_body(), 64 * 1024)
        .await
        .expect("read create body");
    let created: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    let character_id = created["id"].as_str().unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/characters/{character_id}"))
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &bob_cookie)
                .body(Body::from(
                    serde_json::json!({ "name": "Stolen" }).to_string(),
                ))
                .expect("build update request"),
        )
        .await
        .expect("update response");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// C2-b tests: open workshops list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_open_workshops_returns_empty_when_none() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/workshops/open")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build open workshops request"),
        )
        .await
        .expect("open workshops response");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(value["workshops"].as_array().unwrap().len(), 0);
    // Empty result must not synthesize bookend cursors — otherwise a Prev/Next
    // click from an empty list would query with garbage cursor values.
    assert!(value.get("nextCursor").map(|v| v.is_null()).unwrap_or(true));
    assert!(value.get("prevCursor").map(|v| v.is_null()).unwrap_or(true));
}

// ---------------------------------------------------------------------------
// Open workshops paging (plan2 item 9)
// ---------------------------------------------------------------------------

async fn seed_open_workshops(state: &AppState, count: usize, start_seconds: i64) {
    // Seed `count` Lobby sessions with staggered created_at so pagination
    // ordering is deterministic. Each session has a distinct 6-digit code
    // and a single host player.
    for i in 0..count {
        let created_at = chrono::DateTime::<Utc>::from_timestamp(start_seconds + i as i64, 0)
            .expect("valid timestamp");
        let code = format!("{:06}", 200_000 + i);
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode(code.clone()),
            created_at,
            protocol::WorkshopCreateConfig::default(),
        );
        session.add_player(session_player_with_state(
            &format!("host-{code}"),
            &format!("Host-{code}"),
            created_at,
            true,
            true,
        ));
        state
            .store
            .save_session(&session)
            .await
            .expect("seed open workshop session");
    }
}

async fn get_open_workshops_json(
    app: &axum::Router,
    cookie: &str,
    query: &str,
) -> (StatusCode, serde_json::Value) {
    let uri = if query.is_empty() {
        "/api/workshops/open".to_string()
    } else {
        format!("/api/workshops/open?{query}")
    };
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header(axum::http::header::COOKIE, cookie)
                .body(Body::empty())
                .expect("build open workshops request"),
        )
        .await
        .expect("open workshops response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 512 * 1024)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    (status, value)
}

#[tokio::test]
async fn list_open_workshops_first_page_returns_next_cursor_when_more_rows() {
    let state = test_state();
    seed_open_workshops(&state, 75, 1_000).await;
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let (status, value) = get_open_workshops_json(&app, &cookie, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["workshops"].as_array().unwrap().len(), 4);
    assert!(value["nextCursor"].is_object(), "expected nextCursor");
    assert!(
        value.get("prevCursor").map(|v| v.is_null()).unwrap_or(true),
        "prevCursor must be absent on the first page"
    );
}

#[tokio::test]
async fn list_open_workshops_forward_paging_returns_older_rows() {
    let state = test_state();
    seed_open_workshops(&state, 75, 1_000).await;
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let (_, page1) = get_open_workshops_json(&app, &cookie, "").await;
    let next = page1["nextCursor"].as_object().expect("next cursor");
    let ts = next["createdAt"].as_str().unwrap().to_string();
    let code = next["sessionCode"].as_str().unwrap().to_string();
    // `:` and `+` inside RFC3339 need percent-encoding on the wire; use the
    // same encoding the web client uses so the test mirrors production.
    let ts_encoded = ts.replace(':', "%3A").replace('+', "%2B");
    let query = format!("after_created_at={ts_encoded}&after_session_code={code}");

    let (status, page2) = get_open_workshops_json(&app, &cookie, &query).await;
    assert_eq!(status, StatusCode::OK);
    let workshops = page2["workshops"].as_array().unwrap();
    // Seeded 75 rows; first page consumed 4, so page 2 returns the next 4.
    assert_eq!(workshops.len(), 4);
    // Every row must be strictly older than the cursor.
    for row in workshops {
        let created = row["createdAt"].as_str().unwrap();
        assert!(created < ts.as_str(), "row {created} older than {ts}");
    }
    // With a page size of 4 and 75 rows total, more older pages still exist.
    assert!(page2["nextCursor"].is_object());
    assert!(page2["prevCursor"].is_object());
}

#[tokio::test]
async fn list_open_workshops_backward_paging_returns_newer_rows() {
    let state = test_state();
    seed_open_workshops(&state, 75, 1_000).await;
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    // Walk forward to page 2, then Prev from there should return page 1's rows.
    let (_, page1) = get_open_workshops_json(&app, &cookie, "").await;
    let next = page1["nextCursor"].as_object().expect("next cursor");
    let ts = next["createdAt"].as_str().unwrap().to_string();
    let code = next["sessionCode"].as_str().unwrap().to_string();
    let ts_encoded = ts.replace(':', "%3A").replace('+', "%2B");
    let forward_query = format!("after_created_at={ts_encoded}&after_session_code={code}");
    let (_, page2) = get_open_workshops_json(&app, &cookie, &forward_query).await;

    let prev = page2["prevCursor"].as_object().expect("prev cursor");
    let prev_ts = prev["createdAt"].as_str().unwrap().to_string();
    let prev_code = prev["sessionCode"].as_str().unwrap().to_string();
    let prev_ts_encoded = prev_ts.replace(':', "%3A").replace('+', "%2B");
    let back_query = format!("before_created_at={prev_ts_encoded}&before_session_code={prev_code}");
    let (status, back_page) = get_open_workshops_json(&app, &cookie, &back_query).await;
    assert_eq!(status, StatusCode::OK);
    // Prev from page 2's first row returns page 1's 4 rows.
    assert_eq!(back_page["workshops"].as_array().unwrap().len(), 4);
    // Every returned row is strictly newer than the cursor.
    for row in back_page["workshops"].as_array().unwrap() {
        let created = row["createdAt"].as_str().unwrap();
        assert!(created > prev_ts.as_str());
    }
}

#[tokio::test]
async fn list_open_workshops_rejects_both_after_and_before() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let (status, _) = get_open_workshops_json(
        &app,
        &cookie,
        "after_created_at=2026-01-01T00%3A00%3A00Z&after_session_code=000001\
         &before_created_at=2026-01-01T00%3A00%3A00Z&before_session_code=000002",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_open_workshops_rejects_unpaired_cursor_half() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    // Only `after_created_at`, no `after_session_code`.
    let (status, _) =
        get_open_workshops_json(&app, &cookie, "after_created_at=2026-01-01T00%3A00%3A00Z").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Only `before_session_code`, no `before_created_at`.
    let (status, _) = get_open_workshops_json(&app, &cookie, "before_session_code=000002").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_open_workshops_treats_empty_string_params_as_absent() {
    // Regression lock: `serde_urlencoded` turns `?after_created_at=` into
    // `Some("")`, not `None`. Without the empty-string filter in the handler,
    // fully-empty pairs would be fed to the cursor path and return an empty
    // list indistinguishable from "no workshops exist". Fully-empty pairs
    // must fall through to the First page instead.
    let state = test_state();
    seed_open_workshops(&state, 3, 1_000).await;
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let (status, value) =
        get_open_workshops_json(&app, &cookie, "after_created_at=&after_session_code=").await;
    assert_eq!(status, StatusCode::OK);
    // Should behave identically to `?` (no params) — return all 3 seeded rows.
    assert_eq!(value["workshops"].as_array().unwrap().len(), 3);

    // Symmetric check for the `before_*` side.
    let (status, value) =
        get_open_workshops_json(&app, &cookie, "before_created_at=&before_session_code=").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["workshops"].as_array().unwrap().len(), 3);
}

// ---------------------------------------------------------------------------
// C2-b tests: eligible characters
// ---------------------------------------------------------------------------

#[tokio::test]
async fn eligible_characters_returns_owned_characters() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    // Create a character.
    let body = serde_json::json!({
        "description": "Eligible dragon",
        "sprites": {
            "neutral": "base64neutral",
            "happy": "base64happy",
            "angry": "base64angry",
            "sleepy": "base64sleepy"
        }
    })
    .to_string();
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build create request"),
        )
        .await
        .expect("create response");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/workshops/123456/eligible-characters")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build eligible characters request"),
        )
        .await
        .expect("eligible characters response");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(value["characters"].as_array().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// C2-b gap tests: unauthenticated 401s
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_character_rejects_unauthenticated() {
    let state = test_state();
    let app = build_app(state);

    let body = serde_json::json!({
        "description": "Sneaky dragon",
        "sprites": {
            "neutral": "base64neutral",
            "happy": "base64happy",
            "angry": "base64angry",
            "sleepy": "base64sleepy"
        }
    })
    .to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("build create request"),
        )
        .await
        .expect("create response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_my_characters_rejects_unauthenticated() {
    let state = test_state();
    let app = build_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/characters/mine")
                .body(Body::empty())
                .expect("build list request"),
        )
        .await
        .expect("list response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_character_rejects_unauthenticated() {
    let state = test_state();
    let app = build_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/characters/some_id")
                .body(Body::empty())
                .expect("build delete request"),
        )
        .await
        .expect("delete response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_open_workshops_rejects_unauthenticated() {
    let state = test_state();
    let app = build_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/workshops/open")
                .body(Body::empty())
                .expect("build open workshops request"),
        )
        .await
        .expect("open workshops response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn eligible_characters_rejects_unauthenticated() {
    let state = test_state();
    let app = build_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/workshops/123456/eligible-characters")
                .body(Body::empty())
                .expect("build eligible characters request"),
        )
        .await
        .expect("eligible characters response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// C2-b gap tests: description validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_character_rejects_empty_description() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let body = serde_json::json!({
        "description": "",
        "sprites": {
            "neutral": "base64neutral",
            "happy": "base64happy",
            "angry": "base64angry",
            "sleepy": "base64sleepy"
        }
    })
    .to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build create request"),
        )
        .await
        .expect("create response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_character_rejects_too_long_description() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let body = serde_json::json!({
        "description": "x".repeat(513),
        "sprites": {
            "neutral": "base64neutral",
            "happy": "base64happy",
            "angry": "base64angry",
            "sleepy": "base64sleepy"
        }
    })
    .to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build create request"),
        )
        .await
        .expect("create response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// C2-b gap tests: delete character ownership check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_workshop_deletes_owned_reserved_lobby() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/lobby")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create lobby request"),
        )
        .await
        .expect("call create workshop lobby");
    assert_eq!(create_response.status(), StatusCode::CREATED);
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create lobby body");
    let create_result: WorkshopCreateResult =
        serde_json::from_slice(&create_body).expect("parse create lobby result");
    let session_code = match create_result {
        WorkshopCreateResult::Success(success) => success.session_code,
        WorkshopCreateResult::Error(error) => panic!("expected success, got {}", error.error),
    };

    let delete_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/workshops/{session_code}"))
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build delete workshop request"),
        )
        .await
        .expect("delete workshop response");
    assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);
    assert!(
        state
            .store
            .load_session_by_code(&session_code)
            .await
            .expect("load deleted session")
            .is_none(),
        "deleted lobby should be removed from the store"
    );
}

#[tokio::test]
async fn delete_workshop_rejects_wrong_owner() {
    let state = test_state();
    let app = build_app(state.clone());
    let alice_cookie = test_auth_cookie(&app, "Alice").await;
    let bob_cookie = test_auth_cookie(&app, "Bob").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/lobby")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &alice_cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create lobby request"),
        )
        .await
        .expect("call create workshop lobby");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create lobby body");
    let create_result: WorkshopCreateResult =
        serde_json::from_slice(&create_body).expect("parse create lobby result");
    let session_code = match create_result {
        WorkshopCreateResult::Success(success) => success.session_code,
        WorkshopCreateResult::Error(error) => panic!("expected success, got {}", error.error),
    };

    let delete_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/workshops/{session_code}"))
                .header(axum::http::header::COOKIE, &bob_cookie)
                .body(Body::empty())
                .expect("build delete workshop request"),
        )
        .await
        .expect("delete workshop response");
    assert_eq!(delete_response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_workshop_allows_legacy_reserved_lobby_owned_by_requester() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let account_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/accounts/me")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build accounts/me request"),
        )
        .await
        .expect("accounts/me response");
    assert_eq!(account_response.status(), StatusCode::OK);
    let account_body = to_bytes(account_response.into_body(), 64 * 1024)
        .await
        .expect("read accounts/me body");
    let account_value: serde_json::Value =
        serde_json::from_slice(&account_body).expect("accounts/me json");
    let account_id = account_value["id"]
        .as_str()
        .expect("account id")
        .to_string();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("654322".to_string()),
        Utc::now(),
        protocol::WorkshopCreateConfig::default(),
    );
    session.reserve_host(account_id, "Alice");
    session.owner_account_id = None;
    state
        .store
        .save_session(&session)
        .await
        .expect("save legacy lobby");

    let delete_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/workshops/654322")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build delete workshop request"),
        )
        .await
        .expect("delete workshop response");
    assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);
    assert!(
        state
            .store
            .load_session_by_code("654322")
            .await
            .expect("load deleted session")
            .is_none(),
        "deleted legacy lobby should be removed from the store"
    );
}

#[tokio::test]
async fn delete_workshop_rejects_non_lobby_session() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/lobby")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create lobby request"),
        )
        .await
        .expect("call create workshop lobby");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create lobby body");
    let create_result: WorkshopCreateResult =
        serde_json::from_slice(&create_body).expect("parse create lobby result");
    let session_code = match create_result {
        WorkshopCreateResult::Success(success) => success.session_code,
        WorkshopCreateResult::Error(error) => panic!("expected success, got {}", error.error),
    };

    let mut session = state
        .store
        .load_session_by_code(&session_code)
        .await
        .expect("load created session")
        .expect("session exists");
    session.phase = protocol::Phase::Phase1;
    state
        .store
        .save_session(&session)
        .await
        .expect("save phase1 session");

    let delete_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/workshops/{session_code}"))
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build delete workshop request"),
        )
        .await
        .expect("delete workshop response");
    assert_eq!(delete_response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_workshop_rejects_owner_after_owner_joins_lobby() {
    let state = test_state();
    let app = build_app(state.clone());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/lobby")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create lobby request"),
        )
        .await
        .expect("call create workshop lobby");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create lobby body");
    let create_result: WorkshopCreateResult =
        serde_json::from_slice(&create_body).expect("parse create lobby result");
    let session_code = match create_result {
        WorkshopCreateResult::Success(success) => success.session_code,
        WorkshopCreateResult::Error(error) => panic!("expected success, got {}", error.error),
    };

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(
                    serde_json::json!({
                        "sessionCode": session_code,
                        "characterId": null,
                        "reconnectToken": null
                    })
                    .to_string(),
                ))
                .expect("build join request"),
        )
        .await
        .expect("join workshop response");
    assert_eq!(join_response.status(), StatusCode::OK);

    let (status, value) = get_open_workshops_json(&app, &cookie, "").await;
    assert_eq!(status, StatusCode::OK);
    let created = value["workshops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["sessionCode"].as_str() == Some(session_code.as_str()))
        .expect("joined lobby appears in open workshops");
    assert_eq!(created["canDelete"].as_bool(), Some(false));

    let delete_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/workshops/{session_code}"))
                .header("origin", "http://localhost:5173")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build delete workshop request"),
        )
        .await
        .expect("delete workshop response");
    assert_eq!(delete_response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_workshop_rejects_when_another_player_has_joined_lobby() {
    let state = test_state();
    let app = build_app(state.clone());
    let alice_cookie = test_auth_cookie(&app, "Alice").await;
    let bob_cookie = test_auth_cookie(&app, "Bob").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/lobby")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &alice_cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create lobby request"),
        )
        .await
        .expect("call create workshop lobby");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create lobby body");
    let create_result: WorkshopCreateResult =
        serde_json::from_slice(&create_body).expect("parse create lobby result");
    let session_code = match create_result {
        WorkshopCreateResult::Success(success) => success.session_code,
        WorkshopCreateResult::Error(error) => panic!("expected success, got {}", error.error),
    };

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &bob_cookie)
                .body(Body::from(
                    serde_json::json!({
                        "sessionCode": session_code,
                        "characterId": null,
                        "reconnectToken": null
                    })
                    .to_string(),
                ))
                .expect("build Bob join request"),
        )
        .await
        .expect("Bob join workshop response");
    assert_eq!(join_response.status(), StatusCode::OK);

    let (status, value) = get_open_workshops_json(&app, &alice_cookie, "").await;
    assert_eq!(status, StatusCode::OK);
    let created = value["workshops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["sessionCode"].as_str() == Some(session_code.as_str()))
        .expect("joined lobby appears in open workshops");
    assert_eq!(created["canDelete"].as_bool(), Some(false));

    let delete_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/workshops/{session_code}"))
                .header("origin", "http://localhost:5173")
                .header(axum::http::header::COOKIE, &alice_cookie)
                .body(Body::empty())
                .expect("build delete workshop request"),
        )
        .await
        .expect("delete workshop response");
    assert_eq!(delete_response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_character_rejects_wrong_owner() {
    let state = test_state();
    let app = build_app(state);

    // Alice creates a character.
    let alice_cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;
    let body = serde_json::json!({
        "description": "Alice's dragon",
        "sprites": {
            "neutral": "base64neutral",
            "happy": "base64happy",
            "angry": "base64angry",
            "sleepy": "base64sleepy"
        }
    })
    .to_string();
    let create_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &alice_cookie)
                .body(Body::from(body))
                .expect("build create request"),
        )
        .await
        .expect("create response");
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let bytes = to_bytes(create_resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let created: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    let character_id = created["id"].as_str().unwrap();

    // Bob tries to delete Alice's character.
    let bob_cookie = signin_and_get_cookie(&app, "knight", "Bob", "correcthorse").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/characters/{character_id}"))
                .header(axum::http::header::COOKIE, &bob_cookie)
                .body(Body::empty())
                .expect("build delete request"),
        )
        .await
        .expect("delete response");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// C2-b gap tests: whitespace-only description
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_character_rejects_whitespace_only_description() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    let body = serde_json::json!({
        "description": "   ",
        "sprites": {
            "neutral": "base64neutral",
            "happy": "base64happy",
            "angry": "base64angry",
            "sleepy": "base64sleepy"
        }
    })
    .to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build create request"),
        )
        .await
        .expect("create response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// C2-d tests: same-account dedup, starter-lease, MissingSelectedCharacter,
//             character-create rate limit
// ---------------------------------------------------------------------------

/// Helper: create a workshop and return its session_code.
async fn create_test_workshop(app: &Router, cookie: &str) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, cookie)
                .body(Body::from(create_workshop_body("Test")))
                .expect("build create workshop request"),
        )
        .await
        .expect("create workshop response");
    assert!(
        response.status() == StatusCode::CREATED || response.status() == StatusCode::OK,
        "create workshop failed with {}",
        response.status()
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let result: WorkshopJoinResult = serde_json::from_slice(&bytes).expect("parse result");
    match result {
        WorkshopJoinResult::Success(s) => s.session_code,
        WorkshopJoinResult::Error(e) => panic!("expected success: {}", e.error),
    }
}

async fn create_test_workshop_lobby(app: &Router, cookie: &str) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/lobby")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, cookie)
                .body(Body::from(create_workshop_body("Test")))
                .expect("build create workshop lobby request"),
        )
        .await
        .expect("create workshop lobby response");
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let result: WorkshopCreateResult = serde_json::from_slice(&bytes).expect("parse result");
    match result {
        WorkshopCreateResult::Success(s) => s.session_code,
        WorkshopCreateResult::Error(e) => panic!("expected success: {}", e.error),
    }
}

#[tokio::test]
async fn creator_can_join_own_create_only_workshop() {
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;

    let session_code = create_test_workshop_lobby(&app, &cookie).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Alice"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("join response");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let result: WorkshopJoinResult = serde_json::from_slice(&bytes).expect("parse result");
    match result {
        WorkshopJoinResult::Success(success) => {
            let player = success
                .state
                .players
                .get(&success.player_id)
                .expect("creator player in state");
            assert!(player.is_host);
        }
        WorkshopJoinResult::Error(e) => panic!("expected success, got: {}", e.error),
    }
}

#[tokio::test]
async fn creator_claims_host_after_guest_joins_reserved_lobby() {
    let app = build_app(test_state());
    let alice_cookie = test_auth_cookie(&app, "Alice").await;
    let bob_cookie = test_auth_cookie(&app, "Bob").await;

    let session_code = create_test_workshop_lobby(&app, &alice_cookie).await;

    let guest_join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &bob_cookie)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build guest join request"),
        )
        .await
        .expect("guest join response");
    assert_eq!(guest_join_response.status(), StatusCode::OK);
    let guest_bytes = to_bytes(guest_join_response.into_body(), usize::MAX)
        .await
        .expect("read guest join body");
    let guest_result: WorkshopJoinResult =
        serde_json::from_slice(&guest_bytes).expect("parse guest join result");
    let guest_success = match guest_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected guest join success, got error: {}", error.error)
        }
    };
    let guest_player = guest_success
        .state
        .players
        .get(&guest_success.player_id)
        .expect("guest player in state");
    assert!(!guest_player.is_host);

    let creator_join_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &alice_cookie)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Alice"}}"#,
                    session_code
                )))
                .expect("build creator join request"),
        )
        .await
        .expect("creator join response");
    assert_eq!(creator_join_response.status(), StatusCode::OK);
    let creator_bytes = to_bytes(creator_join_response.into_body(), usize::MAX)
        .await
        .expect("read creator join body");
    let creator_result: WorkshopJoinResult =
        serde_json::from_slice(&creator_bytes).expect("parse creator join result");
    let creator_success = match creator_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected creator join success, got error: {}", error.error)
        }
    };
    let creator_player = creator_success
        .state
        .players
        .get(&creator_success.player_id)
        .expect("creator player in state");
    assert!(creator_player.is_host);
}

#[tokio::test]
async fn join_workshop_leases_starter_when_zero_owned_characters() {
    let state = test_state();

    // Seed a starter character in the store.
    let starter = persistence::CharacterRecord {
        id: "character_starter_001".to_string(),
        name: None,
        description: "A friendly starter dragon".to_string(),
        sprites: protocol::SpriteSet {
            neutral: "sn".to_string(),
            happy: "sh".to_string(),
            angry: "sa".to_string(),
            sleepy: "ss".to_string(),
        },
        remaining_sprite_regenerations: 0,
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
        owner_account_id: None, // starter
    };
    state
        .store
        .save_character(&starter)
        .await
        .expect("seed starter");

    let app = build_app(state);
    let cookie = test_auth_cookie(&app, "Newbie").await;

    // Newbie has 0 owned characters → gets a leased starter.
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("StarterTest")))
                .expect("build request"),
        )
        .await
        .expect("create response");

    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let result: WorkshopJoinResult = serde_json::from_slice(&bytes).expect("parse result");
    match result {
        WorkshopJoinResult::Success(success) => {
            let player = success
                .state
                .players
                .get(&success.player_id)
                .expect("player in state");
            assert!(
                player.is_ready,
                "player should be ready with leased character"
            );
            assert!(
                player.pet_description.is_some(),
                "leased starter should provide a pet_description"
            );
        }
        WorkshopJoinResult::Error(e) => panic!("expected success, got: {}", e.error),
    }
}

#[tokio::test]
async fn begin_phase1_rejects_player_without_selected_character() {
    let now = chrono::Utc::now();
    let mut session = domain::WorkshopSession::new(
        uuid::Uuid::new_v4(),
        domain::SessionCode("000000".to_string()),
        now,
        protocol::WorkshopCreateConfig {
            phase0_minutes: 5,
            phase1_minutes: 10,
            phase2_minutes: 10,
        },
    );
    session.add_player(domain::SessionPlayer {
        id: "player_1".to_string(),
        name: "Alice".to_string(),
        account_id: Some("acc_1".to_string()),
        character_id: None,
        is_host: true,
        is_connected: true,
        score: 0,
        achievements: Vec::new(),
        current_dragon_id: None,
        selected_character: None,
        is_ready: false,
        joined_at: now,
    });

    let result = session.begin_phase1(&[domain::Phase1Assignment {
        player_id: "player_1".to_string(),
        dragon_id: "dragon_1".to_string(),
    }]);

    match result {
        Err(domain::DomainError::MissingSelectedCharacter { players }) => {
            assert_eq!(players, vec!["player_1".to_string()]);
        }
        Err(other) => panic!("expected MissingSelectedCharacter, got: {other:?}"),
        Ok(()) => panic!("expected begin_phase1 to reject player without selected_character"),
    }
}

#[tokio::test]
async fn character_create_rate_limit_returns_429() {
    let base_config = (*test_state().config).clone();
    let config = AppConfig {
        character_create_rate_limit: 1,
        ..base_config
    };
    let state = AppState::new(
        Arc::new(config),
        Arc::new(InMemorySessionStore::new()),
        timeout_companion_defaults().sprites,
    );
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "RateTester", "correcthorse").await;

    // First create succeeds.
    let body = serde_json::json!({
        "description": "First dragon",
        "sprites": { "neutral": "n", "happy": "h", "angry": "a", "sleepy": "s" }
    })
    .to_string();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::CREATED);

    // Second create within the same window → 429.
    let body2 = serde_json::json!({
        "description": "Second dragon",
        "sprites": { "neutral": "n", "happy": "h", "angry": "a", "sleepy": "s" }
    })
    .to_string();
    let response2 = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body2))
                .expect("build request"),
        )
        .await
        .expect("response");
    assert_eq!(response2.status(), StatusCode::TOO_MANY_REQUESTS);
}

// ---------------------------------------------------------------------------
// POST /api/characters/preview-sprites (plan2.md item 2)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preview_sprites_requires_account_cookie() {
    let state = test_state();
    let app = build_app(state);

    let body = serde_json::json!({
        "description": "A tiny green dragon with a fiery tail",
    })
    .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters/preview-sprites")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("build preview request"),
        )
        .await
        .expect("preview response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn preview_sprites_returns_busy_error_when_generation_unavailable() {
    // No LLM providers are configured in the default test_state. The preview
    // endpoint must surface a retryable busy error instead of returning the
    // default companion as if generation had succeeded.
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "PreviewAlice", "correcthorse").await;

    let body = serde_json::json!({
        "description": "A tiny green dragon with a fiery tail",
    })
    .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters/preview-sprites")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build preview request"),
        )
        .await
        .expect("preview response");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read preview body");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(
        value["error"],
        "Sprite API is busy. Please try again in a few minutes."
    );
    assert!(value.get("sprites").is_none());
}

#[tokio::test]
async fn preview_sprites_queue_timeout_returns_busy_error_without_fallback() {
    let state = test_state_with_sprite_queue_timeout(std::time::Duration::from_millis(20));
    let mut queue_permits = Vec::new();
    for _ in 0..state.config.image_job_max_concurrency {
        queue_permits.push(
            state
                .image_job_queue
                .clone()
                .try_acquire_owned()
                .expect("reserve queue permit for preview timeout test"),
        );
    }

    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "PreviewQueue", "correcthorse").await;

    let body = serde_json::json!({
        "description": "A tiny green dragon with a fiery tail",
    })
    .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters/preview-sprites")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build preview request"),
        )
        .await
        .expect("preview response");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read preview body");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(
        value["error"],
        "Sprite API is busy. Please try again in a few minutes."
    );
    assert!(value.get("sprites").is_none());

    drop(queue_permits);
}

#[tokio::test]
async fn preview_sprites_does_not_create_character_record() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "PreviewNoPersist", "correcthorse").await;

    // Baseline: list-my-characters returns zero.
    let before = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/characters/mine")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build list request"),
        )
        .await
        .expect("list response");
    assert_eq!(before.status(), StatusCode::OK);
    let before_bytes = to_bytes(before.into_body(), 64 * 1024)
        .await
        .expect("read list body");
    let before_json: serde_json::Value =
        serde_json::from_slice(&before_bytes).expect("list is json");
    assert_eq!(before_json["characters"].as_array().unwrap().len(), 0);

    // Call preview-sprites.
    let body = serde_json::json!({
        "description": "A tiny green dragon with a fiery tail",
    })
    .to_string();
    let preview = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters/preview-sprites")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build preview request"),
        )
        .await
        .expect("preview response");
    assert_eq!(preview.status(), StatusCode::SERVICE_UNAVAILABLE);

    // After: list-my-characters still returns zero — preview did not persist.
    let after = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/characters/mine")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build list request"),
        )
        .await
        .expect("list response");
    assert_eq!(after.status(), StatusCode::OK);
    let after_bytes = to_bytes(after.into_body(), 64 * 1024)
        .await
        .expect("read list body");
    let after_json: serde_json::Value = serde_json::from_slice(&after_bytes).expect("list is json");
    assert_eq!(after_json["characters"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn preview_sprites_rejects_empty_description() {
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "PreviewEmpty", "correcthorse").await;

    let body = serde_json::json!({ "description": "   " }).to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters/preview-sprites")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build preview request"),
        )
        .await
        .expect("preview response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn preview_sprites_rate_limit_returns_429() {
    // The preview route shares the per-account `character_create_limiter`
    // with `POST /api/characters`, so setting the limit to 1 and calling
    // preview twice must trip the limiter on the second call.
    let base_config = (*test_state().config).clone();
    let config = AppConfig {
        character_create_rate_limit: 1,
        ..base_config
    };
    let state = AppState::new(
        Arc::new(config),
        Arc::new(InMemorySessionStore::new()),
        timeout_companion_defaults().sprites,
    );
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "PreviewRate", "correcthorse").await;

    let body = serde_json::json!({
        "description": "A tiny green dragon with a fiery tail",
    })
    .to_string();
    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters/preview-sprites")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body.clone()))
                .expect("build first preview request"),
        )
        .await
        .expect("first preview response");
    assert_eq!(first.status(), StatusCode::SERVICE_UNAVAILABLE);

    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters/preview-sprites")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build second preview request"),
        )
        .await
        .expect("second preview response");
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
}

// ---------------------------------------------------------------------------
// plan2.md item 3: starter-lease uniqueness inside a workshop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn join_workshop_assigns_distinct_starters_to_two_accounts() {
    // `InMemorySessionStore::new()` seeds 4 starters so any two zero-character
    // accounts that lease from the pool must receive distinct ids.
    let state = test_state();
    let app = build_app(state);

    // Alice creates the workshop (zero owned characters → leased starter).
    let alice_cookie = test_auth_cookie(&app, "Alice").await;
    let session_code = create_test_workshop(&app, &alice_cookie).await;

    // Bob joins with zero owned characters → should lease a different starter.
    let bob_cookie = test_auth_cookie(&app, "Bob").await;
    let join_body = format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &bob_cookie)
                .body(Body::from(join_body))
                .expect("build join request"),
        )
        .await
        .expect("join response");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let result: WorkshopJoinResult = serde_json::from_slice(&bytes).expect("parse result");
    let success = match result {
        WorkshopJoinResult::Success(s) => s,
        WorkshopJoinResult::Error(e) => panic!("expected success, got: {}", e.error),
    };

    // Both players are in the state snapshot returned to Bob. Collect their
    // character ids and assert they differ.
    let mut character_ids: Vec<String> = success
        .state
        .players
        .values()
        .filter_map(|player| player.character_id.clone())
        .collect();
    assert_eq!(
        character_ids.len(),
        2,
        "expected both players to have a leased character, got: {:?}",
        success.state.players
    );
    character_ids.sort();
    character_ids.dedup();
    assert_eq!(
        character_ids.len(),
        2,
        "expected two distinct starter character ids across the two players, got: {:?}",
        success.state.players
    );
}

#[tokio::test]
async fn join_workshop_returns_error_when_all_starters_leased_in_session() {
    // `InMemorySessionStore::new()` seeds exactly 4 starters (see
    // `starter_character_defaults`). With 4 zero-character accounts the pool
    // is fully leased; the 5th joiner has no starter left and must be
    // rejected with the "no starter available" error.
    let state = test_state();
    let app = build_app(state);

    let alice_cookie = test_auth_cookie(&app, "Alice").await;
    let session_code = create_test_workshop(&app, &alice_cookie).await;

    // 3 more joiners fill up the remaining 3 seeded starters.
    for name in ["Bob", "Carol", "Dave"] {
        let cookie = test_auth_cookie(&app, name).await;
        let body = format!(r#"{{"sessionCode":"{}","name":"{}"}}"#, session_code, name);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/join")
                    .header("content-type", "application/json")
                    .header(axum::http::header::COOKIE, &cookie)
                    .body(Body::from(body))
                    .expect("build join request"),
            )
            .await
            .expect("join response");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "joiner {name} should have leased a distinct starter"
        );
    }

    // 5th joiner: pool exhausted → expect "no starter available".
    let eve_cookie = test_auth_cookie(&app, "Eve").await;
    let eve_body = format!(r#"{{"sessionCode":"{}","name":"Eve"}}"#, session_code);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &eve_cookie)
                .body(Body::from(eve_body))
                .expect("build join request"),
        )
        .await
        .expect("join response");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let result: WorkshopJoinResult = serde_json::from_slice(&bytes).expect("parse result");
    match result {
        WorkshopJoinResult::Error(e) => {
            assert!(
                e.error.contains("no starter available"),
                "expected 'no starter available' in error, got: {}",
                e.error
            );
        }
        WorkshopJoinResult::Success(s) => {
            panic!(
                "expected error when all starters already leased in session; got success with players: {:?}",
                s.state.players
            )
        }
    }
}

#[tokio::test]
async fn join_workshop_rejects_explicit_starter_already_leased() {
    // plan2.md item 3: an explicit `characterId` pointing at a starter that
    // another seated player already leased in this session must be rejected.
    // Without this guard, a client can observe another player's starter id
    // from a GameState broadcast and POST /api/workshops/join with that id to
    // bypass the auto-lease exclusion.
    let state = test_state();
    let app = build_app(state.clone());

    // Alice creates the workshop; auto-lease grants her one of the seeded
    // starters. Capture that starter id from the persisted session.
    let alice_cookie = test_auth_cookie(&app, "Alice").await;
    let session_code = create_test_workshop(&app, &alice_cookie).await;

    let session = state
        .store
        .load_session_by_code(&session_code)
        .await
        .expect("load session")
        .expect("session exists");
    let alice_starter_id = session
        .players
        .values()
        .find_map(|player| player.character_id.clone())
        .expect("alice has a leased starter character_id");

    // Bob attempts to join with Alice's starter id explicitly. Must be
    // rejected with HTTP 400 and the "already taken" error string.
    let bob_cookie = test_auth_cookie(&app, "Bob").await;
    let join_body = format!(
        r#"{{"sessionCode":"{}","name":"Bob","characterId":"{}"}}"#,
        session_code, alice_starter_id
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &bob_cookie)
                .body(Body::from(join_body))
                .expect("build join request"),
        )
        .await
        .expect("join response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let result: WorkshopJoinResult = serde_json::from_slice(&bytes).expect("parse result");
    match result {
        WorkshopJoinResult::Error(e) => {
            assert!(
                e.error.contains("that starter is already taken"),
                "expected 'that starter is already taken' in error, got: {}",
                e.error
            );
        }
        WorkshopJoinResult::Success(s) => {
            panic!(
                "expected error when Bob explicitly requested Alice's leased starter; got success with players: {:?}",
                s.state.players
            )
        }
    }
}

#[tokio::test]
async fn create_character_returns_400_when_character_limit_reached() {
    // Covers plan2 item 10: character-limit exhaustion is a client input
    // problem (the account simply cannot hold more), not a state conflict.
    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;

    for i in 0..MAX_CHARACTERS_PER_ACCOUNT {
        let body = serde_json::json!({
            "description": format!("Dragon #{i}"),
            "sprites": {
                "neutral": "base64neutral",
                "happy": "base64happy",
                "angry": "base64angry",
                "sleepy": "base64sleepy"
            }
        })
        .to_string();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/characters")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .header(axum::http::header::COOKIE, &cookie)
                    .body(Body::from(body))
                    .expect("build create character request"),
            )
            .await
            .expect("create character response");
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    let body = serde_json::json!({
        "description": "One too many",
        "sprites": {
            "neutral": "base64neutral",
            "happy": "base64happy",
            "angry": "base64angry",
            "sleepy": "base64sleepy"
        }
    })
    .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(body))
                .expect("build over-limit create request"),
        )
        .await
        .expect("over-limit create response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body_bytes = to_bytes(response.into_body(), 4096)
        .await
        .expect("read over-limit body");
    let body_json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("over-limit body is JSON");
    let error_string = body_json
        .get("error")
        .and_then(|v| v.as_str())
        .expect("over-limit body carries error string");
    assert!(
        error_string.contains("character limit reached"),
        "expected error string to mention 'character limit reached', got {error_string:?}"
    );
}

#[tokio::test]
async fn join_workshop_returns_409_when_workshop_already_started() {
    // Covers plan2 item 10: joining a workshop that has advanced past Lobby
    // is a state conflict (the resource is not in an acceptable state), not
    // a validation problem with the request body.
    let state = test_state();
    let app = build_app(state);
    let host_cookie = test_auth_cookie(&app, "Alice").await;
    let joiner_cookie = test_auth_cookie(&app, "Bob").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &host_cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_success = match serde_json::from_slice::<WorkshopJoinResult>(&create_body)
        .expect("parse create result")
    {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    // Advance past Lobby so the next join attempt is rejected as a conflict.
    let start_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build startPhase1 request"),
        )
        .await
        .expect("call startPhase1");
    assert_eq!(start_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &joiner_cookie)
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    create_success.session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Error(error) => {
            assert_eq!(
                error.error,
                "This workshop has already started. New players can only join in the lobby."
            );
        }
        WorkshopJoinResult::Success(_) => panic!("expected 409 error response"),
    }
}

#[tokio::test]
async fn workshop_command_returns_409_when_phase_gate_blocks_transition() {
    // Covers plan2 item 10: phase-gated transitions are state conflicts. A
    // fresh workshop is in Lobby, so `beginPhase2` (which requires Handover)
    // must fail with 409, not 400.
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_success = match serde_json::from_slice::<WorkshopJoinResult>(&create_body)
        .expect("parse create result")
    {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build startPhase2 request"),
        )
        .await
        .expect("call startPhase2");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Phase 2 can only begin from handover.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn llm_judge_rejects_non_host_requests() {
    let state = test_state();
    let app = build_app(state.clone());
    let alice_cookie = test_auth_cookie(&app, "Alice").await;
    let bob_cookie = test_auth_cookie(&app, "Bob").await;

    let host = create_workshop_success(&app, &alice_cookie, "Alice").await;
    let guest = join_workshop_success(&app, &bob_cookie, &host.session_code, "Bob").await;
    progress_workshop_to_end(&app, &state, &host, &guest).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/llm/judge")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                    host.session_code, guest.reconnect_token
                )))
                .expect("build llm judge request"),
        )
        .await
        .expect("call llm judge");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read llm judge body");
    let result: LlmJudgeResult = serde_json::from_slice(&body).expect("parse llm judge result");
    match result {
        LlmJudgeResult::Error(error) => {
            assert_eq!(error.error, "Only the host can run judge scoring.");
        }
        LlmJudgeResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn llm_judge_rejects_requests_before_game_end() {
    let state = test_state();
    let app = build_app(state.clone());
    let alice_cookie = test_auth_cookie(&app, "Alice").await;
    let bob_cookie = test_auth_cookie(&app, "Bob").await;

    let host = create_workshop_success(&app, &alice_cookie, "Alice").await;
    let guest = join_workshop_success(&app, &bob_cookie, &host.session_code, "Bob").await;

    seed_selected_characters(&state, &host.session_code).await;
    for request_body in [
        setup_phase1_body(&host.session_code, &host.reconnect_token),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            host.session_code, host.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            host.session_code, host.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            host.session_code, guest.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            host.session_code, host.reconnect_token
        ),
    ] {
        send_command_ok(&app, request_body).await;
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/llm/judge")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                    host.session_code, host.reconnect_token
                )))
                .expect("build llm judge request"),
        )
        .await
        .expect("call llm judge");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read llm judge body");
    let result: LlmJudgeResult = serde_json::from_slice(&body).expect("parse llm judge result");
    match result {
        LlmJudgeResult::Error(error) => {
            assert_eq!(
                error.error,
                "Judge scoring is only available after the game ends."
            );
        }
        LlmJudgeResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn llm_judge_allows_host_after_game_end() {
    let state = test_state();
    let app = build_app(state.clone());
    let alice_cookie = test_auth_cookie(&app, "Alice").await;
    let bob_cookie = test_auth_cookie(&app, "Bob").await;

    let host = create_workshop_success(&app, &alice_cookie, "Alice").await;
    let guest = join_workshop_success(&app, &bob_cookie, &host.session_code, "Bob").await;
    progress_workshop_to_end(&app, &state, &host, &guest).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/llm/judge")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                    host.session_code, host.reconnect_token
                )))
                .expect("build llm judge request"),
        )
        .await
        .expect("call llm judge");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read llm judge body");
    let result: LlmJudgeResult = serde_json::from_slice(&body).expect("parse llm judge result");
    match result {
        LlmJudgeResult::Success(success) => {
            assert!(success.ok);
            assert!(!success.evaluation.summary.trim().is_empty());
            assert_eq!(success.evaluation.dragon_evaluations.len(), 2);
        }
        LlmJudgeResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }
}

#[tokio::test]
async fn llm_generate_image_returns_409_when_outside_phase0() {
    // Covers plan2 item 10: image generation is gated to Phase0 (character
    // creation). A fresh workshop is in Lobby, so requesting image generation
    // is a state conflict (409), not a malformed request (400).
    let app = build_app(test_state());
    let cookie = test_auth_cookie(&app, "Alice").await;
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header(axum::http::header::COOKIE, &cookie)
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_success = match serde_json::from_slice::<WorkshopJoinResult>(&create_body)
        .expect("parse create result")
    {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/llm/images")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","dragonId":"dragon-1","prompt":"a fierce dragon"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build llm image request"),
        )
        .await
        .expect("call llm image");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read llm image body");
    let result: protocol::LlmImageResult =
        serde_json::from_slice(&body).expect("parse llm image result");
    match result {
        protocol::LlmImageResult::Error(error) => {
            assert_eq!(
                error.error,
                "Image generation is only available during character creation."
            );
        }
        protocol::LlmImageResult::Success(_) => panic!("expected error response"),
    }
}

// ---------------------------------------------------------------------------
// Round B regression tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signin_accepts_empty_hero_and_defaults_to_name() {
    // Regression for orchestrator Round A ship-blocker: the SignIn UI submits
    // `hero: ""`; the server must accept it and backfill `hero` with `name`
    // so the new SignIn screen (components/sign_in.rs) is reachable.
    let state = test_state();
    let app = build_app(state);
    let body = serde_json::json!({
        "hero": "",
        "name": "Alice",
        "password": "correcthorse",
    })
    .to_string();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/signin")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("build signin request"),
        )
        .await
        .expect("signin response");
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "empty hero should be accepted as signup"
    );
    let body_bytes = to_bytes(response.into_body(), 4096)
        .await
        .expect("read signin body");
    let body_json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("signin body is JSON");
    // Hero is backfilled with the name so AccountProfile invariants hold.
    assert_eq!(
        body_json
            .pointer("/account/hero")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "Alice"
    );
}

#[tokio::test]
async fn signin_rejects_hero_longer_than_64_chars() {
    let state = test_state();
    let app = build_app(state);
    let body = serde_json::json!({
        "hero": "x".repeat(65),
        "name": "Alice",
        "password": "correcthorse",
    })
    .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/signin")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("build signin request"),
        )
        .await
        .expect("signin response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_character_is_atomic_under_concurrent_creates() {
    // Regression for orchestrator Round A V3 correctness ship-blocker: the
    // previous count+insert was not atomic; concurrent POSTs at count=4
    // could both observe 4 and insert, yielding 6 rows.
    //
    // This test fires MAX+N concurrent requests from the same account at
    // the same instant and asserts the server never allows more than MAX
    // rows through. Uses the in-memory store (serializes via RwLock write
    // guard); the Postgres path uses `SELECT ... FOR UPDATE` for the same
    // invariant (see persistence::save_character_enforcing_cap).
    use std::sync::Arc;

    let state = test_state();
    let app = build_app(state);
    let cookie = signin_and_get_cookie(&app, "knight", "Alice", "correcthorse").await;
    let app = Arc::new(app);
    let cookie = Arc::new(cookie);

    const ATTEMPTS: usize = MAX_CHARACTERS_PER_ACCOUNT + 5;
    let mut handles = Vec::with_capacity(ATTEMPTS);
    for i in 0..ATTEMPTS {
        let app = Arc::clone(&app);
        let cookie = Arc::clone(&cookie);
        handles.push(tokio::spawn(async move {
            let body = serde_json::json!({
                "description": format!("Concurrent dragon #{i}"),
                "sprites": {
                    "neutral": "base64neutral",
                    "happy": "base64happy",
                    "angry": "base64angry",
                    "sleepy": "base64sleepy"
                }
            })
            .to_string();
            let response = app
                .as_ref()
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/characters")
                        .header(axum::http::header::CONTENT_TYPE, "application/json")
                        .header(axum::http::header::COOKIE, cookie.as_str())
                        .body(Body::from(body))
                        .expect("build concurrent create request"),
                )
                .await
                .expect("concurrent create response");
            response.status()
        }));
    }

    let mut successes = 0usize;
    let mut limit_hits = 0usize;
    for handle in handles {
        match handle.await.expect("join concurrent task") {
            StatusCode::CREATED => successes += 1,
            StatusCode::BAD_REQUEST => limit_hits += 1,
            other => panic!("unexpected status {other} from concurrent create"),
        }
    }
    assert_eq!(
        successes, MAX_CHARACTERS_PER_ACCOUNT,
        "exactly MAX_CHARACTERS_PER_ACCOUNT creates should succeed under concurrency"
    );
    assert_eq!(
        limit_hits,
        ATTEMPTS - MAX_CHARACTERS_PER_ACCOUNT,
        "remaining creates should all hit the cap"
    );
}
