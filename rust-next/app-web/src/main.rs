use dioxus::prelude::*;
use protocol::{
    create_default_session_settings, ClientDragon, ClientGameState, ClientSessionSnapshot,
    CoordinatorType, CreateWorkshopRequest, DragonAction, DragonEmotion, JoinWorkshopRequest,
    NoticeLevel, Phase, Player, ServerWsMessage, SessionCommand, SessionEnvelope, SessionMeta,
    SessionNotice as ProtocolSessionNotice,
    WorkshopCommandRequest, WorkshopCommandResult, WorkshopJoinResult, WorkshopJoinSuccess,
};
use std::collections::BTreeMap;

#[cfg(target_arch = "wasm32")]
use protocol::ClientWsMessage;
#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{closure::Closure, JsCast};

#[allow(dead_code)]
const SESSION_SNAPSHOT_STORAGE_KEY: &str = "dragon-switch/rust-next/session-snapshot";
const APP_STYLE: &str = r#"
    :root {
        color-scheme: dark;
        font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, sans-serif;
        background: #08111f;
        color: #ecf3ff;
    }
    body {
        margin: 0;
        min-height: 100vh;
        background:
            radial-gradient(circle at top, rgba(83, 124, 255, 0.24), transparent 32%),
            linear-gradient(180deg, #08111f 0%, #101b31 100%);
    }
    .shell {
        min-height: 100vh;
        padding: 48px 24px;
    }
    .shell__container {
        max-width: 1040px;
        margin: 0 auto;
        display: grid;
        gap: 24px;
    }
    .hero, .panel {
        border: 1px solid rgba(166, 185, 255, 0.16);
        background: rgba(9, 16, 30, 0.72);
        box-shadow: 0 18px 48px rgba(0, 0, 0, 0.24);
        backdrop-filter: blur(18px);
        border-radius: 24px;
    }
    .hero {
        padding: 32px;
        display: grid;
        gap: 16px;
    }
    .hero__eyebrow {
        margin: 0;
        color: #8bb0ff;
        font-size: 14px;
        font-weight: 700;
        letter-spacing: 0.08em;
        text-transform: uppercase;
    }
    .hero__title {
        margin: 0;
        font-size: clamp(32px, 5vw, 52px);
        line-height: 1.05;
    }
    .hero__body, .panel__body, .meta {
        margin: 0;
        color: #c6d6f6;
        line-height: 1.6;
    }
    .hero__meta {
        display: flex;
        gap: 12px;
        flex-wrap: wrap;
    }
    .badge {
        display: inline-flex;
        align-items: center;
        gap: 8px;
        padding: 10px 14px;
        border-radius: 999px;
        border: 1px solid rgba(166, 185, 255, 0.16);
        background: rgba(255, 255, 255, 0.04);
        font-size: 14px;
    }
    .status-offline { color: #ffcf84; }
    .status-connecting { color: #8bb0ff; }
    .status-connected { color: #7ef0b0; }
    .grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
        gap: 24px;
    }
    .panel {
        padding: 24px;
        display: grid;
        gap: 12px;
    }
    .panel__title {
        margin: 0;
        font-size: 20px;
    }
    .panel__list {
        margin: 0;
        padding-left: 18px;
        color: #c6d6f6;
        display: grid;
        gap: 8px;
    }
    .panel__stack {
        display: grid;
        gap: 12px;
    }
    .input {
        width: 100%;
        box-sizing: border-box;
        border-radius: 14px;
        border: 1px solid rgba(166, 185, 255, 0.16);
        background: rgba(255, 255, 255, 0.04);
        color: #ecf3ff;
        padding: 12px 14px;
        font: inherit;
    }
    .button-row {
        display: flex;
        gap: 12px;
        flex-wrap: wrap;
    }
    .button {
        border: 0;
        border-radius: 14px;
        padding: 12px 16px;
        font: inherit;
        font-weight: 700;
        cursor: pointer;
        color: #08111f;
        background: #d7e4ff;
    }
    .button:disabled {
        opacity: 0.6;
        cursor: wait;
    }
    .button--primary {
        background: linear-gradient(135deg, #8bb0ff 0%, #b6c8ff 100%);
    }
    .button--secondary {
        background: rgba(255, 255, 255, 0.12);
        color: #ecf3ff;
        border: 1px solid rgba(166, 185, 255, 0.16);
    }
    .notice {
        border-radius: 16px;
        padding: 14px 16px;
        border: 1px solid rgba(166, 185, 255, 0.16);
        font-size: 14px;
    }
    .notice-info {
        background: rgba(139, 176, 255, 0.12);
        color: #cfe0ff;
    }
    .notice-success {
        background: rgba(126, 240, 176, 0.12);
        color: #b8f3d0;
    }
    .notice-error {
        background: rgba(255, 120, 120, 0.12);
        color: #ffc8c8;
    }
    .session-summary {
        display: grid;
        gap: 10px;
    }
    .roster {
        display: grid;
        gap: 10px;
    }
    .roster__item {
        border: 1px solid rgba(166, 185, 255, 0.16);
        border-radius: 16px;
        background: rgba(255, 255, 255, 0.04);
        padding: 14px 16px;
        display: flex;
        justify-content: space-between;
        gap: 12px;
        align-items: center;
    }
    .roster__name {
        margin: 0;
        font-size: 14px;
        font-weight: 700;
    }
    .roster__meta {
        margin: 4px 0 0;
        color: #9fb5df;
        font-size: 12px;
        text-transform: uppercase;
        letter-spacing: 0.08em;
    }
    .roster__status {
        font-size: 12px;
        text-transform: uppercase;
        letter-spacing: 0.08em;
    }
"#;

fn main() {
    launch(App);
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShellScreen {
    Home,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionStatus {
    Offline,
    Connecting,
    Connected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingFlow {
    Create,
    Join,
    Reconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoticeTone {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellNotice {
    tone: NoticeTone,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionIdentity {
    session_code: String,
    player_id: String,
    reconnect_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LobbyPlayerRow {
    name: String,
    role_label: &'static str,
    readiness_label: &'static str,
    connectivity_label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellState {
    screen: ShellScreen,
    connection_status: ConnectionStatus,
    coordinator: CoordinatorType,
    identity: Option<SessionIdentity>,
    session_snapshot: Option<ClientSessionSnapshot>,
    session_state: Option<ClientGameState>,
    api_base_url: String,
    create_name: String,
    join_session_code: String,
    join_name: String,
    reconnect_session_code: String,
    reconnect_token: String,
    pending_flow: Option<PendingFlow>,
    pending_command: Option<SessionCommand>,
    handover_tags_input: String,
    realtime_bootstrap_attempted: bool,
    notice: Option<ShellNotice>,
}

#[derive(Clone)]
struct AppWebApi {
    base_url: String,
    client: reqwest::Client,
}

impl AppWebApi {
    fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: normalize_api_base_url(&base_url.into()),
            client: reqwest::Client::new(),
        }
    }

    async fn create_workshop(&self, name: String) -> Result<WorkshopJoinSuccess, String> {
        let response = self
            .client
            .post(format!("{}/api/workshops", self.base_url))
            .json(&CreateWorkshopRequest { name })
            .send()
            .await
            .map_err(|error| format!("failed to reach backend: {error}"))?;

        Self::parse_join_response(response).await
    }

    async fn join_workshop(&self, request: JoinWorkshopRequest) -> Result<WorkshopJoinSuccess, String> {
        let response = self
            .client
            .post(format!("{}/api/workshops/join", self.base_url))
            .json(&request)
            .send()
            .await
            .map_err(|error| format!("failed to reach backend: {error}"))?;

        Self::parse_join_response(response).await
    }

    async fn reconnect_workshop(
        &self,
        session_code: String,
        reconnect_token: String,
    ) -> Result<WorkshopJoinSuccess, String> {
        self.join_workshop(build_reconnect_request(&session_code, &reconnect_token)).await
    }

    async fn send_command(&self, request: WorkshopCommandRequest) -> Result<(), String> {
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

    async fn parse_join_response(response: reqwest::Response) -> Result<WorkshopJoinSuccess, String> {
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

fn default_api_base_url() -> String {
    "http://127.0.0.1:4100".to_string()
}

fn restore_shell_state(snapshot: Option<ClientSessionSnapshot>) -> ShellState {
    let mut state = ShellState {
        screen: ShellScreen::Home,
        connection_status: ConnectionStatus::Offline,
        coordinator: CoordinatorType::Rust,
        identity: None,
        session_snapshot: None,
        session_state: None,
        api_base_url: default_api_base_url(),
        create_name: String::new(),
        join_session_code: String::new(),
        join_name: String::new(),
        reconnect_session_code: String::new(),
        reconnect_token: String::new(),
        pending_flow: None,
        pending_command: None,
        handover_tags_input: String::new(),
        realtime_bootstrap_attempted: false,
        notice: None,
    };

    if let Some(snapshot) = snapshot {
        hydrate_shell_from_snapshot(&mut state, &snapshot);
        state.notice = Some(info_notice("Restored reconnect session from browser storage."));
    }

    state
}

fn default_shell_state() -> ShellState {
    restore_shell_state(None)
}

fn bootstrap_shell_state() -> ShellState {
    match load_browser_session_snapshot() {
        Ok(snapshot) => restore_shell_state(snapshot),
        Err(error) => {
            let mut state = default_shell_state();
            state.notice = Some(error_notice(&format!("Failed to restore browser session: {error}")));
            state
        }
    }
}

fn screen_title(screen: &ShellScreen) -> &'static str {
    match screen {
        ShellScreen::Home => "Create, join, or reconnect to a workshop",
        ShellScreen::Session => "Workshop session connected",
    }
}

fn connection_status_label(status: &ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::Offline => "Offline",
        ConnectionStatus::Connecting => "Connecting",
        ConnectionStatus::Connected => "Connected",
    }
}

fn connection_status_class(status: &ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::Offline => "status-offline",
        ConnectionStatus::Connecting => "status-connecting",
        ConnectionStatus::Connected => "status-connected",
    }
}

fn notice_class(tone: NoticeTone) -> &'static str {
    match tone {
        NoticeTone::Info => "notice-info",
        NoticeTone::Success => "notice-success",
        NoticeTone::Error => "notice-error",
    }
}

fn pending_flow_label(flow: PendingFlow) -> &'static str {
    match flow {
        PendingFlow::Create => "Creating workshop…",
        PendingFlow::Join => "Joining workshop…",
        PendingFlow::Reconnect => "Reconnecting…",
    }
}

fn pending_command_label(command: SessionCommand) -> &'static str {
    match command {
        SessionCommand::StartPhase1 => "Starting Phase 1…",
        SessionCommand::StartHandover => "Starting handover…",
        SessionCommand::SubmitTags => "Saving handover tags…",
        SessionCommand::StartPhase2 => "Starting Phase 2…",
        SessionCommand::EndGame => "Ending workshop…",
        SessionCommand::RevealVotingResults => "Revealing results…",
        SessionCommand::ResetGame => "Resetting workshop…",
        _ => "Sending command…",
    }
}

fn normalize_api_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        default_api_base_url()
    } else {
        trimmed.to_string()
    }
}

fn build_reconnect_request(session_code: &str, reconnect_token: &str) -> JoinWorkshopRequest {
    JoinWorkshopRequest {
        session_code: session_code.trim().to_string(),
        name: None,
        reconnect_token: Some(reconnect_token.trim().to_string()),
    }
}

fn build_client_session_snapshot(success: &WorkshopJoinSuccess) -> ClientSessionSnapshot {
    ClientSessionSnapshot {
        session_code: success.session_code.clone(),
        reconnect_token: success.reconnect_token.clone(),
        player_id: success.player_id.clone(),
        coordinator_type: success.coordinator_type,
    }
}

fn build_command_request(
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

fn build_session_envelope(snapshot: &ClientSessionSnapshot) -> SessionEnvelope {
    SessionEnvelope {
        session_code: snapshot.session_code.clone(),
        player_id: snapshot.player_id.clone(),
        reconnect_token: snapshot.reconnect_token.clone(),
        coordinator_type: Some(snapshot.coordinator_type),
    }
}

fn build_ws_url(base_url: &str) -> String {
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

fn parse_tags_input(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn active_player_name(state: &ClientGameState) -> Option<String> {
    let player_id = state.current_player_id.as_ref()?;
    state.players.get(player_id).map(|player| player.name.clone())
}

fn phase_screen_title(phase: Phase) -> &'static str {
    match phase {
        Phase::Lobby => "Lobby setup",
        Phase::Phase1 => "Phase 1 - Discovery",
        Phase::Handover => "Handover",
        Phase::Phase2 => "Phase 2 - Care loop",
        Phase::Voting => "Voting",
        Phase::End => "Workshop results",
    }
}

fn phase_screen_body(phase: Phase) -> &'static str {
    match phase {
        Phase::Lobby => "Check the player roster, confirm reconnect status, and start when the workshop is ready.",
        Phase::Phase1 => "Discovery controls and dragon observation screens are the next Sprint 7 slices.",
        Phase::Handover => "Handover writing UX will move into a dedicated Dioxus screen in the next slices.",
        Phase::Phase2 => "Phase 2 action panels will replace the generic shell once the gameplay screens land.",
        Phase::Voting => "Voting UI will move from shell controls into a dedicated gameplay screen in Sprint 7.",
        Phase::End => "Final results and judge bundle presentation will be expanded after the core gameplay screens.",
    }
}

fn lobby_player_rows(state: &ClientGameState) -> Vec<LobbyPlayerRow> {
    let mut players = state.players.values().collect::<Vec<_>>();
    players.sort_by(|left, right| {
        right
            .is_host
            .cmp(&left.is_host)
            .then_with(|| left.name.to_ascii_lowercase().cmp(&right.name.to_ascii_lowercase()))
            .then_with(|| left.id.cmp(&right.id))
    });

    players
        .into_iter()
        .map(|player| LobbyPlayerRow {
            name: player.name.clone(),
            role_label: if player.is_host { "Host" } else { "Player" },
            readiness_label: if player.is_ready { "Ready" } else { "Setting up" },
            connectivity_label: if player.is_connected { "Online" } else { "Offline" },
        })
        .collect()
}

fn lobby_ready_summary(state: &ClientGameState) -> String {
    let ready_count = state.players.values().filter(|player| player.is_ready).count();
    format!("{ready_count} / {} ready", state.players.len())
}

fn lobby_status_copy(state: &ClientGameState) -> String {
    let total_players = state.players.len();
    let offline_players = state.players.values().filter(|player| !player.is_connected).count();

    if total_players == 0 {
        "No players have joined the workshop yet.".to_string()
    } else if total_players == 1 {
        "Single-player workshops can start as soon as the host is ready.".to_string()
    } else if offline_players == 0 {
        "All players are online. The host can start once the lobby is ready.".to_string()
    } else {
        format!("{offline_players} player(s) are currently offline and may need to reconnect before start.")
    }
}

fn current_player(state: &ClientGameState) -> Option<&Player> {
    let player_id = state.current_player_id.as_ref()?;
    state.players.get(player_id)
}

fn current_dragon(state: &ClientGameState) -> Option<&ClientDragon> {
    let player = current_player(state)?;
    let dragon_id = player.current_dragon_id.as_ref()?;
    state.dragons.get(dragon_id)
}

fn dragon_action_label(action: DragonAction) -> &'static str {
    match action {
        DragonAction::Feed => "Feed",
        DragonAction::Play => "Play",
        DragonAction::Sleep => "Sleep",
        DragonAction::Idle => "Idle",
    }
}

fn dragon_emotion_label(emotion: DragonEmotion) -> &'static str {
    match emotion {
        DragonEmotion::Happy => "Happy",
        DragonEmotion::Angry => "Angry",
        DragonEmotion::Sleepy => "Sleepy",
        DragonEmotion::Neutral => "Neutral",
    }
}

fn phase1_focus_title(state: &ClientGameState) -> String {
    current_dragon(state)
        .map(|dragon| format!("Meet {}", dragon.name))
        .unwrap_or_else(|| "Awaiting assigned dragon".to_string())
}

fn phase1_focus_body(state: &ClientGameState) -> String {
    let Some(dragon) = current_dragon(state) else {
        return "Phase 1 will unlock once the session assigns you a dragon to observe.".to_string();
    };

    let speech = dragon
        .speech
        .as_deref()
        .filter(|speech| !speech.trim().is_empty())
        .unwrap_or("No direct speech hint yet.");
    let condition = dragon
        .condition_hint
        .as_deref()
        .filter(|hint| !hint.trim().is_empty())
        .unwrap_or("Watch for timing changes between food, play, and sleep.");

    format!("{speech} {condition}")
}

fn phase1_observation_summary(state: &ClientGameState) -> String {
    let Some(dragon) = current_dragon(state) else {
        return "No discovery notes saved yet.".to_string();
    };

    let count = dragon.discovery_observations.len();
    if count == 0 {
        "No discovery notes saved yet.".to_string()
    } else {
        format!("{count} discovery note(s) captured for handover.")
    }
}

fn info_notice(message: &str) -> ShellNotice {
    ShellNotice {
        tone: NoticeTone::Info,
        message: message.to_string(),
    }
}

fn success_notice(message: &str) -> ShellNotice {
    ShellNotice {
        tone: NoticeTone::Success,
        message: message.to_string(),
    }
}

fn error_notice(message: &str) -> ShellNotice {
    ShellNotice {
        tone: NoticeTone::Error,
        message: message.to_string(),
    }
}

fn map_notice_tone(level: NoticeLevel) -> NoticeTone {
    match level {
        NoticeLevel::Info => NoticeTone::Info,
        NoticeLevel::Success => NoticeTone::Success,
        NoticeLevel::Warning | NoticeLevel::Error => NoticeTone::Error,
    }
}

fn apply_join_success(state: &mut ShellState, success: WorkshopJoinSuccess, flow: PendingFlow) {
    let snapshot = build_client_session_snapshot(&success);
    let success_message = match flow {
        PendingFlow::Create => "Workshop created.",
        PendingFlow::Join => "Joined workshop.",
        PendingFlow::Reconnect => "Reconnected to workshop.",
    };

    state.screen = ShellScreen::Session;
    state.connection_status = ConnectionStatus::Connected;
    state.coordinator = success.coordinator_type;
    state.identity = Some(SessionIdentity {
        session_code: success.session_code.clone(),
        player_id: success.player_id.clone(),
        reconnect_token: success.reconnect_token.clone(),
    });
    state.session_snapshot = Some(snapshot.clone());
    state.session_state = Some(success.state);
    state.join_session_code = snapshot.session_code.clone();
    state.reconnect_session_code = snapshot.session_code.clone();
    state.reconnect_token = snapshot.reconnect_token.clone();
    state.pending_flow = None;
    state.notice = Some(success_notice(success_message));
}

fn apply_request_error(state: &mut ShellState, error: String) {
    state.connection_status = ConnectionStatus::Offline;
    state.pending_flow = None;
    state.notice = Some(error_notice(&error));
}

fn command_success_message(command: SessionCommand) -> &'static str {
    match command {
        SessionCommand::StartPhase1 => "Phase 1 started.",
        SessionCommand::StartHandover => "Handover started.",
        SessionCommand::SubmitTags => "Handover tags saved.",
        SessionCommand::StartPhase2 => "Phase 2 started.",
        SessionCommand::EndGame => "Voting started.",
        SessionCommand::RevealVotingResults => "Voting results revealed.",
        SessionCommand::ResetGame => "Workshop reset.",
        _ => "Command sent.",
    }
}

fn apply_successful_command(state: &mut ShellState, command: SessionCommand) {
    state.pending_command = None;
    state.connection_status = ConnectionStatus::Connected;
    if let Some(session) = state.session_state.as_mut() {
        match command {
            SessionCommand::StartPhase1 => session.phase = Phase::Phase1,
            SessionCommand::StartHandover => session.phase = Phase::Handover,
            SessionCommand::StartPhase2 => session.phase = Phase::Phase2,
            SessionCommand::EndGame => session.phase = Phase::Voting,
            SessionCommand::RevealVotingResults => session.phase = Phase::End,
            SessionCommand::ResetGame => session.phase = Phase::Lobby,
            SessionCommand::SubmitTags => {}
            _ => {}
        }
    }
    if command == SessionCommand::SubmitTags {
        state.handover_tags_input.clear();
    }
    state.notice = Some(success_notice(command_success_message(command)));
}

fn apply_command_error(state: &mut ShellState, error: String) {
    state.pending_command = None;
    state.notice = Some(error_notice(&error));
}

#[allow(dead_code)]
fn apply_realtime_bootstrap_error(state: &mut ShellState, error: String) {
    state.connection_status = ConnectionStatus::Offline;
    state.notice = Some(error_notice(&error));
}

#[allow(dead_code)]
fn apply_realtime_connecting(state: &mut ShellState) {
    state.realtime_bootstrap_attempted = true;
    state.connection_status = ConnectionStatus::Connecting;
    state.notice = Some(info_notice("Attaching realtime session…"));
}

fn apply_server_ws_message(state: &mut ShellState, message: ServerWsMessage) {
    match message {
        ServerWsMessage::StateUpdate(client_state) => {
            let first_attach = state.connection_status != ConnectionStatus::Connected;
            state.screen = ShellScreen::Session;
            state.session_state = Some(client_state);
            state.connection_status = ConnectionStatus::Connected;
            state.pending_command = None;
            if first_attach {
                state.notice = Some(info_notice("Realtime session attached."));
            }
        }
        ServerWsMessage::Notice(ProtocolSessionNotice { level, title, message }) => {
            let tone = map_notice_tone(level);
            let combined = if title.trim().is_empty() {
                message
            } else {
                format!("{title}: {message}")
            };
            state.notice = Some(ShellNotice { tone, message: combined });
        }
        ServerWsMessage::Error { message } => {
            state.connection_status = ConnectionStatus::Offline;
            state.notice = Some(error_notice(&message));
        }
        ServerWsMessage::Pong => {
            state.connection_status = ConnectionStatus::Connected;
            state.notice = Some(info_notice("Realtime connection confirmed."));
        }
    }
}

#[cfg(target_arch = "wasm32")]
struct RealtimeClientHandle {
    socket: web_sys::WebSocket,
    onopen: Closure<dyn FnMut(web_sys::Event)>,
    onmessage: Closure<dyn FnMut(web_sys::MessageEvent)>,
    onerror: Closure<dyn FnMut(web_sys::ErrorEvent)>,
    onclose: Closure<dyn FnMut(web_sys::Event)>,
}

#[cfg(target_arch = "wasm32")]
std::thread_local! {
    static REALTIME_CLIENT: RefCell<Option<RealtimeClientHandle>> = const { RefCell::new(None) };
}

#[cfg(target_arch = "wasm32")]
fn bootstrap_realtime(mut shell_state: Signal<ShellState>) -> Result<(), String> {
    let (base_url, snapshot) = {
        let state = shell_state.read();
        (state.api_base_url.clone(), state.session_snapshot.clone())
    };
    let snapshot = snapshot.ok_or_else(|| "Connect to a workshop before attaching realtime.".to_string())?;
    let envelope_json = serde_json::to_string(&ClientWsMessage::AttachSession(build_session_envelope(&snapshot)))
        .map_err(|error| format!("failed to encode attach payload: {error}"))?;
    let socket = web_sys::WebSocket::new(&build_ws_url(&base_url))
        .map_err(|_| "failed to open realtime socket".to_string())?;

    shell_state.with_mut(apply_realtime_connecting);

    let open_socket = socket.clone();
    let open_state = shell_state;
    let onopen = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        if open_socket.send_with_str(&envelope_json).is_err() {
            open_state.with_mut(|state| {
                state.connection_status = ConnectionStatus::Offline;
                state.notice = Some(error_notice("Failed to attach realtime session."));
            });
        }
    }) as Box<dyn FnMut(_)>);
    socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));

    let message_state = shell_state;
    let onmessage = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
        if let Some(text) = event.data().as_string() {
            match serde_json::from_str::<ServerWsMessage>(&text) {
                Ok(message) => message_state.with_mut(|state| apply_server_ws_message(state, message)),
                Err(_) => message_state.with_mut(|state| {
                    state.connection_status = ConnectionStatus::Offline;
                    state.notice = Some(error_notice("Received invalid realtime payload."));
                }),
            }
        }
    }) as Box<dyn FnMut(_)>);
    socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

    let error_state = shell_state;
    let onerror = Closure::wrap(Box::new(move |_event: web_sys::ErrorEvent| {
        error_state.with_mut(|state| {
            state.connection_status = ConnectionStatus::Offline;
            state.notice = Some(error_notice("Realtime connection failed."));
        });
    }) as Box<dyn FnMut(_)>);
    socket.set_onerror(Some(onerror.as_ref().unchecked_ref()));

    let close_state = shell_state;
    let onclose = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        close_state.with_mut(|state| {
            state.connection_status = ConnectionStatus::Offline;
            state.notice = Some(info_notice("Realtime connection closed."));
        });
    }) as Box<dyn FnMut(_)>);
    socket.set_onclose(Some(onclose.as_ref().unchecked_ref()));

    REALTIME_CLIENT.with(|client| {
        if let Some(existing) = client.borrow_mut().take() {
            let _ = existing.socket.close();
        }
        client.borrow_mut().replace(RealtimeClientHandle {
            socket,
            onopen,
            onmessage,
            onerror,
            onclose,
        });
    });

    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn bootstrap_realtime(mut shell_state: Signal<ShellState>) -> Result<(), String> {
    shell_state.with_mut(|state| {
        state.realtime_bootstrap_attempted = true;
    });
    Ok(())
}

fn encode_session_snapshot(snapshot: &ClientSessionSnapshot) -> Result<String, String> {
    serde_json::to_string(snapshot).map_err(|error| format!("failed to encode session snapshot: {error}"))
}

fn decode_session_snapshot(value: &str) -> Result<ClientSessionSnapshot, String> {
    serde_json::from_str(value).map_err(|error| format!("failed to decode session snapshot: {error}"))
}

fn hydrate_shell_from_snapshot(state: &mut ShellState, snapshot: &ClientSessionSnapshot) {
    state.coordinator = snapshot.coordinator_type;
    state.identity = Some(SessionIdentity {
        session_code: snapshot.session_code.clone(),
        player_id: snapshot.player_id.clone(),
        reconnect_token: snapshot.reconnect_token.clone(),
    });
    state.session_snapshot = Some(snapshot.clone());
    state.join_session_code = snapshot.session_code.clone();
    state.reconnect_session_code = snapshot.session_code.clone();
    state.reconnect_token = snapshot.reconnect_token.clone();
}

#[cfg(target_arch = "wasm32")]
fn load_browser_session_snapshot() -> Result<Option<ClientSessionSnapshot>, String> {
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let storage = window
        .local_storage()
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
fn load_browser_session_snapshot() -> Result<Option<ClientSessionSnapshot>, String> {
    Ok(None)
}

#[cfg(target_arch = "wasm32")]
fn persist_browser_session_snapshot(snapshot: &ClientSessionSnapshot) -> Result<(), String> {
    let Some(window) = web_sys::window() else {
        return Err("window is unavailable".to_string());
    };
    let storage = window
        .local_storage()
        .map_err(|_| "failed to access browser storage".to_string())?
        .ok_or_else(|| "browser storage is unavailable".to_string())?;
    let encoded = encode_session_snapshot(snapshot)?;
    storage
        .set_item(SESSION_SNAPSHOT_STORAGE_KEY, &encoded)
        .map_err(|_| "failed to persist browser session".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
fn persist_browser_session_snapshot(snapshot: &ClientSessionSnapshot) -> Result<(), String> {
    let _ = snapshot;
    Ok(())
}

async fn submit_create_flow(mut shell_state: Signal<ShellState>) {
    let (base_url, name) = {
        let state = shell_state.read();
        (state.api_base_url.clone(), state.create_name.trim().to_string())
    };

    if name.is_empty() {
        shell_state.with_mut(|state| state.notice = Some(error_notice("Please enter a host name.")));
        return;
    }

    shell_state.with_mut(|state| {
        state.pending_flow = Some(PendingFlow::Create);
        state.connection_status = ConnectionStatus::Connecting;
        state.notice = Some(info_notice("Creating workshop…"));
    });

    let api = AppWebApi::new(base_url);
    match api.create_workshop(name).await {
        Ok(success) => {
            shell_state.with_mut(|state| apply_join_success(state, success, PendingFlow::Create));
            let persisted_snapshot = { shell_state.read().session_snapshot.clone() };
            if let Some(snapshot) = persisted_snapshot {
                if let Err(error) = persist_browser_session_snapshot(&snapshot) {
                    shell_state.with_mut(|state| {
                        state.notice = Some(error_notice(&format!("Workshop created, but session persistence failed: {error}")))
                    });
                }
            }
            if let Err(error) = bootstrap_realtime(shell_state) {
                shell_state.with_mut(|state| apply_realtime_bootstrap_error(state, error));
            }
        }
        Err(error) => shell_state.with_mut(|state| apply_request_error(state, error)),
    }
}

async fn submit_join_flow(mut shell_state: Signal<ShellState>) {
    let (base_url, session_code, name) = {
        let state = shell_state.read();
        (
            state.api_base_url.clone(),
            state.join_session_code.trim().to_string(),
            state.join_name.trim().to_string(),
        )
    };

    if session_code.is_empty() {
        shell_state.with_mut(|state| state.notice = Some(error_notice("Enter a workshop code.")));
        return;
    }
    if name.is_empty() {
        shell_state.with_mut(|state| state.notice = Some(error_notice("Please enter a player name.")));
        return;
    }

    shell_state.with_mut(|state| {
        state.pending_flow = Some(PendingFlow::Join);
        state.connection_status = ConnectionStatus::Connecting;
        state.notice = Some(info_notice("Joining workshop…"));
    });

    let api = AppWebApi::new(base_url);
    let request = JoinWorkshopRequest {
        session_code,
        name: Some(name),
        reconnect_token: None,
    };
    match api.join_workshop(request).await {
        Ok(success) => {
            shell_state.with_mut(|state| apply_join_success(state, success, PendingFlow::Join));
            let persisted_snapshot = { shell_state.read().session_snapshot.clone() };
            if let Some(snapshot) = persisted_snapshot {
                if let Err(error) = persist_browser_session_snapshot(&snapshot) {
                    shell_state.with_mut(|state| {
                        state.notice = Some(error_notice(&format!("Joined workshop, but session persistence failed: {error}")))
                    });
                }
            }
        }
        Err(error) => shell_state.with_mut(|state| apply_request_error(state, error)),
    }
}

async fn submit_reconnect_flow(mut shell_state: Signal<ShellState>) {
    let (base_url, session_code, reconnect_token) = {
        let state = shell_state.read();
        (
            state.api_base_url.clone(),
            state.reconnect_session_code.trim().to_string(),
            state.reconnect_token.trim().to_string(),
        )
    };

    if session_code.is_empty() || reconnect_token.is_empty() {
        shell_state.with_mut(|state| {
            state.notice = Some(error_notice("Session code and reconnect token are required for reconnect."))
        });
        return;
    }

    shell_state.with_mut(|state| {
        state.pending_flow = Some(PendingFlow::Reconnect);
        state.connection_status = ConnectionStatus::Connecting;
        state.notice = Some(info_notice("Reconnecting…"));
    });

    let api = AppWebApi::new(base_url);
    match api.reconnect_workshop(session_code, reconnect_token).await {
        Ok(success) => {
            shell_state.with_mut(|state| apply_join_success(state, success, PendingFlow::Reconnect));
            let persisted_snapshot = { shell_state.read().session_snapshot.clone() };
            if let Some(snapshot) = persisted_snapshot {
                if let Err(error) = persist_browser_session_snapshot(&snapshot) {
                    shell_state.with_mut(|state| {
                        state.notice = Some(error_notice(&format!("Reconnected, but session persistence failed: {error}")))
                    });
                }
            }
        }
        Err(error) => shell_state.with_mut(|state| apply_request_error(state, error)),
    }
}

async fn submit_workshop_command(
    mut shell_state: Signal<ShellState>,
    command: SessionCommand,
    payload: Option<serde_json::Value>,
) {
    let (base_url, snapshot) = {
        let state = shell_state.read();
        (state.api_base_url.clone(), state.session_snapshot.clone())
    };

    let Some(snapshot) = snapshot else {
        shell_state.with_mut(|state| state.notice = Some(error_notice("Connect to a workshop before sending commands.")));
        return;
    };

    shell_state.with_mut(|state| {
        state.pending_command = Some(command);
        state.notice = Some(info_notice(pending_command_label(command)));
    });

    let api = AppWebApi::new(base_url);
    match api.send_command(build_command_request(&snapshot, command, payload)).await {
        Ok(()) => shell_state.with_mut(|state| apply_successful_command(state, command)),
        Err(error) => shell_state.with_mut(|state| apply_command_error(state, error)),
    }
}

async fn submit_handover_tags_command(mut shell_state: Signal<ShellState>) {
    let tags = {
        let state = shell_state.read();
        parse_tags_input(&state.handover_tags_input)
    };

    if tags.is_empty() {
        shell_state.with_mut(|state| state.notice = Some(error_notice("Enter at least one handover tag.")));
        return;
    }

    submit_workshop_command(shell_state, SessionCommand::SubmitTags, Some(serde_json::json!(tags))).await;
}

#[component]
fn App() -> Element {
    let mut state = use_signal(bootstrap_shell_state);
    let shell = state.read().clone();
    let mut create_state = state;
    let mut join_state = state;
    let mut reconnect_state = state;
    let mut tags_state = state;
    let mut realtime_state = state;
    let mut realtime_effect_state = state;
    let start_phase1_state = state;
    let start_handover_state = state;
    let start_phase2_state = state;
    let end_game_state = state;
    let reveal_results_state = state;
    let reset_game_state = state;
    let submit_tags_state = state;
    let connection_badge_class = format!("badge {}", connection_status_class(&shell.connection_status));
    let identity_label = if shell.identity.is_some() { "present" } else { "empty" };
    let pending_flow_status = shell.pending_flow.map(pending_flow_label).unwrap_or("Idle");
    let pending_command_status = shell.pending_command.map(pending_command_label).unwrap_or("Idle");
    let commands_disabled = shell.pending_flow.is_some() || shell.pending_command.is_some() || shell.session_snapshot.is_none();
    let realtime_button_label = if shell.realtime_bootstrap_attempted {
        "Retry realtime attach"
    } else {
        "Attach realtime"
    };
    let should_bootstrap_realtime = shell.session_snapshot.is_some() && !shell.realtime_bootstrap_attempted;
    let active_player_label = shell
        .session_state
        .as_ref()
        .and_then(active_player_name)
        .unwrap_or_else(|| "Not attached yet".to_string());
    let session_code_label = shell
        .session_snapshot
        .as_ref()
        .map(|snapshot| snapshot.session_code.clone())
        .unwrap_or_else(|| "—".to_string());
    let session_phase_label = shell
        .session_state
        .as_ref()
        .map(|session| format!("{:?}", session.phase))
        .unwrap_or_else(|| "Not connected".to_string());
    let players_count_label = shell
        .session_state
        .as_ref()
        .map(|session| session.players.len().to_string())
        .unwrap_or_else(|| "0".to_string());
    let session_phase_title = shell
        .session_state
        .as_ref()
        .map(|session| phase_screen_title(session.phase))
        .unwrap_or("Awaiting session");
    let session_phase_body = shell
        .session_state
        .as_ref()
        .map(|session| phase_screen_body(session.phase))
        .unwrap_or("Connect to a workshop to see the active gameplay screen.");
    let lobby_rows = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Lobby)
        .map(lobby_player_rows)
        .unwrap_or_default();
    let lobby_ready_label = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Lobby)
        .map(lobby_ready_summary)
        .unwrap_or_else(|| "—".to_string());
    let lobby_status_label = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Lobby)
        .map(lobby_status_copy)
        .unwrap_or_default();
    let phase1_title = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Phase1)
        .map(phase1_focus_title)
        .unwrap_or_default();
    let phase1_body = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Phase1)
        .map(phase1_focus_body)
        .unwrap_or_default();
    let phase1_observations = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Phase1)
        .map(phase1_observation_summary)
        .unwrap_or_default();
    let phase1_emotion = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Phase1)
        .and_then(current_dragon)
        .map(|dragon| dragon_emotion_label(dragon.last_emotion))
        .unwrap_or("");
    let phase1_last_action = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Phase1)
        .and_then(current_dragon)
        .map(|dragon| dragon_action_label(dragon.last_action))
        .unwrap_or("");

    use_effect(move || {
        if should_bootstrap_realtime {
            if let Err(error) = bootstrap_realtime(realtime_effect_state) {
                realtime_effect_state.with_mut(|state| apply_realtime_bootstrap_error(state, error));
            }
        }
    });

    rsx! {
        style {
            {APP_STYLE}
        }
        main { class: "shell",
            section { class: "shell__container",
                section { class: "hero",
                    p { class: "hero__eyebrow", "Rust-only migration / Sprint 6" }
                    h1 { class: "hero__title", "Dragon Switch Rust Next" }
                    p { class: "hero__body", {screen_title(&shell.screen)} }
                    p { class: "meta", "The Dioxus shell now covers session bootstrap, browser persistence, and shell-level HTTP command transport against the Rust Axum backend." }
                    div { class: "hero__meta",
                        span { class: "badge", "Coordinator: Rust" }
                        span { class: connection_badge_class, "Connection: " {connection_status_label(&shell.connection_status)} }
                        span { class: "badge", "Reconnect identity: " {identity_label} }
                        span { class: "badge", "Pending flow: " {pending_flow_status} }
                        span { class: "badge", "Pending command: " {pending_command_status} }
                    }
                }
                if let Some(notice) = shell.notice.clone() {
                    article { class: format!("notice {}", notice_class(notice.tone)),
                        {notice.message}
                    }
                }
                section { class: "grid",
                    article { class: "panel",
                        h2 { class: "panel__title", "Backend target" }
                        p { class: "panel__body", "Point the shell at the Rust Axum backend before running session bootstrap or workshop commands." }
                        div { class: "panel__stack",
                            input {
                                class: "input",
                                value: shell.api_base_url,
                                placeholder: "http://127.0.0.1:4100",
                                oninput: move |event| state.with_mut(|shell| shell.api_base_url = event.value())
                            }
                            p { class: "meta", "HTTP is authoritative for create, join, reconnect, and workshop commands. WebSocket attach/state streaming comes next." }
                        }
                    }
                    article { class: "panel",
                        h2 { class: "panel__title", "Create workshop" }
                        p { class: "panel__body", "Boot a new workshop directly from the Rust UI shell." }
                        div { class: "panel__stack",
                            input {
                                class: "input",
                                value: shell.create_name,
                                placeholder: "Host name",
                                oninput: move |event| create_state.with_mut(|shell| shell.create_name = event.value())
                            }
                            div { class: "button-row",
                                button {
                                    class: "button button--primary",
                                    disabled: shell.pending_flow.is_some(),
                                    onclick: move |_| {
                                        spawn(submit_create_flow(create_state));
                                    },
                                    "Create workshop"
                                }
                            }
                        }
                    }
                    article { class: "panel",
                        h2 { class: "panel__title", "Join or reconnect" }
                        p { class: "panel__body", "Use the same Rust endpoints for new joins and reconnects. Browser persistence now rehydrates the reconnect fields on boot." }
                        div { class: "panel__stack",
                            input {
                                class: "input",
                                value: shell.join_session_code,
                                placeholder: "Workshop code",
                                oninput: move |event| join_state.with_mut(|shell| shell.join_session_code = event.value())
                            }
                            input {
                                class: "input",
                                value: shell.join_name,
                                placeholder: "Player name",
                                oninput: move |event| join_state.with_mut(|shell| shell.join_name = event.value())
                            }
                            div { class: "button-row",
                                button {
                                    class: "button button--primary",
                                    disabled: shell.pending_flow.is_some(),
                                    onclick: move |_| {
                                        spawn(submit_join_flow(join_state));
                                    },
                                    "Join workshop"
                                }
                            }
                            input {
                                class: "input",
                                value: shell.reconnect_session_code,
                                placeholder: "Reconnect session code",
                                oninput: move |event| reconnect_state.with_mut(|shell| shell.reconnect_session_code = event.value())
                            }
                            input {
                                class: "input",
                                value: shell.reconnect_token,
                                placeholder: "Reconnect token",
                                oninput: move |event| reconnect_state.with_mut(|shell| shell.reconnect_token = event.value())
                            }
                            div { class: "button-row",
                                button {
                                    class: "button button--secondary",
                                    disabled: shell.pending_flow.is_some(),
                                    onclick: move |_| {
                                        spawn(submit_reconnect_flow(reconnect_state));
                                    },
                                    "Reconnect"
                                }
                            }
                        }
                    }
                    article { class: "panel",
                        h2 { class: "panel__title", "Current session" }
                        div { class: "session-summary",
                            p { class: "panel__body", "Session code: " {session_code_label} }
                            p { class: "panel__body", "Active player: " {active_player_label} }
                            p { class: "panel__body", "Current phase: " {session_phase_label} }
                            p { class: "panel__body", "Visible players: " {players_count_label} }
                        }
                        h3 { class: "panel__title", {session_phase_title} }
                        p { class: "panel__body", {session_phase_body} }
                        if !lobby_rows.is_empty() {
                            p { class: "meta", "Lobby readiness: " {lobby_ready_label} }
                            p { class: "meta", {lobby_status_label.clone()} }
                            div { class: "roster",
                                for row in lobby_rows {
                                    article { class: "roster__item",
                                        div {
                                            p { class: "roster__name", {row.name} }
                                            p { class: "roster__meta", {row.role_label} " - " {row.readiness_label} }
                                        }
                                        span {
                                            class: format!("roster__status {}", if row.connectivity_label == "Online" { "status-connected" } else { "status-offline" }),
                                            {row.connectivity_label}
                                        }
                                    }
                                }
                            }
                        }
                        if !phase1_title.is_empty() {
                            p { class: "meta", "Current dragon mood: " {phase1_emotion} }
                            p { class: "meta", "Last action: " {phase1_last_action} }
                            article { class: "roster__item",
                                div {
                                    p { class: "roster__name", {phase1_title} }
                                    p { class: "roster__meta", {phase1_observations.clone()} }
                                }
                                span { class: "roster__status status-connecting", "Discovery" }
                            }
                            p { class: "panel__body", {phase1_body} }
                        }
                        div { class: "button-row",
                            button {
                                class: "button button--secondary",
                                disabled: shell.session_snapshot.is_none() || shell.pending_flow.is_some(),
                                onclick: move |_| {
                                    if let Err(error) = bootstrap_realtime(realtime_state) {
                                        realtime_state.with_mut(|state| apply_realtime_bootstrap_error(state, error));
                                    }
                                },
                                {realtime_button_label}
                            }
                        }
                        p { class: "meta", "Browser session persistence now restores the reconnect snapshot on boot so the shell can attempt reconnect without retyping credentials." }
                    }
                    article { class: "panel",
                        h2 { class: "panel__title", "Workshop controls" }
                        p { class: "panel__body", "Host lifecycle controls are now available through the Rust command endpoint. The shell applies lightweight optimistic phase updates until WebSocket state sync lands." }
                        div { class: "panel__stack",
                            div { class: "button-row",
                                button {
                                    class: "button button--primary",
                                    disabled: commands_disabled,
                                    onclick: move |_| {
                                        spawn(submit_workshop_command(start_phase1_state, SessionCommand::StartPhase1, None));
                                    },
                                    "Start Phase 1"
                                }
                                button {
                                    class: "button button--secondary",
                                    disabled: commands_disabled,
                                    onclick: move |_| {
                                        spawn(submit_workshop_command(start_handover_state, SessionCommand::StartHandover, None));
                                    },
                                    "Start handover"
                                }
                            }
                            input {
                                class: "input",
                                value: shell.handover_tags_input,
                                placeholder: "Handover tags, comma separated",
                                oninput: move |event| tags_state.with_mut(|shell| shell.handover_tags_input = event.value())
                            }
                            div { class: "button-row",
                                button {
                                    class: "button button--secondary",
                                    disabled: commands_disabled,
                                    onclick: move |_| {
                                        spawn(submit_handover_tags_command(submit_tags_state));
                                    },
                                    "Save handover tags"
                                }
                                button {
                                    class: "button button--secondary",
                                    disabled: commands_disabled,
                                    onclick: move |_| {
                                        spawn(submit_workshop_command(start_phase2_state, SessionCommand::StartPhase2, None));
                                    },
                                    "Start Phase 2"
                                }
                            }
                            div { class: "button-row",
                                button {
                                    class: "button button--secondary",
                                    disabled: commands_disabled,
                                    onclick: move |_| {
                                        spawn(submit_workshop_command(end_game_state, SessionCommand::EndGame, None));
                                    },
                                    "End game"
                                }
                                button {
                                    class: "button button--secondary",
                                    disabled: commands_disabled,
                                    onclick: move |_| {
                                        spawn(submit_workshop_command(reveal_results_state, SessionCommand::RevealVotingResults, None));
                                    },
                                    "Reveal results"
                                }
                                button {
                                    class: "button button--secondary",
                                    disabled: commands_disabled,
                                    onclick: move |_| {
                                        spawn(submit_workshop_command(reset_game_state, SessionCommand::ResetGame, None));
                                    },
                                    "Reset workshop"
                                }
                            }
                        }
                    }
                    article { class: "panel",
                        h2 { class: "panel__title", "Transport plan" }
                        p { class: "panel__body", "HTTP bootstrap, browser persistence, and shell-level commands are active. The next step is to attach WebSocket runtime state streaming on top of this path." }
                        ul { class: "panel__list",
                            li { "Reuse restored reconnect snapshot after reload" }
                            li { "Auto-attach session over WebSocket" }
                            li { "Replace optimistic local command updates with pushed session state" }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn default_shell_state_boots_home_screen_with_rust_coordinator() {
        let state = default_shell_state();

        assert_eq!(state.screen, ShellScreen::Home);
        assert_eq!(state.connection_status, ConnectionStatus::Offline);
        assert_eq!(state.coordinator, CoordinatorType::Rust);
        assert_eq!(state.identity, None);
        assert_eq!(state.api_base_url, "http://127.0.0.1:4100");
    }

    #[test]
    fn shell_labels_match_bootstrap_state() {
        let state = default_shell_state();

        assert_eq!(screen_title(&state.screen), "Create, join, or reconnect to a workshop");
        assert_eq!(connection_status_label(&state.connection_status), "Offline");
        assert_eq!(connection_status_class(&state.connection_status), "status-offline");
    }

    #[test]
    fn connection_status_variants_have_distinct_labels_and_classes() {
        assert_eq!(connection_status_label(&ConnectionStatus::Connecting), "Connecting");
        assert_eq!(connection_status_class(&ConnectionStatus::Connecting), "status-connecting");
        assert_eq!(connection_status_label(&ConnectionStatus::Connected), "Connected");
        assert_eq!(connection_status_class(&ConnectionStatus::Connected), "status-connected");
    }

    #[test]
    fn normalize_api_base_url_trims_trailing_slashes_and_whitespace() {
        assert_eq!(normalize_api_base_url(" http://localhost:4100/ "), "http://localhost:4100");
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
    fn restore_shell_state_rehydrates_reconnect_fields_from_snapshot() {
        let snapshot = ClientSessionSnapshot {
            session_code: "654321".to_string(),
            reconnect_token: "reconnect-9".to_string(),
            player_id: "player-9".to_string(),
            coordinator_type: CoordinatorType::Rust,
        };

        let state = restore_shell_state(Some(snapshot.clone()));

        assert_eq!(state.screen, ShellScreen::Home);
        assert_eq!(state.connection_status, ConnectionStatus::Offline);
        assert_eq!(state.identity.as_ref().map(|identity| identity.player_id.as_str()), Some("player-9"));
        assert_eq!(state.reconnect_session_code, "654321");
        assert_eq!(state.reconnect_token, "reconnect-9");
        assert_eq!(state.session_snapshot, Some(snapshot));
        assert_eq!(state.notice.as_ref().map(|notice| notice.message.as_str()), Some("Restored reconnect session from browser storage."));
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
        assert_eq!(build_ws_url("http://127.0.0.1:4100/"), "ws://127.0.0.1:4100/api/workshops/ws");
        assert_eq!(build_ws_url("https://dragon-switch.dev"), "wss://dragon-switch.dev/api/workshops/ws");
    }

    #[test]
    fn parse_tags_input_trims_and_filters_empty_segments() {
        let tags = parse_tags_input(" one, two ,, three , ");

        assert_eq!(tags, vec!["one", "two", "three"]);
    }

    #[test]
    fn phase_screen_copy_matches_lobby_and_voting_states() {
        assert_eq!(phase_screen_title(Phase::Lobby), "Lobby setup");
        assert_eq!(phase_screen_title(Phase::Voting), "Voting");
        assert_eq!(
            phase_screen_body(Phase::Lobby),
            "Check the player roster, confirm reconnect status, and start when the workshop is ready."
        );
    }

    #[test]
    fn lobby_player_rows_prioritize_host_and_map_labels() {
        let mut state = mock_join_success().state;
        state.players.insert(
            "player-2".to_string(),
            Player {
                id: "player-2".to_string(),
                name: "Bob".to_string(),
                is_host: false,
                score: 0,
                current_dragon_id: None,
                achievements: Vec::new(),
                is_ready: true,
                is_connected: false,
                pet_description: Some("Bob's workshop dragon".to_string()),
            },
        );

        let rows = lobby_player_rows(&state);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Alice");
        assert_eq!(rows[0].role_label, "Host");
        assert_eq!(rows[0].readiness_label, "Setting up");
        assert_eq!(rows[1].name, "Bob");
        assert_eq!(rows[1].connectivity_label, "Offline");
        assert_eq!(rows[1].readiness_label, "Ready");
    }

    #[test]
    fn lobby_status_copy_handles_empty_and_single_player_states() {
        let mut empty_state = mock_join_success().state;
        empty_state.players.clear();
        assert_eq!(lobby_status_copy(&empty_state), "No players have joined the workshop yet.");

        let single_player_state = mock_join_success().state;
        assert_eq!(
            lobby_status_copy(&single_player_state),
            "Single-player workshops can start as soon as the host is ready."
        );
        assert_eq!(lobby_ready_summary(&single_player_state), "0 / 1 ready");
    }

    fn mock_phase1_state() -> ClientGameState {
        let mut state = mock_join_success().state;
        state.phase = Phase::Phase1;
        state.players.get_mut("player-1").expect("player-1").current_dragon_id = Some("dragon-1".to_string());
        state.dragons.insert(
            "dragon-1".to_string(),
            ClientDragon {
                id: "dragon-1".to_string(),
                name: "Comet".to_string(),
                visuals: protocol::DragonVisuals {
                    base: 1,
                    color_p: "#88ccff".to_string(),
                    color_s: "#4466aa".to_string(),
                    color_a: "#ffee88".to_string(),
                },
                original_owner_id: Some("player-1".to_string()),
                current_owner_id: Some("player-1".to_string()),
                stats: protocol::DragonStats {
                    hunger: 72,
                    energy: 55,
                    happiness: 81,
                },
                condition_hint: Some("Gets restless after long idle stretches.".to_string()),
                discovery_observations: vec!["Loves food at dusk".to_string()],
                handover_tags: Vec::new(),
                last_action: DragonAction::Feed,
                last_emotion: DragonEmotion::Happy,
                speech: Some("The snack worked.".to_string()),
                speech_timer: 2,
                action_cooldown: 0,
                custom_sprites: None,
            },
        );
        state
    }

    #[test]
    fn phase1_focus_helpers_use_current_dragon_context() {
        let state = mock_phase1_state();

        assert_eq!(phase1_focus_title(&state), "Meet Comet");
        assert_eq!(phase1_observation_summary(&state), "1 discovery note(s) captured for handover.");
        assert_eq!(dragon_emotion_label(current_dragon(&state).expect("dragon").last_emotion), "Happy");
        assert_eq!(dragon_action_label(current_dragon(&state).expect("dragon").last_action), "Feed");
        assert!(phase1_focus_body(&state).contains("The snack worked."));
    }

    #[test]
    fn phase1_focus_helpers_fall_back_when_player_has_no_dragon() {
        let state = mock_join_success().state;

        assert_eq!(phase1_focus_title(&state), "Awaiting assigned dragon");
        assert_eq!(phase1_observation_summary(&state), "No discovery notes saved yet.");
    }

    #[test]
    fn apply_join_success_promotes_shell_to_connected_session() {
        let mut state = default_shell_state();
        apply_join_success(&mut state, mock_join_success(), PendingFlow::Join);

        assert_eq!(state.screen, ShellScreen::Session);
        assert_eq!(state.connection_status, ConnectionStatus::Connected);
        assert_eq!(state.pending_flow, None);
        assert_eq!(state.identity.as_ref().map(|identity| identity.session_code.as_str()), Some("123456"));
        assert_eq!(state.session_snapshot, Some(ClientSessionSnapshot {
            session_code: "123456".to_string(),
            reconnect_token: "reconnect-1".to_string(),
            player_id: "player-1".to_string(),
            coordinator_type: CoordinatorType::Rust,
        }));
        assert_eq!(state.join_session_code, "123456");
        assert_eq!(state.reconnect_token, "reconnect-1");
        assert_eq!(active_player_name(state.session_state.as_ref().expect("session state")).as_deref(), Some("Alice"));
        assert_eq!(state.notice.as_ref().map(|notice| notice.message.as_str()), Some("Joined workshop."));
    }

    #[test]
    fn apply_successful_command_updates_phase_and_clears_pending_command() {
        let mut state = default_shell_state();
        apply_join_success(&mut state, mock_join_success(), PendingFlow::Join);
        state.pending_command = Some(SessionCommand::StartPhase1);

        apply_successful_command(&mut state, SessionCommand::StartPhase1);

        assert_eq!(state.pending_command, None);
        assert_eq!(state.session_state.as_ref().map(|session| session.phase), Some(Phase::Phase1));
        assert_eq!(state.notice.as_ref().map(|notice| notice.message.as_str()), Some("Phase 1 started."));
    }

    #[test]
    fn submit_tags_success_clears_handover_input() {
        let mut state = default_shell_state();
        apply_join_success(&mut state, mock_join_success(), PendingFlow::Join);
        state.handover_tags_input = "one, two".to_string();
        state.pending_command = Some(SessionCommand::SubmitTags);

        apply_successful_command(&mut state, SessionCommand::SubmitTags);

        assert_eq!(state.pending_command, None);
        assert!(state.handover_tags_input.is_empty());
        assert_eq!(state.notice.as_ref().map(|notice| notice.message.as_str()), Some("Handover tags saved."));
    }

    #[test]
    fn server_ws_state_update_promotes_shell_to_connected_realtime_session() {
        let mut state = default_shell_state();
        apply_join_success(&mut state, mock_join_success(), PendingFlow::Join);
        state.connection_status = ConnectionStatus::Connecting;
        state.pending_command = Some(SessionCommand::StartPhase1);

        apply_server_ws_message(
            &mut state,
            ServerWsMessage::StateUpdate(mock_join_success().state),
        );

        assert_eq!(state.connection_status, ConnectionStatus::Connected);
        assert_eq!(state.pending_command, None);
        assert_eq!(state.notice.as_ref().map(|notice| notice.message.as_str()), Some("Realtime session attached."));
    }

    #[test]
    fn server_ws_notice_maps_protocol_notice_to_shell_notice() {
        let mut state = default_shell_state();

        apply_server_ws_message(
            &mut state,
            ServerWsMessage::Notice(ProtocolSessionNotice {
                level: NoticeLevel::Success,
                title: "Saved".to_string(),
                message: "Workshop updated".to_string(),
            }),
        );

        assert_eq!(state.notice.as_ref().map(|notice| notice.message.as_str()), Some("Saved: Workshop updated"));
        assert_eq!(state.notice.as_ref().map(|notice| notice.tone), Some(NoticeTone::Success));
    }
}
