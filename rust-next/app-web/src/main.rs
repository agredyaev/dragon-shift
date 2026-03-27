use dioxus::prelude::*;
use protocol::{
    create_default_session_settings, ClientDragon, ClientGameState, ClientSessionSnapshot,
    CoordinatorType, CreateWorkshopRequest, DragonAction, DragonEmotion, JoinWorkshopRequest,
    JudgeBundle, NoticeLevel, Phase, Player, ServerWsMessage, SessionCommand, SessionEnvelope,
    SessionMeta, SessionNotice as ProtocolSessionNotice, WorkshopCommandRequest,
    WorkshopCommandResult, WorkshopJoinResult, WorkshopJoinSuccess, WorkshopJudgeBundleRequest,
    WorkshopJudgeBundleResult,
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
    @import url('https://fonts.googleapis.com/css2?family=Silkscreen:wght@400;700&display=swap');

    :root {
        color-scheme: dark;
        --font-display: 'Silkscreen', ui-sans-serif, system-ui, sans-serif;
        --font-body: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, sans-serif;
        font-family: var(--font-body);
        background: #0f172a;
        color: #f8fafc;
    }
    * {
        box-sizing: border-box;
    }
    html {
        background: #0f172a;
    }
    body {
        margin: 0;
        min-height: 100vh;
        font-family: var(--font-body);
        color: #f8fafc;
        background-color: #0f172a;
        background-image:
            linear-gradient(180deg, rgba(92, 197, 244, 0.16) 0%, rgba(15, 23, 42, 0) 34%),
            linear-gradient(45deg, rgba(165, 228, 255, 0.16) 25%, transparent 25%, transparent 75%, rgba(165, 228, 255, 0.16) 75%, rgba(165, 228, 255, 0.16)),
            linear-gradient(45deg, rgba(76, 99, 217, 0.12) 25%, transparent 25%, transparent 75%, rgba(76, 99, 217, 0.12) 75%, rgba(76, 99, 217, 0.12)),
            linear-gradient(180deg, rgba(15, 23, 42, 0.92) 0%, rgba(12, 18, 32, 1) 100%);
        background-size: 100% 100%, 64px 64px, 64px 64px, 100% 100%;
        background-position: 0 0, 0 0, 32px 32px, 0 0;
        animation: bgScroll 24s linear infinite;
        image-rendering: pixelated;
        -webkit-font-smoothing: antialiased;
        -moz-osx-font-smoothing: grayscale;
    }
    @keyframes bgScroll {
        from { background-position: 0 0, 0 0, 32px 32px, 0 0; }
        to { background-position: 0 0, 64px 64px, 96px 96px, 0 0; }
    }
    @keyframes crtFlicker {
        0% { opacity: 0.26; }
        25% { opacity: 0.31; }
        50% { opacity: 0.28; }
        75% { opacity: 0.34; }
        100% { opacity: 0.27; }
    }
    .shell {
        min-height: 100vh;
        padding: 40px 20px 56px;
        position: relative;
    }
    .shell::before,
    .shell::after {
        content: "";
        position: fixed;
        inset: 0;
        pointer-events: none;
        z-index: 4;
    }
    .shell::before {
        background:
            linear-gradient(180deg, rgba(255,255,255,0.15) 0%, rgba(255,255,255,0.04) 100%),
            repeating-linear-gradient(180deg, rgba(255,255,255,0.28) 0 1px, rgba(203, 213, 225, 0.20) 1px 2px, rgba(15, 23, 42, 0.06) 2px 4px);
        background-size: 100% 100%, 100% 4px;
        mix-blend-mode: multiply;
        opacity: 0.52;
        animation: crtFlicker 0.35s steps(2, end) infinite;
    }
    .shell::after {
        background:
            radial-gradient(circle at center, rgba(255,255,255,0.03) 0%, rgba(255,255,255,0.01) 40%, rgba(0,0,0,0.10) 72%, rgba(0,0,0,0.34) 100%);
    }
    .shell__container {
        max-width: 1100px;
        margin: 0 auto;
        display: grid;
        gap: 24px;
        position: relative;
        z-index: 1;
    }
    .hero,
    .panel {
        background-color: rgba(248, 250, 252, 0.96);
        color: #111827;
        border: 4px solid #111827;
        border-radius: 0;
        box-shadow:
            inset -4px -4px 0 0 rgba(0, 0, 0, 0.18),
            inset 4px 4px 0 0 rgba(255, 255, 255, 0.48),
            8px 8px 0 0 rgba(0, 0, 0, 0.55);
        position: relative;
    }
    .hero::before,
    .panel::before {
        content: "";
        position: absolute;
        inset: 0;
        border: 3px solid rgba(255, 255, 255, 0.72);
        pointer-events: none;
    }
    .hero {
        padding: 32px 36px;
        display: grid;
        gap: 16px;
        background:
            linear-gradient(180deg, rgba(255, 247, 214, 0.98) 0%, rgba(248, 250, 252, 0.96) 100%);
        overflow: hidden;
    }
    .hero__title,
    .hero__body,
    .hero__meta {
        padding-inline-start: 6px;
    }
    .hero__title {
        margin: 0;
        font-family: var(--font-display);
        font-size: clamp(30px, 5vw, 50px);
        line-height: 1.05;
        text-transform: uppercase;
        text-shadow: 3px 3px 0 rgba(15, 23, 42, 0.18);
    }
    .hero__body,
    .panel__body,
    .meta {
        margin: 0;
        font-family: var(--font-body);
        color: #334155;
        line-height: 1.7;
        font-size: 13px;
    }
    .hero__body {
        max-width: 52rem;
    }
    .hero__meta {
        display: flex;
        gap: 12px;
        row-gap: 10px;
        align-items: center;
        flex-wrap: wrap;
    }
    .badge {
        display: inline-flex;
        align-items: center;
        gap: 8px;
        padding: 10px 14px;
        font-family: var(--font-display);
        border: 3px solid #0f172a;
        background: #e2e8f0;
        color: #0f172a;
        box-shadow:
            inset 3px 3px 0 0 rgba(255,255,255,0.55),
            inset -3px -3px 0 0 rgba(0,0,0,0.12),
            3px 3px 0 0 rgba(15, 23, 42, 0.35);
        font-size: 11px;
        text-transform: uppercase;
    }
    .status-offline { color: #b45309; }
    .status-connecting { color: #1d4ed8; }
    .status-connected { color: #166534; }
    .grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
        gap: 24px;
    }
    .panel {
        padding: 30px 22px 22px;
        display: grid;
        gap: 12px;
        background:
            linear-gradient(180deg, rgba(255,255,255,0.96) 0%, rgba(241,245,249,0.96) 100%);
    }
    .panel--session {
        background:
            linear-gradient(180deg, rgba(255, 251, 235, 0.98) 0%, rgba(255,255,255,0.96) 100%);
    }
    .panel--controls {
        background:
            linear-gradient(180deg, rgba(236, 253, 245, 0.98) 0%, rgba(255,255,255,0.96) 100%);
    }
    .panel--judge {
        background:
            linear-gradient(180deg, rgba(239, 246, 255, 0.98) 0%, rgba(255,255,255,0.96) 100%);
    }
    .panel--runtime {
        background:
            linear-gradient(180deg, rgba(224, 231, 255, 0.98) 0%, rgba(255,255,255,0.96) 100%);
    }
    .panel--advanced {
        background:
            linear-gradient(180deg, rgba(241, 245, 249, 0.98) 0%, rgba(255,255,255,0.96) 100%);
    }
    .panel--session::after,
    .panel--controls::after,
    .panel--judge::after,
    .panel--runtime::after,
    .panel--advanced::after {
        content: "";
        position: absolute;
        top: 0;
        left: 0;
        right: 0;
        height: 16px;
        border-bottom: 4px solid #0f172a;
        pointer-events: none;
    }
    .panel--session::after { background: #ca8a04; }
    .panel--controls::after { background: #34d399; }
    .panel--judge::after { background: #38bdf8; }
    .panel--runtime::after { background: #4c63d9; }
    .panel--advanced::after { background: #64748b; }
    .panel__title {
        margin: 0;
        font-family: var(--font-display);
        font-size: 20px;
        text-transform: uppercase;
    }
    .panel__list {
        margin: 0;
        padding-left: 18px;
        color: #334155;
        display: grid;
        gap: 8px;
        font-size: 12px;
    }
    .panel__stack {
        display: grid;
        gap: 12px;
    }
    .flow-cards {
        display: grid;
        gap: 12px;
        grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    }
    .flow-card {
        background: #1d2938;
        color: #f8fafc;
        border: 4px solid #06101f;
        padding: 14px 16px;
        box-shadow:
            inset -4px -4px 0 0 rgba(0,0,0,0.35),
            4px 4px 0 0 rgba(0,0,0,0.35);
    }
    .flow-card__title {
        margin: 0 0 8px;
        font-family: var(--font-display);
        font-size: 12px;
        color: #facc15;
        text-transform: uppercase;
        letter-spacing: 0.08em;
    }
    .flow-card__body {
        margin: 0;
        font-family: var(--font-body);
        color: #cbd5e1;
        font-size: 12px;
        line-height: 1.6;
    }
    .input {
        width: 100%;
        font-family: var(--font-body);
        border-radius: 0;
        border: 4px solid #0f172a;
        background: #ffffff;
        color: #0f172a;
        padding: 10px 12px;
        font: inherit;
        box-shadow: inset 4px 4px 0 0 rgba(0,0,0,0.08);
        outline: none;
    }
    .input::placeholder {
        color: #64748b;
    }
    .input:focus-visible,
    .button:focus-visible {
        outline: 4px solid #fde047;
        outline-offset: 4px;
    }
    .button-row {
        display: flex;
        gap: 12px;
        flex-wrap: wrap;
    }
    .button {
        border: 4px solid #0f172a;
        border-radius: 0;
        padding: 10px 14px;
        font-family: var(--font-display);
        font-size: 12px;
        font-weight: 700;
        text-transform: uppercase;
        cursor: pointer;
        color: #0f172a;
        background: #e2e8f0;
        box-shadow:
            inset 4px 4px 0 0 rgba(255,255,255,0.72),
            inset -4px -4px 0 0 rgba(0,0,0,0.12),
            4px 4px 0 0 rgba(0,0,0,0.45);
        transition: transform 0.08s ease-out;
    }
    .button:hover {
        filter: brightness(1.03);
    }
    .button:active {
        transform: translate(4px, 4px);
        box-shadow:
            inset 4px 4px 0 0 rgba(0,0,0,0.12),
            inset -4px -4px 0 0 rgba(255,255,255,0.72),
            0 0 0 0 transparent;
    }
    .button:disabled {
        opacity: 0.65;
        cursor: wait;
        transform: none;
    }
    .button--primary {
        background: #34d399;
        color: #022c22;
    }
    .button--secondary {
        background: #38bdf8;
        color: #082f49;
    }
    .notice {
        border-radius: 0;
        padding: 14px 16px;
        border: 4px solid #0f172a;
        font-size: 12px;
        box-shadow: 4px 4px 0 0 rgba(0,0,0,0.35);
        position: relative;
        z-index: 1;
    }
    .notice-info {
        background: #dbeafe;
        color: #1e3a8a;
    }
    .notice-success {
        background: #dcfce7;
        color: #166534;
    }
    .notice-error {
        background: #fecdd3;
        color: #9f1239;
    }
    ::-webkit-scrollbar {
        width: 16px;
    }
    ::-webkit-scrollbar-track {
        background: #cbd5e1;
        border-left: 4px solid #0f172a;
    }
    ::-webkit-scrollbar-thumb {
        background: #64748b;
        border: 4px solid #0f172a;
        box-shadow: inset 2px 2px 0 0 rgba(255,255,255,0.3);
    }
    ::-webkit-scrollbar-thumb:hover {
        background: #475569;
    }
    .session-summary {
        display: grid;
        gap: 10px;
        grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    }
    .summary-chip {
        margin: 0;
        padding: 12px;
        font-family: var(--font-body);
        border: 3px solid #0f172a;
        background: #fffdf6;
        color: #0f172a;
        box-shadow:
            inset 3px 3px 0 0 rgba(255,255,255,0.68),
            inset -3px -3px 0 0 rgba(0,0,0,0.08),
            3px 3px 0 0 rgba(0,0,0,0.18);
    }
    .roster {
        display: grid;
        gap: 10px;
    }
    .roster__item {
        border: 4px solid #0f172a;
        border-radius: 0;
        background: #ffffff;
        padding: 14px 16px;
        display: flex;
        justify-content: space-between;
        gap: 12px;
        align-items: center;
        box-shadow:
            inset 3px 3px 0 0 rgba(255,255,255,0.72),
            inset -3px -3px 0 0 rgba(0,0,0,0.08),
            4px 4px 0 0 rgba(0,0,0,0.24);
    }
    .roster__item--phase {
        background: #fff7d6;
    }
    .roster__name {
        margin: 0;
        font-family: var(--font-display);
        font-size: 13px;
        font-weight: 700;
        text-transform: uppercase;
    }
    .roster__meta {
        margin: 6px 0 0;
        font-family: var(--font-body);
        color: #475569;
        font-size: 11px;
        text-transform: none;
        letter-spacing: 0.01em;
        line-height: 1.5;
    }
    .roster__status {
        font-family: var(--font-display);
        font-size: 11px;
        text-transform: uppercase;
        letter-spacing: 0.08em;
    }
    .roster__status--phase {
        padding: 6px 8px;
        border: 3px solid currentColor;
        background: rgba(255,255,255,0.65);
    }
    @media (max-width: 720px) {
        .shell {
            padding: 24px 14px 40px;
        }
        .hero,
        .panel {
            box-shadow:
                inset -4px -4px 0 0 rgba(0, 0, 0, 0.18),
                inset 4px 4px 0 0 rgba(255, 255, 255, 0.48),
                5px 5px 0 0 rgba(0, 0, 0, 0.45);
        }
        .hero {
            padding: 20px;
        }
        .panel {
            padding: 18px;
        }
        .button-row {
            flex-direction: column;
        }
        .button {
            width: 100%;
        }
        .roster__item {
            flex-direction: column;
            align-items: flex-start;
        }
    }
    @media (prefers-reduced-motion: reduce) {
        body {
            animation: none;
        }
        .shell::before {
            animation: none;
        }
        .button {
            transition: none;
        }
        .button:active {
            transform: none;
        }
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
struct VotingOptionRow {
    dragon_id: String,
    dragon_name: String,
    is_selected: bool,
    is_current_players_dragon: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EndVoteResultRow {
    placement_label: String,
    dragon_name: String,
    creator_name: String,
    votes_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EndPlayerScoreRow {
    placement_label: String,
    player_name: String,
    score_label: String,
    achievements_label: String,
    is_winner: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JudgeBundlePlayerRow {
    player_name: String,
    score_label: String,
    achievements_label: String,
    is_top_score: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JudgeBundleDragonRow {
    dragon_name: String,
    creator_name: String,
    caretaker_name: String,
    votes_label: String,
    actions_label: String,
    handover_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellState {
    screen: ShellScreen,
    connection_status: ConnectionStatus,
    coordinator: CoordinatorType,
    identity: Option<SessionIdentity>,
    session_snapshot: Option<ClientSessionSnapshot>,
    session_state: Option<ClientGameState>,
    judge_bundle: Option<JudgeBundle>,
    api_base_url: String,
    create_name: String,
    join_session_code: String,
    join_name: String,
    reconnect_session_code: String,
    reconnect_token: String,
    pending_flow: Option<PendingFlow>,
    pending_command: Option<SessionCommand>,
    pending_judge_bundle: bool,
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

    async fn fetch_judge_bundle(&self, request: WorkshopJudgeBundleRequest) -> Result<JudgeBundle, String> {
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
        judge_bundle: None,
        api_base_url: default_api_base_url(),
        create_name: String::new(),
        join_session_code: String::new(),
        join_name: String::new(),
        reconnect_session_code: String::new(),
        reconnect_token: String::new(),
        pending_flow: None,
        pending_command: None,
        pending_judge_bundle: false,
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
        ShellScreen::Home => "Raise a dragon, hand it off, and jump back into your workshop",
        ShellScreen::Session => "Your Dragon Shift session is live",
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

fn build_judge_bundle_request(snapshot: &ClientSessionSnapshot) -> WorkshopJudgeBundleRequest {
    WorkshopJudgeBundleRequest {
        session_code: snapshot.session_code.clone(),
        reconnect_token: snapshot.reconnect_token.clone(),
        coordinator_type: Some(snapshot.coordinator_type),
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
        Phase::Lobby => "Workshop lobby",
        Phase::Phase1 => "Discovery round",
        Phase::Handover => "Handover",
        Phase::Phase2 => "Care round",
        Phase::Voting => "Voting",
        Phase::End => "Workshop results",
    }
}

fn phase_screen_body(phase: Phase) -> &'static str {
    match phase {
        Phase::Lobby => "Review the roster, make sure everyone is here, and start when the workshop is ready.",
        Phase::Phase1 => "Observe your dragon, capture what stands out, and get ready for the handover.",
        Phase::Handover => "Write the handover notes your teammate will need for the next care round.",
        Phase::Phase2 => "Use the handover notes to guide care actions and keep the dragon thriving.",
        Phase::Voting => "Cast a creative vote, track submission progress, and wait for the host to reveal the standings.",
        Phase::End => "Review creative awards and final standings, then let the host reset when the workshop is complete.",
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

fn handover_focus_title(state: &ClientGameState) -> String {
    current_dragon(state)
        .map(|dragon| format!("Handover for {}", dragon.name))
        .unwrap_or_else(|| "Awaiting dragon handover".to_string())
}

fn handover_saved_tags(state: &ClientGameState) -> Vec<String> {
    current_dragon(state)
        .map(|dragon| dragon.handover_tags.clone())
        .unwrap_or_default()
}

fn handover_saved_summary(state: &ClientGameState) -> String {
    let saved_count = handover_saved_tags(state).len();
    format!("{saved_count} / 3 handover rules saved")
}

fn handover_status_copy(state: &ClientGameState) -> String {
    let saved_count = handover_saved_tags(state).len();
    match saved_count {
        0 => "Write three concrete care rules so the next player can continue without re-discovering everything.".to_string(),
        1 | 2 => format!("Add {} more rule(s) to complete the handover bundle.", 3 - saved_count),
        _ => "Handover bundle is complete. Host can move the workshop into Phase 2 once everyone finishes.".to_string(),
    }
}

fn player_name_by_id(state: &ClientGameState, player_id: Option<&str>) -> String {
    player_id
        .and_then(|player_id| state.players.get(player_id))
        .map(|player| player.name.clone())
        .unwrap_or_else(|| "Unknown".to_string())
}

fn phase2_focus_title(state: &ClientGameState) -> String {
    current_dragon(state)
        .map(|dragon| format!("Phase 2 care for {}", dragon.name))
        .unwrap_or_else(|| "Awaiting Phase 2 dragon".to_string())
}

fn phase2_creator_label(state: &ClientGameState) -> String {
    let Some(dragon) = current_dragon(state) else {
        return "Creator: Unknown".to_string();
    };

    format!("Creator: {}", player_name_by_id(state, dragon.original_owner_id.as_deref()))
}

fn phase2_handover_summary(state: &ClientGameState) -> String {
    let Some(dragon) = current_dragon(state) else {
        return "No handover notes yet.".to_string();
    };

    if dragon.handover_tags.is_empty() {
        "No handover notes yet.".to_string()
    } else {
        format!("{} handover note(s) available from the previous caretaker.", dragon.handover_tags.len())
    }
}

fn phase2_care_copy(state: &ClientGameState) -> String {
    let Some(dragon) = current_dragon(state) else {
        return "Phase 2 will begin once a dragon is assigned.".to_string();
    };

    let condition = dragon
        .condition_hint
        .as_deref()
        .filter(|hint| !hint.trim().is_empty())
        .unwrap_or("Expect faster decay in Phase 2 and react before the bars collapse.");

    format!("{condition} Phase 2 decay is stronger, so adjust faster than in discovery.")
}

fn voting_progress_label(state: &ClientGameState) -> String {
    let Some(voting) = state.voting.as_ref() else {
        return "0 / 0 votes submitted".to_string();
    };

    format!("{} / {} votes submitted", voting.submitted_count, voting.eligible_count)
}

fn voting_status_copy(state: &ClientGameState) -> String {
    let Some(voting) = state.voting.as_ref() else {
        return "Voting has not started yet.".to_string();
    };

    if voting.current_player_vote_dragon_id.is_some() {
        if voting.submitted_count >= voting.eligible_count {
            "Vote submitted. Host can reveal the results now.".to_string()
        } else {
            "Vote submitted. Waiting for the remaining players before reveal.".to_string()
        }
    } else {
        "Choose the most creative dragon that is not currently assigned to you.".to_string()
    }
}

fn voting_reveal_ready(state: &ClientGameState) -> bool {
    state
        .voting
        .as_ref()
        .map(|voting| voting.eligible_count > 0 && voting.submitted_count >= voting.eligible_count)
        .unwrap_or(false)
}

fn voting_option_rows(state: &ClientGameState) -> Vec<VotingOptionRow> {
    let current_player_dragon_id = current_player(state).and_then(|player| player.current_dragon_id.as_deref());
    let current_vote_dragon_id = state
        .voting
        .as_ref()
        .and_then(|voting| voting.current_player_vote_dragon_id.as_deref());
    let mut dragons = state.dragons.values().collect::<Vec<_>>();
    dragons.sort_by(|left, right| {
        left
            .name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });

    dragons
        .into_iter()
        .map(|dragon| VotingOptionRow {
            dragon_id: dragon.id.clone(),
            dragon_name: dragon.name.clone(),
            is_selected: current_vote_dragon_id == Some(dragon.id.as_str()),
            is_current_players_dragon: current_player_dragon_id == Some(dragon.id.as_str()),
        })
        .collect()
}

fn end_vote_result_rows(state: &ClientGameState) -> Vec<EndVoteResultRow> {
    let Some(results) = state.voting.as_ref().and_then(|voting| voting.results.as_ref()) else {
        return Vec::new();
    };
    let mut results = results.iter().collect::<Vec<_>>();
    results.sort_by(|left, right| {
        right.votes.cmp(&left.votes).then_with(|| {
            let left_name = state
                .dragons
                .get(&left.dragon_id)
                .map(|dragon| dragon.name.to_ascii_lowercase())
                .unwrap_or_else(|| left.dragon_id.to_ascii_lowercase());
            let right_name = state
                .dragons
                .get(&right.dragon_id)
                .map(|dragon| dragon.name.to_ascii_lowercase())
                .unwrap_or_else(|| right.dragon_id.to_ascii_lowercase());
            left_name.cmp(&right_name)
        }).then_with(|| left.dragon_id.cmp(&right.dragon_id))
    });

    results
        .into_iter()
        .enumerate()
        .map(|(index, result)| {
            let dragon = state.dragons.get(&result.dragon_id);
            EndVoteResultRow {
                placement_label: format!("#{} Creative pick", index + 1),
                dragon_name: dragon
                    .map(|dragon| dragon.name.clone())
                    .unwrap_or_else(|| "Unknown dragon".to_string()),
                creator_name: player_name_by_id(state, dragon.and_then(|dragon| dragon.original_owner_id.as_deref())),
                votes_label: if result.votes == 1 {
                    "1 vote".to_string()
                } else {
                    format!("{} votes", result.votes)
                },
            }
        })
        .collect()
}

fn end_results_status_copy(state: &ClientGameState) -> String {
    let rows = end_vote_result_rows(state);
    let Some(top_result) = rows.first() else {
        return "Results will appear once the host reveals the creative vote.".to_string();
    };

    format!(
        "Creative awards locked in. {} leads the reveal and the final standings are ready.",
        top_result.dragon_name
    )
}

fn end_player_score_rows(state: &ClientGameState) -> Vec<EndPlayerScoreRow> {
    let mut players = state.players.values().collect::<Vec<_>>();
    players.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.achievements.len().cmp(&left.achievements.len()))
            .then_with(|| left.name.to_ascii_lowercase().cmp(&right.name.to_ascii_lowercase()))
            .then_with(|| left.id.cmp(&right.id))
    });

    players
        .into_iter()
        .enumerate()
        .map(|(index, player)| EndPlayerScoreRow {
            placement_label: format!("#{}", index + 1),
            player_name: player.name.clone(),
            score_label: format!("{} pts", player.score),
            achievements_label: if player.achievements.is_empty() {
                "No achievements yet".to_string()
            } else {
                format!("{} achievement(s)", player.achievements.len())
            },
            is_winner: index == 0,
        })
        .collect()
}

fn judge_bundle_summary(bundle: &JudgeBundle) -> String {
    format!(
        "Artifacts: {} - Dragons: {} - Generated: {}",
        bundle.artifact_count,
        bundle.dragons.len(),
        bundle.generated_at
    )
}

fn judge_bundle_player_rows(bundle: &JudgeBundle) -> Vec<JudgeBundlePlayerRow> {
    let mut players = bundle.players.iter().collect::<Vec<_>>();
    players.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.achievements.len().cmp(&left.achievements.len()))
            .then_with(|| left.name.to_ascii_lowercase().cmp(&right.name.to_ascii_lowercase()))
            .then_with(|| left.player_id.cmp(&right.player_id))
    });

    players
        .into_iter()
        .enumerate()
        .map(|(index, player)| JudgeBundlePlayerRow {
            player_name: player.name.clone(),
            score_label: format!("{} pts", player.score),
            achievements_label: if player.achievements.is_empty() {
                "No achievements yet".to_string()
            } else {
                format!("{} achievement(s)", player.achievements.len())
            },
            is_top_score: index == 0,
        })
        .collect()
}

fn judge_bundle_dragon_rows(bundle: &JudgeBundle) -> Vec<JudgeBundleDragonRow> {
    let mut dragons = bundle.dragons.iter().collect::<Vec<_>>();
    dragons.sort_by(|left, right| {
        right
            .creative_vote_count
            .cmp(&left.creative_vote_count)
            .then_with(|| left.dragon_name.to_ascii_lowercase().cmp(&right.dragon_name.to_ascii_lowercase()))
            .then_with(|| left.dragon_id.cmp(&right.dragon_id))
    });

    dragons
        .into_iter()
        .map(|dragon| JudgeBundleDragonRow {
            dragon_name: dragon.dragon_name.clone(),
            creator_name: dragon.creator_name.clone(),
            caretaker_name: dragon.current_owner_name.clone(),
            votes_label: format!("{} creative vote(s)", dragon.creative_vote_count),
            actions_label: format!("{} phase 2 action(s) captured", dragon.phase2_actions.len()),
            handover_label: if dragon.handover_chain.handover_tags.is_empty() {
                "No handover tags captured".to_string()
            } else {
                format!("{} handover tag(s) captured", dragon.handover_chain.handover_tags.len())
            },
        })
        .collect()
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
    state.judge_bundle = None;
    state.join_session_code = snapshot.session_code.clone();
    state.reconnect_session_code = snapshot.session_code.clone();
    state.reconnect_token = snapshot.reconnect_token.clone();
    state.pending_flow = None;
    state.pending_judge_bundle = false;
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
    if command == SessionCommand::SubmitTags {
        state.handover_tags_input.clear();
    }
    if command == SessionCommand::ResetGame {
        state.judge_bundle = None;
    }
    state.notice = Some(success_notice(command_success_message(command)));
}

fn apply_command_error(state: &mut ShellState, error: String) {
    state.pending_command = None;
    state.notice = Some(error_notice(&error));
}

fn apply_judge_bundle_success(state: &mut ShellState, bundle: JudgeBundle) {
    state.pending_judge_bundle = false;
    state.judge_bundle = Some(bundle);
    state.notice = Some(success_notice("Workshop archive ready."));
}

fn apply_judge_bundle_error(state: &mut ShellState, error: String) {
    state.pending_judge_bundle = false;
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
    state.notice = Some(info_notice("Syncing session…"));
}

fn apply_server_ws_message(state: &mut ShellState, message: ServerWsMessage) {
    match message {
        ServerWsMessage::StateUpdate(client_state) => {
            let first_attach = state.connection_status != ConnectionStatus::Connected;
            let phase = client_state.phase;
            state.screen = ShellScreen::Session;
            state.session_state = Some(client_state);
            state.connection_status = ConnectionStatus::Connected;
            state.pending_command = None;
            if phase != Phase::End {
                state.judge_bundle = None;
                state.pending_judge_bundle = false;
            }
            if first_attach {
                state.notice = Some(info_notice("Session synced."));
            }
        }
        ServerWsMessage::Notice(ProtocolSessionNotice { level, title, message }) => {
            let combined = if title.trim().is_empty() {
                message
            } else {
                format!("{title}: {message}")
            };
            let tone = map_notice_tone(level);
            state.notice = Some(ShellNotice { tone, message: combined });
        }
        ServerWsMessage::Error { message } => {
            state.connection_status = ConnectionStatus::Offline;
            state.notice = Some(error_notice(&message));
        }
        ServerWsMessage::Pong => {
            state.connection_status = ConnectionStatus::Connected;
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
    let snapshot = snapshot.ok_or_else(|| "Join a workshop before syncing the session.".to_string())?;
    let envelope_json = serde_json::to_string(&ClientWsMessage::AttachSession(build_session_envelope(&snapshot)))
        .map_err(|error| format!("failed to encode attach payload: {error}"))?;
    let socket = web_sys::WebSocket::new(&build_ws_url(&base_url))
        .map_err(|_| "failed to open session connection".to_string())?;

    shell_state.with_mut(apply_realtime_connecting);

    let open_socket = socket.clone();
    let mut open_state = shell_state;
    let onopen = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        if open_socket.send_with_str(&envelope_json).is_err() {
            open_state.with_mut(|state| {
                state.connection_status = ConnectionStatus::Offline;
                state.notice = Some(error_notice("Could not sync the session."));
            });
        }
    }) as Box<dyn FnMut(_)>);
    socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));

    let mut message_state = shell_state;
    let onmessage = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
        if let Some(text) = event.data().as_string() {
            match serde_json::from_str::<ServerWsMessage>(&text) {
                Ok(message) => message_state.with_mut(|state| apply_server_ws_message(state, message)),
                Err(_) => message_state.with_mut(|state| {
                    state.connection_status = ConnectionStatus::Offline;
                    state.notice = Some(error_notice("Received an invalid session update."));
                }),
            }
        }
    }) as Box<dyn FnMut(_)>);
    socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

    let mut error_state = shell_state;
    let onerror = Closure::wrap(Box::new(move |_event: web_sys::ErrorEvent| {
        error_state.with_mut(|state| {
            state.connection_status = ConnectionStatus::Offline;
            state.notice = Some(error_notice("Session connection failed."));
        });
    }) as Box<dyn FnMut(_)>);
    socket.set_onerror(Some(onerror.as_ref().unchecked_ref()));

    let mut close_state = shell_state;
    let onclose = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        close_state.with_mut(|state| {
            state.connection_status = ConnectionStatus::Offline;
            state.notice = Some(info_notice("Session connection closed."));
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

async fn submit_judge_bundle_request(mut shell_state: Signal<ShellState>) {
    let (base_url, snapshot) = {
        let state = shell_state.read();
        (state.api_base_url.clone(), state.session_snapshot.clone())
    };

    let Some(snapshot) = snapshot else {
        shell_state.with_mut(|state| state.notice = Some(error_notice("Connect to a workshop before building the archive.")));
        return;
    };

    shell_state.with_mut(|state| {
        state.pending_judge_bundle = true;
        state.notice = Some(info_notice("Building workshop archive…"));
    });

    let api = AppWebApi::new(base_url);
    match api.fetch_judge_bundle(build_judge_bundle_request(&snapshot)).await {
        Ok(bundle) => shell_state.with_mut(|state| apply_judge_bundle_success(state, bundle)),
        Err(error) => shell_state.with_mut(|state| apply_judge_bundle_error(state, error)),
    }
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
    let submit_vote_state = state;
    let build_judge_bundle_state = state;
    let connection_badge_class = format!("badge {}", connection_status_class(&shell.connection_status));
    let commands_disabled = shell.pending_flow.is_some() || shell.pending_command.is_some() || shell.session_snapshot.is_none();
    let judge_bundle_disabled = commands_disabled || shell.pending_judge_bundle;
    let has_session_snapshot = shell.session_snapshot.is_some();
    let realtime_button_label = if shell.realtime_bootstrap_attempted {
        "Reconnect session"
    } else {
        "Sync session"
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
    let handover_title = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Handover)
        .map(handover_focus_title)
        .unwrap_or_default();
    let handover_summary = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Handover)
        .map(handover_saved_summary)
        .unwrap_or_default();
    let handover_status = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Handover)
        .map(handover_status_copy)
        .unwrap_or_default();
    let handover_saved = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Handover)
        .map(handover_saved_tags)
        .unwrap_or_default();
    let handover_draft_count = parse_tags_input(&shell.handover_tags_input).len();
    let phase2_title = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Phase2)
        .map(phase2_focus_title)
        .unwrap_or_default();
    let phase2_creator = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Phase2)
        .map(phase2_creator_label)
        .unwrap_or_default();
    let phase2_handover = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Phase2)
        .map(phase2_handover_summary)
        .unwrap_or_default();
    let phase2_care = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Phase2)
        .map(phase2_care_copy)
        .unwrap_or_default();
    let voting_progress = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Voting)
        .map(voting_progress_label)
        .unwrap_or_default();
    let voting_status = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Voting)
        .map(voting_status_copy)
        .unwrap_or_default();
    let voting_rows = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Voting)
        .map(voting_option_rows)
        .unwrap_or_default();
    let voting_reveal_enabled = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Voting)
        .map(voting_reveal_ready)
        .unwrap_or(false);
    let voting_is_host = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::Voting)
        .and_then(current_player)
        .map(|player| player.is_host)
        .unwrap_or(false);
    let end_results_status = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::End)
        .map(end_results_status_copy)
        .unwrap_or_default();
    let end_vote_rows = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::End)
        .map(end_vote_result_rows)
        .unwrap_or_default();
    let end_score_rows = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::End)
        .map(end_player_score_rows)
        .unwrap_or_default();
    let end_is_host = shell
        .session_state
        .as_ref()
        .filter(|session| session.phase == Phase::End)
        .and_then(current_player)
        .map(|player| player.is_host)
        .unwrap_or(false);
    let judge_bundle_summary_label = shell
        .judge_bundle
        .as_ref()
        .map(judge_bundle_summary)
        .unwrap_or_else(|| {
            if shell.session_state.as_ref().map(|session| session.phase == Phase::End).unwrap_or(false) {
                "Build the workshop archive to capture the final workshop snapshot.".to_string()
            } else {
                String::new()
            }
        });
    let judge_bundle_player_rows = shell
        .judge_bundle
        .as_ref()
        .map(judge_bundle_player_rows)
        .unwrap_or_default();
    let judge_bundle_dragon_rows = shell
        .judge_bundle
        .as_ref()
        .map(judge_bundle_dragon_rows)
        .unwrap_or_default();

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
                    h1 { class: "hero__title", "Dragon Shift" }
                    p { class: "hero__body", {screen_title(&shell.screen)} }
                    p { class: "meta", "Raise a dragon for one short shift, pass along practical care notes, and keep the whole room aligned from lobby to final results." }
                    div { class: "hero__meta",
                        span { class: connection_badge_class, "Connection: " {connection_status_label(&shell.connection_status)} }
                        if has_session_snapshot {
                            span { class: "badge", "Workshop: " {session_code_label.clone()} }
                            span { class: "badge", "Player: " {active_player_label.clone()} }
                        }
                    }
                }
                if let Some(notice) = shell.notice.clone() {
                    article { class: format!("notice {}", notice_class(notice.tone)),
                        {notice.message}
                    }
                }
                section { class: "grid",
                    article { class: "panel panel--runtime",
                        h2 { class: "panel__title", "Workshop brief" }
                        p { class: "panel__body", "One short discovery round, one careful handover, one shared care loop, then a final vote and archive." }
                        div { class: "flow-cards",
                            article { class: "flow-card",
                                p { class: "flow-card__title", "1. Create pet" }
                                p { class: "flow-card__body", "Start a room, describe your dragon, and get everyone ready to begin." }
                            }
                            article { class: "flow-card",
                                p { class: "flow-card__title", "2. Discover rules" }
                                p { class: "flow-card__body", "Observe what changes across the shift and capture the signals that matter." }
                            }
                            article { class: "flow-card",
                                p { class: "flow-card__title", "3. Handover" }
                                p { class: "flow-card__body", "Write practical notes so the next teammate can care for the dragon with confidence." }
                            }
                            article { class: "flow-card",
                                p { class: "flow-card__title", "4. Care and vote" }
                                p { class: "flow-card__body", "Use the handover, finish the round, then celebrate the most creative dragon together." }
                            }
                        }
                    }
                    article { class: "panel",
                        h2 { class: "panel__title", "Create workshop" }
                        p { class: "panel__body", "Start a new workshop and share the code with your group." }
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
                        h2 { class: "panel__title", "Join workshop" }
                        p { class: "panel__body", "Join with a workshop code or reopen the last saved session from this browser." }
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
                    article { class: "panel panel--session",
                        h2 { class: "panel__title", "Shift board" }
                        div { class: "session-summary",
                            p { class: "summary-chip", "Workshop code: " {session_code_label} }
                            p { class: "summary-chip", "Current caretaker: " {active_player_label} }
                            p { class: "summary-chip", "Current round: " {session_phase_label} }
                            p { class: "summary-chip", "Players in view: " {players_count_label} }
                        }
                        h2 { class: "panel__title", "Current round" }
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
                            article { class: "roster__item roster__item--phase",
                                div {
                                    p { class: "roster__name", {phase1_title} }
                                    p { class: "roster__meta", {phase1_observations.clone()} }
                                }
                                span { class: "roster__status roster__status--phase status-connecting", "Discovery" }
                            }
                            p { class: "panel__body", {phase1_body} }
                        }
                        if !handover_title.is_empty() {
                            article { class: "roster__item roster__item--phase",
                                div {
                                    p { class: "roster__name", {handover_title} }
                                    p { class: "roster__meta", {handover_summary.clone()} }
                                }
                                span { class: "roster__status roster__status--phase status-connecting", "Handover" }
                            }
                            p { class: "panel__body", {handover_status.clone()} }
                            p { class: "meta", "Draft rules parsed from input: " {handover_draft_count.to_string()} }
                            if !handover_saved.is_empty() {
                                div { class: "roster",
                                    for tag in handover_saved {
                                        article { class: "roster__item",
                                            div {
                                                p { class: "roster__name", {tag} }
                                                p { class: "roster__meta", "Saved handover rule" }
                                            }
                                            span { class: "roster__status status-connected", "Saved" }
                                        }
                                    }
                                }
                            }
                        }
                        if !phase2_title.is_empty() {
                            article { class: "roster__item roster__item--phase",
                                div {
                                    p { class: "roster__name", {phase2_title} }
                                    p { class: "roster__meta", {phase2_creator.clone()} }
                                }
                                span { class: "roster__status roster__status--phase status-connected", "Care" }
                            }
                            p { class: "panel__body", {phase2_care.clone()} }
                            p { class: "meta", "Handover notes from previous caretaker: " {phase2_handover.clone()} }
                        }
                        if !voting_progress.is_empty() {
                            article { class: "roster__item roster__item--phase",
                                div {
                                    p { class: "roster__name", "Vote for the most creative dragon" }
                                    p { class: "roster__meta", {voting_progress.clone()} }
                                }
                                span { class: "roster__status roster__status--phase status-connected", "Voting" }
                            }
                            p { class: "panel__body", {voting_status.clone()} }
                            div { class: "roster",
                                for row in voting_rows {
                                    article { class: "roster__item",
                                        div {
                                            p { class: "roster__name", {row.dragon_name.clone()} }
                                            p { class: "roster__meta",
                                                if row.is_current_players_dragon {
                                                    "Your current dragon cannot receive your vote."
                                                } else if row.is_selected {
                                                    "Current selection"
                                                } else {
                                                    "Eligible vote target"
                                                }
                                            }
                                        }
                                        if row.is_current_players_dragon {
                                            span { class: "roster__status status-offline", "Blocked" }
                                        } else if row.is_selected {
                                            span { class: "roster__status status-connected", "Selected" }
                                        } else {
                                            button {
                                                class: "button button--secondary",
                                                disabled: commands_disabled,
                                                onclick: {
                                                    let vote_target = row.dragon_id.clone();
                                                    move |_| {
                                                        spawn(submit_workshop_command(
                                                            submit_vote_state,
                                                            SessionCommand::SubmitVote,
                                                            Some(serde_json::json!({ "dragonId": vote_target.clone() })),
                                                        ));
                                                    }
                                                },
                                                "Vote"
                                            }
                                        }
                                    }
                                }
                            }
                            if voting_is_host {
                                p {
                                    class: "meta",
                                    if voting_reveal_enabled {
                                        "All votes are in. Reveal is unlocked for the host."
                                    } else {
                                        "Reveal unlocks after all eligible votes are submitted."
                                    }
                                }
                                div { class: "button-row",
                                    button {
                                        class: "button button--secondary",
                                        disabled: commands_disabled || !voting_reveal_enabled,
                                        onclick: move |_| {
                                            spawn(submit_workshop_command(reveal_results_state, SessionCommand::RevealVotingResults, None));
                                        },
                                        "Reveal results"
                                    }
                                }
                            }
                        }
                        if !end_results_status.is_empty() {
                            article { class: "roster__item roster__item--phase",
                                div {
                                    p { class: "roster__name", "Workshop results" }
                                    p { class: "roster__meta", {end_results_status.clone()} }
                                }
                                span { class: "roster__status roster__status--phase status-connected", "Final" }
                            }
                            if !end_vote_rows.is_empty() {
                                p { class: "meta", "Creative pet awards" }
                                div { class: "roster",
                                    for row in end_vote_rows {
                                        article { class: "roster__item",
                                            div {
                                                p { class: "roster__name", {row.dragon_name.clone()} }
                                                p { class: "roster__meta", {row.placement_label.clone()} " - Created by " {row.creator_name.clone()} }
                                            }
                                            span { class: "roster__status status-connected", {row.votes_label.clone()} }
                                        }
                                    }
                                }
                            }
                            if !end_score_rows.is_empty() {
                                p { class: "meta", "Final player standings" }
                                div { class: "roster",
                                    for row in end_score_rows {
                                        article { class: "roster__item",
                                            div {
                                                p { class: "roster__name", {row.player_name.clone()} }
                                                p { class: "roster__meta", {row.placement_label.clone()} " - " {row.achievements_label.clone()} }
                                            }
                                            span {
                                                class: format!("roster__status {}", if row.is_winner { "status-connected" } else { "status-connecting" }),
                                                {row.score_label.clone()}
                                            }
                                        }
                                    }
                                }
                            }
                            p {
                                class: "meta",
                                if end_is_host {
                                    "Host can reset the workshop when the group is ready for another round."
                                } else {
                                    "Waiting for the host to reset or archive this workshop."
                                }
                            }
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
                        p { class: "meta", "This browser remembers your last workshop so you can reconnect without retyping everything." }
                    }
                    article { class: "panel panel--controls",
                        h2 { class: "panel__title", "Session controls" }
                        p { class: "panel__body", "Use these controls to move the workshop from setup through discovery, handover, care, voting, and the final reset." }
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
                            if end_is_host {
                                div { class: "button-row",
                                    button {
                                        class: "button button--secondary",
                                        disabled: judge_bundle_disabled,
                                        onclick: move |_| {
                                            spawn(submit_judge_bundle_request(build_judge_bundle_state));
                                        },
                                        if shell.pending_judge_bundle {
                                            "Building archive…"
                                        } else {
                                            "Build archive"
                                        }
                                    }
                                }
                            }
                        }
                    }
                    article { class: "panel panel--judge",
                        h2 { class: "panel__title", "Workshop archive" }
                        p { class: "panel__body", {judge_bundle_summary_label.clone()} }
                        if !judge_bundle_player_rows.is_empty() {
                            p { class: "meta", "Captured final standings" }
                            div { class: "roster",
                                for row in judge_bundle_player_rows {
                                    article { class: "roster__item",
                                        div {
                                            p { class: "roster__name", {row.player_name.clone()} }
                                            p { class: "roster__meta", {row.achievements_label.clone()} }
                                        }
                                        span {
                                            class: format!("roster__status {}", if row.is_top_score { "status-connected" } else { "status-connecting" }),
                                            {row.score_label.clone()}
                                        }
                                    }
                                }
                            }
                        }
                        if !judge_bundle_dragon_rows.is_empty() {
                            p { class: "meta", "Captured dragons" }
                            div { class: "roster",
                                for row in judge_bundle_dragon_rows {
                                    article { class: "roster__item",
                                        div {
                                            p { class: "roster__name", {row.dragon_name.clone()} }
                                            p { class: "roster__meta", "Creator: " {row.creator_name.clone()} " - Caretaker: " {row.caretaker_name.clone()} }
                                            p { class: "roster__meta", {row.actions_label.clone()} " - " {row.handover_label.clone()} }
                                        }
                                        span { class: "roster__status status-connected", {row.votes_label.clone()} }
                                    }
                                }
                            }
                        }
                    }
                    article { class: "panel panel--advanced",
                        h2 { class: "panel__title", "Advanced" }
                        p { class: "panel__body", "Use a different address only when you want this screen to point at another workshop server." }
                        div { class: "panel__stack",
                            input {
                                class: "input",
                                value: shell.api_base_url,
                                placeholder: "http://127.0.0.1:4100",
                                oninput: move |event| state.with_mut(|shell| shell.api_base_url = event.value())
                            }
                            p { class: "meta", "Everyone in the workshop sees the same session state through this connection." }
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

        assert_eq!(screen_title(&state.screen), "Raise a dragon, hand it off, and jump back into your workshop");
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
        assert_eq!(phase_screen_title(Phase::Lobby), "Workshop lobby");
        assert_eq!(phase_screen_title(Phase::Voting), "Voting");
        assert_eq!(
            phase_screen_body(Phase::Lobby),
            "Review the roster, make sure everyone is here, and start when the workshop is ready."
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

    fn mock_handover_state() -> ClientGameState {
        let mut state = mock_phase1_state();
        state.phase = Phase::Handover;
        state
            .dragons
            .get_mut("dragon-1")
            .expect("dragon-1")
            .handover_tags = vec!["Feed at dusk".to_string(), "Avoid long idle gaps".to_string()];
        state
    }

    #[test]
    fn handover_helpers_report_saved_rules_and_remaining_work() {
        let state = mock_handover_state();

        assert_eq!(handover_focus_title(&state), "Handover for Comet");
        assert_eq!(handover_saved_summary(&state), "2 / 3 handover rules saved");
        assert_eq!(handover_saved_tags(&state).len(), 2);
        assert_eq!(handover_status_copy(&state), "Add 1 more rule(s) to complete the handover bundle.");
    }

    #[test]
    fn handover_helpers_handle_empty_bundle() {
        let mut state = mock_phase1_state();
        state.phase = Phase::Handover;

        assert_eq!(handover_saved_summary(&state), "0 / 3 handover rules saved");
        assert!(handover_status_copy(&state).contains("Write three concrete care rules"));
    }

    fn mock_phase2_state() -> ClientGameState {
        let mut state = mock_handover_state();
        state.phase = Phase::Phase2;
        state
    }

    #[test]
    fn phase2_helpers_expose_creator_and_handover_context() {
        let state = mock_phase2_state();

        assert_eq!(phase2_focus_title(&state), "Phase 2 care for Comet");
        assert_eq!(phase2_creator_label(&state), "Creator: Alice");
        assert_eq!(phase2_handover_summary(&state), "2 handover note(s) available from the previous caretaker.");
        assert!(phase2_care_copy(&state).contains("Phase 2 decay is stronger"));
    }

    #[test]
    fn phase2_helpers_fall_back_without_handover_notes() {
        let mut state = mock_phase1_state();
        state.phase = Phase::Phase2;

        assert_eq!(phase2_handover_summary(&state), "No handover notes yet.");
        assert_eq!(phase2_creator_label(&state), "Creator: Alice");
    }

    fn mock_voting_state() -> ClientGameState {
        let mut state = mock_phase2_state();
        state.phase = Phase::Voting;
        state.players.insert(
            "player-2".to_string(),
            Player {
                id: "player-2".to_string(),
                name: "Bob".to_string(),
                is_host: false,
                score: 0,
                current_dragon_id: Some("dragon-2".to_string()),
                achievements: Vec::new(),
                is_ready: true,
                is_connected: true,
                pet_description: Some("Bob's workshop dragon".to_string()),
            },
        );
        state.dragons.insert(
            "dragon-2".to_string(),
            ClientDragon {
                id: "dragon-2".to_string(),
                name: "Nova".to_string(),
                visuals: protocol::DragonVisuals {
                    base: 2,
                    color_p: "#ffaa88".to_string(),
                    color_s: "#cc6644".to_string(),
                    color_a: "#fff0aa".to_string(),
                },
                original_owner_id: Some("player-2".to_string()),
                current_owner_id: Some("player-2".to_string()),
                stats: protocol::DragonStats {
                    hunger: 61,
                    energy: 63,
                    happiness: 77,
                },
                condition_hint: Some("Responds well to music at night.".to_string()),
                discovery_observations: vec!["Settles quickly after music".to_string()],
                handover_tags: vec!["Start with music".to_string()],
                last_action: DragonAction::Play,
                last_emotion: DragonEmotion::Neutral,
                speech: Some("A calmer rhythm helps.".to_string()),
                speech_timer: 1,
                action_cooldown: 0,
                custom_sprites: None,
            },
        );
        state.voting = Some(protocol::ClientVotingState {
            eligible_count: 2,
            submitted_count: 1,
            current_player_vote_dragon_id: Some("dragon-2".to_string()),
            results: None,
        });
        state
    }

    #[test]
    fn voting_helpers_expose_progress_selection_and_self_vote_block() {
        let state = mock_voting_state();
        let rows = voting_option_rows(&state);

        assert_eq!(voting_progress_label(&state), "1 / 2 votes submitted");
        assert_eq!(voting_status_copy(&state), "Vote submitted. Waiting for the remaining players before reveal.");
        assert!(!voting_reveal_ready(&state));
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.dragon_name == "Comet" && row.is_current_players_dragon));
        assert!(rows.iter().any(|row| row.dragon_name == "Nova" && row.is_selected));
    }

    #[test]
    fn voting_helpers_mark_reveal_ready_when_all_votes_are_in() {
        let mut state = mock_voting_state();
        state.voting = Some(protocol::ClientVotingState {
            eligible_count: 2,
            submitted_count: 2,
            current_player_vote_dragon_id: Some("dragon-2".to_string()),
            results: None,
        });

        assert!(voting_reveal_ready(&state));
        assert_eq!(voting_status_copy(&state), "Vote submitted. Host can reveal the results now.");
    }

    fn mock_end_state() -> ClientGameState {
        let mut state = mock_voting_state();
        state.phase = Phase::End;
        state.players.get_mut("player-1").expect("player-1").score = 12;
        state.players.get_mut("player-1").expect("player-1").achievements = vec!["careful_observer".to_string()];
        state.players.get_mut("player-2").expect("player-2").score = 18;
        state.players.get_mut("player-2").expect("player-2").achievements = vec!["creative_pick".to_string(), "steady_hands".to_string()];
        state.voting = Some(protocol::ClientVotingState {
            eligible_count: 2,
            submitted_count: 2,
            current_player_vote_dragon_id: Some("dragon-2".to_string()),
            results: Some(vec![
                protocol::VoteResult {
                    dragon_id: "dragon-2".to_string(),
                    votes: 2,
                },
                protocol::VoteResult {
                    dragon_id: "dragon-1".to_string(),
                    votes: 1,
                },
            ]),
        });
        state
    }

    fn mock_judge_bundle() -> JudgeBundle {
        JudgeBundle {
            session_id: "session-1".to_string(),
            session_code: "123456".to_string(),
            current_phase: Phase::End,
            generated_at: "2026-01-01T12:00:00Z".to_string(),
            artifact_count: 6,
            players: vec![
                protocol::JudgePlayerSummary {
                    player_id: "player-1".to_string(),
                    name: "Alice".to_string(),
                    score: 12,
                    achievements: vec!["careful_observer".to_string()],
                },
                protocol::JudgePlayerSummary {
                    player_id: "player-2".to_string(),
                    name: "Bob".to_string(),
                    score: 18,
                    achievements: vec!["creative_pick".to_string(), "steady_hands".to_string()],
                },
            ],
            dragons: vec![
                protocol::JudgeDragonBundle {
                    dragon_id: "dragon-2".to_string(),
                    dragon_name: "Nova".to_string(),
                    creator_player_id: "player-2".to_string(),
                    creator_name: "Bob".to_string(),
                    current_owner_id: "player-2".to_string(),
                    current_owner_name: "Bob".to_string(),
                    creative_vote_count: 2,
                    final_stats: protocol::DragonStats {
                        hunger: 61,
                        energy: 63,
                        happiness: 77,
                    },
                    handover_chain: protocol::JudgeHandoverChain {
                        creator_instructions: "Start with music".to_string(),
                        discovery_observations: vec!["Settles quickly after music".to_string()],
                        handover_tags: vec!["Start with music".to_string()],
                    },
                    phase2_actions: vec![protocol::JudgeActionTrace {
                        player_id: "player-2".to_string(),
                        player_name: "Bob".to_string(),
                        phase: Phase::Phase2,
                        action_type: "play".to_string(),
                        action_value: None,
                        created_at: "2026-01-01T10:00:00Z".to_string(),
                        resulting_stats: None,
                    }],
                },
                protocol::JudgeDragonBundle {
                    dragon_id: "dragon-1".to_string(),
                    dragon_name: "Comet".to_string(),
                    creator_player_id: "player-1".to_string(),
                    creator_name: "Alice".to_string(),
                    current_owner_id: "player-1".to_string(),
                    current_owner_name: "Alice".to_string(),
                    creative_vote_count: 1,
                    final_stats: protocol::DragonStats {
                        hunger: 72,
                        energy: 55,
                        happiness: 81,
                    },
                    handover_chain: protocol::JudgeHandoverChain {
                        creator_instructions: "Feed at dusk".to_string(),
                        discovery_observations: vec!["Loves food at dusk".to_string()],
                        handover_tags: vec!["Feed at dusk".to_string(), "Avoid long idle gaps".to_string()],
                    },
                    phase2_actions: vec![],
                },
            ],
        }
    }

    #[test]
    fn end_helpers_rank_creative_results_and_final_scores() {
        let state = mock_end_state();
        let vote_rows = end_vote_result_rows(&state);
        let score_rows = end_player_score_rows(&state);

        assert_eq!(end_results_status_copy(&state), "Creative awards locked in. Nova leads the reveal and the final standings are ready.");
        assert_eq!(vote_rows.len(), 2);
        assert_eq!(vote_rows[0].dragon_name, "Nova");
        assert_eq!(vote_rows[0].creator_name, "Bob");
        assert_eq!(vote_rows[0].votes_label, "2 votes");
        assert_eq!(score_rows[0].player_name, "Bob");
        assert_eq!(score_rows[0].score_label, "18 pts");
        assert!(score_rows[0].is_winner);
    }

    #[test]
    fn end_helpers_fall_back_before_results_are_revealed() {
        let mut state = mock_voting_state();
        state.phase = Phase::End;
        state.players.get_mut("player-1").expect("player-1").score = 4;
        state.players.get_mut("player-2").expect("player-2").score = 3;

        assert_eq!(end_results_status_copy(&state), "Results will appear once the host reveals the creative vote.");
        assert!(end_vote_result_rows(&state).is_empty());
        assert_eq!(end_player_score_rows(&state)[0].player_name, "Alice");
    }

    #[test]
    fn judge_bundle_helpers_summarize_players_and_dragons() {
        let bundle = mock_judge_bundle();
        let players = judge_bundle_player_rows(&bundle);
        let dragons = judge_bundle_dragon_rows(&bundle);

        assert_eq!(judge_bundle_summary(&bundle), "Artifacts: 6 - Dragons: 2 - Generated: 2026-01-01T12:00:00Z");
        assert_eq!(players[0].player_name, "Bob");
        assert_eq!(players[0].score_label, "18 pts");
        assert!(players[0].is_top_score);
        assert_eq!(dragons[0].dragon_name, "Nova");
        assert_eq!(dragons[0].votes_label, "2 creative vote(s)");
        assert_eq!(dragons[0].actions_label, "1 phase 2 action(s) captured");
    }

    #[test]
    fn apply_judge_bundle_success_stores_bundle_and_clears_pending() {
        let mut state = default_shell_state();
        state.pending_judge_bundle = true;

        apply_judge_bundle_success(&mut state, mock_judge_bundle());

        assert!(!state.pending_judge_bundle);
        assert!(state.judge_bundle.is_some());
        assert_eq!(state.notice.as_ref().map(|notice| notice.message.as_str()), Some("Workshop archive ready."));
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
    fn apply_successful_command_keeps_phase_authoritative_and_clears_pending_command() {
        let mut state = default_shell_state();
        apply_join_success(&mut state, mock_join_success(), PendingFlow::Join);
        state.pending_command = Some(SessionCommand::StartPhase1);
        let original_phase = state.session_state.as_ref().map(|session| session.phase);

        apply_successful_command(&mut state, SessionCommand::StartPhase1);

        assert_eq!(state.pending_command, None);
        assert_eq!(state.session_state.as_ref().map(|session| session.phase), original_phase);
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
        assert_eq!(state.notice.as_ref().map(|notice| notice.message.as_str()), Some("Session synced."));
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
