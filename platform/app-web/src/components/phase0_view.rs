use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle, SessionCommand, SpriteSet};

use crate::flows::{
    submit_sprite_sheet_request, submit_update_player_pet, submit_workshop_command,
};
use crate::helpers::{current_player, fallback_pet_description};
use crate::state::{IdentityState, OperationState};

const EMOTION_LABELS: [&str; 4] = ["Neutral", "Happy", "Angry", "Sleepy"];

fn sprite_for_index(sprites: &SpriteSet, index: usize) -> &str {
    match index {
        0 => &sprites.neutral,
        1 => &sprites.happy,
        2 => &sprites.angry,
        _ => &sprites.sleepy,
    }
}

#[component]
pub fn Phase0View(
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

    let default_description = current_player(state)
        .map(|player| fallback_pet_description(&player.name))
        .unwrap_or_else(|| fallback_pet_description("this player"));

    let mut dragon_description = use_signal(|| {
        current_player(state)
            .and_then(|player| player.pet_description.clone())
            .unwrap_or_else(|| default_description.clone())
    });
    let generated_sprites: Signal<Option<SpriteSet>> =
        use_signal(|| current_player(state).and_then(|player| player.custom_sprites.clone()));
    let mut generating = use_signal(|| false);
    let mut saving = use_signal(|| false);

    let is_host = current_player(state).map(|p| p.is_host).unwrap_or(false);
    let commands_disabled = {
        let o = ops.read();
        o.pending_flow.is_some() || o.pending_command.is_some()
    };
    let has_sprites = generated_sprites.read().is_some();

    drop(gs);

    rsx! {
        article { class: "panel phase0-card",
            // ---- Title ----
            h1 { class: "phase0-card__title", "Dragon Shift" }
            p { class: "phase0-card__subtitle", "A collaborative pet game" }

            // ---- Design section ----
            h2 { class: "panel__title phase0-card__section-title", "Design your pet" }
            p { class: "panel__body phase0-card__section-desc",
                "Describe your pet, and our AI will draw it for you!"
            }

            textarea {
                class: "phase0-textarea",
                placeholder: "> e.g. A tiny green dragon with a fiery tail...",
                rows: 4,
                "data-testid": "dragon-description-input",
                value: "{dragon_description}",
                disabled: *generating.read(),
                oninput: move |evt| dragon_description.set(evt.value().clone()),
            }

            if !has_sprites {
                button {
                    class: "button phase0-action-button",
                    "data-testid": "generate-sprites-button",
                    disabled: commands_disabled || *generating.read(),
                    onclick: {
                        let desc = dragon_description.read().clone();
                        move |_| {
                            let desc = desc.clone();
                            generating.set(true);
                            spawn(async move {
                                submit_sprite_sheet_request(identity, ops, generated_sprites, desc).await;
                                generating.set(false);
                            });
                        }
                    },
                    if *generating.read() { "Drawing..." } else { "Generate pet" }
                }
            }

            // ---- Sprite review section ----
            if has_sprites {
                h2 { class: "panel__title phase0-card__section-title", "Review your pet" }
                div { class: "sprite-grid",
                    {
                        let sprites = generated_sprites.read();
                        if let Some(ref sp) = *sprites {
                            rsx! {
                                for (i, label) in EMOTION_LABELS.iter().enumerate() {
                                    div { class: "sprite-grid__cell",
                                        div { class: "sprite-grid__image-wrap phase0-sprite-frame",
                                            img {
                                                class: "sprite-grid__image",
                                                src: "data:image/png;base64,{sprite_for_index(sp, i)}",
                                                alt: "Dragon emotion: {label}",
                                            }
                                        }
                                        p { class: "sprite-grid__label", "{label}" }
                                    }
                                }
                            }
                        } else {
                            rsx! {}
                        }
                    }
                }
                button {
                    class: "button phase0-action-button",
                    "data-testid": "save-dragon-button",
                    disabled: commands_disabled || *saving.read(),
                    onclick: {
                        let desc = dragon_description.read().clone();
                        move |_| {
                            let desc = desc.clone();
                            let sprites = generated_sprites.read().clone();
                            saving.set(true);
                            spawn(async move {
                                submit_update_player_pet(
                                    identity,
                                    ops,
                                    handover_tags_input,
                                    judge_bundle,
                                    desc,
                                    sprites,
                                )
                                .await;
                                saving.set(false);
                            });
                        }
                    },
                    if *saving.read() { "Saving..." } else { "Looks good!" }
                }
                button {
                    class: "button button--secondary phase0-action-button",
                    disabled: commands_disabled || *generating.read(),
                    onclick: {
                        let desc = dragon_description.read().clone();
                        move |_| {
                            let desc = desc.clone();
                            generating.set(true);
                            spawn(async move {
                                submit_sprite_sheet_request(identity, ops, generated_sprites, desc).await;
                                generating.set(false);
                            });
                        }
                    },
                    if *generating.read() { "Drawing..." } else { "Regenerate" }
                }
            }

            // ---- Host controls / non-host guidance ----
            if is_host {
                div { class: "phase0-host-controls",
                    button {
                        class: "button button--primary phase0-action-button",
                        "data-testid": "start-phase1-button",
                        disabled: commands_disabled,
                        onclick: move |_| {
                            spawn(submit_workshop_command(
                                identity,
                                ops,
                                handover_tags_input,
                                judge_bundle,
                                SessionCommand::StartPhase1,
                                None,
                            ));
                        },
                        "Start phase 1"
                    }
                }
            } else {
                p { class: "meta phase0-card__meta",
                    "Save your profile, then wait for the host to start discovery."
                }
            }
        }
    }
}
