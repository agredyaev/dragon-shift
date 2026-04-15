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
    let rule1 = use_signal(|| String::new());
    let rule2 = use_signal(|| String::new());
    let rule3 = use_signal(|| String::new());
    let sprite_recommendation = use_signal(|| String::new());

    let gs = game_state.read();
    let Some(state) = gs.as_ref() else {
        return rsx! {};
    };

    let dragon_name = current_dragon(state)
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "your dragon".to_string());

    let saved_tags = handover_saved_tags(state);

    let is_host = current_player(state)
        .map(|player| player.is_host)
        .unwrap_or(false);

    let commands_disabled = {
        let id = identity.read();
        let o = ops.read();
        o.pending_flow.is_some() || o.pending_command.is_some() || id.session_snapshot.is_none()
    };

    let rule1_val = rule1.read().clone();
    let rule2_val = rule2.read().clone();
    let rule3_val = rule3.read().clone();
    let sprite_rec_val = sprite_recommendation.read().clone();

    drop(gs);

    let mut rule1_w = rule1;
    let mut rule2_w = rule2;
    let mut rule3_w = rule3;
    let mut sprite_rec_w = sprite_recommendation;
    let mut handover_tags_input_w = handover_tags_input;

    rsx! {
        div { class: "panel handover-card",
            h1 { class: "handover-card__title", "Shift Change!" }
            p { class: "handover-card__subtitle",
                "Your shift is over. Pass "
                strong { {dragon_name.clone()} }
                " to the next caretaker."
            }
            p { class: "panel__body", style: "margin-bottom:20px;",
                "Provide exactly 3 key rules about their care!"
            }

            // 3 rule inputs
            div { class: "handover-input-group",
                // Rule 1
                div { style: "display:flex;align-items:center;gap:12px;",
                    div { class: "handover-rule-number", "1" }
                    input {
                        class: "input",
                        "data-testid": "handover-rule-1",
                        value: rule1_val,
                        placeholder: "> Rule 1",
                        oninput: move |event| rule1_w.set(event.value()),
                    }
                }
                // Rule 2
                div { style: "display:flex;align-items:center;gap:12px;",
                    div { class: "handover-rule-number", "2" }
                    input {
                        class: "input",
                        "data-testid": "handover-rule-2",
                        value: rule2_val,
                        placeholder: "> Rule 2",
                        oninput: move |event| rule2_w.set(event.value()),
                    }
                }
                // Rule 3
                div { style: "display:flex;align-items:center;gap:12px;",
                    div { class: "handover-rule-number", "3" }
                    input {
                        class: "input",
                        "data-testid": "handover-rule-3",
                        value: rule3_val,
                        placeholder: "> Rule 3",
                        oninput: move |event| rule3_w.set(event.value()),
                    }
                }
            }

            // Dragon sprite recommendation
            div { class: "handover-input-group", style: "margin-top:16px;",
                p { class: "handover-input-label", "Dragon Sprite Notes" }
                input {
                    class: "input",
                    "data-testid": "handover-sprite-recommendation",
                    value: sprite_rec_val,
                    placeholder: "Describe how the dragon should look for the next caretaker\u{2026}",
                    oninput: move |event| sprite_rec_w.set(event.value()),
                }
            }

            // Save Notes button
            div { class: "button-row", style: "margin-top:20px;",
                button {
                    class: "button button--secondary",
                    style: "width:100%;",
                    "data-testid": "save-handover-tags-button",
                    disabled: commands_disabled,
                    onclick: move |_| {
                        // Combine rules into comma-separated string for existing flow
                        let r1 = rule1.read().trim().to_string();
                        let r2 = rule2.read().trim().to_string();
                        let r3 = rule3.read().trim().to_string();
                        let combined = [r1, r2, r3]
                            .into_iter()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(", ");
                        handover_tags_input_w.set(combined);
                        spawn(submit_handover_tags_command(identity, ops, handover_tags_input, judge_bundle));
                    },
                    "Save Notes"
                }
            }

            // Host-only Start Phase 2 button
            if is_host {
                div { style: "margin-top:20px;padding-top:20px;border-top:4px solid #0f172a;",
                    button {
                        class: "button button--primary",
                        style: "width:100%;",
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
                        "Start Phase 2"
                    }
                }
            }

            // Show saved tags
            if !saved_tags.is_empty() {
                div { class: "handover-input-group", style: "margin-top:20px;",
                    p { class: "handover-input-label", "Saved Rules" }
                    for (i, tag) in saved_tags.iter().enumerate() {
                        article { class: "roster__item",
                            div {
                                p { class: "roster__name", {tag.clone()} }
                                p { class: "roster__meta", "Rule #" {(i + 1).to_string()} }
                            }
                            span { class: "roster__status status-connected", "Saved" }
                        }
                    }
                }
            }
        }
    }
}
