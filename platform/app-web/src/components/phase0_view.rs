use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle, SessionCommand, SpriteSet};

use crate::flows::{
    submit_sprite_sheet_request, submit_update_player_pet, submit_workshop_command,
};
use crate::helpers::{current_player, fallback_pet_description};
use crate::state::{IdentityState, OperationState};

const EMOTION_LABELS: [&str; 8] = [
    "Happy", "Content", "Angry", "Tired", "Excited", "Hungry", "Sleepy", "Neutral",
];

fn sprite_for_index(sprites: &SpriteSet, index: usize) -> &str {
    match index {
        0 => &sprites.happy,
        1 => &sprites.content,
        2 => &sprites.angry,
        3 => &sprites.tired,
        4 => &sprites.excited,
        5 => &sprites.hungry,
        6 => &sprites.sleepy,
        _ => &sprites.neutral,
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
    let mut preview_emotion_index = use_signal(|| 0_usize);
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
        article { class: "roster__item roster__item--phase",
            div {
                p { class: "roster__name", "Create your dragon" }
                p { class: "roster__meta", "Save a profile before discovery begins" }
            }
            span { class: "roster__status roster__status--phase status-connected", "Phase 0" }
        }

        div { class: "panel__stack",
            p { class: "meta", "Start from the default training-manikin description or replace it with your own dragon concept." }
            textarea {
                class: "input sprite-description-input",
                placeholder: "Describe your dragon's look, silhouette, colors, and attitude...",
                rows: 4,
                "data-testid": "dragon-description-input",
                value: "{dragon_description}",
                oninput: move |evt| dragon_description.set(evt.value().clone()),
            }
            div { class: "button-row",
                button {
                    class: "button button--secondary",
                    disabled: commands_disabled,
                    onclick: {
                        let default_description = default_description.clone();
                        move |_| dragon_description.set(default_description.clone())
                    },
                    "Use blank manikin"
                }
                button {
                    class: "button button--primary",
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
                    if *generating.read() { "Generating..." } else { "Generate sprites" }
                }
                button {
                    class: "button button--primary",
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
                    if *saving.read() { "Saving..." } else { "Save profile" }
                }
            }
        }

        if has_sprites {
            div { class: "panel__stack sprite-preview-section",
                h3 { class: "panel__title", "Sprite preview" }
                div { class: "sprite-preview",
                    {
                        let sprites = generated_sprites.read();
                        let idx = *preview_emotion_index.read();
                        if let Some(ref sp) = *sprites {
                            let b64 = sprite_for_index(sp, idx);
                            let label = EMOTION_LABELS[idx];
                            rsx! {
                                img {
                                    class: "sprite-preview__image",
                                    src: "data:image/png;base64,{b64}",
                                    alt: "Dragon emotion: {label}",
                                    "data-testid": "sprite-preview-image",
                                }
                                p { class: "sprite-preview__label", "{label}" }
                            }
                        } else {
                            rsx! {}
                        }
                    }
                }
                div { class: "sprite-emotion-nav",
                    for (i, label) in EMOTION_LABELS.iter().enumerate() {
                        button {
                            class: if *preview_emotion_index.read() == i { "button button--secondary sprite-emotion-btn sprite-emotion-btn--active" } else { "button sprite-emotion-btn" },
                            onclick: move |_| preview_emotion_index.set(i),
                            "{label}"
                        }
                    }
                }
            }
        }

        if is_host {
            div { class: "button-row",
                button {
                    class: "button button--primary",
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
                    "Start discovery"
                }
            }
        } else {
            p { class: "meta", "Save your profile, then wait for the host to start discovery." }
        }
    }
}
