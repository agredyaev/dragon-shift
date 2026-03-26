use protocol::ServerWsMessage;
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RealtimeError {
    #[error("connection is closed")]
    ConnectionClosed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundEvent {
    pub connection_id: String,
    pub message: ServerWsMessage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionRegistration {
    pub session_code: String,
    pub player_id: String,
    pub connection_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachResult {
    pub replaced_connection_id: Option<String>,
}

#[derive(Debug, Default)]
pub struct SessionRegistry {
    connections_by_id: BTreeMap<String, ConnectionRegistration>,
    connection_by_session_player: BTreeMap<(String, String), String>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn attach(
        &mut self,
        session_code: &str,
        player_id: &str,
        connection_id: &str,
    ) -> AttachResult {
        let key = (session_code.to_string(), player_id.to_string());
        let replaced_connection_id = self.connection_by_session_player.insert(key.clone(), connection_id.to_string());

        if let Some(previous_connection_id) = &replaced_connection_id {
            self.connections_by_id.remove(previous_connection_id);
        }

        self.connections_by_id.insert(
            connection_id.to_string(),
            ConnectionRegistration {
                session_code: session_code.to_string(),
                player_id: player_id.to_string(),
                connection_id: connection_id.to_string(),
            },
        );

        AttachResult {
            replaced_connection_id,
        }
    }

    pub fn detach(&mut self, connection_id: &str) -> bool {
        let Some(registration) = self.connections_by_id.remove(connection_id) else {
            return false;
        };

        let key = (registration.session_code, registration.player_id);
        self.connection_by_session_player.remove(&key);
        true
    }

    pub fn broadcast_to_session(
        &self,
        session_code: &str,
        message: &ServerWsMessage,
    ) -> Vec<OutboundEvent> {
        self.connections_by_id
            .values()
            .filter(|registration| registration.session_code == session_code)
            .map(|registration| OutboundEvent {
                connection_id: registration.connection_id.clone(),
                message: message.clone(),
            })
            .collect()
    }

    pub fn session_connection_count(&self, session_code: &str) -> usize {
        self.connections_by_id
            .values()
            .filter(|registration| registration.session_code == session_code)
            .count()
    }

    pub fn total_connection_count(&self) -> usize {
        self.connections_by_id.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> ServerWsMessage {
        ServerWsMessage::Error {
            message: "boom".to_string(),
        }
    }

    #[test]
    fn duplicate_attach_replaces_previous_connection_for_same_player() {
        let mut registry = SessionRegistry::new();

        let first = registry.attach("123456", "player-1", "conn-1");
        let second = registry.attach("123456", "player-1", "conn-2");

        assert_eq!(first.replaced_connection_id, None);
        assert_eq!(second.replaced_connection_id, Some("conn-1".to_string()));
        assert_eq!(registry.session_connection_count("123456"), 1);
        let events = registry.broadcast_to_session("123456", &message());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].connection_id, "conn-2");
    }

    #[test]
    fn detach_returns_false_for_missing_connection() {
        let mut registry = SessionRegistry::new();

        let removed = registry.detach("missing-connection");

        assert!(!removed);
    }

    #[test]
    fn session_broadcast_fans_out_only_to_target_session() {
        let mut registry = SessionRegistry::new();
        registry.attach("123456", "player-1", "conn-1");
        registry.attach("123456", "player-2", "conn-2");
        registry.attach("654321", "player-3", "conn-3");

        let events = registry.broadcast_to_session("123456", &message());

        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|event| event.connection_id == "conn-1"));
        assert!(events.iter().any(|event| event.connection_id == "conn-2"));
        assert!(!events.iter().any(|event| event.connection_id == "conn-3"));
    }
}
