use dioxus::prelude::*;
use protocol::{
    create_default_session_settings, ClientGameState, ClientSessionSnapshot, CoordinatorType,
    CreateWorkshopRequest, JoinWorkshopRequest, Phase, Player, SessionMeta, WorkshopJoinResult,
    WorkshopJoinSuccess,
};
use std::collections::BTreeMap;

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
        line-height: 1.5;
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

fn default_shell_state() -> ShellState {
    ShellState {
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
        notice: None,
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

fn active_player_name(state: &ClientGameState) -> Option<String> {
    let player_id = state.current_player_id.as_ref()?;
    state.players.get(player_id).map(|player| player.name.clone())
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
        Ok(success) => shell_state.with_mut(|state| apply_join_success(state, success, PendingFlow::Create)),
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
        Ok(success) => shell_state.with_mut(|state| apply_join_success(state, success, PendingFlow::Join)),
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
        Ok(success) => shell_state.with_mut(|state| apply_join_success(state, success, PendingFlow::Reconnect)),
        Err(error) => shell_state.with_mut(|state| apply_request_error(state, error)),
    }
}

#[component]
fn App() -> Element {
    let mut state = use_signal(default_shell_state);
    let shell = state.read().clone();
    let mut create_state = state;
    let mut join_state = state;
    let mut reconnect_state = state;
    let connection_badge_class = format!("badge {}", connection_status_class(&shell.connection_status));
    let identity_label = if shell.identity.is_some() { "present" } else { "empty" };
    let pending_label = shell.pending_flow.map(pending_flow_label).unwrap_or("Idle");
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
                    p { class: "meta", "The Dioxus shell is now wired to the Rust HTTP API for create, join, and reconnect flows, while browser persistence remains the next isolated slice." }
                    div { class: "hero__meta",
                        span { class: "badge", "Coordinator: Rust" }
                        span { class: connection_badge_class, "Connection: " {connection_status_label(&shell.connection_status)} }
                        span { class: "badge", "Reconnect identity: " {identity_label} }
                        span { class: "badge", "Pending flow: " {pending_label} }
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
                        p { class: "panel__body", "Point the shell at the Rust Axum backend before running create, join, or reconnect flows." }
                        div { class: "panel__stack",
                            input {
                                class: "input",
                                value: shell.api_base_url,
                                placeholder: "http://127.0.0.1:4100",
                                oninput: move |event| state.with_mut(|shell| shell.api_base_url = event.value())
                            }
                            p { class: "meta", "The HTTP command channel remains authoritative. WebSocket attach/state streaming comes in the next Sprint 6 slices." }
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
                        p { class: "panel__body", "Use the same Rust endpoints for new joins and reconnects; browser persistence will wrap these fields in the next slice." }
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
                        p { class: "meta", "Once browser session persistence lands, this shell state will rehydrate automatically before reconnect is attempted." }
                    }
                    article { class: "panel",
                        h2 { class: "panel__title", "Transport plan" }
                        p { class: "panel__body", "HTTP is now active inside the shell. The next step is to layer browser persistence on top, then attach WebSocket runtime state streaming." }
                        ul { class: "panel__list",
                            li { "Persist reconnect snapshot in browser storage" }
                            li { "Auto-reconnect on reload" }
                            li { "Attach WebSocket client after HTTP bootstrap" }
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
    fn apply_join_success_promotes_shell_to_connected_session() {
        let mut state = default_shell_state();
        apply_join_success(&mut state, mock_join_success(), PendingFlow::Join);

        assert_eq!(state.screen, ShellScreen::Session);
        assert_eq!(state.connection_status, ConnectionStatus::Connected);
        assert_eq!(state.pending_flow, None);
        assert_eq!(state.identity.as_ref().map(|identity| identity.session_code.as_str()), Some("123456"));
        assert_eq!(state.session_snapshot.as_ref().map(|snapshot| snapshot.player_id.as_str()), Some("player-1"));
        assert_eq!(state.join_session_code, "123456");
        assert_eq!(state.reconnect_token, "reconnect-1");
        assert_eq!(active_player_name(state.session_state.as_ref().expect("session state")).as_deref(), Some("Alice"));
        assert_eq!(state.notice.as_ref().map(|notice| notice.message.as_str()), Some("Joined workshop."));
    }
}
