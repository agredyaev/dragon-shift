use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle};

use crate::flows::{submit_handover_tags_command, submit_workshop_command};
use crate::helpers::*;
use crate::state::{IdentityState, OperationState};

#[component]
pub fn HandoverView(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    handover_tags_input: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) -> Element {
    let gs = game_state.read();
    let Some(state) = gs.as_ref() else {
        return rsx! {};
    };

    let title = handover_focus_title(state);
    let summary = handover_saved_summary(state);
    let status = handover_status_copy(state);
    let saved_tags = handover_saved_tags(state);
    let draft_count = {
        let input = handover_tags_input.read();
        parse_tags_input(&input).len()
    };
    let is_host = current_player(state)
        .map(|player| player.is_host)
        .unwrap_or(false);
    let commands_disabled = {
        let id = identity.read();
        let o = ops.read();
        o.pending_flow.is_some() || o.pending_command.is_some() || id.session_snapshot.is_none()
    };
    let handover_tags_value = handover_tags_input.read().clone();

    drop(gs);

    let mut handover_tags_input_w = handover_tags_input;

    rsx! {
        article { class: "roster__item roster__item--phase",
            div {
                p { class: "roster__name", {title} }
                p { class: "roster__meta", {summary} }
            }
            span { class: "roster__status roster__status--phase status-connecting", "Handover" }
        }
        p { class: "panel__body", {status} }
        p { class: "meta", "Draft rules parsed from input: " {draft_count.to_string()} }
        div { class: "panel__stack",
            input {
                class: "input",
                "data-testid": "handover-tags-input",
                value: handover_tags_value,
                placeholder: "Three handover tags, comma separated",
                oninput: move |event| handover_tags_input_w.set(event.value())
            }
            div { class: "button-row",
                button {
                    class: "button button--secondary",
                    "data-testid": "save-handover-tags-button",
                    disabled: commands_disabled,
                    onclick: move |_| {
                        spawn(submit_handover_tags_command(identity, ops, handover_tags_input, judge_bundle));
                    },
                    "Save handover tags"
                }
                if is_host {
                    button {
                        class: "button button--primary",
                        "data-testid": "start-phase2-button",
                        disabled: commands_disabled,
                        onclick: move |_| {
                            spawn(submit_workshop_command(
                                identity,
                                ops,
                                handover_tags_input,
                                judge_bundle,
                                protocol::SessionCommand::StartPhase2,
                                None,
                            ));
                        },
                        "Start care round"
                    }
                }
            }
        }
        if !saved_tags.is_empty() {
            div { class: "roster",
                for tag in saved_tags {
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
}
