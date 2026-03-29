use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle, Phase, SessionCommand};

use crate::flows::{
    submit_handover_tags_command, submit_judge_bundle_request, submit_workshop_command,
};
use crate::helpers::current_player;
use crate::state::{IdentityState, OperationState};

#[component]
pub fn ControlsPanel(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    handover_tags_input: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) -> Element {
    let id = identity.read();
    let gs = game_state.read();
    let o = ops.read();

    let commands_disabled =
        o.pending_flow.is_some() || o.pending_command.is_some() || id.session_snapshot.is_none();
    let judge_bundle_disabled = commands_disabled || o.pending_judge_bundle;
    let pending_judge_bundle = o.pending_judge_bundle;

    let end_is_host = gs
        .as_ref()
        .filter(|s| s.phase == Phase::End)
        .and_then(current_player)
        .map(|p| p.is_host)
        .unwrap_or(false);

    let handover_tags_value = handover_tags_input.read().clone();

    drop(id);
    drop(gs);
    drop(o);

    let mut handover_tags_input_w = handover_tags_input;

    rsx! {
        article { class: "panel panel--controls",
            h2 { class: "panel__title", "Session controls" }
            p { class: "panel__body", "Use these controls to move the workshop from setup through discovery, handover, care, voting, and the final reset." }
            div { class: "panel__stack",
                div { class: "button-row",
                    button {
                        class: "button button--primary",
                        disabled: commands_disabled,
                        onclick: move |_| {
                            spawn(submit_workshop_command(identity, ops, handover_tags_input, judge_bundle, SessionCommand::StartPhase1, None));
                        },
                        "Start Phase 1"
                    }
                    button {
                        class: "button button--secondary",
                        disabled: commands_disabled,
                        onclick: move |_| {
                            spawn(submit_workshop_command(identity, ops, handover_tags_input, judge_bundle, SessionCommand::StartHandover, None));
                        },
                        "Start handover"
                    }
                }
                input {
                    class: "input",
                    value: handover_tags_value,
                    placeholder: "Handover tags, comma separated",
                    oninput: move |event| handover_tags_input_w.set(event.value())
                }
                div { class: "button-row",
                    button {
                        class: "button button--secondary",
                        disabled: commands_disabled,
                        onclick: move |_| {
                            spawn(submit_handover_tags_command(identity, ops, handover_tags_input, judge_bundle));
                        },
                        "Save handover tags"
                    }
                    button {
                        class: "button button--secondary",
                        disabled: commands_disabled,
                        onclick: move |_| {
                            spawn(submit_workshop_command(identity, ops, handover_tags_input, judge_bundle, SessionCommand::StartPhase2, None));
                        },
                        "Start Phase 2"
                    }
                }
                div { class: "button-row",
                    button {
                        class: "button button--secondary",
                        disabled: commands_disabled,
                        onclick: move |_| {
                            spawn(submit_workshop_command(identity, ops, handover_tags_input, judge_bundle, SessionCommand::EndGame, None));
                        },
                        "End game"
                    }
                    button {
                        class: "button button--secondary",
                        disabled: commands_disabled,
                        onclick: move |_| {
                            spawn(submit_workshop_command(identity, ops, handover_tags_input, judge_bundle, SessionCommand::RevealVotingResults, None));
                        },
                        "Reveal results"
                    }
                    button {
                        class: "button button--secondary",
                        disabled: commands_disabled,
                        onclick: move |_| {
                            spawn(submit_workshop_command(identity, ops, handover_tags_input, judge_bundle, SessionCommand::ResetGame, None));
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
                                spawn(submit_judge_bundle_request(identity, game_state, ops, judge_bundle));
                            },
                            if pending_judge_bundle {
                                "Building archive\u{2026}"
                            } else {
                                "Build archive"
                            }
                        }
                    }
                }
            }
        }
    }
}
