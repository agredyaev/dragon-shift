use domain::SessionSummary;
use protocol::SessionArtifactRecord;
use std::collections::HashMap;
use std::sync::RwLock;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("session store lock poisoned")]
    LockPoisoned,
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
    fn init(&self) -> Result<(), PersistenceError>;
    fn health_check(&self) -> Result<bool, PersistenceError>;
    fn load_session_by_code(&self, session_code: &str) -> Result<Option<SessionSummary>, PersistenceError>;
    fn save_session(&self, session: &SessionSummary) -> Result<(), PersistenceError>;
    fn append_session_artifact(&self, artifact: &SessionArtifactRecord) -> Result<(), PersistenceError>;
    fn list_session_artifacts(&self, session_id: &str) -> Result<Vec<SessionArtifactRecord>, PersistenceError>;
    fn create_player_identity(&self, identity: &PlayerIdentity) -> Result<(), PersistenceError>;
    fn find_player_identity(
        &self,
        session_code: &str,
        reconnect_token: &str,
    ) -> Result<Option<PlayerIdentityMatch>, PersistenceError>;
    fn touch_player_identity(&self, reconnect_token: &str, last_seen_at: &str) -> Result<(), PersistenceError>;
    fn revoke_player_identity(&self, reconnect_token: &str) -> Result<(), PersistenceError>;
}

#[derive(Debug, Default)]
pub struct InMemorySessionStore {
    sessions_by_code: RwLock<HashMap<String, SessionSummary>>,
    sessions_by_id: RwLock<HashMap<String, SessionSummary>>,
    artifacts_by_session_id: RwLock<HashMap<String, Vec<SessionArtifactRecord>>>,
    identities_by_token: RwLock<HashMap<String, PlayerIdentity>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SessionStore for InMemorySessionStore {
    fn init(&self) -> Result<(), PersistenceError> {
        Ok(())
    }

    fn health_check(&self) -> Result<bool, PersistenceError> {
        Ok(true)
    }

    fn load_session_by_code(&self, session_code: &str) -> Result<Option<SessionSummary>, PersistenceError> {
        let guard = self
            .sessions_by_code
            .read()
            .map_err(|_| PersistenceError::LockPoisoned)?;
        Ok(guard.get(session_code).cloned())
    }

    fn save_session(&self, session: &SessionSummary) -> Result<(), PersistenceError> {
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
    }

    fn append_session_artifact(&self, artifact: &SessionArtifactRecord) -> Result<(), PersistenceError> {
        let mut guard = self
            .artifacts_by_session_id
            .write()
            .map_err(|_| PersistenceError::LockPoisoned)?;
        let artifacts = guard.entry(artifact.session_id.clone()).or_default();
        artifacts.push(artifact.clone());
        Ok(())
    }

    fn list_session_artifacts(&self, session_id: &str) -> Result<Vec<SessionArtifactRecord>, PersistenceError> {
        let guard = self
            .artifacts_by_session_id
            .read()
            .map_err(|_| PersistenceError::LockPoisoned)?;
        Ok(guard.get(session_id).cloned().unwrap_or_default())
    }

    fn create_player_identity(&self, identity: &PlayerIdentity) -> Result<(), PersistenceError> {
        let mut guard = self
            .identities_by_token
            .write()
            .map_err(|_| PersistenceError::LockPoisoned)?;
        guard.insert(identity.reconnect_token.clone(), identity.clone());
        Ok(())
    }

    fn find_player_identity(
        &self,
        session_code: &str,
        reconnect_token: &str,
    ) -> Result<Option<PlayerIdentityMatch>, PersistenceError> {
        let identity = {
            let identities = self
                .identities_by_token
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            identities.get(reconnect_token).cloned()
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
    }

    fn touch_player_identity(&self, reconnect_token: &str, last_seen_at: &str) -> Result<(), PersistenceError> {
        let mut guard = self
            .identities_by_token
            .write()
            .map_err(|_| PersistenceError::LockPoisoned)?;
        if let Some(identity) = guard.get_mut(reconnect_token) {
            identity.last_seen_at = last_seen_at.to_string();
        }
        Ok(())
    }

    fn revoke_player_identity(&self, reconnect_token: &str) -> Result<(), PersistenceError> {
        let mut guard = self
            .identities_by_token
            .write()
            .map_err(|_| PersistenceError::LockPoisoned)?;
        guard.remove(reconnect_token);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use domain::SessionCode;
    use protocol::{Phase, SessionArtifactKind};
    use serde_json::json;
    use uuid::Uuid;

    fn ts(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).expect("valid timestamp")
    }

    fn summary(code: &str, phase: Phase, updated_at_seconds: i64) -> SessionSummary {
        SessionSummary {
            id: Uuid::new_v4(),
            code: SessionCode(code.to_string()),
            phase,
            updated_at: ts(updated_at_seconds),
        }
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

    #[test]
    fn load_missing_session_returns_none() {
        let store = InMemorySessionStore::new();

        let session = store.load_session_by_code("missing").expect("load missing session");

        assert_eq!(session, None);
    }

    #[test]
    fn save_and_load_session_roundtrip() {
        let store = InMemorySessionStore::new();
        let saved = summary("123456", Phase::Lobby, 1);

        store.save_session(&saved).expect("save session");
        let loaded = store
            .load_session_by_code("123456")
            .expect("load session")
            .expect("session exists");

        assert_eq!(loaded, saved);
    }

    #[test]
    fn save_session_overwrites_existing_session_by_code() {
        let store = InMemorySessionStore::new();
        let first = summary("123456", Phase::Lobby, 1);
        let second = summary("123456", Phase::Phase1, 2);

        store.save_session(&first).expect("save first session");
        store.save_session(&second).expect("save second session");
        let loaded = store
            .load_session_by_code("123456")
            .expect("load session")
            .expect("session exists");

        assert_eq!(loaded.phase, Phase::Phase1);
        assert_eq!(loaded.updated_at, ts(2));
    }

    #[test]
    fn health_check_is_true_for_memory_store() {
        let store = InMemorySessionStore::new();

        let health = store.health_check().expect("health check");

        assert!(health);
    }

    #[test]
    fn appended_artifacts_are_listed_in_append_order() {
        let store = InMemorySessionStore::new();
        store
            .append_session_artifact(&artifact("session-1", "a1", "2026-01-01T00:00:00Z"))
            .expect("append first artifact");
        store
            .append_session_artifact(&artifact("session-1", "a2", "2026-01-01T00:00:01Z"))
            .expect("append second artifact");

        let artifacts = store
            .list_session_artifacts("session-1")
            .expect("list artifacts");

        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].id, "a1");
        assert_eq!(artifacts[1].id, "a2");
    }

    #[test]
    fn find_player_identity_returns_none_for_missing_token() {
        let store = InMemorySessionStore::new();
        let session = summary("123456", Phase::Lobby, 1);
        store.save_session(&session).expect("save session");

        let found = store
            .find_player_identity("123456", "missing-token")
            .expect("find identity");

        assert_eq!(found, None);
    }

    #[test]
    fn revoke_player_identity_removes_existing_identity() {
        let store = InMemorySessionStore::new();
        let session = summary("123456", Phase::Lobby, 1);
        store.save_session(&session).expect("save session");
        store
            .create_player_identity(&identity(&session.id.to_string(), "player-1", "token-1"))
            .expect("create identity");
        store
            .revoke_player_identity("token-1")
            .expect("revoke identity");

        let found = store
            .find_player_identity("123456", "token-1")
            .expect("find identity");

        assert_eq!(found, None);
    }

    #[test]
    fn touch_player_identity_updates_last_seen_without_changing_match() {
        let store = InMemorySessionStore::new();
        let session = summary("123456", Phase::Lobby, 1);
        store.save_session(&session).expect("save session");
        store
            .create_player_identity(&identity(&session.id.to_string(), "player-1", "token-1"))
            .expect("create identity");
        store
            .touch_player_identity("token-1", "2026-01-01T00:00:05Z")
            .expect("touch identity");

        let found = store
            .find_player_identity("123456", "token-1")
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
