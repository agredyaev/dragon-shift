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
        if let Some(existing_registration) = self.connections_by_id.get(connection_id)
            && (existing_registration.session_code != key.0
                || existing_registration.player_id != key.1)
        {
            self.connection_by_session_player.remove(&(
                existing_registration.session_code.clone(),
                existing_registration.player_id.clone(),
            ));
        }
        let replaced_connection_id = self
            .connection_by_session_player
            .insert(key.clone(), connection_id.to_string());

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
            replaced_connection_id: replaced_connection_id.filter(|value| value != connection_id),
        }
    }

    pub fn connection_registration(&self, connection_id: &str) -> Option<ConnectionRegistration> {
        self.connections_by_id.get(connection_id).cloned()
    }

    pub fn detach(&mut self, connection_id: &str) -> Option<ConnectionRegistration> {
        let registration = self.connections_by_id.remove(connection_id)?;

        let key = (
            registration.session_code.clone(),
            registration.player_id.clone(),
        );
        self.connection_by_session_player.remove(&key);
        Some(registration)
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

    pub fn session_registrations(&self, session_code: &str) -> Vec<ConnectionRegistration> {
        self.connections_by_id
            .values()
            .filter(|registration| registration.session_code == session_code)
            .cloned()
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
    fn detach_returns_registration_for_existing_connection() {
        let mut registry = SessionRegistry::new();
        registry.attach("123456", "player-1", "conn-1");

        let removed = registry.detach("conn-1");

        assert_eq!(
            removed,
            Some(ConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
            })
        );
        assert_eq!(registry.session_connection_count("123456"), 0);
    }

    #[test]
    fn detach_returns_none_for_missing_connection() {
        let mut registry = SessionRegistry::new();

        let removed = registry.detach("missing-connection");

        assert_eq!(removed, None);
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

    #[test]
    fn session_registrations_return_player_specific_connections() {
        let mut registry = SessionRegistry::new();
        registry.attach("123456", "player-1", "conn-1");
        registry.attach("123456", "player-2", "conn-2");
        registry.attach("654321", "player-3", "conn-3");

        let registrations = registry.session_registrations("123456");

        assert_eq!(registrations.len(), 2);
        assert!(
            registrations
                .iter()
                .any(|registration| registration.player_id == "player-1")
        );
        assert!(
            registrations
                .iter()
                .any(|registration| registration.player_id == "player-2")
        );
        assert!(
            !registrations
                .iter()
                .any(|registration| registration.player_id == "player-3")
        );
    }

    #[test]
    fn reusing_same_connection_id_removes_stale_session_player_mapping() {
        let mut registry = SessionRegistry::new();
        registry.attach("123456", "player-1", "conn-1");
        registry.attach("654321", "player-2", "conn-1");

        let reattached = registry.attach("123456", "player-1", "conn-2");

        assert_eq!(reattached.replaced_connection_id, None);
        assert_eq!(registry.session_connection_count("123456"), 1);
        assert_eq!(registry.session_connection_count("654321"), 1);
        let original_session = registry.session_registrations("123456");
        assert_eq!(original_session[0].connection_id, "conn-2");
        let replacement_session = registry.session_registrations("654321");
        assert_eq!(replacement_session[0].connection_id, "conn-1");
    }
}
