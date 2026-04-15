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

    let all_ready = {
        let total = state.players.len();
        let ready = state.players.values().filter(|p| p.is_ready).count();
        total > 0 && ready == total
    };

    drop(gs);

    rsx! {
        div { class: "panel handover-card",
            // Game title
            h1 { class: "handover-card__title", "Dragon Shift" }
            p { class: "handover-card__subtitle", "A collaborative pet care game" }

            // Status
            p { class: "panel__body", style: "margin-bottom:20px;", {status_label} }

            // Ready counter
            div { style: "background:#0f172a;padding:16px;border:4px solid #0f172a;margin-bottom:20px;text-align:center;box-shadow:inset 4px 4px 0 rgba(0,0,0,0.5);",
                p { style: "font-family:var(--font-display);font-size:24px;font-weight:900;letter-spacing:0.12em;color:#34d399;",
                    {ready_label}
                }
            }

            // Player roster
            div { class: "roster", style: "margin-bottom:20px;",
                for row in rows {
                    article { class: "roster__item",
                        div {
                            p { class: "roster__name", {row.name} }
                            p { class: "roster__meta", {row.role_label} " \u{2014} " {row.readiness_label} }
                        }
                        span {
                            class: format!("roster__status {}", if row.connectivity_label == "Online" { "status-connected" } else { "status-offline" }),
                            {row.connectivity_label}
                        }
                    }
                }
            }

            // Host controls
            if is_host {
                div { style: "padding-top:20px;border-top:4px solid #0f172a;",
                    button {
                        class: "button button--primary",
                        style: "width:100%;",
                        "data-testid": "start-phase0-button",
                        disabled: commands_disabled || !all_ready,
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
                        "Open Character Creation"
                    }
                }
            } else {
                p { class: "meta", style: "text-align:center;", "Only the host can move the group into character creation." }
            }
        }
    }
}
