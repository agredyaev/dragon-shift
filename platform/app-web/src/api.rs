use protocol::{
    ClientSessionSnapshot, CreateWorkshopRequest, JoinWorkshopRequest, JudgeBundle, SessionCommand,
    SessionEnvelope, WorkshopCommandRequest, WorkshopCommandResult, WorkshopCreateConfig,
    WorkshopJoinResult, WorkshopJoinSuccess, WorkshopJudgeBundleRequest, WorkshopJudgeBundleResult,
};

use serde::de::DeserializeOwned;

#[cfg(target_arch = "wasm32")]
use js_sys::Promise;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;

use crate::state::default_api_base_url;

// ---------------------------------------------------------------------------
// API client
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AppWebApi {
    pub base_url: String,
}

impl AppWebApi {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: normalize_api_base_url(&base_url.into()),
        }
    }

    pub async fn create_workshop(
        &self,
        name: String,
        config: WorkshopCreateConfig,
    ) -> Result<WorkshopJoinSuccess, String> {
        Self::parse_join_response(
            self.post_json("/api/workshops", &CreateWorkshopRequest { name, config })
                .await?,
        )
    }

    pub async fn join_workshop(
        &self,
        request: JoinWorkshopRequest,
    ) -> Result<WorkshopJoinSuccess, String> {
        Self::parse_join_response(self.post_json("/api/workshops/join", &request).await?)
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
        let payload: WorkshopCommandResult =
            self.post_json("/api/workshops/command", &request).await?;

        match payload {
            WorkshopCommandResult::Success(_) => Ok(()),
            WorkshopCommandResult::Error(error) => Err(error.error),
        }
    }

    pub async fn fetch_judge_bundle(
        &self,
        request: WorkshopJudgeBundleRequest,
    ) -> Result<JudgeBundle, String> {
        let payload: WorkshopJudgeBundleResult = self
            .post_json("/api/workshops/judge-bundle", &request)
            .await?;

        match payload {
            WorkshopJudgeBundleResult::Success(success) => Ok(success.bundle),
            WorkshopJudgeBundleResult::Error(error) => Err(error.error),
        }
    }

    fn parse_join_response(payload: WorkshopJoinResult) -> Result<WorkshopJoinSuccess, String> {
        match payload {
            WorkshopJoinResult::Success(success) => Ok(success),
            WorkshopJoinResult::Error(error) => Err(error.error),
        }
    }

    #[cfg(target_arch = "wasm32")]
    async fn post_json<Req, Res>(&self, path: &str, body: &Req) -> Result<Res, String>
    where
        Req: serde::Serialize,
        Res: DeserializeOwned,
    {
        let body_json = serde_json::to_string(body)
            .map_err(|error| format!("failed to encode request body: {error}"))?;

        let init = web_sys::RequestInit::new();
        init.set_method("POST");
        init.set_body(&JsValue::from_str(&body_json));

        let headers =
            web_sys::Headers::new().map_err(|_| "failed to prepare request headers".to_string())?;
        headers
            .set("Content-Type", "application/json")
            .map_err(|_| "failed to set request headers".to_string())?;
        init.set_headers_headers(&headers);

        let url = format!("{}{}", self.base_url, path);
        let request = web_sys::Request::new_with_str_and_init(&url, &init)
            .map_err(|_| "failed to prepare browser request".to_string())?;
        let window = web_sys::window().ok_or_else(|| "window is unavailable".to_string())?;
        let response = JsFuture::from(window.fetch_with_request(&request))
            .await
            .map_err(|e| format!("failed to reach backend: {}", js_error_message(e)))?;
        let response: web_sys::Response = response
            .dyn_into()
            .map_err(|_| "failed to read browser response".to_string())?;
        let text = js_future_string(
            response
                .text()
                .map_err(|_| "failed to read backend response".to_string())?,
        )
        .await?;

        if !response.ok() {
            return Err(extract_backend_error(&text).unwrap_or_else(|| {
                format!("backend request failed with status {}", response.status())
            }));
        }

        serde_json::from_str(&text)
            .map_err(|error| format!("failed to parse backend response: {error}"))
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn post_json<Req, Res>(&self, path: &str, body: &Req) -> Result<Res, String>
    where
        Req: serde::Serialize,
        Res: DeserializeOwned,
    {
        let response = reqwest::Client::new()
            .post(format!("{}{}", self.base_url, path))
            .json(body)
            .send()
            .await
            .map_err(|error| format!("failed to reach backend: {error}"))?;

        response
            .json::<Res>()
            .await
            .map_err(|error| format!("failed to parse backend response: {error}"))
    }
}

#[cfg(target_arch = "wasm32")]
async fn js_future_string(promise: Promise) -> Result<String, String> {
    let value = JsFuture::from(promise).await.map_err(js_error_message)?;
    value
        .as_string()
        .ok_or_else(|| "backend response was not text".to_string())
}

#[cfg(target_arch = "wasm32")]
fn js_error_message(error: JsValue) -> String {
    error
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(&error, &JsValue::from_str("message"))
                .ok()?
                .as_string()
        })
        .unwrap_or_else(|| "failed to reach backend".to_string())
}

#[cfg(target_arch = "wasm32")]
fn extract_backend_error(text: &str) -> Option<String> {
    serde_json::from_str::<WorkshopErrorEnvelope>(text)
        .ok()
        .map(|payload| payload.error)
}

#[cfg(target_arch = "wasm32")]
#[derive(serde::Deserialize)]
struct WorkshopErrorEnvelope {
    error: String,
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

pub fn build_reconnect_request(session_code: &str, reconnect_token: &str) -> JoinWorkshopRequest {
    JoinWorkshopRequest {
        session_code: session_code.trim().to_string(),
        name: None,
        reconnect_token: Some(reconnect_token.trim().to_string()),
    }
}

pub fn build_client_session_snapshot(success: &WorkshopJoinSuccess) -> ClientSessionSnapshot {
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

pub fn build_judge_bundle_request(snapshot: &ClientSessionSnapshot) -> WorkshopJudgeBundleRequest {
    WorkshopJudgeBundleRequest {
        session_code: snapshot.session_code.clone(),
        reconnect_token: snapshot.reconnect_token.clone(),
        coordinator_type: Some(snapshot.coordinator_type),
    }
}

#[allow(dead_code)]
pub fn build_session_envelope(snapshot: &ClientSessionSnapshot) -> SessionEnvelope {
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
