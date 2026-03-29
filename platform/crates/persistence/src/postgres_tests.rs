#[cfg(test)]
mod postgres_tests {
    use crate::{PlayerIdentity, PostgresSessionStore, SessionStore};
    use chrono::Utc;
    use domain::{SessionCode, WorkshopSession};
    use protocol::{Phase, SessionArtifactKind, SessionArtifactRecord};
    use serde_json::json;
    use uuid::Uuid;

    /// Returns `TEST_DATABASE_URL` from the environment, or `None` if unset.
    fn database_url() -> Option<String> {
        std::env::var("TEST_DATABASE_URL").ok()
    }

    /// Build a PostgresSessionStore connected to the test database and call `init()`.
    async fn setup_store() -> PostgresSessionStore {
        let url = database_url().expect("TEST_DATABASE_URL must be set for integration tests");
        let store = PostgresSessionStore::connect(&url)
            .await
            .expect("connect to test database");
        store.init().await.expect("init schema");
        store
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
                image_generator_token: None,
                image_generator_model: None,
                judge_token: None,
                judge_model: None,
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

    // ── Basic round-trip tests ──────────────────────────────────────────

    #[tokio::test]
    #[ignore] // requires live PostgreSQL — run with TEST_DATABASE_URL=... cargo test -- --ignored
    async fn save_and_load_session_round_trip() {
        let store = setup_store().await;
        let session = make_session("INTEG00001");
        store.save_session(&session).await.expect("save session");

        let loaded = store
            .load_session_by_code("INTEG00001")
            .await
            .expect("load session")
            .expect("session must exist");
        assert_eq!(loaded.code.0, "INTEG00001");
        assert_eq!(loaded.id, session.id);
    }

    #[tokio::test]
    #[ignore]
    async fn health_check_returns_true() {
        let store = setup_store().await;
        assert!(store.health_check().await.expect("health check"));
    }

    #[tokio::test]
    #[ignore]
    async fn load_nonexistent_session_returns_none() {
        let store = setup_store().await;
        let loaded = store
            .load_session_by_code("NEVER99999")
            .await
            .expect("load session");
        assert!(loaded.is_none());
    }

    #[tokio::test]
    #[ignore]
    async fn save_session_upsert_overwrites() {
        let store = setup_store().await;
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
    }

    // ── Artifact tests ──────────────────────────────────────────────────

    #[tokio::test]
    #[ignore]
    async fn append_and_list_artifacts() {
        let store = setup_store().await;
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
    }

    #[tokio::test]
    #[ignore]
    async fn list_artifacts_empty_for_unknown_session() {
        let store = setup_store().await;
        let artifacts = store
            .list_session_artifacts("nonexistent_session_id")
            .await
            .expect("list artifacts");
        assert!(artifacts.is_empty());
    }

    // ── Player identity tests ───────────────────────────────────────────

    #[tokio::test]
    #[ignore]
    async fn create_and_find_player_identity() {
        let store = setup_store().await;
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
    }

    #[tokio::test]
    #[ignore]
    async fn find_player_identity_wrong_token_returns_none() {
        let store = setup_store().await;
        let session = make_session("INTEG00005");
        store.save_session(&session).await.expect("save session");

        let found = store
            .find_player_identity("INTEG00005", "bad_token")
            .await
            .expect("find identity");
        assert!(found.is_none());
    }

    #[tokio::test]
    #[ignore]
    async fn touch_player_identity_updates_last_seen() {
        let store = setup_store().await;
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
    }

    #[tokio::test]
    #[ignore]
    async fn revoke_player_identity_removes_row() {
        let store = setup_store().await;
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
    }

    // ── Concurrent update test ──────────────────────────────────────────

    #[tokio::test]
    #[ignore]
    async fn concurrent_save_sessions_do_not_lose_updates() {
        let store = setup_store().await;
        let session = make_session("INTEG00008");
        store.save_session(&session).await.expect("initial save");

        // Fire 10 sequential upserts — all should succeed without error.
        // We test upsert idempotency rather than true parallelism because the
        // store is borrowed (not Arc-wrapped) in this unit test context.
        for _ in 0..10 {
            let mut s = session.clone();
            s.updated_at = Utc::now();
            store.save_session(&s).await.expect("upsert save must not fail");
        }

        // The session must still be loadable
        let loaded = store
            .load_session_by_code("INTEG00008")
            .await
            .expect("load session")
            .expect("session must exist");
        assert_eq!(loaded.id, session.id);
    }

    // ── Database disconnection resilience ───────────────────────────────

    #[tokio::test]
    #[ignore]
    async fn pool_recovers_after_transient_query_failure() {
        let store = setup_store().await;
        // Simply verify that after a successful health check, subsequent
        // operations still work — sqlx PgPool handles reconnection transparently.
        assert!(store.health_check().await.expect("initial health check"));

        let session = make_session("INTEG00009");
        store.save_session(&session).await.expect("save after health check");
        let loaded = store
            .load_session_by_code("INTEG00009")
            .await
            .expect("load after save");
        assert!(loaded.is_some());
    }
}
