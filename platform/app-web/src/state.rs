#![allow(clippy::too_many_arguments)]

use protocol::{
    AccountProfile, CharacterProfile, ClientGameState, ClientSessionSnapshot, CoordinatorType,
    JudgeBundle, NoticeLevel, OpenWorkshopCursor, OpenWorkshopSummary, Phase, ServerWsMessage,
    SessionCommand, SessionNotice as ProtocolSessionNotice, SessionNoticeCode, WorkshopJoinSuccess,
};

use crate::api::build_client_session_snapshot;
use crate::realtime::disconnect_realtime;

#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;

#[cfg(target_arch = "wasm32")]
std::thread_local! {
    static PENDING_SESSION_GAME_STATE_PERSIST: RefCell<Option<ClientGameState>> = const { RefCell::new(None) };
    static SESSION_GAME_STATE_PERSIST_TIMER: RefCell<Option<gloo_timers::callback::Timeout>> = const { RefCell::new(None) };
}

#[cfg(target_arch = "wasm32")]
const SESSION_GAME_STATE_PERSIST_DEBOUNCE_MS: u32 = 500;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellScreen {
    SignIn,
    AccountHome,
    CreateCharacter,
    PickCharacter { workshop_code: String },
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
    SignIn,
    Create,
    Join,
    Resume,
    Review,
    Reconnect,
    DeleteCharacter,
    RenameCharacter,
    UpdateWorkshop,
    DeleteWorkshop,
    Logout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeTone {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SpriteGenerationStage {
    Preparing,
    Queued,
    Drawing,
    Fallback,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellNotice {
    pub tone: NoticeTone,
    pub message: String,
    pub scope: NoticeScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeScope {
    SignIn,
    AccountHome,
    CreateCharacter,
    PickCharacter,
    Session,
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
    pub account: Option<AccountProfile>,
    pub identity: Option<SessionIdentity>,
    pub session_snapshot: Option<ClientSessionSnapshot>,
    pub api_base_url: String,
    pub realtime_bootstrap_attempted: bool,
    pub restored_session_needs_realtime: bool,
}

/// Transient operation state — changes on command send/receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationState {
    pub pending_flow: Option<PendingFlow>,
    pub pending_command: Option<SessionCommand>,
    pub pending_flow_generation: u64,
    pub pending_command_generation: u64,
    pub my_characters_request_generation: u64,
    pub open_workshops_request_generation: u64,
    pub eligible_characters_request_generation: u64,
    pub pending_judge_bundle: bool,
    pub pending_judge_bundle_generation: u64,
    pub sprite_generation_request_pending: bool,
    pub sprite_generation_stage: Option<SpriteGenerationStage>,
    pub selected_character_id: Option<String>,
    pub my_characters_loading: bool,
    pub my_characters_loaded: bool,
    pub my_characters_load_failed: bool,
    pub my_characters: Vec<CharacterProfile>,
    pub my_characters_limit: u8,
    pub open_workshops_loading: bool,
    pub open_workshops_loaded: bool,
    pub open_workshops_load_failed: bool,
    pub open_workshops: Vec<OpenWorkshopSummary>,
    /// Cursor to use for the "Next" (older) pager button; `None` disables it.
    pub open_workshops_next_cursor: Option<OpenWorkshopCursor>,
    /// Cursor to use for the "Prev" (newer) pager button; `None` disables it.
    pub open_workshops_prev_cursor: Option<OpenWorkshopCursor>,
    pub eligible_characters_loading: bool,
    pub eligible_characters_loaded: bool,
    pub eligible_characters_load_failed: bool,
    pub eligible_characters_workshop_code: Option<String>,
    pub eligible_characters: Vec<CharacterProfile>,
    pub notice: Option<ShellNotice>,
    /// Notice to show on the first realtime attach instead of the default
    /// "Session synced." message.  Set by `apply_join_success` for
    /// flow-specific notices (e.g. "Reconnected to workshop.") that would
    /// otherwise be overwritten by the realtime bootstrap sequence.
    pub pending_realtime_notice: Option<ShellNotice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFlowTicket {
    flow: PendingFlow,
    generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCommandTicket {
    command: SessionCommand,
    generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingJudgeBundleTicket {
    generation: u64,
}

// ---------------------------------------------------------------------------
// Notice helpers
// ---------------------------------------------------------------------------

pub fn info_notice(message: &str) -> ShellNotice {
    ShellNotice {
        tone: NoticeTone::Info,
        message: message.to_string(),
        scope: NoticeScope::Session,
    }
}

pub fn success_notice(message: &str) -> ShellNotice {
    ShellNotice {
        tone: NoticeTone::Success,
        message: message.to_string(),
        scope: NoticeScope::Session,
    }
}

pub fn warning_notice(message: &str) -> ShellNotice {
    ShellNotice {
        tone: NoticeTone::Warning,
        message: message.to_string(),
        scope: NoticeScope::Session,
    }
}

pub fn error_notice(message: &str) -> ShellNotice {
    ShellNotice {
        tone: NoticeTone::Error,
        message: message.to_string(),
        scope: NoticeScope::Session,
    }
}

pub fn scoped_notice(scope: NoticeScope, mut notice: ShellNotice) -> ShellNotice {
    notice.scope = scope;
    notice
}

fn next_submission_generation(generation: u64) -> u64 {
    generation.checked_add(1).unwrap_or(1).max(1)
}

pub fn reserve_pending_flow(
    ops: &mut OperationState,
    flow: PendingFlow,
    notice: ShellNotice,
) -> Option<PendingFlowTicket> {
    if ops.pending_flow.is_some() || ops.pending_command.is_some() || ops.pending_judge_bundle {
        return None;
    }
    ops.pending_flow_generation = next_submission_generation(ops.pending_flow_generation);
    ops.pending_flow = Some(flow);
    ops.notice = Some(notice);
    Some(PendingFlowTicket {
        flow,
        generation: ops.pending_flow_generation,
    })
}

pub fn pending_flow_ticket_is_current(ops: &OperationState, ticket: &PendingFlowTicket) -> bool {
    ops.pending_flow == Some(ticket.flow) && ops.pending_flow_generation == ticket.generation
}

pub fn clear_pending_flow_if_current(ops: &mut OperationState, ticket: &PendingFlowTicket) {
    if pending_flow_ticket_is_current(ops, ticket) {
        ops.pending_flow = None;
    }
}

pub fn reserve_pending_command(
    ops: &mut OperationState,
    command: SessionCommand,
    notice: ShellNotice,
) -> Option<PendingCommandTicket> {
    if ops.pending_flow.is_some() || ops.pending_command.is_some() || ops.pending_judge_bundle {
        return None;
    }
    ops.pending_command_generation = next_submission_generation(ops.pending_command_generation);
    ops.pending_command = Some(command);
    ops.notice = Some(notice);
    Some(PendingCommandTicket {
        command,
        generation: ops.pending_command_generation,
    })
}

pub fn pending_command_ticket_is_current(
    ops: &OperationState,
    ticket: &PendingCommandTicket,
) -> bool {
    ops.pending_command == Some(ticket.command)
        && ops.pending_command_generation == ticket.generation
}

pub fn clear_pending_command_if_current(ops: &mut OperationState, ticket: &PendingCommandTicket) {
    if pending_command_ticket_is_current(ops, ticket) {
        ops.pending_command = None;
    }
}

pub fn reserve_pending_judge_bundle(
    ops: &mut OperationState,
    notice: ShellNotice,
) -> Option<PendingJudgeBundleTicket> {
    if ops.pending_flow.is_some() || ops.pending_command.is_some() || ops.pending_judge_bundle {
        return None;
    }
    ops.pending_judge_bundle_generation =
        next_submission_generation(ops.pending_judge_bundle_generation);
    ops.pending_judge_bundle = true;
    ops.notice = Some(notice);
    Some(PendingJudgeBundleTicket {
        generation: ops.pending_judge_bundle_generation,
    })
}

pub fn pending_judge_bundle_ticket_is_current(
    ops: &OperationState,
    ticket: &PendingJudgeBundleTicket,
) -> bool {
    ops.pending_judge_bundle && ops.pending_judge_bundle_generation == ticket.generation
}

pub fn clear_pending_judge_bundle_if_current(
    ops: &mut OperationState,
    ticket: &PendingJudgeBundleTicket,
) {
    if pending_judge_bundle_ticket_is_current(ops, ticket) {
        ops.pending_judge_bundle = false;
    }
}

pub fn notice_scope_for_screen(screen: &ShellScreen) -> NoticeScope {
    match screen {
        ShellScreen::SignIn => NoticeScope::SignIn,
        ShellScreen::AccountHome => NoticeScope::AccountHome,
        ShellScreen::CreateCharacter => NoticeScope::CreateCharacter,
        ShellScreen::PickCharacter { .. } => NoticeScope::PickCharacter,
        ShellScreen::Session => NoticeScope::Session,
    }
}

fn scope_notice_for_identity(identity: &IdentityState, notice: ShellNotice) -> ShellNotice {
    scoped_notice(notice_scope_for_screen(&identity.screen), notice)
}

#[cfg_attr(not(test), allow(dead_code))]
fn map_notice_tone(level: NoticeLevel) -> NoticeTone {
    match level {
        NoticeLevel::Info => NoticeTone::Info,
        NoticeLevel::Success => NoticeTone::Success,
        NoticeLevel::Warning => NoticeTone::Warning,
        NoticeLevel::Error => NoticeTone::Error,
    }
}

fn sprite_generation_stage_from_notice(
    code: Option<SessionNoticeCode>,
) -> Option<SpriteGenerationStage> {
    match code {
        Some(SessionNoticeCode::SpriteAtelierAccepted) => Some(SpriteGenerationStage::Preparing),
        Some(SessionNoticeCode::SpriteAtelierQueued) => Some(SpriteGenerationStage::Queued),
        Some(SessionNoticeCode::SpriteAtelierDrawing) => Some(SpriteGenerationStage::Drawing),
        Some(SessionNoticeCode::SpriteAtelierFallback) => Some(SpriteGenerationStage::Fallback),
        _ => None,
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
        screen: ShellScreen::SignIn,
        connection_status: ConnectionStatus::Offline,
        coordinator: CoordinatorType::Rust,
        account: None,
        identity: None,
        session_snapshot: None,
        api_base_url: default_api_base_url(),
        realtime_bootstrap_attempted: false,
        restored_session_needs_realtime: false,
    }
}

pub fn default_operation_state() -> OperationState {
    OperationState {
        pending_flow: None,
        pending_command: None,
        pending_flow_generation: 0,
        pending_command_generation: 0,
        my_characters_request_generation: 0,
        open_workshops_request_generation: 0,
        eligible_characters_request_generation: 0,
        pending_judge_bundle: false,
        pending_judge_bundle_generation: 0,
        sprite_generation_request_pending: false,
        sprite_generation_stage: None,
        selected_character_id: None,
        my_characters_loading: false,
        my_characters_loaded: false,
        my_characters_load_failed: false,
        my_characters: Vec::new(),
        my_characters_limit: 5,
        open_workshops_loading: false,
        open_workshops_loaded: false,
        open_workshops_load_failed: false,
        open_workshops: Vec::new(),
        open_workshops_next_cursor: None,
        open_workshops_prev_cursor: None,
        eligible_characters_loading: false,
        eligible_characters_loaded: false,
        eligible_characters_load_failed: false,
        eligible_characters_workshop_code: None,
        eligible_characters: Vec::new(),
        notice: None,
        pending_realtime_notice: None,
    }
}

pub fn hydrate_from_snapshot(
    identity: &mut IdentityState,
    reconnect_session_code: &mut String,
    reconnect_token: &mut String,
    snapshot: &ClientSessionSnapshot,
) {
    identity.screen = ShellScreen::Session;
    identity.connection_status = ConnectionStatus::Connecting;
    identity.restored_session_needs_realtime = true;
    identity.coordinator = snapshot.coordinator_type;
    identity.identity = Some(SessionIdentity {
        session_code: snapshot.session_code.clone(),
        player_id: snapshot.player_id.clone(),
        reconnect_token: snapshot.reconnect_token.clone(),
    });
    identity.session_snapshot = Some(snapshot.clone());
    *reconnect_session_code = snapshot.session_code.clone();
    *reconnect_token = snapshot.reconnect_token.clone();
}

#[derive(Clone)]
pub struct BootstrapResult {
    pub identity: IdentityState,
    pub game_state: Option<ClientGameState>,
    pub reconnect_session_code: String,
    pub reconnect_token: String,
    pub handover_tags_input: String,
    pub ops: OperationState,
    pub judge_bundle: Option<JudgeBundle>,
}

pub fn restore_bootstrap(
    account: Option<AccountProfile>,
    snapshot: Option<ClientSessionSnapshot>,
    restored_game_state: Option<ClientGameState>,
) -> BootstrapResult {
    let mut identity = default_identity_state();
    let mut reconnect_session_code = String::new();
    let mut reconnect_token = String::new();
    let handover_tags_input = String::new();
    let mut ops = default_operation_state();
    let mut game_state = None;

    match (&account, &snapshot) {
        (Some(_), Some(snapshot)) => {
            identity.account = account;
            hydrate_from_snapshot(
                &mut identity,
                &mut reconnect_session_code,
                &mut reconnect_token,
                snapshot,
            );
            game_state = restored_game_state.filter(|state| {
                state.session.code == snapshot.session_code
                    && state.current_player_id.as_deref() == Some(snapshot.player_id.as_str())
            });
            ops.notice = Some(scoped_notice(
                NoticeScope::Session,
                info_notice("Syncing session…"),
            ));
        }
        (Some(_), None) => {
            identity.account = account;
            identity.screen = ShellScreen::AccountHome;
        }
        _ => {
            // No account snapshot → SignIn (the default)
        }
    }

    BootstrapResult {
        identity,
        game_state,
        reconnect_session_code,
        reconnect_token,
        handover_tags_input,
        ops,
        judge_bundle: None,
    }
}

fn resolve_bootstrap_api_base_url(
    current_origin_api_base_url: &str,
    query_api_base_url: Option<&str>,
    saved_api_base_url: Option<&str>,
) -> String {
    if let Some(api_base_url) = query_api_base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return api_base_url.to_string();
    }

    if current_origin_api_base_url.trim().is_empty()
        && let Some(api_base_url) = saved_api_base_url
            .map(str::trim)
            .filter(|value| !value.is_empty())
    {
        return api_base_url.to_string();
    }

    current_origin_api_base_url.trim().to_string()
}

pub fn bootstrap_state() -> BootstrapResult {
    let account = load_browser_account_snapshot().ok().flatten();
    let session = load_browser_session_snapshot();
    let (restored_game_state, restored_game_state_warning) = match load_browser_session_game_state()
    {
        Ok(state) => (state, None),
        Err(error) => {
            let _ = clear_browser_session_game_state();
            (
                None,
                Some(warning_notice(&format!(
                    "Failed to restore browser session state: {error}"
                ))),
            )
        }
    };
    let had_restored_game_state = restored_game_state.is_some();

    let mut result = match session {
        Ok(snapshot) => restore_bootstrap(account, snapshot, restored_game_state),
        Err(error) => {
            let _ = clear_browser_session_snapshot();
            let mut result = restore_bootstrap(account, None, None);
            result.ops.notice = Some(scoped_notice(
                notice_scope_for_screen(&result.identity.screen),
                error_notice(&format!("Failed to restore browser session: {error}")),
            ));
            result
        }
    };

    if had_restored_game_state && result.game_state.is_none() {
        let _ = clear_browser_session_game_state();
    }

    // Hidden localStorage overrides made the served page and API target drift
    // apart. Only an explicit `?apiBaseUrl=` override should cross origins.
    let current_origin_api_base_url = result.identity.api_base_url.clone();
    let query_api_base_url = load_browser_query_api_base_url().ok().flatten();
    let saved_api_base_url = load_browser_api_base_url().ok().flatten();

    result.identity.api_base_url = resolve_bootstrap_api_base_url(
        &current_origin_api_base_url,
        query_api_base_url.as_deref(),
        saved_api_base_url.as_deref(),
    );

    if query_api_base_url.is_some() {
        let _ = persist_browser_api_base_url(&result.identity.api_base_url);
    }

    if result.ops.notice.is_none()
        && let Some(warning) = restored_game_state_warning
    {
        result.ops.notice = Some(scoped_notice(
            notice_scope_for_screen(&result.identity.screen),
            warning,
        ));
    }

    result
}

// ---------------------------------------------------------------------------
// Browser persistence
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub const SESSION_SNAPSHOT_STORAGE_KEY: &str = "dragon-switch/platform/session-snapshot";

#[allow(dead_code)]
pub const SESSION_GAME_STATE_STORAGE_KEY: &str = "dragon-switch/platform/session-game-state";

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

#[allow(dead_code)]
pub fn encode_session_game_state(state: &ClientGameState) -> Result<String, String> {
    serde_json::to_string(state)
        .map_err(|error| format!("failed to encode session game state: {error}"))
}

#[allow(dead_code)]
pub fn decode_session_game_state(value: &str) -> Result<ClientGameState, String> {
    serde_json::from_str(value)
        .map_err(|error| format!("failed to decode session game state: {error}"))
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
pub fn load_browser_session_game_state() -> Result<Option<ClientGameState>, String> {
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let storage = window
        .session_storage()
        .map_err(|_| "failed to access browser storage".to_string())?
        .ok_or_else(|| "browser storage is unavailable".to_string())?;

    let Some(encoded) = storage
        .get_item(SESSION_GAME_STATE_STORAGE_KEY)
        .map_err(|_| "failed to read browser storage".to_string())?
    else {
        return Ok(None);
    };

    decode_session_game_state(&encoded).map(Some)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load_browser_session_game_state() -> Result<Option<ClientGameState>, String> {
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
pub fn persist_browser_session_game_state(state: &ClientGameState) -> Result<(), String> {
    cancel_pending_browser_session_game_state_persist();
    persist_browser_session_game_state_now(state)
}

#[cfg(target_arch = "wasm32")]
fn persist_browser_session_game_state_now(state: &ClientGameState) -> Result<(), String> {
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let storage = window
        .session_storage()
        .map_err(|_| "failed to access browser storage".to_string())?
        .ok_or_else(|| "browser storage is unavailable".to_string())?;
    let encoded = encode_session_game_state(state)?;
    storage
        .set_item(SESSION_GAME_STATE_STORAGE_KEY, &encoded)
        .map_err(|_| "failed to persist browser session state".to_string())
}

#[cfg(target_arch = "wasm32")]
pub fn persist_browser_session_game_state_debounced(state: &ClientGameState) -> Result<(), String> {
    PENDING_SESSION_GAME_STATE_PERSIST.with(|pending| {
        pending.borrow_mut().replace(state.clone());
    });
    SESSION_GAME_STATE_PERSIST_TIMER.with(|timer| {
        timer.borrow_mut().take();
        let timeout = gloo_timers::callback::Timeout::new(
            SESSION_GAME_STATE_PERSIST_DEBOUNCE_MS,
            move || {
                let pending_state =
                    PENDING_SESSION_GAME_STATE_PERSIST.with(|pending| pending.borrow_mut().take());
                SESSION_GAME_STATE_PERSIST_TIMER.with(|timer| {
                    timer.borrow_mut().take();
                });
                if let Some(state) = pending_state {
                    let _ = persist_browser_session_game_state_now(&state);
                }
            },
        );
        timer.borrow_mut().replace(timeout);
        Ok(())
    })
}

#[cfg(target_arch = "wasm32")]
fn cancel_pending_browser_session_game_state_persist() {
    PENDING_SESSION_GAME_STATE_PERSIST.with(|pending| {
        pending.borrow_mut().take();
    });
    SESSION_GAME_STATE_PERSIST_TIMER.with(|timer| {
        timer.borrow_mut().take();
    });
}

#[cfg(not(target_arch = "wasm32"))]
pub fn persist_browser_session_game_state(state: &ClientGameState) -> Result<(), String> {
    let _ = state;
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn persist_browser_session_game_state_debounced(state: &ClientGameState) -> Result<(), String> {
    let _ = state;
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
pub fn clear_browser_session_game_state() -> Result<(), String> {
    cancel_pending_browser_session_game_state_persist();
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let storage = window
        .session_storage()
        .map_err(|_| "failed to access browser storage".to_string())?
        .ok_or_else(|| "browser storage is unavailable".to_string())?;
    storage
        .remove_item(SESSION_GAME_STATE_STORAGE_KEY)
        .map_err(|_| "failed to clear browser session state".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn clear_browser_session_game_state() -> Result<(), String> {
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
// Account snapshot persistence (localStorage)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub const ACCOUNT_SNAPSHOT_STORAGE_KEY: &str = "dragon-switch/platform/account-snapshot";

#[allow(dead_code)]
pub fn encode_account_snapshot(profile: &AccountProfile) -> Result<String, String> {
    serde_json::to_string(profile)
        .map_err(|error| format!("failed to encode account snapshot: {error}"))
}

#[allow(dead_code)]
pub fn decode_account_snapshot(value: &str) -> Result<AccountProfile, String> {
    serde_json::from_str(value)
        .map_err(|error| format!("failed to decode account snapshot: {error}"))
}

#[cfg(target_arch = "wasm32")]
pub fn load_browser_account_snapshot() -> Result<Option<AccountProfile>, String> {
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let storage = window
        .local_storage()
        .map_err(|_| "failed to access browser storage".to_string())?
        .ok_or_else(|| "browser storage is unavailable".to_string())?;

    let Some(encoded) = storage
        .get_item(ACCOUNT_SNAPSHOT_STORAGE_KEY)
        .map_err(|_| "failed to read browser storage".to_string())?
    else {
        return Ok(None);
    };

    decode_account_snapshot(&encoded).map(Some)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load_browser_account_snapshot() -> Result<Option<AccountProfile>, String> {
    Ok(None)
}

#[cfg(target_arch = "wasm32")]
pub fn persist_browser_account_snapshot(profile: &AccountProfile) -> Result<(), String> {
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let storage = window
        .local_storage()
        .map_err(|_| "failed to access browser storage".to_string())?
        .ok_or_else(|| "browser storage is unavailable".to_string())?;
    let encoded = encode_account_snapshot(profile)?;
    storage
        .set_item(ACCOUNT_SNAPSHOT_STORAGE_KEY, &encoded)
        .map_err(|_| "failed to persist account snapshot".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn persist_browser_account_snapshot(profile: &AccountProfile) -> Result<(), String> {
    let _ = profile;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub fn clear_browser_account_snapshot() -> Result<(), String> {
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let storage = window
        .local_storage()
        .map_err(|_| "failed to access browser storage".to_string())?
        .ok_or_else(|| "browser storage is unavailable".to_string())?;
    storage
        .remove_item(ACCOUNT_SNAPSHOT_STORAGE_KEY)
        .map_err(|_| "failed to clear account snapshot".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn clear_browser_account_snapshot() -> Result<(), String> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Mutation functions
// ---------------------------------------------------------------------------

pub fn apply_join_success(
    identity: &mut IdentityState,
    game_state: &mut Option<ClientGameState>,
    ops: &mut OperationState,
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
        PendingFlow::Resume => "Resumed workshop.",
        PendingFlow::Review => "Workshop review opened.",
        PendingFlow::Reconnect => "Reconnected to workshop.",
        PendingFlow::DeleteCharacter => "Character deleted.",
        PendingFlow::RenameCharacter => "Character renamed.",
        PendingFlow::DeleteWorkshop => "Workshop deleted.",
        PendingFlow::UpdateWorkshop => "Workshop updated.",
        PendingFlow::SignIn => "Workshop created.",
        PendingFlow::Logout => "Signed out.",
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

    ops.selected_character_id = game_state.as_ref().and_then(|state| {
        state
            .current_player_id
            .as_ref()
            .and_then(|player_id| state.players.get(player_id))
            .and_then(|player| player.character_id.clone())
    });
    if !matches!(flow, PendingFlow::Reconnect | PendingFlow::Resume) {
        ops.my_characters.clear();
    }

    *reconnect_session_code = snapshot.session_code.clone();
    *reconnect_token = snapshot.reconnect_token.clone();

    ops.pending_flow = None;
    ops.pending_judge_bundle = false;
    ops.notice = Some(scoped_notice(
        NoticeScope::Session,
        success_notice(success_message),
    ));
    ops.pending_realtime_notice = match flow {
        PendingFlow::Resume | PendingFlow::Review | PendingFlow::Reconnect => Some(scoped_notice(
            NoticeScope::Session,
            success_notice(success_message),
        )),
        _ => None,
    };
}

pub fn apply_request_error(identity: &mut IdentityState, ops: &mut OperationState, error: String) {
    identity.connection_status = ConnectionStatus::Offline;
    ops.pending_flow = None;
    if should_clear_session_snapshot(&error) {
        clear_session_identity(identity);
    }
    ops.notice = Some(scope_notice_for_identity(identity, error_notice(&error)));
}

pub fn command_success_message(command: SessionCommand) -> &'static str {
    match command {
        SessionCommand::SelectCharacter => "Dragon profile saved.",
        SessionCommand::StartPhase1 => "Phase 1 started.",
        SessionCommand::SubmitObservation => "Observation saved.",
        SessionCommand::StartHandover => "Handover started.",
        SessionCommand::SubmitTags => "Handover tags saved.",
        SessionCommand::StartPhase2 => "Phase 2 started.",
        SessionCommand::EndGame => "Scoring opened.",
        SessionCommand::StartVoting => "Design voting started.",
        SessionCommand::RevealVotingResults => "Voting finished.",
        SessionCommand::ResetGame => "Workshop reset.",
        SessionCommand::EndSession => "Game over ready.",
        _ => "Command sent.",
    }
}

fn command_completed_by_state_update(command: SessionCommand, state: &ClientGameState) -> bool {
    match command {
        SessionCommand::StartPhase1 => state.phase == Phase::Phase1,
        SessionCommand::StartHandover => state.phase == Phase::Handover,
        SessionCommand::StartPhase2 => state.phase == Phase::Phase2,
        SessionCommand::EndGame | SessionCommand::StartVoting => state.phase == Phase::Voting,
        SessionCommand::RevealVotingResults => state
            .voting
            .as_ref()
            .is_some_and(|voting| voting.results_revealed),
        SessionCommand::EndSession => state.phase == Phase::End,
        SessionCommand::ResetGame => state.phase == Phase::Lobby,
        _ => false,
    }
}

fn should_apply_state_update(
    identity: &IdentityState,
    current_state: Option<&ClientGameState>,
    next_state: &ClientGameState,
) -> bool {
    let Some(snapshot) = identity.session_snapshot.as_ref() else {
        return false;
    };
    if next_state.session.code != snapshot.session_code
        || next_state.current_player_id.as_deref() != Some(snapshot.player_id.as_str())
    {
        return false;
    }

    if let Some(current_state) = current_state {
        if current_state.session.code == next_state.session.code
            && current_state.current_player_id == next_state.current_player_id
            && next_state.session.state_revision < current_state.session.state_revision
        {
            return false;
        }
    }

    true
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
    if command == SessionCommand::SelectCharacter {
        ops.sprite_generation_request_pending = false;
        ops.sprite_generation_stage = None;
    }
    ops.notice = Some(scoped_notice(
        NoticeScope::Session,
        success_notice(command_success_message(command)),
    ));
}

pub fn apply_command_error(identity: &mut IdentityState, ops: &mut OperationState, error: String) {
    ops.pending_command = None;
    if should_clear_session_snapshot(&error) {
        clear_session_identity(identity);
    }
    ops.notice = Some(scope_notice_for_identity(identity, error_notice(&error)));
}

pub fn apply_judge_bundle_success(
    ops: &mut OperationState,
    judge_bundle: &mut Option<JudgeBundle>,
    bundle: JudgeBundle,
) {
    ops.pending_judge_bundle = false;
    *judge_bundle = Some(bundle);
    ops.notice = Some(scoped_notice(
        NoticeScope::Session,
        success_notice("Workshop archive ready."),
    ));
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
    ops.notice = Some(scope_notice_for_identity(identity, error_notice(&error)));
}

#[allow(dead_code)]
pub fn apply_realtime_bootstrap_error(
    identity: &mut IdentityState,
    game_state: &mut Option<ClientGameState>,
    ops: &mut OperationState,
    error: String,
) {
    identity.realtime_bootstrap_attempted = true;
    identity.connection_status = ConnectionStatus::Offline;
    if identity.restored_session_needs_realtime {
        *game_state = None;
        let _ = clear_browser_session_game_state();
    }
    if should_clear_session_snapshot(&error) {
        clear_session_identity(identity);
    }
    ops.notice = Some(scope_notice_for_identity(identity, error_notice(&error)));
}

#[allow(dead_code)]
pub fn apply_realtime_connecting(identity: &mut IdentityState, ops: &mut OperationState) {
    identity.realtime_bootstrap_attempted = true;
    identity.connection_status = ConnectionStatus::Connecting;
    ops.notice = Some(scoped_notice(
        NoticeScope::Session,
        info_notice("Syncing session…"),
    ));
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
    disconnect_realtime();
    identity.screen = ShellScreen::AccountHome;
    identity.connection_status = ConnectionStatus::Offline;
    identity.identity = None;
    identity.session_snapshot = None;
    identity.realtime_bootstrap_attempted = false;
    identity.restored_session_needs_realtime = false;
    let _ = clear_browser_session_snapshot();
    let _ = clear_browser_session_game_state();
}

pub fn clear_pre_session_caches(ops: &mut OperationState) {
    ops.my_characters_request_generation = ops
        .my_characters_request_generation
        .checked_add(1)
        .unwrap_or(1)
        .max(1);
    ops.open_workshops_request_generation = ops
        .open_workshops_request_generation
        .checked_add(1)
        .unwrap_or(1)
        .max(1);
    ops.eligible_characters_request_generation = ops
        .eligible_characters_request_generation
        .checked_add(1)
        .unwrap_or(1)
        .max(1);
    ops.my_characters_loading = false;
    ops.my_characters_loaded = false;
    ops.my_characters_load_failed = false;
    ops.my_characters.clear();
    ops.my_characters_limit = 5;
    ops.open_workshops_loading = false;
    ops.open_workshops_loaded = false;
    ops.open_workshops_load_failed = false;
    ops.open_workshops.clear();
    ops.open_workshops_next_cursor = None;
    ops.open_workshops_prev_cursor = None;
    ops.eligible_characters_loading = false;
    ops.eligible_characters_loaded = false;
    ops.eligible_characters_load_failed = false;
    ops.eligible_characters_workshop_code = None;
    ops.eligible_characters.clear();
}

pub fn navigate_to_screen(
    identity: &mut IdentityState,
    ops: &mut OperationState,
    screen: ShellScreen,
) {
    let next_scope = notice_scope_for_screen(&screen);
    if ops
        .notice
        .as_ref()
        .is_some_and(|notice| notice.scope != next_scope)
    {
        ops.notice = None;
    }
    identity.screen = screen;
}

/// Full logout: clears session + account → goes to SignIn.
pub fn clear_account_identity(identity: &mut IdentityState) {
    clear_session_identity(identity);
    identity.screen = ShellScreen::SignIn;
    identity.account = None;
    let _ = clear_browser_account_snapshot();
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
            if !should_apply_state_update(identity, game_state.as_ref(), &client_state) {
                return;
            }
            let first_attach = identity.connection_status != ConnectionStatus::Connected;
            let phase = client_state.phase;
            let completed_pending_command = ops
                .pending_command
                .filter(|command| command_completed_by_state_update(*command, &client_state));
            identity.screen = ShellScreen::Session;
            *game_state = Some(client_state);
            if let Some(state) = game_state.as_ref() {
                let _ = persist_browser_session_game_state_debounced(state);
            }
            identity.connection_status = ConnectionStatus::Connected;
            identity.restored_session_needs_realtime = false;
            ops.selected_character_id = game_state.as_ref().and_then(|state| {
                state
                    .current_player_id
                    .as_ref()
                    .and_then(|player_id| state.players.get(player_id))
                    .and_then(|player| player.character_id.clone())
            });
            if phase != Phase::Lobby {
                ops.sprite_generation_request_pending = false;
                ops.sprite_generation_stage = None;
            }
            if phase != Phase::End {
                *judge_bundle = None;
                ops.pending_judge_bundle = false;
            }
            if first_attach {
                if let Some(command) = completed_pending_command {
                    ops.pending_command = None;
                    ops.pending_realtime_notice = None;
                    ops.notice = Some(scoped_notice(
                        NoticeScope::Session,
                        success_notice(command_success_message(command)),
                    ));
                } else {
                    ops.notice = Some(ops.pending_realtime_notice.take().unwrap_or_else(|| {
                        scoped_notice(NoticeScope::Session, info_notice("Session synced."))
                    }));
                }
            } else if let Some(command) = completed_pending_command {
                // Phase-transition commands can unmount the source component before
                // the HTTP task applies its success notice, so confirm them from the
                // resulting state update as well.
                ops.pending_command = None;
                ops.notice = Some(scoped_notice(
                    NoticeScope::Session,
                    success_notice(command_success_message(command)),
                ));
            }
        }
        ServerWsMessage::PlayerUpsert {
            state_revision,
            player,
        } => {
            let Some(state) = game_state.as_mut() else {
                return;
            };
            if state_revision < state.session.state_revision {
                return;
            }
            state.players.insert(player.id.clone(), player);
            state.session.state_revision = state.session.state_revision.max(state_revision);
        }
        ServerWsMessage::DragonPatch {
            state_revision,
            dragon,
        } => {
            let Some(state) = game_state.as_mut() else {
                return;
            };
            if state_revision < state.session.state_revision {
                return;
            }
            state.dragons.insert(dragon.id.clone(), dragon);
            state.session.state_revision = state.session.state_revision.max(state_revision);
        }
        ServerWsMessage::PhaseChanged {
            phase,
            time,
            session,
        } => {
            let Some(state) = game_state.as_mut() else {
                return;
            };
            if session.state_revision < state.session.state_revision {
                return;
            }
            state.phase = phase;
            state.time = time;
            state.session = session;
        }
        ServerWsMessage::TimeTick {
            state_revision,
            time,
        } => {
            let Some(state) = game_state.as_mut() else {
                return;
            };
            if state_revision < state.session.state_revision {
                return;
            }
            state.time = time;
            state.session.state_revision = state.session.state_revision.max(state_revision);
        }
        ServerWsMessage::Notice(ProtocolSessionNotice {
            level,
            title,
            message,
            code,
        }) => {
            if let Some(stage) = sprite_generation_stage_from_notice(code) {
                ops.sprite_generation_request_pending = false;
                ops.sprite_generation_stage = Some(stage);
            }
            let combined = if title.trim().is_empty() {
                message
            } else {
                format!("{title}: {message}")
            };
            let tone = map_notice_tone(level);
            ops.notice = Some(ShellNotice {
                tone,
                message: combined,
                scope: NoticeScope::Session,
            });
        }
        ServerWsMessage::Error { message } => {
            if identity.restored_session_needs_realtime {
                apply_realtime_bootstrap_error(identity, game_state, ops, message);
            } else {
                identity.connection_status = ConnectionStatus::Offline;
                ops.sprite_generation_request_pending = false;
                ops.sprite_generation_stage = None;
                if should_clear_session_snapshot(&message) {
                    clear_session_identity(identity);
                }
                ops.notice = Some(scope_notice_for_identity(identity, error_notice(&message)));
            }
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
        ClientDragon, ClientGameState, ClientVotingState, CoordinatorType, DragonAction,
        DragonEmotion, DragonStats, DragonVisuals, Phase, Player, SessionMeta, SessionNoticeCode,
        WorkshopJoinSuccess, create_default_session_settings,
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
                character_id: None,
                pet_description: Some("Alice's workshop dragon".to_string()),
                custom_sprites: None,
                remaining_sprite_regenerations: 1,
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
                    state_revision: 0,
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

    fn mock_client_dragon(id: &str) -> ClientDragon {
        ClientDragon {
            id: id.to_string(),
            name: "Pebble".to_string(),
            visuals: DragonVisuals {
                base: 1,
                color_p: "#112233".to_string(),
                color_s: "#445566".to_string(),
                color_a: "#778899".to_string(),
            },
            original_owner_id: Some("player-1".to_string()),
            design_creator_name: None,
            current_owner_id: Some("player-1".to_string()),
            stats: DragonStats {
                hunger: 50,
                energy: 60,
                happiness: 70,
            },
            condition_hint: None,
            discovery_observations: Vec::new(),
            handover_tags: Vec::new(),
            last_action: DragonAction::Idle,
            last_emotion: DragonEmotion::Neutral,
            speech: None,
            speech_timer: 0,
            action_cooldown: 0,
            custom_sprites: None,
            judge_observation_score: None,
            judge_care_score: None,
            judge_feedback: None,
            judge_observation_feedback: None,
            judge_care_feedback: None,
        }
    }

    #[test]
    fn default_identity_boots_signin_screen_with_rust_coordinator() {
        let identity = default_identity_state();

        assert_eq!(identity.screen, ShellScreen::SignIn);
        assert_eq!(identity.connection_status, ConnectionStatus::Offline);
        assert_eq!(identity.coordinator, CoordinatorType::Rust);
        assert_eq!(identity.account, None);
        assert_eq!(identity.identity, None);
        assert_eq!(identity.api_base_url, "");
    }

    #[test]
    fn restore_bootstrap_rehydrates_reconnect_fields_from_snapshot() {
        let account = AccountProfile {
            id: "acct-9".to_string(),
            hero: "hero-9".to_string(),
            name: "TestUser".to_string(),
        };
        let snapshot = ClientSessionSnapshot {
            session_code: "654321".to_string(),
            reconnect_token: "reconnect-9".to_string(),
            player_id: "player-9".to_string(),
            coordinator_type: CoordinatorType::Rust,
        };
        let mut restored_game_state = mock_join_success().state;
        restored_game_state.session.code = "654321".to_string();
        restored_game_state.current_player_id = Some("player-9".to_string());

        let result = restore_bootstrap(
            Some(account.clone()),
            Some(snapshot.clone()),
            Some(restored_game_state.clone()),
        );

        assert_eq!(result.identity.screen, ShellScreen::Session);
        assert_eq!(
            result.identity.connection_status,
            ConnectionStatus::Connecting
        );
        assert_eq!(result.identity.account, Some(account));
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
        assert_eq!(result.game_state, Some(restored_game_state));
        assert_eq!(
            result.ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Syncing session…")
        );
        assert!(!result.identity.realtime_bootstrap_attempted);
    }

    #[test]
    fn restore_bootstrap_discards_restored_game_state_for_different_session_code() {
        let account = AccountProfile {
            id: "acct-9".to_string(),
            hero: "hero-9".to_string(),
            name: "TestUser".to_string(),
        };
        let snapshot = ClientSessionSnapshot {
            session_code: "654321".to_string(),
            reconnect_token: "reconnect-9".to_string(),
            player_id: "player-9".to_string(),
            coordinator_type: CoordinatorType::Rust,
        };
        let mut restored_game_state = mock_join_success().state;
        restored_game_state.session.code = "123456".to_string();

        let result = restore_bootstrap(Some(account), Some(snapshot), Some(restored_game_state));

        assert_eq!(result.identity.screen, ShellScreen::Session);
        assert_eq!(result.game_state, None);
    }

    #[test]
    fn restore_bootstrap_with_account_only_goes_to_account_home() {
        let account = AccountProfile {
            id: "acct-1".to_string(),
            hero: "hero-1".to_string(),
            name: "Alice".to_string(),
        };

        let result = restore_bootstrap(Some(account.clone()), None, None);

        assert_eq!(result.identity.screen, ShellScreen::AccountHome);
        assert_eq!(result.identity.account, Some(account));
        assert_eq!(result.identity.identity, None);
        assert_eq!(result.identity.session_snapshot, None);
    }

    #[test]
    fn restore_bootstrap_without_account_goes_to_signin() {
        let result = restore_bootstrap(None, None, None);

        assert_eq!(result.identity.screen, ShellScreen::SignIn);
        assert_eq!(result.identity.account, None);
    }

    #[test]
    fn resolve_bootstrap_api_base_url_prefers_explicit_query_override() {
        assert_eq!(
            resolve_bootstrap_api_base_url(
                "http://127.0.0.1:4100",
                Some(" https://api.example.test/alt "),
                Some("http://127.0.0.1:32000"),
            ),
            "https://api.example.test/alt"
        );
    }

    #[test]
    fn resolve_bootstrap_api_base_url_ignores_saved_override_when_origin_exists() {
        assert_eq!(
            resolve_bootstrap_api_base_url(
                "http://127.0.0.1:4100",
                None,
                Some("http://127.0.0.1:32000"),
            ),
            "http://127.0.0.1:4100"
        );
    }

    #[test]
    fn resolve_bootstrap_api_base_url_uses_saved_override_when_origin_missing() {
        assert_eq!(
            resolve_bootstrap_api_base_url("", None, Some(" http://127.0.0.1:32000/ ")),
            "http://127.0.0.1:32000/"
        );
    }

    #[test]
    fn clear_session_identity_returns_to_account_home_after_character_pick() {
        let mut identity = default_identity_state();
        identity.screen = ShellScreen::PickCharacter {
            workshop_code: "123456".to_string(),
        };

        clear_session_identity(&mut identity);

        assert_eq!(identity.screen, ShellScreen::AccountHome);
        assert_eq!(identity.identity, None);
        assert_eq!(identity.session_snapshot, None);
    }

    #[test]
    fn clear_pre_session_caches_invalidates_request_generations() {
        let mut ops = default_operation_state();
        ops.my_characters_request_generation = 7;
        ops.open_workshops_request_generation = 11;
        ops.eligible_characters_request_generation = 13;

        clear_pre_session_caches(&mut ops);

        assert_eq!(ops.my_characters_request_generation, 8);
        assert_eq!(ops.open_workshops_request_generation, 12);
        assert_eq!(ops.eligible_characters_request_generation, 14);
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
    fn session_game_state_round_trips_through_json_encoding() {
        let state = mock_join_success().state;

        let encoded = encode_session_game_state(&state).expect("encode session game state");
        let decoded = decode_session_game_state(&encoded).expect("decode session game state");

        assert_eq!(decoded, state);
    }

    #[test]
    fn apply_join_success_promotes_to_connected_session() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
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
        assert_eq!(reconnect_session_code, "123456");
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
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut handover_tags_input = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
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
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut handover_tags_input = "one, two".to_string();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
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
    fn submit_observation_success_uses_specific_notice_copy() {
        let mut identity = default_identity_state();
        let mut ops = default_operation_state();
        let mut handover_tags_input = String::new();
        let mut judge_bundle = None;

        apply_successful_command(
            &mut identity,
            &mut ops,
            &mut handover_tags_input,
            &mut judge_bundle,
            SessionCommand::SubmitObservation,
        );

        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Observation saved.")
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
    fn pending_flow_reservation_is_single_flight_until_cleared() {
        let mut ops = default_operation_state();

        let first = reserve_pending_flow(
            &mut ops,
            PendingFlow::SignIn,
            scoped_notice(NoticeScope::SignIn, info_notice("Signing in…")),
        )
        .expect("first signin reservation succeeds");

        assert_eq!(ops.pending_flow, Some(PendingFlow::SignIn));
        assert!(pending_flow_ticket_is_current(&ops, &first));
        assert!(
            reserve_pending_flow(
                &mut ops,
                PendingFlow::SignIn,
                scoped_notice(NoticeScope::SignIn, info_notice("Signing in…")),
            )
            .is_none()
        );

        clear_pending_flow_if_current(&mut ops, &first);

        assert_eq!(ops.pending_flow, None);
        assert!(
            reserve_pending_flow(
                &mut ops,
                PendingFlow::SignIn,
                scoped_notice(NoticeScope::SignIn, info_notice("Signing in…")),
            )
            .is_some()
        );
    }

    #[test]
    fn pending_command_reservation_blocks_flows_until_cleared() {
        let mut ops = default_operation_state();

        let command_ticket = reserve_pending_command(
            &mut ops,
            SessionCommand::StartPhase1,
            scoped_notice(NoticeScope::Session, info_notice("Starting Phase 1…")),
        )
        .expect("first command reservation succeeds");

        assert_eq!(ops.pending_command, Some(SessionCommand::StartPhase1));
        assert!(pending_command_ticket_is_current(&ops, &command_ticket));
        assert!(
            reserve_pending_command(
                &mut ops,
                SessionCommand::ResetGame,
                scoped_notice(NoticeScope::Session, info_notice("Resetting workshop…")),
            )
            .is_none()
        );
        assert!(
            reserve_pending_flow(
                &mut ops,
                PendingFlow::Logout,
                scoped_notice(NoticeScope::AccountHome, info_notice("Signing out…")),
            )
            .is_none()
        );

        clear_pending_command_if_current(&mut ops, &command_ticket);

        assert_eq!(ops.pending_command, None);
        assert!(
            reserve_pending_flow(
                &mut ops,
                PendingFlow::Logout,
                scoped_notice(NoticeScope::AccountHome, info_notice("Signing out…")),
            )
            .is_some()
        );
    }

    #[test]
    fn stale_pending_flow_ticket_cannot_clear_newer_reservation() {
        let mut ops = default_operation_state();

        let stale = reserve_pending_flow(
            &mut ops,
            PendingFlow::Create,
            scoped_notice(NoticeScope::AccountHome, info_notice("Creating workshop…")),
        )
        .expect("first create reservation succeeds");
        clear_pending_flow_if_current(&mut ops, &stale);
        let current = reserve_pending_flow(
            &mut ops,
            PendingFlow::Create,
            scoped_notice(NoticeScope::AccountHome, info_notice("Creating workshop…")),
        )
        .expect("second create reservation succeeds");

        clear_pending_flow_if_current(&mut ops, &stale);

        assert_eq!(ops.pending_flow, Some(PendingFlow::Create));
        assert!(!pending_flow_ticket_is_current(&ops, &stale));
        assert!(pending_flow_ticket_is_current(&ops, &current));
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

        assert_eq!(identity.screen, ShellScreen::AccountHome);
        assert_eq!(identity.identity, None);
        assert_eq!(identity.session_snapshot, None);
        assert!(!identity.realtime_bootstrap_attempted);
        assert_eq!(ops.pending_command, None);
    }

    #[test]
    fn realtime_bootstrap_error_marks_attempted_even_when_connect_fails_early() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();

        apply_realtime_bootstrap_error(
            &mut identity,
            &mut game_state,
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
    fn realtime_bootstrap_error_clears_restored_session_state_until_reconnect_succeeds() {
        let mut identity = default_identity_state();
        identity.screen = ShellScreen::Session;
        identity.connection_status = ConnectionStatus::Connecting;
        identity.restored_session_needs_realtime = true;
        let mut game_state = Some(mock_join_success().state);
        let mut ops = default_operation_state();

        apply_realtime_bootstrap_error(
            &mut identity,
            &mut game_state,
            &mut ops,
            "Could not sync the session.".to_string(),
        );

        assert!(identity.realtime_bootstrap_attempted);
        assert_eq!(identity.connection_status, ConnectionStatus::Offline);
        assert!(identity.restored_session_needs_realtime);
        assert_eq!(game_state, None);
        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Could not sync the session.")
        );
    }

    #[test]
    fn server_ws_state_update_promotes_to_connected_realtime() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &mut judge_bundle,
            mock_join_success(),
            PendingFlow::Join,
        );
        identity.connection_status = ConnectionStatus::Connecting;
        ops.pending_command = Some(SessionCommand::SubmitObservation);

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::StateUpdate(mock_join_success().state),
        );

        assert_eq!(identity.connection_status, ConnectionStatus::Connected);
        assert_eq!(ops.pending_command, Some(SessionCommand::SubmitObservation));
        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Session synced.")
        );
    }

    #[test]
    fn server_ws_state_update_rejects_wrong_session_or_player() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &mut judge_bundle,
            mock_join_success(),
            PendingFlow::Join,
        );

        let mut wrong_session = mock_join_success().state;
        wrong_session.session.code = "654321".to_string();
        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::StateUpdate(wrong_session),
        );
        assert_eq!(
            game_state.as_ref().map(|state| state.session.code.as_str()),
            Some("123456")
        );

        let mut wrong_player = mock_join_success().state;
        wrong_player.current_player_id = Some("player-2".to_string());
        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::StateUpdate(wrong_player),
        );
        assert_eq!(
            game_state
                .as_ref()
                .and_then(|state| state.current_player_id.as_deref()),
            Some("player-1")
        );
    }

    #[test]
    fn server_ws_state_update_orders_by_revision_not_updated_at() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &mut judge_bundle,
            mock_join_success(),
            PendingFlow::Join,
        );

        let mut newer = mock_join_success().state;
        newer.session.updated_at = "2026-01-01T00:00:00Z".to_string();
        newer.session.state_revision = 10;
        newer.time = 18;
        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::StateUpdate(newer),
        );

        let mut older = mock_join_success().state;
        older.session.updated_at = "2026-01-01T00:00:10Z".to_string();
        older.session.state_revision = 9;
        older.time = 16;
        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::StateUpdate(older),
        );

        assert_eq!(game_state.as_ref().map(|state| state.time), Some(18));

        let mut later_revision_with_older_timestamp = mock_join_success().state;
        later_revision_with_older_timestamp.session.updated_at = "2026-01-01T00:00:01Z".to_string();
        later_revision_with_older_timestamp.session.state_revision = 11;
        later_revision_with_older_timestamp.time = 19;
        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::StateUpdate(later_revision_with_older_timestamp),
        );

        assert_eq!(game_state.as_ref().map(|state| state.time), Some(19));
    }

    #[test]
    fn server_ws_deltas_update_existing_game_state() {
        let mut identity = default_identity_state();
        let mut game_state = Some(mock_join_success().state);
        let mut ops = default_operation_state();
        let mut judge_bundle = None;

        let mut player = game_state
            .as_ref()
            .and_then(|state| state.players.get("player-1"))
            .cloned()
            .expect("player");
        player.score = 25;
        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::PlayerUpsert {
                state_revision: 0,
                player,
            },
        );
        assert_eq!(
            game_state
                .as_ref()
                .and_then(|state| state.players.get("player-1"))
                .map(|player| player.score),
            Some(25)
        );

        let dragon = mock_client_dragon("dragon-1");
        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::DragonPatch {
                state_revision: 1,
                dragon: dragon.clone(),
            },
        );
        assert_eq!(
            game_state
                .as_ref()
                .and_then(|state| state.dragons.get("dragon-1")),
            Some(&dragon)
        );

        let mut session = game_state.as_ref().expect("state").session.clone();
        session.state_revision = 7;
        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::PhaseChanged {
                phase: Phase::Phase1,
                time: 14,
                session,
            },
        );
        assert_eq!(
            game_state.as_ref().map(|state| state.phase),
            Some(Phase::Phase1)
        );
        assert_eq!(game_state.as_ref().map(|state| state.time), Some(14));
        assert_eq!(
            game_state
                .as_ref()
                .map(|state| state.session.state_revision),
            Some(7)
        );

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::TimeTick {
                state_revision: 7,
                time: 15,
            },
        );
        assert_eq!(game_state.as_ref().map(|state| state.time), Some(15));
    }

    #[test]
    fn server_ws_stale_deltas_are_ignored() {
        let mut identity = default_identity_state();
        let mut game_state = Some(mock_join_success().state);
        let mut ops = default_operation_state();
        let mut judge_bundle = None;
        game_state.as_mut().expect("state").session.state_revision = 5;

        let mut player = game_state
            .as_ref()
            .and_then(|state| state.players.get("player-1"))
            .cloned()
            .expect("player");
        player.score = 25;
        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::PlayerUpsert {
                state_revision: 4,
                player,
            },
        );
        assert_eq!(
            game_state
                .as_ref()
                .and_then(|state| state.players.get("player-1"))
                .map(|player| player.score),
            Some(0)
        );

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::TimeTick {
                state_revision: 4,
                time: 15,
            },
        );
        assert_eq!(game_state.as_ref().map(|state| state.time), Some(8));
    }

    #[test]
    fn server_ws_equal_and_newer_deltas_are_applied() {
        let mut identity = default_identity_state();
        let mut game_state = Some(mock_join_success().state);
        let mut ops = default_operation_state();
        let mut judge_bundle = None;
        game_state.as_mut().expect("state").session.state_revision = 5;

        let dragon = mock_client_dragon("dragon-1");
        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::DragonPatch {
                state_revision: 5,
                dragon: dragon.clone(),
            },
        );
        assert_eq!(
            game_state
                .as_ref()
                .and_then(|state| state.dragons.get("dragon-1")),
            Some(&dragon)
        );
        assert_eq!(
            game_state
                .as_ref()
                .map(|state| state.session.state_revision),
            Some(5)
        );

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::TimeTick {
                state_revision: 6,
                time: 15,
            },
        );
        assert_eq!(game_state.as_ref().map(|state| state.time), Some(15));
        assert_eq!(
            game_state
                .as_ref()
                .map(|state| state.session.state_revision),
            Some(6)
        );
    }

    #[test]
    fn server_ws_deltas_are_ignored_without_game_state() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut judge_bundle = None;

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::TimeTick {
                state_revision: 1,
                time: 15,
            },
        );

        assert!(game_state.is_none());
    }

    #[test]
    fn first_attach_phase_update_confirms_matching_pending_command() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &mut judge_bundle,
            mock_join_success(),
            PendingFlow::Join,
        );
        identity.connection_status = ConnectionStatus::Connecting;
        ops.pending_command = Some(SessionCommand::StartPhase1);
        ops.notice = Some(scoped_notice(
            NoticeScope::Session,
            info_notice("Starting Phase 1…"),
        ));

        let mut next_state = mock_join_success().state;
        next_state.phase = Phase::Phase1;

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
            Some("Phase 1 started.")
        );
    }

    #[test]
    fn voting_update_confirms_reveal_only_after_results_revealed() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &mut judge_bundle,
            mock_join_success(),
            PendingFlow::Join,
        );
        let mut next_state = mock_join_success().state;
        next_state.phase = Phase::Voting;
        next_state.voting = Some(ClientVotingState {
            eligible_count: 2,
            submitted_count: 1,
            current_player_vote_dragon_id: None,
            results_revealed: false,
            results: None,
        });
        ops.pending_command = Some(SessionCommand::RevealVotingResults);

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::StateUpdate(next_state.clone()),
        );

        assert_eq!(
            ops.pending_command,
            Some(SessionCommand::RevealVotingResults)
        );

        next_state
            .voting
            .as_mut()
            .expect("voting state")
            .results_revealed = true;
        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::StateUpdate(next_state),
        );

        assert_eq!(ops.pending_command, None);
        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Voting finished.")
        );
    }

    #[test]
    fn server_ws_phase_update_confirms_pending_start_phase1_command() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &mut judge_bundle,
            mock_join_success(),
            PendingFlow::Join,
        );

        ops.pending_command = Some(SessionCommand::StartPhase1);
        ops.notice = Some(scoped_notice(
            NoticeScope::Session,
            info_notice("Starting Phase 1…"),
        ));

        let mut next_state = mock_join_success().state;
        next_state.phase = Phase::Phase1;

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
            Some("Phase 1 started.")
        );
        assert_eq!(
            game_state.as_ref().map(|state| state.phase),
            Some(Phase::Phase1)
        );
    }

    #[test]
    fn server_ws_first_attach_after_reconnect_preserves_reconnect_notice() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
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
    fn server_ws_error_during_restored_bootstrap_clears_stale_game_state() {
        let mut identity = default_identity_state();
        identity.screen = ShellScreen::Session;
        identity.connection_status = ConnectionStatus::Connecting;
        identity.restored_session_needs_realtime = true;
        let mut game_state = Some(mock_join_success().state);
        let mut ops = default_operation_state();
        let mut judge_bundle = None;

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::Error {
                message: "Workshop not found.".to_string(),
            },
        );

        assert_eq!(identity.connection_status, ConnectionStatus::Offline);
        assert_eq!(game_state, None);
        assert_eq!(identity.screen, ShellScreen::AccountHome);
        assert_eq!(identity.identity, None);
        assert_eq!(identity.session_snapshot, None);
        assert_eq!(
            ops.notice.as_ref().map(|n| n.message.as_str()),
            Some("Workshop not found.")
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
                code: None,
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

    #[test]
    fn server_ws_notice_sets_sprite_generation_stage_from_notice_code() {
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
                level: NoticeLevel::Info,
                title: "Sprite Atelier".to_string(),
                message: "Queued".to_string(),
                code: Some(SessionNoticeCode::SpriteAtelierAccepted),
            }),
        );
        assert_eq!(
            ops.sprite_generation_stage,
            Some(SpriteGenerationStage::Preparing)
        );

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::Notice(ProtocolSessionNotice {
                level: NoticeLevel::Info,
                title: "Sprite Atelier".to_string(),
                message: "Queued".to_string(),
                code: Some(SessionNoticeCode::SpriteAtelierQueued),
            }),
        );
        assert_eq!(
            ops.sprite_generation_stage,
            Some(SpriteGenerationStage::Queued)
        );

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::Notice(ProtocolSessionNotice {
                level: NoticeLevel::Info,
                title: "Sprite Atelier".to_string(),
                message: "Drawing".to_string(),
                code: Some(SessionNoticeCode::SpriteAtelierDrawing),
            }),
        );
        assert_eq!(
            ops.sprite_generation_stage,
            Some(SpriteGenerationStage::Drawing)
        );

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::Notice(ProtocolSessionNotice {
                level: NoticeLevel::Warning,
                title: "Sprite Atelier".to_string(),
                message: "Fallback".to_string(),
                code: Some(SessionNoticeCode::SpriteAtelierFallback),
            }),
        );
        assert_eq!(
            ops.sprite_generation_stage,
            Some(SpriteGenerationStage::Fallback)
        );
    }

    #[test]
    fn server_ws_uncoded_sprite_notice_does_not_change_generation_stage() {
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
                level: NoticeLevel::Info,
                title: "Sprite Atelier".to_string(),
                message: "Queued".to_string(),
                code: None,
            }),
        );

        assert_eq!(ops.sprite_generation_stage, None);
    }

    #[test]
    fn state_update_keeps_terminal_sprite_generation_stage_visible_in_lobby() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut judge_bundle = None;
        let mut lobby_state = mock_join_success().state;
        lobby_state.phase = Phase::Lobby;
        lobby_state
            .players
            .get_mut("player-1")
            .expect("player-1")
            .custom_sprites = Some(protocol::SpriteSet {
            neutral: "n".into(),
            happy: "h".into(),
            angry: "a".into(),
            sleepy: "s".into(),
        });
        ops.sprite_generation_stage = Some(SpriteGenerationStage::Completed);

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::StateUpdate(lobby_state),
        );

        assert_eq!(
            ops.sprite_generation_stage,
            Some(SpriteGenerationStage::Completed)
        );
    }

    #[test]
    fn server_ws_state_update_ignores_cleared_session_identity() {
        let mut identity = default_identity_state();
        let mut game_state = None;
        let mut ops = default_operation_state();
        let mut reconnect_session_code = String::new();
        let mut reconnect_token = String::new();
        let mut judge_bundle = None;

        apply_join_success(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut reconnect_session_code,
            &mut reconnect_token,
            &mut judge_bundle,
            mock_join_success(),
            PendingFlow::Join,
        );
        clear_session_identity(&mut identity);
        let previous_state = game_state.clone();
        let mut stale_update = mock_join_success().state;
        stale_update.session.state_revision = stale_update.session.state_revision.saturating_add(1);

        apply_server_ws_message(
            &mut identity,
            &mut game_state,
            &mut ops,
            &mut judge_bundle,
            ServerWsMessage::StateUpdate(stale_update),
        );

        assert_eq!(identity.session_snapshot, None);
        assert_eq!(identity.connection_status, ConnectionStatus::Offline);
        assert_eq!(game_state, previous_state);
    }

    #[test]
    fn update_player_pet_success_clears_sprite_generation_stage() {
        let mut identity = default_identity_state();
        let mut ops = default_operation_state();
        let mut handover_tags_input = String::new();
        let mut judge_bundle = None;
        ops.sprite_generation_request_pending = true;
        ops.sprite_generation_stage = Some(SpriteGenerationStage::Completed);

        apply_successful_command(
            &mut identity,
            &mut ops,
            &mut handover_tags_input,
            &mut judge_bundle,
            SessionCommand::SelectCharacter,
        );

        assert!(!ops.sprite_generation_request_pending);
        assert_eq!(ops.sprite_generation_stage, None);
    }
}
