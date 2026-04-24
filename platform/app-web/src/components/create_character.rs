use dioxus::prelude::*;
use protocol::{CharacterSpritePreviewRequest, CreateCharacterRequest, SpriteSet};

use crate::api::AppWebApi;
use crate::state::{
    IdentityState, OperationState, PendingFlow, ShellScreen, error_notice, info_notice,
    success_notice,
};

const EMOTION_LABELS: [&str; 4] = ["Neutral", "Happy", "Angry", "Sleepy"];
const GENERATION_PROGRESS_SEGMENTS: usize = 16;

#[derive(Clone, PartialEq, Eq)]
enum GenerationStatus {
    Idle,
    Generating,
    Error(String),
}

impl GenerationStatus {
    fn label(&self) -> &str {
        match self {
            GenerationStatus::Idle => "Ready to generate pet sprites.",
            GenerationStatus::Generating => "Generating pet sprites...",
            GenerationStatus::Error(msg) => msg.as_str(),
        }
    }

    fn is_generating(&self) -> bool {
        matches!(self, GenerationStatus::Generating)
    }

    fn is_error(&self) -> bool {
        matches!(self, GenerationStatus::Error(_))
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

/// Standalone character creation screen (account-scoped, not workshop-scoped).
///
/// Flow:
/// 1. Player enters a description.
/// 2. Clicks "Generate pet" → `POST /api/characters/preview-sprites`.
/// 3. Generated sprites render in the grid.
/// 4. Player can regenerate (new preview call) or save.
/// 5. On save → `POST /api/characters` with the populated `SpriteSet`.
///
/// Save is disabled until `generated_sprites` is `Some`. On generation
/// failure the error state is shown with a retryable action; save remains
/// disabled so empty sprites can never reach the server.
#[component]
pub fn CreateCharacterView(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
) -> Element {
    let dragon_description = use_signal(String::new);
    let generated_sprites: Signal<Option<SpriteSet>> = use_signal(|| None);
    // Trimmed description that produced the current `generated_sprites`.
    // Used to invalidate stale previews when the user edits the textarea
    // after a successful generation, so Save cannot submit a description
    // that doesn't match the sprites.
    let last_generated_for: Signal<Option<String>> = use_signal(|| None);
    let generation_status = use_signal(|| GenerationStatus::Idle);
    let saving = use_signal(|| false);

    let desc_value = dragon_description.read().clone();
    let description_empty = desc_value.trim().is_empty();
    let status_snapshot = generation_status.read().clone();
    let generation_in_flight = status_snapshot.is_generating();
    let has_sprites = generated_sprites.read().is_some();
    let saving_now = *saving.read();
    let pending = ops.read().pending_flow.is_some();

    let generate_onclick = {
        let mut dragon_description = dragon_description;
        let mut generated_sprites = generated_sprites;
        let mut last_generated_for = last_generated_for;
        let mut generation_status = generation_status;
        move |_| {
            if generation_status.read().is_generating() {
                return;
            }
            let desc = dragon_description.read().trim().to_string();
            if desc.is_empty() {
                return;
            }
            let base_url = { identity.read().api_base_url.clone() };
            generation_status.set(GenerationStatus::Generating);
            spawn(async move {
                let api = AppWebApi::new(base_url);
                let request = CharacterSpritePreviewRequest {
                    description: desc.clone(),
                };
                match api.preview_character_sprites(&request).await {
                    Ok(response) => {
                        generated_sprites.set(Some(response.sprites));
                        last_generated_for.set(Some(desc));
                        generation_status.set(GenerationStatus::Idle);
                    }
                    Err(error) => {
                        // Leave `generated_sprites` untouched: if a prior
                        // preview succeeded the user can keep it and retry;
                        // if it was None it stays None.
                        generation_status.set(GenerationStatus::Error(format!(
                            "Sprite generation failed: {error}"
                        )));
                    }
                }
            });
            // silence the unused-mut warnings on the capture in wasm targets
            let _ = &mut dragon_description;
        }
    };

    let save_onclick = {
        let mut identity = identity;
        let mut ops = ops;
        let mut saving = saving;
        move |_| {
            let Some(sprites) = generated_sprites.read().clone() else {
                return;
            };
            let desc = dragon_description.read().trim().to_string();
            if desc.is_empty() {
                return;
            }
            let base_url = { identity.read().api_base_url.clone() };
            saving.set(true);
            ops.with_mut(|o| {
                o.pending_flow = Some(PendingFlow::Create);
                o.notice = Some(info_notice("Creating character…"));
            });
            spawn(async move {
                let api = AppWebApi::new(base_url);
                let request = CreateCharacterRequest {
                    description: desc,
                    sprites,
                };
                match api.create_character(&request).await {
                    Ok(_profile) => {
                        identity.with_mut(|id| {
                            id.screen = ShellScreen::AccountHome;
                        });
                        ops.with_mut(|o| {
                            o.pending_flow = None;
                            o.notice = Some(success_notice("Character created."));
                        });
                    }
                    Err(error) => {
                        ops.with_mut(|o| {
                            o.pending_flow = None;
                            o.notice = Some(error_notice(&error));
                        });
                    }
                }
                saving.set(false);
            });
        }
    };

    let generation_bar_class = if generation_in_flight {
        "phase0-generation-bar phase0-generation-bar--loading"
    } else if status_snapshot.is_error() {
        "phase0-generation-bar phase0-generation-bar--error"
    } else {
        "phase0-generation-bar phase0-generation-bar--idle"
    };

    let generate_button_label = if generation_in_flight {
        "Drawing..."
    } else if has_sprites {
        "Regenerate"
    } else if status_snapshot.is_error() {
        "Retry"
    } else {
        "Generate pet"
    };

    // Button state machine (§10 step 7 / §4.F):
    //   textarea empty            -> Generate disabled,   Save disabled
    //   text entered, no sprites  -> Generate primary,    Save disabled (ghost)
    //   sprites exist             -> Save primary,        Regenerate ghost, Generate hidden
    // The Generate/Regenerate affordance lives on the same button because
    // the existing component shape pairs it with `GenerationStatus`; we
    // just swap the visual class between primary and ghost (secondary).
    let generate_button_class = if has_sprites {
        "button button--secondary phase0-action-button"
    } else {
        "button button--primary phase0-action-button"
    };

    rsx! {
        article { class: "panel phase0-card", "data-testid": "create-character-panel",
            h1 { class: "phase0-card__title", "Create Character" }
            p { class: "phase0-card__subtitle", "Describe your dragon character" }

            h2 { class: "panel__title phase0-card__section-title", "Design your pet" }
            p { class: "panel__body phase0-card__section-desc",
                "Describe your pet, and our AI will draw it for you!"
            }

            textarea {
                class: "phase0-textarea",
                "data-testid": "character-description-input",
                placeholder: "> e.g. A tiny green dragon with a fiery tail...",
                rows: 4,
                maxlength: 512,
                value: "{desc_value}",
                disabled: generation_in_flight || saving_now,
                oninput: move |event| {
                    let new_value = event.value();
                    let mut desc = dragon_description;
                    desc.set(new_value.clone());
                    // If a prior preview exists and the user edits the
                    // description away from the one that produced it,
                    // invalidate the stale sprites so Save is disabled
                    // until they regenerate. Whitespace-only edits that
                    // leave `trim()` unchanged keep the preview.
                    let mut last_for = last_generated_for;
                    let prev = last_for.read().clone();
                    if let Some(prev) = prev
                        && new_value.trim() != prev
                    {
                        let mut sprites = generated_sprites;
                        sprites.set(None);
                        last_for.set(None);
                        // Reset status unless a generation is in flight.
                        // (Textarea is `disabled` while generating, so
                        // this branch is normally unreachable then — be
                        // defensive.)
                        let mut status = generation_status;
                        if !status.read().is_generating() {
                            status.set(GenerationStatus::Idle);
                        }
                    }
                },
            }

            div {
                class: "{generation_bar_class}",
                role: "status",
                "aria-live": "polite",
                "aria-busy": if generation_in_flight { "true" } else { "false" },
                p { class: "phase0-generation-bar__label", {status_snapshot.label().to_string()} }
                span { class: "sr-only", {status_snapshot.label().to_string()} }
                for index in 0..GENERATION_PROGRESS_SEGMENTS {
                    div {
                        class: "phase0-generation-bar__segment",
                        style: "animation-delay: {index * 90}ms;",
                    }
                }
            }

            button {
                class: "{generate_button_class}",
                "data-testid": "generate-sprites-button",
                disabled: generation_in_flight
                    || saving_now
                    || description_empty
                    || pending,
                onclick: generate_onclick,
                {generate_button_label}
            }

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
            }

            div { class: "button-row",
                button {
                    class: "button button--secondary",
                    "data-testid": "back-to-account-button",
                    disabled: generation_in_flight || saving_now,
                    onclick: move |_| {
                        identity.clone().with_mut(|id| {
                            id.screen = ShellScreen::AccountHome;
                        });
                    },
                    "Back"
                }
                button {
                    class: "button button--primary phase0-action-button",
                    "data-testid": "save-character-button",
                    disabled: !has_sprites
                        || saving_now
                        || generation_in_flight
                        || description_empty
                        || pending,
                    onclick: save_onclick,
                    if saving_now { "Saving..." } else { "Save Character" }
                }
            }
        }
    }
}
