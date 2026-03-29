use protocol::{
    ClientSessionSnapshot, CreateWorkshopRequest, JoinWorkshopRequest, JudgeBundle, SessionCommand,
    SessionEnvelope, WorkshopCommandRequest, WorkshopCommandResult, WorkshopJoinResult,
    WorkshopJoinSuccess, WorkshopJudgeBundleRequest, WorkshopJudgeBundleResult,
};

use crate::state::default_api_base_url;

// ---------------------------------------------------------------------------
// API client
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AppWebApi {
    pub base_url: String,
    pub client: reqwest::Client,
}

impl AppWebApi {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: normalize_api_base_url(&base_url.into()),
            client: reqwest::Client::new(),
        }
    }

    pub async fn create_workshop(&self, name: String) -> Result<WorkshopJoinSuccess, String> {
        let response = self
            .client
            .post(format!("{}/api/workshops", self.base_url))
            .json(&CreateWorkshopRequest { name })
            .send()
            .await
            .map_err(|error| format!("failed to reach backend: {error}"))?;

        Self::parse_join_response(response).await
    }

    pub async fn join_workshop(
        &self,
        request: JoinWorkshopRequest,
    ) -> Result<WorkshopJoinSuccess, String> {
        let response = self
            .client
            .post(format!("{}/api/workshops/join", self.base_url))
            .json(&request)
            .send()
            .await
            .map_err(|error| format!("failed to reach backend: {error}"))?;

        Self::parse_join_response(response).await
    }

    pub async fn reconnect_workshop(
        &self,
        session_code: String,
        reconnect_token: String,
    ) -> Result<WorkshopJoinSuccess, String> {
        self.join_workshop(build_reconnect_request(&session_code, &reconnect_token))
            .await
    }

    pub async fn send_command(&self, request: WorkshopCommandRequest) -> Result<(), String> {
        let response = self
            .client
            .post(format!("{}/api/workshops/command", self.base_url))
            .json(&request)
            .send()
            .await
            .map_err(|error| format!("failed to reach backend: {error}"))?;

        let payload = response
            .json::<WorkshopCommandResult>()
            .await
            .map_err(|error| format!("failed to parse backend response: {error}"))?;

        match payload {
            WorkshopCommandResult::Success(_) => Ok(()),
            WorkshopCommandResult::Error(error) => Err(error.error),
        }
    }

    pub async fn fetch_judge_bundle(
        &self,
        request: WorkshopJudgeBundleRequest,
    ) -> Result<JudgeBundle, String> {
        let response = self
            .client
            .post(format!("{}/api/workshops/judge-bundle", self.base_url))
            .json(&request)
            .send()
            .await
            .map_err(|error| format!("failed to reach backend: {error}"))?;

        let payload = response
            .json::<WorkshopJudgeBundleResult>()
            .await
            .map_err(|error| format!("failed to parse backend response: {error}"))?;

        match payload {
            WorkshopJudgeBundleResult::Success(success) => Ok(success.bundle),
            WorkshopJudgeBundleResult::Error(error) => Err(error.error),
        }
    }

    async fn parse_join_response(
        response: reqwest::Response,
    ) -> Result<WorkshopJoinSuccess, String> {
        let payload = response
            .json::<WorkshopJoinResult>()
            .await
            .map_err(|error| format!("failed to parse backend response: {error}"))?;

        match payload {
            WorkshopJoinResult::Success(success) => Ok(success),
            WorkshopJoinResult::Error(error) => Err(error.error),
        }
    }
}

// ---------------------------------------------------------------------------
// URL / request builders
// ---------------------------------------------------------------------------

pub fn normalize_api_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        default_api_base_url()
    } else {
        trimmed.to_string()
    }
}

pub fn build_reconnect_request(
    session_code: &str,
    reconnect_token: &str,
) -> JoinWorkshopRequest {
    JoinWorkshopRequest {
        session_code: session_code.trim().to_string(),
        name: None,
        reconnect_token: Some(reconnect_token.trim().to_string()),
    }
}

pub fn build_client_session_snapshot(
    success: &WorkshopJoinSuccess,
) -> ClientSessionSnapshot {
    ClientSessionSnapshot {
        session_code: success.session_code.clone(),
        reconnect_token: success.reconnect_token.clone(),
        player_id: success.player_id.clone(),
        coordinator_type: success.coordinator_type,
    }
}

pub fn build_command_request(
    snapshot: &ClientSessionSnapshot,
    command: SessionCommand,
    payload: Option<serde_json::Value>,
) -> WorkshopCommandRequest {
    WorkshopCommandRequest {
        session_code: snapshot.session_code.clone(),
        reconnect_token: snapshot.reconnect_token.clone(),
        coordinator_type: Some(snapshot.coordinator_type),
        command,
        payload,
    }
}

pub fn build_judge_bundle_request(
    snapshot: &ClientSessionSnapshot,
) -> WorkshopJudgeBundleRequest {
    WorkshopJudgeBundleRequest {
        session_code: snapshot.session_code.clone(),
        reconnect_token: snapshot.reconnect_token.clone(),
        coordinator_type: Some(snapshot.coordinator_type),
    }
}

#[allow(dead_code)]
pub fn build_session_envelope(
    snapshot: &ClientSessionSnapshot,
) -> SessionEnvelope {
    SessionEnvelope {
        session_code: snapshot.session_code.clone(),
        player_id: snapshot.player_id.clone(),
        reconnect_token: snapshot.reconnect_token.clone(),
        coordinator_type: Some(snapshot.coordinator_type),
    }
}

#[allow(dead_code)]
pub fn build_ws_url(base_url: &str) -> String {
    let normalized = normalize_api_base_url(base_url);
    let ws_base = if let Some(rest) = normalized.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = normalized.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        normalized
    };

    format!("{ws_base}/api/workshops/ws")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::CoordinatorType;

    #[test]
    fn normalize_api_base_url_trims_trailing_slashes_and_whitespace() {
        assert_eq!(
            normalize_api_base_url(" http://localhost:4100/ "),
            "http://localhost:4100"
        );
        assert_eq!(normalize_api_base_url("   "), "http://127.0.0.1:4100");
    }

    #[test]
    fn reconnect_request_uses_token_without_name() {
        let request = build_reconnect_request(" 123456 ", " reconnect-1 ");

        assert_eq!(request.session_code, "123456");
        assert_eq!(request.name, None);
        assert_eq!(request.reconnect_token.as_deref(), Some("reconnect-1"));
    }

    #[test]
    fn build_command_request_uses_snapshot_credentials() {
        let snapshot = ClientSessionSnapshot {
            session_code: "123456".to_string(),
            reconnect_token: "reconnect-1".to_string(),
            player_id: "player-1".to_string(),
            coordinator_type: CoordinatorType::Rust,
        };

        let request = build_command_request(&snapshot, SessionCommand::StartPhase1, None);

        assert_eq!(request.session_code, "123456");
        assert_eq!(request.reconnect_token, "reconnect-1");
        assert_eq!(request.coordinator_type, Some(CoordinatorType::Rust));
        assert_eq!(request.command, SessionCommand::StartPhase1);
        assert_eq!(request.payload, None);
    }

    #[test]
    fn build_judge_bundle_request_uses_snapshot_credentials() {
        let snapshot = ClientSessionSnapshot {
            session_code: "123456".to_string(),
            reconnect_token: "reconnect-1".to_string(),
            player_id: "player-1".to_string(),
            coordinator_type: CoordinatorType::Rust,
        };

        let request = build_judge_bundle_request(&snapshot);

        assert_eq!(request.session_code, "123456");
        assert_eq!(request.reconnect_token, "reconnect-1");
        assert_eq!(request.coordinator_type, Some(CoordinatorType::Rust));
    }

    #[test]
    fn build_session_envelope_uses_snapshot_identity() {
        let snapshot = ClientSessionSnapshot {
            session_code: "123456".to_string(),
            reconnect_token: "reconnect-1".to_string(),
            player_id: "player-1".to_string(),
            coordinator_type: CoordinatorType::Rust,
        };

        let envelope = build_session_envelope(&snapshot);

        assert_eq!(envelope.session_code, "123456");
        assert_eq!(envelope.player_id, "player-1");
        assert_eq!(envelope.reconnect_token, "reconnect-1");
        assert_eq!(envelope.coordinator_type, Some(CoordinatorType::Rust));
    }

    #[test]
    fn build_ws_url_maps_http_scheme_to_ws_endpoint() {
        assert_eq!(
            build_ws_url("http://127.0.0.1:4100/"),
            "ws://127.0.0.1:4100/api/workshops/ws"
        );
        assert_eq!(
            build_ws_url("https://dragon-switch.dev"),
            "wss://dragon-switch.dev/api/workshops/ws"
        );
    }
}
