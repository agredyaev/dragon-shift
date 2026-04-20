use dioxus::prelude::*;
use protocol::{ClientGameState, SessionCommand};

use crate::flows::{leave_workshop, submit_workshop_command};
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

    let session_code = state.session.code.clone();
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
        article { class: "panel", "data-testid": "lobby-panel",
            h2 { class: "panel__title", "Workshop Lobby" }
            p { class: "panel__body",
                "Code: "
                strong { {session_code} }
            }
            p { class: "panel__body", {status_label} }

            div { class: "panel__stack",
                div { style: "background:#0f172a;padding:16px;border:4px solid #0f172a;text-align:center;box-shadow:inset 4px 4px 0 rgba(0,0,0,0.5);",
                    p { style: "font-family:var(--font-display);font-size:20px;font-weight:900;letter-spacing:0.12em;color:#34d399;",
                        {ready_label}
                    }
                }

                div { class: "roster",
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

                if is_host {
                    div { class: "button-row",
                        button {
                            class: "button button--primary",
                            style: "width:100%;",
                            "data-testid": "start-phase1-button",
                            disabled: commands_disabled,
                            onclick: move |_| {
                                spawn(submit_workshop_command(
                                    identity, ops, handover_tags_input, judge_bundle,
                                    SessionCommand::StartPhase1, None,
                                ));
                            },
                            "Start Phase 1"
                        }
                    }
                } else {
                    p { class: "meta", style: "text-align:center;", "Waiting for the host to start Phase 1." }
                }

                div { class: "button-row",
                    button {
                        class: "button button--secondary",
                        "data-testid": "leave-workshop-button",
                        disabled: commands_disabled,
                        onclick: move |_| {
                            leave_workshop(identity, ops);
                        },
                        "Leave workshop"
                    }
                }
            }
        }
    }
}
