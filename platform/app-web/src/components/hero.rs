use dioxus::prelude::*;
use protocol::ClientGameState;

use crate::helpers::{
    active_player_name, connection_status_class, connection_status_label, screen_title,
};
use crate::state::IdentityState;

#[component]
pub fn Hero(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
) -> Element {
    let id = identity.read();
    let gs = game_state.read();

    let connection_badge_class =
        format!("badge {}", connection_status_class(&id.connection_status));
    let has_session_snapshot = id.session_snapshot.is_some();
    let session_code_label = id
        .session_snapshot
        .as_ref()
        .map(|s| s.session_code.clone())
        .unwrap_or_else(|| "\u{2014}".to_string());
    let active_player_label = gs
        .as_ref()
        .and_then(active_player_name)
        .unwrap_or_else(|| "Not attached yet".to_string());

    rsx! {
        section { class: "hero", "data-testid": "hero-panel",
            h1 { class: "hero__title", "Dragon Shift" }
            p { class: "hero__body", {screen_title(&id.screen)} }
            div { class: "hero__meta",
                span { class: connection_badge_class, "data-testid": "connection-badge", "Connection: " {connection_status_label(&id.connection_status)} }
                if has_session_snapshot {
                    span { class: "badge", "data-testid": "workshop-code-badge", "Workshop: " {session_code_label} }
                    span { class: "badge", "Player: " {active_player_label} }
                }
            }
        }
    }
}
