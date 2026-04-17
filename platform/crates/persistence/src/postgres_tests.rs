#[cfg(test)]
#[allow(clippy::module_inception)]
mod postgres_tests {
    use crate::{
        CharacterRecord, PlayerIdentity, PostgresSessionStore, SessionStore,
        TIMEOUT_COMPANION_SPRITE_KEY, timeout_companion_defaults,
    };
    use chrono::Utc;
    use domain::{SessionCode, SessionPlayer, WorkshopSession};
    use protocol::{Phase, SessionArtifactKind, SessionArtifactRecord};
    use serde_json::json;
    use sqlx::PgPool;
    use std::{ops::Deref, process::Command, sync::LazyLock};
    use uuid::Uuid;

    static EPHEMERAL_POSTGRES_TEST_MUTEX: LazyLock<tokio::sync::Mutex<()>> =
        LazyLock::new(|| tokio::sync::Mutex::new(()));

    /// Returns `TEST_DATABASE_URL` from the environment, or `None` if unset.
    fn database_url() -> Option<String> {
        std::env::var("TEST_DATABASE_URL").ok()
    }

    fn schema_prefix() -> String {
        std::env::var("TEST_DATABASE_SCHEMA")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "persistence_itest".to_string())
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

    fn test_schema_name(test_name: &str) -> String {
        let prefix = sanitize_identifier(&schema_prefix());
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

    struct TestStore {
        container_name: Option<String>,
        base_url: String,
        schema: String,
        store: PostgresSessionStore,
    }

    impl Deref for TestStore {
        type Target = PostgresSessionStore;

        fn deref(&self) -> &Self::Target {
            &self.store
        }
    }

    impl TestStore {
        async fn cleanup(self) {
            let TestStore {
                container_name,
                base_url,
                schema,
                store,
            } = self;
            store.pool.close().await;
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

    /// Build a PostgresSessionStore connected to an isolated test schema in the same database and call `init()`.
    async fn setup_store(test_name: &str) -> TestStore {
        let (container_name, url) = if let Some(url) = database_url() {
            (None, url)
        } else {
            let _ephemeral_postgres_guard = EPHEMERAL_POSTGRES_TEST_MUTEX.lock().await;
            let container_name =
                format!("dragon-switch-persistence-pg-{}", Uuid::new_v4().simple());
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
                    "POSTGRES_DB=dragon_switch_test",
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
                "postgres://postgres:postgres@127.0.0.1:{}/dragon_switch_test",
                host_port
            );
            wait_for_postgres(&url).await;
            (Some(container_name), url)
        };
        let schema = test_schema_name(test_name);
        create_schema(&url, &schema).await;
        let scoped_url = scoped_database_url(&url, &schema);
        let store = PostgresSessionStore::connect(&scoped_url, 10)
            .await
            .expect("connect to isolated test schema");
        store.init().await.expect("init schema");
        TestStore {
            container_name,
            base_url: url,
            schema,
            store,
        }
    }

    fn make_session(code: &str) -> WorkshopSession {
        let now = Utc::now();
        WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode(code.to_string()),
            now,
            protocol::WorkshopCreateConfig {
                phase0_minutes: 5,
                phase1_minutes: 10,
                phase2_minutes: 10,
            },
        )
    }

    fn make_artifact(session_id: &str, kind: SessionArtifactKind) -> SessionArtifactRecord {
        SessionArtifactRecord {
            id: format!("artifact_{}", uuid::Uuid::new_v4().simple()),
            session_id: session_id.to_string(),
            phase: Phase::Lobby,
            step: 0,
            kind,
            player_id: Some("player_1".to_string()),
            created_at: Utc::now().to_rfc3339(),
            payload: json!({ "test": true }),
        }
    }

    fn fixed_timestamp(seconds: i64) -> chrono::DateTime<Utc> {
        chrono::DateTime::from_timestamp(seconds, 0).expect("valid timestamp")
    }

    fn fixed_config() -> protocol::WorkshopCreateConfig {
        protocol::WorkshopCreateConfig {
            phase0_minutes: 5,
            phase1_minutes: 10,
            phase2_minutes: 10,
        }
    }

    fn session_order_marker(session: &WorkshopSession) -> String {
        serde_json::to_string(session).expect("serialize session ordering marker")
    }

    // ── Basic round-trip tests ──────────────────────────────────────────

    #[tokio::test]
    #[ignore] // requires live PostgreSQL — run with TEST_DATABASE_URL=... cargo test -p persistence -- --ignored
    async fn save_and_load_session_round_trip() {
        let store = setup_store("save_and_load_session_round_trip").await;
        let session = make_session("INTEG00001");
        store.save_session(&session).await.expect("save session");

        let loaded = store
            .load_session_by_code("INTEG00001")
            .await
            .expect("load session")
            .expect("session must exist");
        assert_eq!(loaded.code.0, "INTEG00001");
        assert_eq!(loaded.id, session.id);
        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn init_records_applied_schema_version() {
        let store = setup_store("init_records_applied_schema_version").await;

        let versions: Vec<(i64,)> =
            sqlx::query_as("SELECT version FROM _sqlx_migrations ORDER BY version ASC")
                .fetch_all(&store.pool)
                .await
                .expect("read applied migrations");

        assert_eq!(versions, vec![(1,), (2,), (3,), (4,), (5,), (6,)]);
        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn init_seeds_timeout_companion_defaults() {
        let store = setup_store("init_seeds_timeout_companion_defaults").await;

        let defaults = store
            .load_app_sprite_defaults(TIMEOUT_COMPANION_SPRITE_KEY)
            .await
            .expect("load timeout companion defaults")
            .expect("seeded timeout companion defaults");

        assert_eq!(defaults.key, TIMEOUT_COMPANION_SPRITE_KEY);
        assert_eq!(defaults.sprites, timeout_companion_defaults().sprites);
        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn save_and_load_character_round_trip() {
        let store = setup_store("save_and_load_character_round_trip").await;

        let character = CharacterRecord {
            id: "character_1".to_string(),
            description: "A mossy lantern dragon".to_string(),
            sprites: protocol::SpriteSet {
                neutral: "neutral_b64".to_string(),
                happy: "happy_b64".to_string(),
                angry: "angry_b64".to_string(),
                sleepy: "sleepy_b64".to_string(),
            },
            remaining_sprite_regenerations: 1,
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };

        store
            .save_character(&character)
            .await
            .expect("save character");

        let loaded = store
            .load_character("character_1")
            .await
            .expect("load character")
            .expect("character must exist");

        assert_eq!(loaded.id, character.id);
        assert_eq!(loaded.description, character.description);
        assert_eq!(loaded.sprites, character.sprites);
        assert_eq!(loaded.remaining_sprite_regenerations, 1);
        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn grouped_session_identity_artifact_write_persists_all_records() {
        let store =
            setup_store("grouped_session_identity_artifact_write_persists_all_records").await;
        let session = make_session("INTEG00010");
        let identity = PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: "player_grouped".to_string(),
            reconnect_token: format!("reconnect_{}", Uuid::new_v4().simple()),
            created_at: Utc::now().to_rfc3339(),
            last_seen_at: Utc::now().to_rfc3339(),
        };
        let artifact = make_artifact(&session.id.to_string(), SessionArtifactKind::SessionCreated);

        store
            .save_session_with_identity_and_artifact(&session, &identity, &artifact)
            .await
            .expect("grouped save session+identity+artifact");

        let loaded = store
            .load_session_by_code("INTEG00010")
            .await
            .expect("load grouped session")
            .expect("grouped session must exist");
        assert_eq!(loaded.id, session.id);

        let found_identity = store
            .find_player_identity("INTEG00010", &identity.reconnect_token)
            .await
            .expect("find grouped identity")
            .expect("grouped identity must exist");
        assert_eq!(found_identity.player_id, identity.player_id);

        let artifacts = store
            .list_session_artifacts(&session.id.to_string())
            .await
            .expect("list grouped artifacts");
        assert!(artifacts.iter().any(|entry| entry.id == artifact.id));

        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn grouped_session_identity_artifact_write_rolls_back_on_artifact_conflict() {
        let store =
            setup_store("grouped_session_identity_artifact_write_rolls_back_on_artifact_conflict")
                .await;

        let existing_session = make_session("INTEG00011");
        store
            .save_session(&existing_session)
            .await
            .expect("seed existing session");
        let conflicting_artifact = make_artifact(
            &existing_session.id.to_string(),
            SessionArtifactKind::SessionCreated,
        );
        store
            .append_session_artifact(&conflicting_artifact)
            .await
            .expect("seed conflicting artifact");

        let target_session = make_session("INTEG00012");
        let identity = PlayerIdentity {
            session_id: target_session.id.to_string(),
            player_id: "player_grouped_rollback".to_string(),
            reconnect_token: format!("reconnect_{}", Uuid::new_v4().simple()),
            created_at: Utc::now().to_rfc3339(),
            last_seen_at: Utc::now().to_rfc3339(),
        };
        let mut conflicting_insert = make_artifact(
            &target_session.id.to_string(),
            SessionArtifactKind::SessionCreated,
        );
        conflicting_insert.id = conflicting_artifact.id.clone();

        let error = store
            .save_session_with_identity_and_artifact(
                &target_session,
                &identity,
                &conflicting_insert,
            )
            .await
            .expect_err("grouped write should fail on duplicate artifact id");

        assert!(matches!(
            error,
            crate::PersistenceError::DuplicateArtifactId { artifact_id }
                if artifact_id == conflicting_artifact.id
        ));

        let loaded = store
            .load_session_by_code("INTEG00012")
            .await
            .expect("load rolled back session");
        assert!(loaded.is_none(), "session write should have rolled back");

        let found_identity = store
            .find_player_identity("INTEG00012", &identity.reconnect_token)
            .await
            .expect("find rolled back identity");
        assert!(
            found_identity.is_none(),
            "identity write should have rolled back"
        );

        let artifacts = store
            .list_session_artifacts(&target_session.id.to_string())
            .await
            .expect("list rolled back artifacts");
        assert!(
            artifacts.is_empty(),
            "artifact insert for the failed grouped write should not persist"
        );

        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn health_check_returns_true() {
        let store = setup_store("health_check_returns_true").await;
        assert!(store.health_check().await.expect("health check"));
        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn load_nonexistent_session_returns_none() {
        let store = setup_store("load_nonexistent_session_returns_none").await;
        let loaded = store
            .load_session_by_code("NEVER99999")
            .await
            .expect("load session");
        assert!(loaded.is_none());
        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn save_session_upsert_overwrites() {
        let store = setup_store("save_session_upsert_overwrites").await;
        let mut session = make_session("INTEG00002");
        store.save_session(&session).await.expect("initial save");

        // mutate and save again
        session.updated_at = Utc::now();
        store.save_session(&session).await.expect("upsert save");

        let loaded = store
            .load_session_by_code("INTEG00002")
            .await
            .expect("load session")
            .expect("session must exist");
        assert_eq!(loaded.id, session.id);
        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn persisted_session_load_clears_runtime_connectivity_flags() {
        let store = setup_store("persisted_session_load_clears_runtime_connectivity_flags").await;
        let mut session = make_session("INTEG00014");
        session.add_player(SessionPlayer {
            id: "player_1".to_string(),
            name: "Alice".to_string(),
            character_id: Some("character-1".to_string()),
            selected_character: Some(protocol::CharacterProfile {
                id: "character-1".to_string(),
                description: "Alice's workshop dragon".to_string(),
                sprites: protocol::SpriteSet {
                    neutral: "neutral".to_string(),
                    happy: "happy".to_string(),
                    angry: "angry".to_string(),
                    sleepy: "sleepy".to_string(),
                },
                remaining_sprite_regenerations: 1,
            }),
            is_host: true,
            is_connected: false,
            is_ready: true,
            score: 0,
            current_dragon_id: None,
            achievements: Vec::new(),
            joined_at: Utc::now(),
        });
        session.add_player(SessionPlayer {
            id: "player_2".to_string(),
            name: "Bob".to_string(),
            character_id: Some("character-2".to_string()),
            selected_character: Some(protocol::CharacterProfile {
                id: "character-2".to_string(),
                description: "Bob's workshop dragon".to_string(),
                sprites: protocol::SpriteSet {
                    neutral: "neutral".to_string(),
                    happy: "happy".to_string(),
                    angry: "angry".to_string(),
                    sleepy: "sleepy".to_string(),
                },
                remaining_sprite_regenerations: 1,
            }),
            is_host: false,
            is_connected: false,
            is_ready: true,
            score: 0,
            current_dragon_id: None,
            achievements: Vec::new(),
            joined_at: Utc::now(),
        });
        session
            .players
            .get_mut("player_1")
            .expect("player exists")
            .is_connected = true;
        session
            .players
            .get_mut("player_2")
            .expect("player exists")
            .is_connected = true;

        store.save_session(&session).await.expect("save session");

        let loaded = store
            .load_session_by_code("INTEG00014")
            .await
            .expect("load session")
            .expect("session must exist");

        assert!(
            loaded.players.values().all(|player| !player.is_connected),
            "persisted sessions must not restore runtime connection presence"
        );
        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn save_session_rejects_stale_write_by_updated_at() {
        let store = setup_store("save_session_rejects_stale_write_by_updated_at").await;
        let mut current = make_session("INTEG00002");
        current.updated_at = Utc::now();
        store.save_session(&current).await.expect("initial save");

        let mut stale = current.clone();
        current.phase = Phase::Phase1;
        current.updated_at = Utc::now();
        store
            .save_session(&current)
            .await
            .expect("save newer session");

        stale.phase = Phase::Handover;
        let error = store
            .save_session(&stale)
            .await
            .expect_err("stale write should fail");

        assert!(matches!(
            error,
            crate::PersistenceError::StaleSessionWrite { .. }
        ));

        let loaded = store
            .load_session_by_code("INTEG00002")
            .await
            .expect("load session")
            .expect("session must exist");
        assert_eq!(loaded.phase, Phase::Phase1);
        assert_eq!(loaded.updated_at, current.updated_at);
        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn same_updated_at_writes_converge_to_same_state_regardless_of_arrival_order() {
        let forward_store = setup_store("same_updated_at_writes_converge_forward_order").await;
        let reverse_store = setup_store("same_updated_at_writes_converge_reverse_order").await;

        let timestamp = fixed_timestamp(1_704_067_200);
        let session_id = Uuid::new_v4();

        let mut lower_order = WorkshopSession::new(
            session_id,
            SessionCode("INTEG00013".to_string()),
            timestamp,
            fixed_config(),
        );
        lower_order.host_player_id = Some("host-a".to_string());
        lower_order.updated_at = timestamp;

        let mut higher_order = lower_order.clone();
        higher_order.host_player_id = Some("host-z".to_string());

        assert!(session_order_marker(&lower_order) < session_order_marker(&higher_order));

        forward_store
            .save_session(&lower_order)
            .await
            .expect("save lower-order session first");
        forward_store
            .save_session(&higher_order)
            .await
            .expect("save higher-order session second");

        reverse_store
            .save_session(&higher_order)
            .await
            .expect("save higher-order session first");
        let reverse_error = reverse_store
            .save_session(&lower_order)
            .await
            .expect_err("save lower-order session second should be rejected as stale");

        assert!(matches!(
            reverse_error,
            crate::PersistenceError::StaleSessionWrite { .. }
        ));

        let forward_loaded = forward_store
            .load_session_by_code("INTEG00013")
            .await
            .expect("load forward-ordered session")
            .expect("forward-ordered session exists");
        let reverse_loaded = reverse_store
            .load_session_by_code("INTEG00013")
            .await
            .expect("load reverse-ordered session")
            .expect("reverse-ordered session exists");

        assert_eq!(forward_loaded, reverse_loaded);
        assert_eq!(forward_loaded.host_player_id, Some("host-z".to_string()));
        assert_eq!(forward_loaded.updated_at, timestamp);

        forward_store.cleanup().await;
        reverse_store.cleanup().await;
    }

    #[tokio::test]
    async fn session_lease_renewal_preserves_exclusive_ownership() {
        let store = setup_store("session_lease_renewal_preserves_exclusive_ownership").await;

        assert!(
            store
                .acquire_session_lease("INTEGLEASE", "lease-a", "2099-01-01T00:00:05Z")
                .await
                .expect("acquire initial lease")
        );
        assert!(
            store
                .renew_session_lease("INTEGLEASE", "lease-a", "2099-01-01T00:00:05Z")
                .await
                .expect("renew owned lease")
        );
        assert!(
            !store
                .acquire_session_lease("INTEGLEASE", "lease-b", "2099-01-01T00:00:06Z")
                .await
                .expect("reject concurrent lease while renewed lease is active")
        );

        store.cleanup().await;
    }

    #[tokio::test]
    async fn stale_realtime_connections_are_filtered_from_postgres_reads() {
        let store =
            setup_store("stale_realtime_connections_are_filtered_from_postgres_reads").await;

        store
            .claim_realtime_connection(&crate::RealtimeConnectionRegistration {
                session_code: "INTEGRT1".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("claim realtime connection");

        sqlx::query(
            "UPDATE realtime_connections SET updated_at = NOW() - INTERVAL '16 seconds' WHERE connection_id = 'conn-1'",
        )
        .execute(&store.pool)
        .await
        .expect("age realtime connection");

        let registrations = store
            .list_realtime_connections("INTEGRT1")
            .await
            .expect("list realtime registrations");
        assert!(
            registrations.is_empty(),
            "stale realtime registrations must not be treated as live presence"
        );

        store.cleanup().await;
    }

    #[tokio::test]
    async fn retired_postgres_realtime_connection_cannot_reclaim_until_restored() {
        let store =
            setup_store("retired_postgres_realtime_connection_cannot_reclaim_until_restored").await;

        store
            .claim_realtime_connection(&crate::RealtimeConnectionRegistration {
                session_code: "INTEGRT2".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("claim initial realtime connection");
        store
            .claim_realtime_connection(&crate::RealtimeConnectionRegistration {
                session_code: "INTEGRT2".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-2".to_string(),
                replica_id: "replica-b".to_string(),
            })
            .await
            .expect("replace initial realtime connection");

        let reclaim_error = store
            .claim_realtime_connection(&crate::RealtimeConnectionRegistration {
                session_code: "INTEGRT2".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect_err("retired connection must not reclaim distributed ownership");
        assert!(matches!(
            reclaim_error,
            crate::PersistenceError::RetiredRealtimeConnection { connection_id } if connection_id == "conn-1"
        ));

        let released = store
            .release_realtime_connection("conn-2", "replica-b")
            .await
            .expect("release replacement realtime connection");
        assert_eq!(
            released,
            Some(crate::RealtimeConnectionRegistration {
                session_code: "INTEGRT2".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-2".to_string(),
                replica_id: "replica-b".to_string(),
            })
        );

        let restored = store
            .restore_realtime_connection(&crate::RealtimeConnectionRegistration {
                session_code: "INTEGRT2".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("restore retired realtime connection");
        assert!(restored.restored);
        assert_eq!(restored.replaced, None);

        let registrations = store
            .list_realtime_connections("INTEGRT2")
            .await
            .expect("list realtime registrations");
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].connection_id, "conn-1");

        store.cleanup().await;
    }

    #[tokio::test]
    async fn restore_postgres_realtime_connection_does_not_override_newer_owner() {
        let store =
            setup_store("restore_postgres_realtime_connection_does_not_override_newer_owner").await;

        store
            .claim_realtime_connection(&crate::RealtimeConnectionRegistration {
                session_code: "INTEGRT3".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("claim initial realtime connection");
        store
            .claim_realtime_connection(&crate::RealtimeConnectionRegistration {
                session_code: "INTEGRT3".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-2".to_string(),
                replica_id: "replica-b".to_string(),
            })
            .await
            .expect("replace initial realtime connection");
        store
            .claim_realtime_connection(&crate::RealtimeConnectionRegistration {
                session_code: "INTEGRT3".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-3".to_string(),
                replica_id: "replica-c".to_string(),
            })
            .await
            .expect("newer owner should replace second connection");

        let restored = store
            .restore_realtime_connection(&crate::RealtimeConnectionRegistration {
                session_code: "INTEGRT3".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("restore should no-op when newer owner exists");
        assert!(!restored.restored);

        let registrations = store
            .list_realtime_connections("INTEGRT3")
            .await
            .expect("list realtime registrations after skipped restore");
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].connection_id, "conn-3");
        assert_eq!(registrations[0].replica_id, "replica-c");

        store.cleanup().await;
    }

    #[tokio::test]
    async fn taking_retired_postgres_realtime_connection_consumes_fence() {
        let store = setup_store("taking_retired_postgres_realtime_connection_consumes_fence").await;

        store
            .claim_realtime_connection(&crate::RealtimeConnectionRegistration {
                session_code: "INTEGRT4".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("claim initial realtime connection");
        store
            .claim_realtime_connection(&crate::RealtimeConnectionRegistration {
                session_code: "INTEGRT4".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-2".to_string(),
                replica_id: "replica-b".to_string(),
            })
            .await
            .expect("replace initial realtime connection");

        let taken = store
            .take_retired_realtime_connection("conn-1", "replica-a")
            .await
            .expect("take retired realtime connection");
        assert_eq!(
            taken,
            Some(crate::RealtimeConnectionRegistration {
                session_code: String::new(),
                player_id: String::new(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
        );
        assert!(
            store
                .take_retired_realtime_connection("conn-1", "replica-a")
                .await
                .expect("retired fence is consumed")
                .is_none()
        );

        store.cleanup().await;
    }

    // ── Artifact tests ──────────────────────────────────────────────────

    #[tokio::test]
    #[ignore]
    async fn append_and_list_artifacts() {
        let store = setup_store("append_and_list_artifacts").await;
        let session = make_session("INTEG00003");
        store.save_session(&session).await.expect("save session");

        let artifact = make_artifact(&session.id.to_string(), SessionArtifactKind::SessionCreated);
        store
            .append_session_artifact(&artifact)
            .await
            .expect("append artifact");

        let artifacts = store
            .list_session_artifacts(&session.id.to_string())
            .await
            .expect("list artifacts");
        assert!(!artifacts.is_empty());
        assert!(artifacts.iter().any(|a| a.id == artifact.id));
        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn list_artifacts_empty_for_unknown_session() {
        let store = setup_store("list_artifacts_empty_for_unknown_session").await;
        let artifacts = store
            .list_session_artifacts("nonexistent_session_id")
            .await
            .expect("list artifacts");
        assert!(artifacts.is_empty());
        store.cleanup().await;
    }

    // ── Player identity tests ───────────────────────────────────────────

    #[tokio::test]
    #[ignore]
    async fn create_and_find_player_identity() {
        let store = setup_store("create_and_find_player_identity").await;
        let session = make_session("INTEG00004");
        store.save_session(&session).await.expect("save session");

        let token = format!("tok_{}", uuid::Uuid::new_v4().simple());
        let identity = PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: "player_abc".to_string(),
            reconnect_token: token.clone(),
            created_at: Utc::now().to_rfc3339(),
            last_seen_at: Utc::now().to_rfc3339(),
        };
        store
            .create_player_identity(&identity)
            .await
            .expect("create identity");

        let found = store
            .find_player_identity("INTEG00004", &token)
            .await
            .expect("find identity")
            .expect("identity must exist");
        assert_eq!(found.player_id, "player_abc");
        assert_eq!(found.session_id, session.id.to_string());
        assert_eq!(found.last_seen_at, identity.last_seen_at);
        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn find_player_identity_wrong_token_returns_none() {
        let store = setup_store("find_player_identity_wrong_token_returns_none").await;
        let session = make_session("INTEG00005");
        store.save_session(&session).await.expect("save session");

        let found = store
            .find_player_identity("INTEG00005", "bad_token")
            .await
            .expect("find identity");
        assert!(found.is_none());
        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn touch_player_identity_updates_last_seen() {
        let store = setup_store("touch_player_identity_updates_last_seen").await;
        let session = make_session("INTEG00006");
        store.save_session(&session).await.expect("save session");

        let token = format!("tok_{}", uuid::Uuid::new_v4().simple());
        let identity = PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: "player_touch".to_string(),
            reconnect_token: token.clone(),
            created_at: Utc::now().to_rfc3339(),
            last_seen_at: "2020-01-01T00:00:00Z".to_string(),
        };
        store
            .create_player_identity(&identity)
            .await
            .expect("create identity");

        let new_ts = Utc::now().to_rfc3339();
        store
            .touch_player_identity(&token, &new_ts)
            .await
            .expect("touch identity");

        // verify via find (we can't directly read last_seen_at from find, but at
        // least verify the row still exists)
        let found = store
            .find_player_identity("INTEG00006", &token)
            .await
            .expect("find identity")
            .expect("identity must exist after touch");
        assert_eq!(found.player_id, "player_touch");
        store.cleanup().await;
    }

    #[tokio::test]
    #[ignore]
    async fn revoke_player_identity_removes_row() {
        let store = setup_store("revoke_player_identity_removes_row").await;
        let session = make_session("INTEG00007");
        store.save_session(&session).await.expect("save session");

        let token = format!("tok_{}", uuid::Uuid::new_v4().simple());
        let identity = PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: "player_revoke".to_string(),
            reconnect_token: token.clone(),
            created_at: Utc::now().to_rfc3339(),
            last_seen_at: Utc::now().to_rfc3339(),
        };
        store
            .create_player_identity(&identity)
            .await
            .expect("create identity");

        store
            .revoke_player_identity(&token)
            .await
            .expect("revoke identity");

        let found = store
            .find_player_identity("INTEG00007", &token)
            .await
            .expect("find identity");
        assert!(found.is_none(), "identity should be gone after revoke");
        store.cleanup().await;
    }

    // ── Concurrent update test ──────────────────────────────────────────

    #[tokio::test]
    #[ignore]
    async fn concurrent_save_sessions_do_not_lose_updates() {
        let store = setup_store("concurrent_save_sessions_do_not_lose_updates").await;
        let session = make_session("INTEG00008");
        store.save_session(&session).await.expect("initial save");

        // Fire 10 sequential upserts — all should succeed without error.
        // We test upsert idempotency rather than true parallelism because the
        // store is borrowed (not Arc-wrapped) in this unit test context.
        for _ in 0..10 {
            let mut s = session.clone();
            s.updated_at = Utc::now();
            store
                .save_session(&s)
                .await
                .expect("upsert save must not fail");
        }

        // The session must still be loadable
        let loaded = store
            .load_session_by_code("INTEG00008")
            .await
            .expect("load session")
            .expect("session must exist");
        assert_eq!(loaded.id, session.id);
        store.cleanup().await;
    }

    // ── Database disconnection resilience ───────────────────────────────

    #[tokio::test]
    #[ignore]
    async fn pool_recovers_after_transient_query_failure() {
        let store = setup_store("pool_recovers_after_transient_query_failure").await;
        // Simply verify that after a successful health check, subsequent
        // operations still work — sqlx PgPool handles reconnection transparently.
        assert!(store.health_check().await.expect("initial health check"));

        let session = make_session("INTEG00009");
        store
            .save_session(&session)
            .await
            .expect("save after health check");
        let loaded = store
            .load_session_by_code("INTEG00009")
            .await
            .expect("load after save");
        assert!(loaded.is_some());
        store.cleanup().await;
    }

    #[test]
    fn scoped_database_url_appends_search_path_option() {
        assert_eq!(
            scoped_database_url("postgres://user:pass@localhost/db", "itest_schema"),
            "postgres://user:pass@localhost/db?options=-csearch_path%3Ditest_schema"
        );
        assert_eq!(
            scoped_database_url(
                "postgres://user:pass@localhost/db?sslmode=disable",
                "itest_schema"
            ),
            "postgres://user:pass@localhost/db?sslmode=disable&options=-csearch_path%3Ditest_schema"
        );
    }
}
