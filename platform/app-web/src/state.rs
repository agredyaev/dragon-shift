#![allow(clippy::too_many_arguments)]

use protocol::{
    ClientGameState, ClientSessionSnapshot, CoordinatorType, JudgeBundle, NoticeLevel, Phase,
    ServerWsMessage, SessionCommand, SessionNotice as ProtocolSessionNotice, WorkshopJoinSuccess,
};

use crate::api::build_client_session_snapshot;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellScreen {
    Home,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Offline,
    Connecting,
    Connected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingFlow {
    Create,
    Join,
    Reconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeTone {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellNotice {
    pub tone: NoticeTone,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdentity {
    pub session_code: String,
    pub player_id: String,
    pub reconnect_token: String,
}

// ---------------------------------------------------------------------------
// Signal group structs
// ---------------------------------------------------------------------------

/// Identity & connection — changes on join/disconnect, not during gameplay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityState {
    pub screen: ShellScreen,
    pub connection_status: ConnectionStatus,
    pub coordinator: CoordinatorType,
    pub identity: Option<SessionIdentity>,
    pub session_snapshot: Option<ClientSessionSnapshot>,
    pub api_base_url: String,
    pub realtime_bootstrap_attempted: bool,
}

/// Transient operation state — changes on command send/receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationState {
    pub pending_flow: Option<PendingFlow>,
    pub pending_command: Option<SessionCommand>,
    pub pending_judge_bundle: bool,
    pub notice: Option<ShellNotice>,
    /// Notice to show on the first realtime attach instead of the default
    /// "Session synced." message.  Set by `apply_join_success` for
    /// flow-specific notices (e.g. "Reconnected to workshop.") that would
    /// otherwise be overwritten by the realtime bootstrap sequence.
    pub pending_realtime_notice: Option<ShellNotice>,
}

// ---------------------------------------------------------------------------
// Notice helpers
// ---------------------------------------------------------------------------

pub fn info_notice(message: &str) -> ShellNotice {
    ShellNotice {
        tone: NoticeTone::Info,
        message: message.to_string(),
    }
}

pub fn success_notice(message: &str) -> ShellNotice {
    ShellNotice {
        tone: NoticeTone::Success,
        message: message.to_string(),
    }
}

pub fn error_notice(message: &str) -> ShellNotice {
    ShellNotice {
        tone: NoticeTone::Error,
        message: message.to_string(),
    }
}

#[allow(dead_code)]
fn map_notice_tone(level: NoticeLevel) -> NoticeTone {
    match level {
        NoticeLevel::Info => NoticeTone::Info,
        NoticeLevel::Success => NoticeTone::Success,
        NoticeLevel::Warning => NoticeTone::Warning,
        NoticeLevel::Error => NoticeTone::Error,
    }
}

// ---------------------------------------------------------------------------
// Bootstrap / restore
// ---------------------------------------------------------------------------

pub fn default_api_base_url() -> String {
    browser_default_api_base_url().unwrap_or_default()
}

#[cfg(target_arch = "wasm32")]
fn browser_default_api_base_url() -> Option<String> {
    let window = web_sys::window()?;
    let origin = window.location().origin().ok()?;
    if origin.trim().is_empty() {
        None
    } else {
        Some(origin)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn browser_default_api_base_url() -> Option<String> {
    None
}

pub fn default_identity_state() -> IdentityState {
    IdentityState {
        screen: ShellScreen::Home,
        connection_status: ConnectionStatus::Offline,
        coordinator: CoordinatorType::Rust,
        identity: None,
        session_snapshot: None,
        api_base_url: default_api_base_url(),
        realtime_bootstrap_attempted: false,
    }
}

pub fn default_operation_state() -> OperationState {
    OperationState {
        pending_flow: None,
        pending_command: None,
        pending_judge_bundle: false,
        notice: None,
        pending_realtime_notice: None,
    }
}

pub fn hydrate_from_snapshot(
    identity: &mut IdentityState,
    join_session_code: &mut String,
    reconnect_session_code: &mut String,
    reconnect_token: &mut String,
    snapshot: &ClientSessionSnapshot,
) {
    identity.screen = ShellScreen::Session;
    identity.connection_status = ConnectionStatus::Offline;
    identity.coordinator = snapshot.coordinator_type;
    identity.identity = Some(SessionIdentity {
        session_code: snapshot.session_code.clone(),
        player_id: snapshot.player_id.clone(),
        reconnect_token: snapshot.reconnect_token.clone(),
    });
    identity.session_snapshot = Some(snapshot.clone());
    *join_session_code = snapshot.session_code.clone();
    *reconnect_session_code = snapshot.session_code.clone();
    *reconnect_token = snapshot.reconnect_token.clone();
}

#[derive(Clone)]
pub struct BootstrapResult {
    pub identity: IdentityState,
    pub game_state: Option<ClientGameState>,
    pub create_name: String,
    pub join_session_code: String,
    pub join_name: String,
    pub reconnect_session_code: String,
    pub reconnect_token: String,
    pub handover_tags_input: String,
    pub ops: OperationState,
    pub judge_bundle: Option<JudgeBundle>,
}

pub fn restore_bootstrap(snapshot: Option<ClientSessionSnapshot>) -> BootstrapResult {
    let mut identity = default_identity_state();
    let create_name = String::new();
    let mut join_session_code = String::new();
    let join_name = String::new();
    let mut reconnect_session_code = String::new();
    let mut reconnect_token = String::new();
    let handover_tags_input = String::new();
    let mut ops = default_operation_state();

    if let Some(snapshot) = snapshot {
        hydrate_from_snapshot(
            &mut identity,
            &mut join_session_code,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &snapshot,
        );
        ops.notice = Some(info_notice(
            "Restored reconnect session from browser storage.",
        ));
    }

    BootstrapResult {
        identity,
        game_state: None,
        create_name,
        join_session_code,
        join_name,
        reconnect_session_code,
        reconnect_token,
        handover_tags_input,
        ops,
        judge_bundle: None,
    }
}

pub fn bootstrap_state() -> BootstrapResult {
    let mut result = match load_browser_session_snapshot() {
        Ok(snapshot) => restore_bootstrap(snapshot),
        Err(error) => {
            let mut result = restore_bootstrap(None);
            result.ops.notice = Some(error_notice(&format!(
                "Failed to restore browser session: {error}"
            )));
            result
        }
    };

    if let Ok(Some(api_base_url)) = load_browser_query_api_base_url() {
        result.identity.api_base_url = api_base_url;
        let _ = persist_browser_api_base_url(&result.identity.api_base_url);
        return result;
    }

    if let Ok(Some(api_base_url)) = load_browser_api_base_url() {
        result.identity.api_base_url = api_base_url;
    }

    result
}

// ---------------------------------------------------------------------------
// Browser persistence
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub const SESSION_SNAPSHOT_STORAGE_KEY: &str = "dragon-switch/platform/session-snapshot";

#[allow(dead_code)]
pub const API_BASE_URL_STORAGE_KEY: &str = "dragon-switch/platform/api-base-url";

#[allow(dead_code)]
pub fn encode_session_snapshot(snapshot: &ClientSessionSnapshot) -> Result<String, String> {
    serde_json::to_string(snapshot)
        .map_err(|error| format!("failed to encode session snapshot: {error}"))
}

#[allow(dead_code)]
pub fn decode_session_snapshot(value: &str) -> Result<ClientSessionSnapshot, String> {
    serde_json::from_str(value)
        .map_err(|error| format!("failed to decode session snapshot: {error}"))
}

#[cfg(target_arch = "wasm32")]
pub fn load_browser_session_snapshot() -> Result<Option<ClientSessionSnapshot>, String> {
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let storage = window
        .session_storage()
        .map_err(|_| "failed to access browser storage".to_string())?
        .ok_or_else(|| "browser storage is unavailable".to_string())?;

    let Some(encoded) = storage
        .get_item(SESSION_SNAPSHOT_STORAGE_KEY)
        .map_err(|_| "failed to read browser storage".to_string())?
    else {
        return Ok(None);
    };

    decode_session_snapshot(&encoded).map(Some)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load_browser_session_snapshot() -> Result<Option<ClientSessionSnapshot>, String> {
    Ok(None)
}

#[cfg(target_arch = "wasm32")]
pub fn load_browser_api_base_url() -> Result<Option<String>, String> {
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let storage = window
        .local_storage()
        .map_err(|_| "failed to access browser storage".to_string())?
        .ok_or_else(|| "browser storage is unavailable".to_string())?;

    let Some(value) = storage
        .get_item(API_BASE_URL_STORAGE_KEY)
        .map_err(|_| "failed to read browser storage".to_string())?
    else {
        return Ok(None);
    };

    let value = value.trim().to_string();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load_browser_api_base_url() -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(target_arch = "wasm32")]
pub fn load_browser_query_api_base_url() -> Result<Option<String>, String> {
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let search = window
        .location()
        .search()
        .map_err(|_| "failed to read browser location".to_string())?;
    let params = web_sys::UrlSearchParams::new_with_str(&search)
        .map_err(|_| "failed to parse browser query parameters".to_string())?;

    let Some(value) = params.get("apiBaseUrl") else {
        return Ok(None);
    };

    let value = value.trim().to_string();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load_browser_query_api_base_url() -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(target_arch = "wasm32")]
pub fn persist_browser_session_snapshot(snapshot: &ClientSessionSnapshot) -> Result<(), String> {
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let storage = window
        .session_storage()
        .map_err(|_| "failed to access browser storage".to_string())?
        .ok_or_else(|| "browser storage is unavailable".to_string())?;
    let encoded = encode_session_snapshot(snapshot)?;
    storage
        .set_item(SESSION_SNAPSHOT_STORAGE_KEY, &encoded)
        .map_err(|_| "failed to persist browser session".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn persist_browser_session_snapshot(snapshot: &ClientSessionSnapshot) -> Result<(), String> {
    let _ = snapshot;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub fn clear_browser_session_snapshot() -> Result<(), String> {
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let storage = window
        .session_storage()
        .map_err(|_| "failed to access browser storage".to_string())?
        .ok_or_else(|| "browser storage is unavailable".to_string())?;
    storage
        .remove_item(SESSION_SNAPSHOT_STORAGE_KEY)
        .map_err(|_| "failed to clear browser session".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn clear_browser_session_snapshot() -> Result<(), String> {
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub fn persist_browser_api_base_url(api_base_url: &str) -> Result<(), String> {
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let storage = window
        .local_storage()
        .map_err(|_| "failed to access browser storage".to_string())?
        .ok_or_else(|| "browser storage is unavailable".to_string())?;

    if api_base_url.trim().is_empty() {
        storage
            .remove_item(API_BASE_URL_STORAGE_KEY)
            .map_err(|_| "failed to clear browser API address".to_string())
    } else {
        storage
            .set_item(API_BASE_URL_STORAGE_KEY, api_base_url.trim())
            .map_err(|_| "failed to persist browser API address".to_string())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn persist_browser_api_base_url(api_base_url: &str) -> Result<(), String> {
    let _ = api_base_url;
    Ok(())
}

// ---------------------------------------------------------------------------
// Mutation functions
// ---------------------------------------------------------------------------

pub fn apply_join_success(
    identity: &mut IdentityState,
    game_state: &mut Option<ClientGameState>,
    ops: &mut OperationState,
    join_session_code: &mut String,
    reconnect_session_code: &mut String,
    reconnect_token: &mut String,
    judge_bundle: &mut Option<JudgeBundle>,
    success: WorkshopJoinSuccess,
    flow: PendingFlow,
) {
    let snapshot = build_client_session_snapshot(&success);
    let success_message = match flow {
        PendingFlow::Create => "Workshop created.",
        PendingFlow::Join => "Joined workshop.",
        PendingFlow::Reconnect => "Reconnected to workshop.",
    };

    identity.screen = ShellScreen::Session;
    identity.connection_status = ConnectionStatus::Connected;
    identity.coordinator = success.coordinator_type;
    identity.identity = Some(SessionIdentity {
        session_code: success.session_code.clone(),
        player_id: success.player_id.clone(),
        reconnect_token: success.reconnect_token.clone(),
    });
    identity.session_snapshot = Some(snapshot.clone());

    *game_state = Some(success.state);
    *judge_bundle = None;

    *join_session_code = snapshot.session_code.clone();
    *reconnect_session_code = snapshot.session_code.clone();
    *reconnect_token = snapshot.reconnect_token.clone();

    ops.pending_flow = None;
    ops.pending_judge_bundle = false;
    ops.notice = Some(success_notice(success_message));
    ops.pending_realtime_notice = match flow {
        PendingFlow::Reconnect => Some(success_notice(success_message)),
        _ => None,
    };
}

pub fn apply_request_error(identity: &mut IdentityState, ops: &mut OperationState, error: String) {
    identity.connection_status = ConnectionStatus::Offline;
    ops.pending_flow = None;
    if should_clear_session_snapshot(&error) {
        clear_session_identity(identity);
    }
    ops.notice = Some(error_notice(&error));
}

pub fn command_success_message(command: SessionCommand) -> &'static str {
    match command {
        SessionCommand::StartPhase0 => "Character creation opened.",
        SessionCommand::UpdatePlayerPet => "Dragon profile saved.",
        SessionCommand::StartPhase1 => "Phase 1 started.",
        SessionCommand::StartHandover => "Handover started.",
        SessionCommand::SubmitTags => "Handover tags saved.",
        SessionCommand::StartPhase2 => "Phase 2 started.",
        SessionCommand::EndGame => "Judge review started.",
        SessionCommand::StartVoting => "Design voting started.",
        SessionCommand::RevealVotingResults => "Voting results revealed.",
        SessionCommand::ResetGame => "Workshop reset.",
        SessionCommand::EndSession => "Session ended.",
        _ => "Command sent.",
    }
}

fn command_completed_by_phase_update(command: SessionCommand, phase: Phase) -> bool {
    matches!(
        (command, phase),
        (SessionCommand::StartPhase0, Phase::Phase0)
            | (SessionCommand::StartPhase1, Phase::Phase1)
            | (SessionCommand::StartHandover, Phase::Handover)
            | (SessionCommand::StartPhase2, Phase::Phase2)
            | (SessionCommand::EndGame, Phase::Judge)
            | (SessionCommand::StartVoting, Phase::Voting)
            | (SessionCommand::RevealVotingResults, Phase::End)
            | (SessionCommand::EndSession, Phase::End)
            | (SessionCommand::ResetGame, Phase::Lobby)
    )
}

pub fn apply_successful_command(
    identity: &mut IdentityState,
    ops: &mut OperationState,
    handover_tags_input: &mut String,
    judge_bundle: &mut Option<JudgeBundle>,
    command: SessionCommand,
) {
    ops.pending_command = None;
    identity.connection_status = ConnectionStatus::Connected;
    if command == SessionCommand::SubmitTags {
        handover_tags_input.clear();
    }
    if command == SessionCommand::ResetGame {
        *judge_bundle = None;
    }
    ops.notice = Some(success_notice(command_success_message(command)));
}

pub fn apply_command_error(identity: &mut IdentityState, ops: &mut OperationState, error: String) {
    ops.pending_command = None;
    if should_clear_session_snapshot(&error) {
        clear_session_identity(identity);
    }
    ops.notice = Some(error_notice(&error));
}

pub fn apply_judge_bundle_success(
    ops: &mut OperationState,
    judge_bundle: &mut Option<JudgeBundle>,
    bundle: JudgeBundle,
) {
    ops.pending_judge_bundle = false;
    *judge_bundle = Some(bundle);
    ops.notice = Some(success_notice("Workshop archive ready."));
}

pub fn apply_judge_bundle_error(
    identity: &mut IdentityState,
    ops: &mut OperationState,
    error: String,
) {
    ops.pending_judge_bundle = false;
    if should_clear_session_snapshot(&error) {
        clear_session_identity(identity);
    }
    ops.notice = Some(error_notice(&error));
}

#[allow(dead_code)]
pub fn apply_realtime_bootstrap_error(
    identity: &mut IdentityState,
    ops: &mut OperationState,
    error: String,
) {
    identity.realtime_bootstrap_attempted = true;
    identity.connection_status = ConnectionStatus::Offline;
    if should_clear_session_snapshot(&error) {
        clear_session_identity(identity);
    }
    ops.notice = Some(error_notice(&error));
}

#[allow(dead_code)]
pub fn apply_realtime_connecting(identity: &mut IdentityState, ops: &mut OperationState) {
    identity.realtime_bootstrap_attempted = true;
    identity.connection_status = ConnectionStatus::Connecting;
    ops.notice = Some(info_notice("Syncing session…"));
}

fn should_clear_session_snapshot(error: &str) -> bool {
    matches!(
        error.trim(),
        "Missing workshop credentials."
            | "Session identity is invalid or expired."
            | "Workshop not found."
    )
}

pub fn clear_session_identity(identity: &mut IdentityState) {
    identity.screen = ShellScreen::Home;
    identity.connection_status = ConnectionStatus::Offline;
    identity.identity = None;
    identity.session_snapshot = None;
    identity.realtime_bootstrap_attempted = false;
    let _ = clear_browser_session_snapshot();
}

#[allow(dead_code)]
pub fn apply_server_ws_message(
    identity: &mut IdentityState,
    game_state: &mut Option<ClientGameState>,
    ops: &mut OperationState,
    judge_bundle: &mut Option<JudgeBundle>,
    message: ServerWsMessage,
) {
    match message {
        ServerWsMessage::StateUpdate(client_state) => {
            let first_attach = identity.connection_status != ConnectionStatus::Connected;
            let phase = client_state.phase;
            let completed_pending_command = ops
                .pending_command
                .filter(|command| command_completed_by_phase_update(*command, phase));
            identity.screen = ShellScreen::Session;
            *game_state = Some(client_state);
            identity.connection_status = ConnectionStatus::Connected;
            if phase != Phase::End {
                *judge_bundle = None;
                ops.pending_judge_bundle = false;
            }
            if first_attach {
                ops.pending_command = None;
                ops.notice = Some(
                    ops.pending_realtime_notice
                        .take()
                        .unwrap_or_else(|| info_notice("Session synced.")),
                );
            } else if let Some(command) = completed_pending_command {
                // Phase-transition commands can unmount the source component before
                // the HTTP task applies its success notice, so confirm them from the
                // resulting state update as well.
                ops.pending_command = None;
                ops.notice = Some(success_notice(command_success_message(command)));
            }
        }
        ServerWsMessage::Notice(ProtocolSessionNotice {
            level,
            title,
            message,
        }) => {
            let combined = if title.trim().is_empty() {
                message
            } else {
                format!("{title}: {message}")
            };
            let tone = map_notice_tone(level);
            ops.notice = Some(ShellNotice {
                tone,
                message: combined,
            });
        }
        ServerWsMessage::Error { message } => {
            identity.connection_status = ConnectionStatus::Offline;
            if should_clear_session_snapshot(&message) {
                clear_session_identity(identity);
            }
            ops.notice = Some(error_notice(&message));
        }
        ServerWsMessage::Pong => {
            identity.connection_status = ConnectionStatus::Connected;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{
        create_default_session_settings, ClientGameState, CoordinatorType, Phase, Player,
        SessionMeta, WorkshopJoinSuccess,
    };
    use std::collections::BTreeMap;

    fn mock_join_success() -> WorkshopJoinSuccess {
        let mut players = BTreeMap::new();
        players.insert(
            "player-1".to_string(),
            Player {
                id: "player-1".to_string(),
                name: "Alice".to_string(),
                is_host: true,
                score: 0,
                current_dragon_id: None,
                achievements: Vec::new(),
                is_ready: false,
                is_connected: true,
                pet_description: Some("Alice's workshop dragon".to_string()),
                custom_sprites: None,
            },
        );

        WorkshopJoinSuccess {
            ok: true,
            session_code: "123456".to_string(),
            player_id: "player-1".to_string(),
            reconnect_token: "reconnect-1".to_string(),
            coordinator_type: CoordinatorType::Rust,
            state: ClientGameState {
                session: SessionMeta {
                    id: "session-1".to_string(),
                    code: "123456".to_string(),
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                    updated_at: "2026-01-01T00:00:00Z".to_string(),
                    phase_started_at: "2026-01-01T00:00:00Z".to_string(),
                    host_player_id: Some("player-1".to_string()),
                    settings: create_default_session_settings(),
                },
                phase: Phase::Lobby,
                time: 8,
                players,
                dragons: BTreeMap::new(),
                current_player_id: Some("player-1".to_string()),
                voting: None,
            },
        }
    }

    #[test]
    fn default_identity_boots_home_screen_with_rust_coordinator() {
        let identity = default_identity_state();

        assert_eq!(identity.screen, ShellScreen::Home);
        assert_eq!(identity.connection_status, ConnectionStatus::Offline);
        assert_eq!(identity.coordinator, CoordinatorType::Rust);
        assert_eq!(identity.identity, None);
        assert_eq!(identity.api_base_url, "");
    }

    #[test]
    fn restore_bootstrap_rehydrates_reconnect_fields_from_snapshot() {
        let snapshot = ClientSessionSnapshot {
            session_code: "654321".to_string(),
            reconnect_token: "reconnect-9".to_string(),
            player_id: "player-9".to_string(),
            coordinator_type: CoordinatorType::Rust,
        };

        let result = restore_bootstrap(Some(snapshot.clone()));

        assert_eq!(result.identity.screen, ShellScreen::Session);
        assert_eq!(result.identity.connection_status, ConnectionStatus::Offline);
        assert_eq!(
            result
                .identity
                .identity
                .as_ref()
                .map(|i| i.player_id.as_str()),
            Some("player-9")
        );
        assert_eq!(result.reconnect_session_code, "654321");
        assert_eq!(result.reconnect_token, "reconnect-9");
        assert_eq!(result.identity.session_snapshot, Some(snapshot));
        assert_eq!(
            result.ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Restored reconnect session from browser storage.")
        );
    }

    #[test]
    fn session_snapshot_round_trips_through_json_encoding() {
        let snapshot = ClientSessionSnapshot {
            session_code: "123456".to_string(),
            reconnect_token: "reconnect-1".to_string(),
            player_id: "player-1".to_string(),
            coordinator_type: CoordinatorType::Rust,
        };

        let encoded = encode_session_snapshot(&snapshot).expect("encode snapshot");
        let decoded = decode_session_snapshot(&encoded).expect("decode snapshot");

        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn apply_join_success_promotes_to_connected_session() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut join_session_code = String::new();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut join_session_code,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &mut judge_bundle,
            mock_join_success(),
            PendingFlow::Join,
        );

        assert_eq!(identity.screen, ShellScreen::Session);
        assert_eq!(identity.connection_status, ConnectionStatus::Connected);
        assert_eq!(ops.pending_flow, None);
        assert_eq!(
            identity.identity.as_ref().map(|i| i.session_code.as_str()),
            Some("123456")
        );
        assert_eq!(
            identity.session_snapshot,
            Some(ClientSessionSnapshot {
                session_code: "123456".to_string(),
                reconnect_token: "reconnect-1".to_string(),
                player_id: "player-1".to_string(),
                coordinator_type: CoordinatorType::Rust,
            })
        );
        assert_eq!(join_session_code, "123456");
        assert_eq!(reconnect_token, "reconnect-1");
        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Joined workshop.")
        );
    }

    #[test]
    fn apply_successful_command_clears_pending_command() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut join_session_code = String::new();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut handover_tags_input = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut join_session_code,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &mut judge_bundle,
            mock_join_success(),
            PendingFlow::Join,
        );
        ops.pending_command = Some(SessionCommand::StartPhase1);
        let original_phase = game_state.as_ref().map(|s| s.phase);

        apply_successful_command(
            &mut identity,
            &mut ops,
            &mut handover_tags_input,
            &mut judge_bundle,
            SessionCommand::StartPhase1,
        );

        assert_eq!(ops.pending_command, None);
        assert_eq!(game_state.as_ref().map(|s| s.phase), original_phase);
        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Phase 1 started.")
        );
    }

    #[test]
    fn submit_tags_success_clears_handover_input() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut join_session_code = String::new();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut handover_tags_input = "one, two".to_string();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut join_session_code,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &mut judge_bundle,
            mock_join_success(),
            PendingFlow::Join,
        );
        ops.pending_command = Some(SessionCommand::SubmitTags);

        apply_successful_command(
            &mut identity,
            &mut ops,
            &mut handover_tags_input,
            &mut judge_bundle,
            SessionCommand::SubmitTags,
        );

        assert_eq!(ops.pending_command, None);
        assert!(handover_tags_input.is_empty());
        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Handover tags saved.")
        );
    }

    #[test]
    fn apply_judge_bundle_success_stores_bundle_and_clears_pending() {
        let mut ops = default_operation_state();
        ops.pending_judge_bundle = true;
        let mut judge_bundle = None;

        let bundle = crate::helpers::tests::mock_judge_bundle();
        apply_judge_bundle_success(&mut ops, &mut judge_bundle, bundle);

        assert!(!ops.pending_judge_bundle);
        assert!(judge_bundle.is_some());
        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Workshop archive ready.")
        );
    }

    #[test]
    fn auth_errors_clear_stale_session_snapshot() {
        let mut identity = default_identity_state();
        identity.screen = ShellScreen::Session;
        identity.realtime_bootstrap_attempted = true;
        identity.identity = Some(SessionIdentity {
            session_code: "123456".to_string(),
            player_id: "player-1".to_string(),
            reconnect_token: "reconnect-1".to_string(),
        });
        identity.session_snapshot = Some(ClientSessionSnapshot {
            session_code: "123456".to_string(),
            reconnect_token: "reconnect-1".to_string(),
            player_id: "player-1".to_string(),
            coordinator_type: CoordinatorType::Rust,
        });
        let mut ops = default_operation_state();
        ops.pending_command = Some(SessionCommand::StartPhase1);

        apply_command_error(
            &mut identity,
            &mut ops,
            "Session identity is invalid or expired.".to_string(),
        );

        assert_eq!(identity.screen, ShellScreen::Home);
        assert_eq!(identity.identity, None);
        assert_eq!(identity.session_snapshot, None);
        assert!(!identity.realtime_bootstrap_attempted);
        assert_eq!(ops.pending_command, None);
    }

    #[test]
    fn realtime_bootstrap_error_marks_attempted_even_when_connect_fails_early() {
        let mut identity = default_identity_state();
        let mut ops = default_operation_state();

        apply_realtime_bootstrap_error(
            &mut identity,
            &mut ops,
            "failed to open session connection".to_string(),
        );

        assert!(identity.realtime_bootstrap_attempted);
        assert_eq!(identity.connection_status, ConnectionStatus::Offline);
        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("failed to open session connection")
        );
    }

    #[test]
    fn server_ws_state_update_promotes_to_connected_realtime() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut join_session_code = String::new();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut join_session_code,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &mut judge_bundle,
            mock_join_success(),
            PendingFlow::Join,
        );
        identity.connection_status = ConnectionStatus::Connecting;
        ops.pending_command = Some(SessionCommand::StartPhase1);

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::StateUpdate(mock_join_success().state),
        );

        assert_eq!(identity.connection_status, ConnectionStatus::Connected);
        assert_eq!(ops.pending_command, None);
        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Session synced.")
        );
    }

    #[test]
    fn server_ws_phase_update_confirms_pending_transition_command() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut join_session_code = String::new();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut join_session_code,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &mut judge_bundle,
            mock_join_success(),
            PendingFlow::Join,
        );

        ops.pending_command = Some(SessionCommand::StartPhase0);
        ops.notice = Some(info_notice("Opening character creation…"));

        let mut next_state = mock_join_success().state;
        next_state.phase = Phase::Phase0;

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::StateUpdate(next_state),
        );

        assert_eq!(identity.connection_status, ConnectionStatus::Connected);
        assert_eq!(ops.pending_command, None);
        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Character creation opened.")
        );
        assert_eq!(
            game_state.as_ref().map(|state| state.phase),
            Some(Phase::Phase0)
        );
    }

    #[test]
    fn server_ws_first_attach_after_reconnect_preserves_reconnect_notice() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut join_session_code = String::new();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut join_session_code,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &mut judge_bundle,
            mock_join_success(),
            PendingFlow::Reconnect,
        );

        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Reconnected to workshop.")
        );
        assert!(
            ops.pending_realtime_notice.is_some(),
            "reconnect flow must queue a pending realtime notice"
        );

        // Simulate realtime bootstrap overwriting the notice.
        apply_realtime_connecting(&mut identity, &mut ops);
        assert_eq!(identity.connection_status, ConnectionStatus::Connecting);

        // First WS attach should use the pending reconnect notice, not
        // the default "Session synced." message.
        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::StateUpdate(mock_join_success().state),
        );

        assert_eq!(identity.connection_status, ConnectionStatus::Connected);
        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Reconnected to workshop.")
        );
        assert_eq!(
            ops.notice.as_ref().map(|n| n.tone),
            Some(NoticeTone::Success)
        );
        assert!(
            ops.pending_realtime_notice.is_none(),
            "pending realtime notice must be consumed after first attach"
        );
    }

    #[test]
    fn server_ws_notice_maps_protocol_notice_to_shell_notice() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut judge_bundle = None;

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::Notice(ProtocolSessionNotice {
                level: NoticeLevel::Success,
                title: "Saved".to_string(),
                message: "Workshop updated".to_string(),
            }),
        );

        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Saved: Workshop updated")
        );
        assert_eq!(
            ops.notice.as_ref().map(|n| n.tone),
            Some(NoticeTone::Success)
        );
    }
}
