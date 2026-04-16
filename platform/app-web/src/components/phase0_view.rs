use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use gloo_timers::future::TimeoutFuture;
use protocol::{ClientGameState, JudgeBundle, SessionCommand, SpriteSet};

use super::notice::NoticeBar;
use crate::flows::{
    SpriteSheetSubmitError, submit_sprite_sheet_request, submit_update_player_pet,
    submit_workshop_command,
};
use crate::helpers::current_player;
use crate::state::{IdentityState, OperationState};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

const EMOTION_LABELS: [&str; 4] = ["Neutral", "Happy", "Angry", "Sleepy"];
const GENERATION_PROGRESS_SEGMENTS: usize = 16;
#[cfg(target_arch = "wasm32")]
const GENERATION_ERROR_FLASH_MS: u32 = 1400;
#[cfg(not(target_arch = "wasm32"))]
const GENERATION_ERROR_FLASH_MS: u32 = 1400;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SpriteGenerationStatus {
    Idle,
    Generating,
    Error,
}

impl SpriteGenerationStatus {
    fn label(self) -> &'static str {
        match self {
            SpriteGenerationStatus::Idle => "Ready to generate pet sprites.",
            SpriteGenerationStatus::Generating => "Generating pet sprites...",
            SpriteGenerationStatus::Error => "Sprite generation failed. You can try again.",
        }
    }
}

async fn wait_generation_error_flash() {
    #[cfg(target_arch = "wasm32")]
    {
        TimeoutFuture::new(GENERATION_ERROR_FLASH_MS).await;
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        tokio::time::sleep(std::time::Duration::from_millis(GENERATION_ERROR_FLASH_MS as u64)).await;
    }
}

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

    let mut dragon_description = use_signal(|| {
        current_player(state)
            .and_then(|player| player.pet_description.clone())
            .unwrap_or_default()
    });
    let generated_sprites: Signal<Option<SpriteSet>> =
        use_signal(|| current_player(state).and_then(|player| player.custom_sprites.clone()));
    let mut saving = use_signal(|| false);
    let mut generation_status = use_signal(|| SpriteGenerationStatus::Idle);
    let generation_attempt = use_signal(|| Arc::new(AtomicU64::new(0)));

    let is_host = current_player(state).map(|p| p.is_host).unwrap_or(false);
    let commands_disabled = {
        let o = ops.read();
        o.pending_flow.is_some() || o.pending_command.is_some()
    };
    let has_sprites = generated_sprites.read().is_some();
    let description_empty = dragon_description.read().trim().is_empty();
    let generation_in_flight = *generation_status.read() == SpriteGenerationStatus::Generating;

    drop(gs);

    rsx! {
        article { class: "panel phase0-card",
            // ---- Title ----
            h1 { class: "phase0-card__title", "Dragon Shift" }
            p { class: "phase0-card__subtitle", "A collaborative pet game" }
            NoticeBar { ops }

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
                disabled: generation_in_flight,
                oninput: move |evt| dragon_description.set(evt.value().clone()),
            }

            div {
                class: {
                    let status = *generation_status.read();
                    match status {
                        SpriteGenerationStatus::Generating => "phase0-generation-bar phase0-generation-bar--loading",
                        SpriteGenerationStatus::Error => "phase0-generation-bar phase0-generation-bar--error",
                        SpriteGenerationStatus::Idle => "phase0-generation-bar phase0-generation-bar--idle",
                    }
                },
                role: "status",
                "aria-live": "polite",
                "aria-busy": if *generation_status.read() == SpriteGenerationStatus::Generating { "true" } else { "false" },
                p { class: "phase0-generation-bar__label", {generation_status.read().label()} }
                span { class: "sr-only", {generation_status.read().label()} }
                for index in 0..GENERATION_PROGRESS_SEGMENTS {
                    div {
                        class: "phase0-generation-bar__segment",
                        style: "animation-delay: {index * 90}ms;",
                    }
                }
            }

            if !has_sprites {
                button {
                    class: "button phase0-action-button",
                    "data-testid": "generate-sprites-button",
                    disabled: commands_disabled || generation_in_flight || description_empty,
                    onclick: {
                        let attempt_counter = generation_attempt.read().clone();
                        move |_| {
                            if *generation_status.read() == SpriteGenerationStatus::Generating {
                                return;
                            }
                            let desc = dragon_description.read().clone();
                            let attempt_id = attempt_counter.fetch_add(1, Ordering::Relaxed) + 1;
                            let task_attempt_counter = attempt_counter.clone();
                            generation_status.set(SpriteGenerationStatus::Generating);
                            spawn(async move {
                                let result = submit_sprite_sheet_request(
                                    identity,
                                    ops,
                                    generated_sprites,
                                    desc,
                                )
                                .await;
                                if task_attempt_counter.load(Ordering::Relaxed) != attempt_id {
                                    return;
                                }
                                match result {
                                    Ok(()) => {
                                        generation_status.set(SpriteGenerationStatus::Idle);
                                    }
                                    Err(SpriteSheetSubmitError::Preflight(_)) => {
                                        generation_status.set(SpriteGenerationStatus::Idle);
                                    }
                                    Err(SpriteSheetSubmitError::Request(_)) => {
                                        generation_status.set(SpriteGenerationStatus::Error);
                                        wait_generation_error_flash().await;
                                        if task_attempt_counter.load(Ordering::Relaxed) != attempt_id {
                                            return;
                                        }
                                        generation_status.set(SpriteGenerationStatus::Idle);
                                    }
                                }
                            });
                        }
                    },
                    if generation_in_flight { "Drawing..." } else { "Generate pet" }
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
