use domain::WorkshopSession;
use protocol::SessionArtifactRecord;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::RwLock;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("session store lock poisoned")]
    LockPoisoned,
    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerIdentity {
    pub session_id: String,
    pub player_id: String,
    pub reconnect_token: String,
    pub created_at: String,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerIdentityMatch {
    pub session_id: String,
    pub player_id: String,
}

pub trait SessionStore: Send + Sync {
    fn init(&self) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn health_check(&self) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>>;
    fn load_session_by_code(
        &self,
        session_code: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<WorkshopSession>, PersistenceError>> + Send + '_>>;
    fn save_session(
        &self,
        session: &WorkshopSession,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn append_session_artifact(
        &self,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn list_session_artifacts(
        &self,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionArtifactRecord>, PersistenceError>> + Send + '_>>;
    fn create_player_identity(
        &self,
        identity: &PlayerIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn find_player_identity(
        &self,
        session_code: &str,
        reconnect_token: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<PlayerIdentityMatch>, PersistenceError>> + Send + '_>>;
    fn touch_player_identity(
        &self,
        reconnect_token: &str,
        last_seen_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn revoke_player_identity(
        &self,
        reconnect_token: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
}

#[derive(Debug, Default)]
pub struct InMemorySessionStore {
    sessions_by_code: RwLock<HashMap<String, WorkshopSession>>,
    sessions_by_id: RwLock<HashMap<String, WorkshopSession>>,
    artifacts_by_session_id: RwLock<HashMap<String, Vec<SessionArtifactRecord>>>,
    identities_by_token: RwLock<HashMap<String, PlayerIdentity>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

pub struct PostgresSessionStore {
    pool: sqlx::PgPool,
}

impl PostgresSessionStore {
    pub async fn connect(database_url: &str) -> Result<Self, PersistenceError> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }
}

impl SessionStore for PostgresSessionStore {
    fn init(&self) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        Box::pin(async move {
            sqlx::raw_sql(
                "
                CREATE TABLE IF NOT EXISTS workshop_sessions (
                    session_id TEXT PRIMARY KEY,
                    session_code TEXT UNIQUE NOT NULL,
                    payload JSONB NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_workshop_sessions_code ON workshop_sessions(session_code);

                CREATE TABLE IF NOT EXISTS session_artifacts (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    payload JSONB NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_session_artifacts_session_created
                    ON session_artifacts(session_id, created_at, id);

                CREATE TABLE IF NOT EXISTS player_identities (
                    reconnect_token TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    player_id TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    last_seen_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_player_identities_session_id ON player_identities(session_id);
                ",
            )
            .execute(&self.pool)
            .await?;
            Ok(())
        })
    }

    fn health_check(&self) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        Box::pin(async move {
            sqlx::query("SELECT 1").fetch_one(&self.pool).await?;
            Ok(true)
        })
    }

    fn load_session_by_code(
        &self,
        session_code: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<WorkshopSession>, PersistenceError>> + Send + '_>> {
        let session_code = session_code.to_string();
        Box::pin(async move {
            let row = sqlx::query("SELECT payload FROM workshop_sessions WHERE session_code = $1")
                .bind(&session_code)
                .fetch_optional(&self.pool)
                .await?;
            let Some(row) = row else {
                return Ok(None);
            };
            use sqlx::Row;
            let payload: sqlx::types::Json<serde_json::Value> = row.get("payload");
            let session = serde_json::from_value(payload.0)?;
            Ok(Some(session))
        })
    }

    fn save_session(
        &self,
        session: &WorkshopSession,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let session = session.clone();
        Box::pin(async move {
            let payload = serde_json::to_value(&session)?;
            let mut tx = self.pool.begin().await?;
            sqlx::query(
                "
                INSERT INTO workshop_sessions (session_id, session_code, payload, updated_at)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (session_id) DO UPDATE SET
                    session_code = EXCLUDED.session_code,
                    payload = EXCLUDED.payload,
                    updated_at = EXCLUDED.updated_at
                ",
            )
            .bind(&session.id.to_string())
            .bind(&session.code.0)
            .bind(sqlx::types::Json(&payload))
            .bind(&session.updated_at.to_rfc3339())
            .execute(&mut *tx)
            .await?;

            sqlx::query("SELECT pg_notify('session_updates', $1)")
                .bind(&session.code.0)
                .execute(&mut *tx)
                .await?;

            tx.commit().await?;
            Ok(())
        })
    }

    fn append_session_artifact(
        &self,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let artifact = artifact.clone();
        Box::pin(async move {
            let payload = serde_json::to_value(&artifact)?;
            sqlx::query(
                "
                INSERT INTO session_artifacts (id, session_id, created_at, payload)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (id) DO UPDATE SET
                    session_id = EXCLUDED.session_id,
                    created_at = EXCLUDED.created_at,
                    payload = EXCLUDED.payload
                ",
            )
            .bind(&artifact.id)
            .bind(&artifact.session_id)
            .bind(&artifact.created_at)
            .bind(sqlx::types::Json(&payload))
            .execute(&self.pool)
            .await?;
            Ok(())
        })
    }

    fn list_session_artifacts(
        &self,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionArtifactRecord>, PersistenceError>> + Send + '_>> {
        let session_id = session_id.to_string();
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT payload FROM session_artifacts WHERE session_id = $1 ORDER BY created_at ASC, id ASC",
            )
            .bind(&session_id)
            .fetch_all(&self.pool)
            .await?;
            rows.into_iter()
                .map(|row| {
                    use sqlx::Row;
                    let payload: sqlx::types::Json<serde_json::Value> = row.get("payload");
                    serde_json::from_value(payload.0).map_err(PersistenceError::from)
                })
                .collect()
        })
    }

    fn create_player_identity(
        &self,
        identity: &PlayerIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let identity = identity.clone();
        Box::pin(async move {
            sqlx::query(
                "
                INSERT INTO player_identities (reconnect_token, session_id, player_id, created_at, last_seen_at)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (reconnect_token) DO UPDATE SET
                    session_id = EXCLUDED.session_id,
                    player_id = EXCLUDED.player_id,
                    created_at = EXCLUDED.created_at,
                    last_seen_at = EXCLUDED.last_seen_at
                ",
            )
            .bind(&identity.reconnect_token)
            .bind(&identity.session_id)
            .bind(&identity.player_id)
            .bind(&identity.created_at)
            .bind(&identity.last_seen_at)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
    }

    fn find_player_identity(
        &self,
        session_code: &str,
        reconnect_token: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<PlayerIdentityMatch>, PersistenceError>> + Send + '_>> {
        let session_code = session_code.to_string();
        let reconnect_token = reconnect_token.to_string();
        Box::pin(async move {
            let row = sqlx::query(
                "
                SELECT identities.session_id, identities.player_id
                FROM player_identities identities
                INNER JOIN workshop_sessions sessions ON sessions.session_id = identities.session_id
                WHERE identities.reconnect_token = $1 AND sessions.session_code = $2
                ",
            )
            .bind(&reconnect_token)
            .bind(&session_code)
            .fetch_optional(&self.pool)
            .await?;
            use sqlx::Row;
            Ok(row.map(|row| PlayerIdentityMatch {
                session_id: row.get("session_id"),
                player_id: row.get("player_id"),
            }))
        })
    }

    fn touch_player_identity(
        &self,
        reconnect_token: &str,
        last_seen_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let reconnect_token = reconnect_token.to_string();
        let last_seen_at = last_seen_at.to_string();
        Box::pin(async move {
            sqlx::query("UPDATE player_identities SET last_seen_at = $2 WHERE reconnect_token = $1")
                .bind(&reconnect_token)
                .bind(&last_seen_at)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
    }

    fn revoke_player_identity(
        &self,
        reconnect_token: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let reconnect_token = reconnect_token.to_string();
        Box::pin(async move {
            sqlx::query("DELETE FROM player_identities WHERE reconnect_token = $1")
                .bind(&reconnect_token)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
    }
}

impl SessionStore for InMemorySessionStore {
    fn init(&self) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        Box::pin(async move { Ok(()) })
    }

    fn health_check(&self) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        Box::pin(async move { Ok(true) })
    }

    fn load_session_by_code(
        &self,
        session_code: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<WorkshopSession>, PersistenceError>> + Send + '_>> {
        let session_code = session_code.to_string();
        Box::pin(async move {
            let guard = self
                .sessions_by_code
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            Ok(guard.get(&session_code).cloned())
        })
    }

    fn save_session(
        &self,
        session: &WorkshopSession,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let session = session.clone();
        Box::pin(async move {
            {
                let mut sessions_by_code = self
                    .sessions_by_code
                    .write()
                    .map_err(|_| PersistenceError::LockPoisoned)?;
                if let Some(previous) = sessions_by_code.insert(session.code.0.clone(), session.clone()) {
                    let mut sessions_by_id = self
                        .sessions_by_id
                        .write()
                        .map_err(|_| PersistenceError::LockPoisoned)?;
                    sessions_by_id.remove(&previous.id.to_string());
                    sessions_by_id.insert(session.id.to_string(), session.clone());
                    return Ok(());
                }
            }

            let mut sessions_by_id = self
                .sessions_by_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            sessions_by_id.insert(session.id.to_string(), session.clone());
            Ok(())
        })
    }

    fn append_session_artifact(
        &self,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let artifact = artifact.clone();
        Box::pin(async move {
            let mut guard = self
                .artifacts_by_session_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let artifacts = guard.entry(artifact.session_id.clone()).or_default();
            artifacts.push(artifact);
            Ok(())
        })
    }

    fn list_session_artifacts(
        &self,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionArtifactRecord>, PersistenceError>> + Send + '_>> {
        let session_id = session_id.to_string();
        Box::pin(async move {
            let guard = self
                .artifacts_by_session_id
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            Ok(guard.get(&session_id).cloned().unwrap_or_default())
        })
    }

    fn create_player_identity(
        &self,
        identity: &PlayerIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let identity = identity.clone();
        Box::pin(async move {
            let mut guard = self
                .identities_by_token
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            guard.insert(identity.reconnect_token.clone(), identity);
            Ok(())
        })
    }

    fn find_player_identity(
        &self,
        session_code: &str,
        reconnect_token: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<PlayerIdentityMatch>, PersistenceError>> + Send + '_>> {
        let session_code = session_code.to_string();
        let reconnect_token = reconnect_token.to_string();
        Box::pin(async move {
            let identity = {
                let identities = self
                    .identities_by_token
                    .read()
                    .map_err(|_| PersistenceError::LockPoisoned)?;
                identities.get(&reconnect_token).cloned()
            };

            let Some(identity) = identity else {
                return Ok(None);
            };

            let sessions = self
                .sessions_by_id
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let Some(session) = sessions.get(&identity.session_id) else {
                return Ok(None);
            };
            if session.code.0 != session_code {
                return Ok(None);
            }

            Ok(Some(PlayerIdentityMatch {
                session_id: identity.session_id,
                player_id: identity.player_id,
            }))
        })
    }

    fn touch_player_identity(
        &self,
        reconnect_token: &str,
        last_seen_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let reconnect_token = reconnect_token.to_string();
        let last_seen_at = last_seen_at.to_string();
        Box::pin(async move {
            let mut guard = self
                .identities_by_token
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            if let Some(identity) = guard.get_mut(&reconnect_token) {
                identity.last_seen_at = last_seen_at;
            }
            Ok(())
        })
    }

    fn revoke_player_identity(
        &self,
        reconnect_token: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let reconnect_token = reconnect_token.to_string();
        Box::pin(async move {
            let mut guard = self
                .identities_by_token
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            guard.remove(&reconnect_token);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use domain::{SessionCode, WorkshopSession};
    use protocol::{Phase, SessionArtifactKind};
    use serde_json::json;
    use uuid::Uuid;

    fn ts(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).expect("valid timestamp")
    }

    fn session(code: &str, phase: Phase, updated_at_seconds: i64) -> WorkshopSession {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode(code.to_string()), ts(updated_at_seconds));
        session.phase = phase;
        session.updated_at = ts(updated_at_seconds);
        session
    }

    fn artifact(session_id: &str, id: &str, created_at: &str) -> SessionArtifactRecord {
        SessionArtifactRecord {
            id: id.to_string(),
            session_id: session_id.to_string(),
            phase: Phase::Lobby,
            step: 0,
            kind: SessionArtifactKind::SessionCreated,
            player_id: None,
            created_at: created_at.to_string(),
            payload: json!({ "id": id }),
        }
    }

    fn identity(session_id: &str, player_id: &str, reconnect_token: &str) -> PlayerIdentity {
        PlayerIdentity {
            session_id: session_id.to_string(),
            player_id: player_id.to_string(),
            reconnect_token: reconnect_token.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_seen_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[tokio::test]
    async fn load_missing_session_returns_none() {
        let store = InMemorySessionStore::new();

        let session = store.load_session_by_code("missing").await.expect("load missing session");

        assert_eq!(session, None);
    }

    #[tokio::test]
    async fn save_and_load_session_roundtrip() {
        let store = InMemorySessionStore::new();
        let saved = session("123456", Phase::Lobby, 1);

        store.save_session(&saved).await.expect("save session");
        let loaded = store
            .load_session_by_code("123456")
            .await
            .expect("load session")
            .expect("session exists");

        assert_eq!(loaded, saved);
    }

    #[tokio::test]
    async fn save_session_overwrites_existing_session_by_code() {
        let store = InMemorySessionStore::new();
        let first = session("123456", Phase::Lobby, 1);
        let second = session("123456", Phase::Phase1, 2);

        store.save_session(&first).await.expect("save first session");
        store.save_session(&second).await.expect("save second session");
        let loaded = store
            .load_session_by_code("123456")
            .await
            .expect("load session")
            .expect("session exists");

        assert_eq!(loaded.phase, Phase::Phase1);
        assert_eq!(loaded.updated_at, ts(2));
    }

    #[tokio::test]
    async fn health_check_is_true_for_memory_store() {
        let store = InMemorySessionStore::new();

        let health = store.health_check().await.expect("health check");

        assert!(health);
    }

    #[tokio::test]
    async fn appended_artifacts_are_listed_in_append_order() {
        let store = InMemorySessionStore::new();
        store
            .append_session_artifact(&artifact("session-1", "a1", "2026-01-01T00:00:00Z"))
            .await
            .expect("append first artifact");
        store
            .append_session_artifact(&artifact("session-1", "a2", "2026-01-01T00:00:01Z"))
            .await
            .expect("append second artifact");

        let artifacts = store
            .list_session_artifacts("session-1")
            .await
            .expect("list artifacts");

        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].id, "a1");
        assert_eq!(artifacts[1].id, "a2");
    }

    #[tokio::test]
    async fn find_player_identity_returns_none_for_missing_token() {
        let store = InMemorySessionStore::new();
        let session = session("123456", Phase::Lobby, 1);
        store.save_session(&session).await.expect("save session");

        let found = store
            .find_player_identity("123456", "missing-token")
            .await
            .expect("find identity");

        assert_eq!(found, None);
    }

    #[tokio::test]
    async fn revoke_player_identity_removes_existing_identity() {
        let store = InMemorySessionStore::new();
        let session = session("123456", Phase::Lobby, 1);
        store.save_session(&session).await.expect("save session");
        store
            .create_player_identity(&identity(&session.id.to_string(), "player-1", "token-1"))
            .await
            .expect("create identity");
        store
            .revoke_player_identity("token-1")
            .await
            .expect("revoke identity");

        let found = store
            .find_player_identity("123456", "token-1")
            .await
            .expect("find identity");

        assert_eq!(found, None);
    }

    #[tokio::test]
    async fn touch_player_identity_updates_last_seen_without_changing_match() {
        let store = InMemorySessionStore::new();
        let session = session("123456", Phase::Lobby, 1);
        store.save_session(&session).await.expect("save session");
        store
            .create_player_identity(&identity(&session.id.to_string(), "player-1", "token-1"))
            .await
            .expect("create identity");
        store
            .touch_player_identity("token-1", "2026-01-01T00:00:05Z")
            .await
            .expect("touch identity");

        let found = store
            .find_player_identity("123456", "token-1")
            .await
            .expect("find identity");

        assert_eq!(
            found,
            Some(PlayerIdentityMatch {
                session_id: session.id.to_string(),
                player_id: "player-1".to_string(),
            })
        );
    }
}
