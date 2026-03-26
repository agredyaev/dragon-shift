use dioxus::prelude::*;
use protocol::CoordinatorType;

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
"#;

fn main() {
    launch(App);
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShellScreen {
    Home,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConnectionStatus {
    Offline,
    Connecting,
    Connected,
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
}

fn default_shell_state() -> ShellState {
    ShellState {
        screen: ShellScreen::Home,
        connection_status: ConnectionStatus::Offline,
        coordinator: CoordinatorType::Rust,
        identity: None,
    }
}

fn screen_title(screen: &ShellScreen) -> &'static str {
    match screen {
        ShellScreen::Home => "Create or join a workshop",
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

#[component]
fn App() -> Element {
    let state = use_signal(default_shell_state);
    let shell = state.read().clone();
    let connection_badge_class = format!("badge {}", connection_status_class(&shell.connection_status));
    let identity_label = if shell.identity.is_some() { "present" } else { "empty" };

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
                    p { class: "meta", "Dioxus shell is booted independently from the legacy React/Vite runtime and is ready for the next create/join slices." }
                    div { class: "hero__meta",
                        span { class: "badge", "Coordinator: Rust" }
                        span { class: connection_badge_class, "Connection: " {connection_status_label(&shell.connection_status)} }
                        span { class: "badge", "Reconnect identity: " {identity_label} }
                    }
                }
                section { class: "grid",
                    article { class: "panel",
                        h2 { class: "panel__title", "Session flows queued" }
                        p { class: "panel__body", "The shell owns route/state bootstrap first. Network-backed create, join, and reconnect flows will be added on top of this state container." }
                        ul { class: "panel__list",
                            li { "Create workshop" }
                            li { "Join workshop" }
                            li { "Reconnect with persisted identity" }
                        }
                    }
                    article { class: "panel",
                        h2 { class: "panel__title", "Transport plan" }
                        p { class: "panel__body", "HTTP remains the command path, while WebSocket state streaming will be attached after the shell and browser persistence slices are stable." }
                        ul { class: "panel__list",
                            li { "HTTP command client" }
                            li { "WebSocket client bootstrap" }
                            li { "Connection status and toasts" }
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

    #[test]
    fn default_shell_state_boots_home_screen_with_rust_coordinator() {
        let state = default_shell_state();

        assert_eq!(state.screen, ShellScreen::Home);
        assert_eq!(state.connection_status, ConnectionStatus::Offline);
        assert_eq!(state.coordinator, CoordinatorType::Rust);
        assert_eq!(state.identity, None);
    }

    #[test]
    fn shell_labels_match_bootstrap_state() {
        let state = default_shell_state();

        assert_eq!(screen_title(&state.screen), "Create or join a workshop");
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
}
