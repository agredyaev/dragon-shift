use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle};

use crate::flows::start_workshop_command;
use crate::helpers::*;
use crate::state::{ConnectionStatus, IdentityState, OperationState};

#[component]
pub fn HandoverView(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    handover_tags_input: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) -> Element {
    let rule1 = use_signal(String::new);
    let rule2 = use_signal(String::new);
    let rule3 = use_signal(String::new);

    let gs = game_state.read();
    let Some(state) = gs.as_ref() else {
        return rsx! {};
    };

    let dragon_name = current_dragon(state)
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "your dragon".to_string());

    let saved_tags = handover_saved_tags(state);
    let session_code = state.session.code.clone();

    let is_host = current_player(state)
        .map(|player| player.is_host)
        .unwrap_or(false);
    let connection_status = identity.read().connection_status;
    let connection_label = match connection_status {
        ConnectionStatus::Offline => "Offline",
        ConnectionStatus::Connecting => "Connecting",
        ConnectionStatus::Connected => "Connected",
    };
    let connection_class = match connection_status {
        ConnectionStatus::Offline => "status-offline",
        ConnectionStatus::Connecting => "status-connecting",
        ConnectionStatus::Connected => "status-connected",
    };

    let commands_disabled = {
        let id = identity.read();
        let o = ops.read();
        o.pending_flow.is_some()
            || o.pending_command.is_some()
            || o.pending_judge_bundle
            || id.session_snapshot.is_none()
            || id.connection_status != ConnectionStatus::Connected
    };

    let rule1_val = rule1.read().clone();
    let rule2_val = rule2.read().clone();
    let rule3_val = rule3.read().clone();
    let valid_rule_count = [rule1_val.trim(), rule2_val.trim(), rule3_val.trim()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .count();
    let parsed_rule_count = parse_tags_input(&format!(
        "{}, {}, {}",
        rule1_val.trim(),
        rule2_val.trim(),
        rule3_val.trim()
    ))
    .len();
    let save_disabled = commands_disabled || valid_rule_count != 3 || parsed_rule_count != 3;

    drop(gs);

    let mut rule1_w = rule1;
    let mut rule2_w = rule2;
    let mut rule3_w = rule3;
    let mut handover_tags_input_w = handover_tags_input;

    rsx! {
        div { class: "sr-only", "data-testid": "workshop-code-badge", {session_code} }
        div {
            class: format!("sr-only {}", connection_class),
            "data-testid": "connection-badge",
            {connection_label}
        }
        div { class: "sr-only", "data-testid": "controls-panel", if is_host { "visible" } else { "hidden" } }

        div { class: "panel handover-card", "data-testid": "session-panel",
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

            // Save Notes button
            div { class: "button-row", style: "margin-top:20px;",
                button {
                    class: "button button--secondary",
                    "data-testid": "save-handover-tags-button",
                    disabled: save_disabled,
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
                        if start_workshop_command(
                            identity,
                            ops,
                            handover_tags_input,
                            judge_bundle,
                            protocol::SessionCommand::SubmitTags,
                            Some(serde_json::json!(parse_tags_input(&combined))),
                        ) {
                            handover_tags_input_w.set(combined);
                        }
                    },
                    "Save Notes"
                }
            }

            // Host-only Start Phase 2 button
            if is_host {
                div { style: "margin-top:20px;padding-top:20px;border-top:4px solid #0f172a;",
                    button {
                        class: "button button--primary",
                        "data-testid": "start-phase2-button",
                        disabled: commands_disabled,
                        onclick: move |_| {
                            let _ = start_workshop_command(
                                identity,
                                ops,
                                handover_tags_input,
                                judge_bundle,
                                protocol::SessionCommand::StartPhase2,
                                None,
                            );
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
