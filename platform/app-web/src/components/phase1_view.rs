use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle, SessionCommand};

use crate::flows::submit_workshop_command;
use crate::helpers::*;
use crate::state::{IdentityState, OperationState};

#[component]
pub fn Phase1View(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    handover_tags_input: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) -> Element {
    let observation_input = use_signal(|| String::new());

    let gs = game_state.read();
    let Some(state) = gs.as_ref() else {
        return rsx! {};
    };

    let commands_disabled = {
        let id = identity.read();
        let o = ops.read();
        o.pending_flow.is_some() || o.pending_command.is_some() || id.session_snapshot.is_none()
    };

    let title = phase1_focus_title(state);
    let body = phase1_focus_body(state);
    let observations = phase1_observation_summary(state);
    let emotion = current_dragon(state)
        .map(|d| dragon_emotion_label(d.last_emotion))
        .unwrap_or("");
    let last_action = current_dragon(state)
        .map(|d| dragon_action_label(d.last_action))
        .unwrap_or("");

    let (hunger, energy, happiness) = current_dragon(state)
        .map(|d| (d.stats.hunger, d.stats.energy, d.stats.happiness))
        .unwrap_or((0, 0, 0));

    let cooldown = current_dragon(state)
        .map(|d| d.action_cooldown)
        .unwrap_or(0);
    let on_cooldown = cooldown > 0;

    let speech = current_dragon(state)
        .and_then(|d| d.speech.clone())
        .filter(|s| !s.trim().is_empty());

    let saved_observations: Vec<String> = current_dragon(state)
        .map(|d| d.discovery_observations.clone())
        .unwrap_or_default();

    let observation_draft = observation_input.read().clone();

    // Drop read guards before rsx closures that capture mutable signals
    drop(gs);

    let mut observation_input_w = observation_input;

    rsx! {
        // Dragon identity
        article { class: "roster__item roster__item--phase",
            div {
                p { class: "roster__name", {title} }
                p { class: "roster__meta", {observations} }
            }
            span { class: "roster__status roster__status--phase status-connecting", "Discovery" }
        }
        p { class: "panel__body", {body} }

        // Dragon speech bubble
        if let Some(speech_text) = speech {
            p { class: "meta", "\u{1f4ac} " {speech_text} }
        }

        // Stats bars
        div { class: "panel__stack",
            p { class: "meta", "Mood: " {emotion} " | Last action: " {last_action} }
            div { class: "stat-bars",
                div { class: "stat-bar",
                    span { class: "stat-bar__label", "Hunger" }
                    div { class: "stat-bar__track",
                        div {
                            class: "stat-bar__fill",
                            style: format!("width:{}%", hunger.clamp(0, 100)),
                        }
                    }
                    span { class: "stat-bar__value", {hunger.to_string()} }
                }
                div { class: "stat-bar",
                    span { class: "stat-bar__label", "Energy" }
                    div { class: "stat-bar__track",
                        div {
                            class: "stat-bar__fill",
                            style: format!("width:{}%", energy.clamp(0, 100)),
                        }
                    }
                    span { class: "stat-bar__value", {energy.to_string()} }
                }
                div { class: "stat-bar",
                    span { class: "stat-bar__label", "Happy" }
                    div { class: "stat-bar__track",
                        div {
                            class: "stat-bar__fill",
                            style: format!("width:{}%", happiness.clamp(0, 100)),
                        }
                    }
                    span { class: "stat-bar__value", {happiness.to_string()} }
                }
            }
            if on_cooldown {
                p { class: "meta", "Action cooldown: " {cooldown.to_string()} "s" }
            }
        }

        // Action buttons
        div { class: "panel__stack",
            p { class: "meta", "Actions" }
            div { class: "button-row",
                button {
                    class: "button button--secondary",
                    "data-testid": "action-feed-meat",
                    disabled: commands_disabled || on_cooldown,
                    onclick: move |_| {
                        spawn(submit_workshop_command(
                            identity, ops, handover_tags_input, judge_bundle,
                            SessionCommand::Action,
                            Some(serde_json::json!({"type": "feed", "value": "meat"})),
                        ));
                    },
                    "Feed meat"
                }
                button {
                    class: "button button--secondary",
                    "data-testid": "action-feed-fruit",
                    disabled: commands_disabled || on_cooldown,
                    onclick: move |_| {
                        spawn(submit_workshop_command(
                            identity, ops, handover_tags_input, judge_bundle,
                            SessionCommand::Action,
                            Some(serde_json::json!({"type": "feed", "value": "fruit"})),
                        ));
                    },
                    "Feed fruit"
                }
                button {
                    class: "button button--secondary",
                    "data-testid": "action-feed-fish",
                    disabled: commands_disabled || on_cooldown,
                    onclick: move |_| {
                        spawn(submit_workshop_command(
                            identity, ops, handover_tags_input, judge_bundle,
                            SessionCommand::Action,
                            Some(serde_json::json!({"type": "feed", "value": "fish"})),
                        ));
                    },
                    "Feed fish"
                }
            }
            div { class: "button-row",
                button {
                    class: "button button--secondary",
                    "data-testid": "action-play-fetch",
                    disabled: commands_disabled || on_cooldown,
                    onclick: move |_| {
                        spawn(submit_workshop_command(
                            identity, ops, handover_tags_input, judge_bundle,
                            SessionCommand::Action,
                            Some(serde_json::json!({"type": "play", "value": "fetch"})),
                        ));
                    },
                    "Play fetch"
                }
                button {
                    class: "button button--secondary",
                    "data-testid": "action-play-puzzle",
                    disabled: commands_disabled || on_cooldown,
                    onclick: move |_| {
                        spawn(submit_workshop_command(
                            identity, ops, handover_tags_input, judge_bundle,
                            SessionCommand::Action,
                            Some(serde_json::json!({"type": "play", "value": "puzzle"})),
                        ));
                    },
                    "Play puzzle"
                }
                button {
                    class: "button button--secondary",
                    "data-testid": "action-play-music",
                    disabled: commands_disabled || on_cooldown,
                    onclick: move |_| {
                        spawn(submit_workshop_command(
                            identity, ops, handover_tags_input, judge_bundle,
                            SessionCommand::Action,
                            Some(serde_json::json!({"type": "play", "value": "music"})),
                        ));
                    },
                    "Play music"
                }
            }
            div { class: "button-row",
                button {
                    class: "button button--secondary",
                    "data-testid": "action-sleep",
                    disabled: commands_disabled || on_cooldown,
                    onclick: move |_| {
                        spawn(submit_workshop_command(
                            identity, ops, handover_tags_input, judge_bundle,
                            SessionCommand::Action,
                            Some(serde_json::json!({"type": "sleep"})),
                        ));
                    },
                    "Sleep"
                }
            }
        }

        // Observation input
        div { class: "panel__stack",
            p { class: "meta", "Discovery observations" }
            input {
                class: "input",
                "data-testid": "observation-input",
                value: observation_draft,
                placeholder: "Write what you noticed about this dragon\u{2026}",
                oninput: move |event| observation_input_w.set(event.value()),
            }
            div { class: "button-row",
                button {
                    class: "button button--secondary",
                    "data-testid": "submit-observation-button",
                    disabled: commands_disabled || observation_input.read().trim().is_empty(),
                    onclick: move |_| {
                        let text = observation_input.read().trim().to_string();
                        if !text.is_empty() {
                            observation_input_w.set(String::new());
                            spawn(submit_workshop_command(
                                identity, ops, handover_tags_input, judge_bundle,
                                SessionCommand::SubmitObservation,
                                Some(serde_json::json!({"text": text})),
                            ));
                        }
                    },
                    "Save observation"
                }
            }
            if !saved_observations.is_empty() {
                div { class: "roster",
                    for (i, obs) in saved_observations.iter().enumerate() {
                        article { class: "roster__item",
                            div {
                                p { class: "roster__name", {obs.clone()} }
                                p { class: "roster__meta", "Observation #" {(i + 1).to_string()} }
                            }
                            span { class: "roster__status status-connected", "Saved" }
                        }
                    }
                }
            }
        }
    }
}
