use dioxus::prelude::*;
use protocol::{ClientGameState, SessionCommand};

use crate::flows::submit_workshop_command;
use crate::helpers::{current_player, lobby_player_rows, lobby_ready_summary, lobby_status_copy};
use crate::state::{IdentityState, OperationState};

#[component]
pub fn LobbyView(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    handover_tags_input: Signal<String>,
    judge_bundle: Signal<Option<protocol::JudgeBundle>>,
) -> Element {
    let gs = game_state.read();
    let Some(state) = gs.as_ref() else {
        return rsx! {};
    };

    let rows = lobby_player_rows(state);
    let ready_label = lobby_ready_summary(state);
    let status_label = lobby_status_copy(state);
    let is_host = current_player(state).map(|p| p.is_host).unwrap_or(false);
    let commands_disabled = {
        let o = ops.read();
        o.pending_flow.is_some() || o.pending_command.is_some()
    };

    drop(gs);

    rsx! {
        article { class: "roster__item roster__item--phase",
            div {
                p { class: "roster__name", "Workshop waiting room" }
                p { class: "roster__meta", {ready_label} }
            }
            span { class: "roster__status roster__status--phase status-connecting", "Lobby" }
        }
        p { class: "panel__body", {status_label} }
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
        if is_host {
            div { class: "button-row",
                button {
                    class: "button button--primary",
                    "data-testid": "start-phase0-button",
                    disabled: commands_disabled,
                    onclick: move |_| {
                        spawn(submit_workshop_command(
                            identity,
                            ops,
                            handover_tags_input,
                            judge_bundle,
                            SessionCommand::StartPhase0,
                            None,
                        ));
                    },
                    "Open character creation"
                }
            }
        } else {
            p { class: "meta", "Only the host can move the group into character creation." }
        }
    }
}
