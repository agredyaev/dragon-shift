use dioxus::prelude::*;
use protocol::ClientGameState;

use crate::helpers::*;

#[component]
pub fn LobbyView(game_state: Signal<Option<ClientGameState>>) -> Element {
    let gs = game_state.read();
    let Some(state) = gs.as_ref() else {
        return rsx! {};
    };

    let rows = lobby_player_rows(state);
    let ready_label = lobby_ready_summary(state);
    let status_label = lobby_status_copy(state);

    rsx! {
        p { class: "meta", "Lobby readiness: " {ready_label} }
        p { class: "meta", {status_label} }
        div { class: "roster",
            for row in rows {
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
}
