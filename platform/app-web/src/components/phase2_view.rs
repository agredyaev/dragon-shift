use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle, SessionCommand};

use crate::flows::start_workshop_command;
use crate::helpers::*;
use crate::state::{ConnectionStatus, IdentityState, OperationState};

const PROGRESS_STEPS: i32 = 20;

#[component]
pub fn Phase2View(
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

    let commands_disabled = {
        let id = identity.read();
        let o = ops.read();
        o.pending_flow.is_some()
            || o.pending_command.is_some()
            || o.pending_judge_bundle
            || id.session_snapshot.is_none()
            || id.connection_status != ConnectionStatus::Connected
    };

    let dragon_name = current_dragon(state)
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "Unknown".to_string());
    let creator = phase2_creator_label(state);
    let emotion = current_dragon(state)
        .map(|d| d.last_emotion)
        .unwrap_or(protocol::DragonEmotion::Neutral);
    let emotion_label = dragon_emotion_label(emotion);
    let anim_class = dragon_emotion_anim_class(emotion);

    let sprite_url = current_dragon(state).and_then(|d| {
        d.custom_sprites
            .as_ref()
            .map(|sp| sprite_url_for_emotion(sp, d.last_emotion))
    });

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

    let handover_tags: Vec<String> = current_dragon(state)
        .map(|d| d.handover_tags.clone())
        .unwrap_or_default();
    let session_code = state.session.code.clone();

    let achievements = current_player(state)
        .map(|p| p.achievements.clone())
        .unwrap_or_default();
    let achievements = unique_achievement_ids(&achievements);

    let is_host = current_player(state).map(|p| p.is_host).unwrap_or(false);
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

    // Phase countdown (§10 step 9).
    let phase_countdown = phase_remaining_seconds(state, now_epoch_seconds()).map(format_mm_ss);

    // Drop read guard before rsx closures
    drop(gs);

    rsx! {
        div { class: "sr-only", "data-testid": "workshop-code-badge", {session_code} }
        div {
            class: format!("sr-only {}", connection_class),
            "data-testid": "connection-badge",
            {connection_label}
        }
        div { class: "sr-only", "data-testid": "controls-panel", if is_host { "visible" } else { "hidden" } }

        // ---- Phase 2: 3-column grid layout (same as Phase 1) ----
        div { class: "phase1-grid", "data-testid": "session-panel",
            // ==== LEFT COLUMN: Dragon + Stats + Handover Notes (2/3 width) ====
            div { class: "phase1-dragon-col",
                // Dragon info panel
                div { class: "panel panel--session",
                    // Header row with creator + 2X DECAY badge
                    div { class: "pixel-header",
                        div {
                            h2 { class: "pixel-header__title", {dragon_name} }
                            p { class: "phase2-creator-label", {creator} }
                        }
                        div { style: "display:flex;align-items:center;gap:12px;",
                            p { class: "pixel-header__title", style: "font-size:12px;", "Phase 2: New Shift" }
                            if let Some(remaining) = phase_countdown.clone() {
                                span { class: "pixel-header__title", style: "font-size:12px;", "data-testid": "phase-countdown", {remaining} }
                            }
                            span { class: "decay-badge decay-badge--pulse",
                                img { class: "pixel-icon", src: "{poke_icon_url(\"clock\")}", alt: "clock", width: 16, height: 16 }
                                "2X Decay"
                            }
                        }
                    }

                    // Dragon sprite display
                    div { class: "dragon-stage",
                        if let Some(ref speech_text) = speech {
                            div { class: "speech-bubble",
                                p { class: "speech-bubble__text", {speech_text.clone()} }
                                div { class: "speech-bubble__tail" }
                            }
                        }
                        if let Some(ref url) = sprite_url {
                            div { class: "{anim_class}",
                                img {
                                    class: "dragon-stage__sprite",
                                    src: "{url}",
                                    alt: "Dragon feeling {emotion_label}",
                                }
                            }
                        } else {
                            p { class: "meta", "Mood: {emotion_label}" }
                        }
                    }

                    // Pixel progress bars
                    div { style: "padding: 0 22px 22px;",
                        // Happiness
                        div { class: "pixel-stat-row",
                            div { class: "pixel-stat-row__header",
                                span { class: "pixel-stat-row__label",
                                    img {
                                        class: "pixel-icon",
                                        src: "{poke_icon_url(\"heart\")}",
                                        alt: "heart",
                                        width: 24, height: 24,
                                    }
                                    "Happiness"
                                }
                                span { class: "pixel-stat-row__value", "{happiness}%" }
                            }
                            div { class: "pixel-progress-container",
                                {(0..PROGRESS_STEPS).map(|i| {
                                    let filled = i < (happiness as f64 / 100.0 * PROGRESS_STEPS as f64).round() as i32;
                                    let cls = if filled { "pixel-progress-bar pixel-progress-bar--pink" } else { "pixel-progress-bar pixel-progress-bar--empty" };
                                    rsx! { div { class: "{cls}", key: "{i}" } }
                                })}
                            }
                        }
                        // Hunger
                        div { class: "pixel-stat-row",
                            div { class: "pixel-stat-row__header",
                                span { class: "pixel-stat-row__label",
                                    img {
                                        class: "pixel-icon",
                                        src: "{poke_icon_url(\"meat\")}",
                                        alt: "meat",
                                        width: 24, height: 24,
                                    }
                                    "Hunger"
                                }
                                span { class: "pixel-stat-row__value", "{hunger}%" }
                            }
                            div { class: "pixel-progress-container",
                                {(0..PROGRESS_STEPS).map(|i| {
                                    let filled = i < (hunger as f64 / 100.0 * PROGRESS_STEPS as f64).round() as i32;
                                    let cls = if filled { "pixel-progress-bar pixel-progress-bar--orange" } else { "pixel-progress-bar pixel-progress-bar--empty" };
                                    rsx! { div { class: "{cls}", key: "{i}" } }
                                })}
                            }
                        }
                        // Energy
                        div { class: "pixel-stat-row",
                            div { class: "pixel-stat-row__header",
                                span { class: "pixel-stat-row__label",
                                    img {
                                        class: "pixel-icon",
                                        src: "{poke_icon_url(\"zap\")}",
                                        alt: "energy",
                                        width: 24, height: 24,
                                    }
                                    "Energy"
                                }
                                span { class: "pixel-stat-row__value", "{energy}%" }
                            }
                            div { class: "pixel-progress-container",
                                {(0..PROGRESS_STEPS).map(|i| {
                                    let filled = i < (energy as f64 / 100.0 * PROGRESS_STEPS as f64).round() as i32;
                                    let cls = if filled { "pixel-progress-bar pixel-progress-bar--blue" } else { "pixel-progress-bar pixel-progress-bar--empty" };
                                    rsx! { div { class: "{cls}", key: "{i}" } }
                                })}
                            }
                        }
                    }
                }

                // ---- Handover notes from previous owner ----
                if !handover_tags.is_empty() {
                    div { class: "panel",
                        div { class: "pixel-header-amber",
                            h3 { class: "pixel-header__title", style: "display:flex;align-items:center;gap:8px;",
                                img { class: "pixel-icon", src: "{poke_icon_url(\"alert\")}", alt: "notes", width: 24, height: 24 }
                                "Previous Owner's Notes"
                            }
                        }
                        div { class: "handover-notes",
                            for tag in handover_tags.iter() {
                                div { class: "handover-note", "* {tag}" }
                            }
                        }
                    }
                }
            }

            // ==== RIGHT COLUMN: Actions + Achievements + Host Controls (1/3 width) ====
            div { class: "phase1-actions-col",
                // ---- Actions panel ----
                div { class: "panel panel--controls",
                    div { class: "pixel-header",
                        h3 { class: "pixel-header__title", "Actions" }
                        if on_cooldown {
                            span { class: "cooldown-label cooldown-label--pulse",
                                "Wait {cooldown}s..."
                            }
                        }
                    }
                    div { style: "padding:16px;",
                        // Feed section
                        p { class: "action-section-label", "Feed" }
                        div { class: "action-grid",
                            button {
                                class: "action-btn action-btn--meat",
                                "data-testid": "action-feed-meat",
                                disabled: commands_disabled || on_cooldown,
                                onclick: move |_| {
                                    let _ = start_workshop_command(
                                        identity, ops, handover_tags_input, judge_bundle,
                                        SessionCommand::Action,
                                        Some(serde_json::json!({"type": "feed", "value": "meat"})),
                                    );
                                },
                                img { class: "pixel-icon", src: "{poke_icon_url(\"meat\")}", alt: "meat", width: 32, height: 32 }
                                "Meat"
                            }
                            button {
                                class: "action-btn action-btn--fruit",
                                "data-testid": "action-feed-fruit",
                                disabled: commands_disabled || on_cooldown,
                                onclick: move |_| {
                                    let _ = start_workshop_command(
                                        identity, ops, handover_tags_input, judge_bundle,
                                        SessionCommand::Action,
                                        Some(serde_json::json!({"type": "feed", "value": "fruit"})),
                                    );
                                },
                                img { class: "pixel-icon", src: "{poke_icon_url(\"fruit\")}", alt: "fruit", width: 32, height: 32 }
                                "Fruit"
                            }
                            button {
                                class: "action-btn action-btn--fish",
                                "data-testid": "action-feed-fish",
                                disabled: commands_disabled || on_cooldown,
                                onclick: move |_| {
                                    let _ = start_workshop_command(
                                        identity, ops, handover_tags_input, judge_bundle,
                                        SessionCommand::Action,
                                        Some(serde_json::json!({"type": "feed", "value": "fish"})),
                                    );
                                },
                                img { class: "pixel-icon", src: "{poke_icon_url(\"fish\")}", alt: "fish", width: 32, height: 32 }
                                "Fish"
                            }
                        }
                        // Play section
                        p { class: "action-section-label", style: "margin-top:16px;", "Play" }
                        div { class: "action-grid",
                            button {
                                class: "action-btn action-btn--fetch",
                                "data-testid": "action-play-fetch",
                                disabled: commands_disabled || on_cooldown,
                                onclick: move |_| {
                                    let _ = start_workshop_command(
                                        identity, ops, handover_tags_input, judge_bundle,
                                        SessionCommand::Action,
                                        Some(serde_json::json!({"type": "play", "value": "fetch"})),
                                    );
                                },
                                img { class: "pixel-icon", src: "{poke_icon_url(\"fetch\")}", alt: "fetch", width: 32, height: 32 }
                                "Fetch"
                            }
                            button {
                                class: "action-btn action-btn--puzzle",
                                "data-testid": "action-play-puzzle",
                                disabled: commands_disabled || on_cooldown,
                                onclick: move |_| {
                                    let _ = start_workshop_command(
                                        identity, ops, handover_tags_input, judge_bundle,
                                        SessionCommand::Action,
                                        Some(serde_json::json!({"type": "play", "value": "puzzle"})),
                                    );
                                },
                                img { class: "pixel-icon", src: "{poke_icon_url(\"puzzle\")}", alt: "puzzle", width: 32, height: 32 }
                                "Puzzle"
                            }
                            button {
                                class: "action-btn action-btn--music",
                                "data-testid": "action-play-music",
                                disabled: commands_disabled || on_cooldown,
                                onclick: move |_| {
                                    let _ = start_workshop_command(
                                        identity, ops, handover_tags_input, judge_bundle,
                                        SessionCommand::Action,
                                        Some(serde_json::json!({"type": "play", "value": "music"})),
                                    );
                                },
                                img { class: "pixel-icon", src: "{poke_icon_url(\"music\")}", alt: "music", width: 32, height: 32 }
                                "Music"
                            }
                        }
                        // Rest section
                        p { class: "action-section-label", style: "margin-top:16px;", "Rest" }
                        div { class: "action-grid",
                            button {
                                class: "action-btn action-btn--sleep",
                                "data-testid": "action-sleep",
                                disabled: commands_disabled || on_cooldown,
                                onclick: move |_| {
                                    let _ = start_workshop_command(
                                        identity, ops, handover_tags_input, judge_bundle,
                                        SessionCommand::Action,
                                        Some(serde_json::json!({"type": "sleep"})),
                                    );
                                },
                                img { class: "pixel-icon", src: "{poke_icon_url(\"sleep\")}", alt: "sleep", width: 32, height: 32 }
                                "Put to Sleep"
                            }
                        }
                    }
                }

                // ---- Achievements panel ----
                div { class: "panel",
                    div { class: "pixel-header",
                        h3 { class: "pixel-header__title", "Achievements" }
                    }
                    div { style: "padding:16px;",
                        if achievements.is_empty() {
                            p { class: "meta", style: "text-align:center;padding:12px;", "No achievements yet." }
                        } else {
                            div { class: "achievements",
                                for ach_id in achievements.iter() {
                                    if let Some((name, desc, icon)) = achievement_def(ach_id) {
                                        div { class: "achievement-card",
                                            img { class: "pixel-icon", src: "{poke_icon_url(icon)}", alt: "{icon}", width: 32, height: 32 }
                                            div {
                                                p { class: "achievement-card__name", "{name}" }
                                                p { class: "achievement-card__desc", "{desc}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // ---- Host controls ----
                if is_host {
                    div { class: "panel panel--controls",
                        div { class: "pixel-header",
                            h3 { class: "pixel-header__title", "Host Controls" }
                        }
                        div { style: "padding:16px;display:grid;gap:12px;",
                            button {
                                class: "button button--primary",
                                "data-testid": "end-game-button",
                                disabled: commands_disabled,
                                onclick: move |_| {
                                    let _ = start_workshop_command(
                                        identity, ops, handover_tags_input, judge_bundle,
                                        SessionCommand::EndGame,
                                        None,
                                    );
                                },
                                "Open scoring"
                            }
                            button {
                                class: "button button--secondary",
                                "data-testid": "reset-workshop-button",
                                disabled: commands_disabled,
                                onclick: move |_| {
                                    let _ = start_workshop_command(
                                        identity, ops, handover_tags_input, judge_bundle,
                                        SessionCommand::ResetGame,
                                        None,
                                    );
                                },
                                "Reset Workshop"
                            }
                        }
                    }
                }
            }
        }
    }
}
