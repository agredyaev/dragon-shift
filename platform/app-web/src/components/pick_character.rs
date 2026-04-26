use dioxus::prelude::*;

use crate::flows::{
    begin_load_eligible_characters, load_eligible_characters_flow, start_join_with_character_flow,
};
use crate::state::{IdentityState, OperationState, ShellScreen, navigate_to_screen};
use protocol::{ClientGameState, JudgeBundle, SpriteSet};

const CHARACTER_SPRITE_LABELS: [&str; 4] = ["Neutral", "Happy", "Angry", "Sleepy"];

fn sprite_for_index(sprites: &SpriteSet, index: usize) -> &str {
    match index {
        0 => &sprites.neutral,
        1 => &sprites.happy,
        2 => &sprites.angry,
        _ => &sprites.sleepy,
    }
}

fn character_display_name(character_name: Option<&str>, fallback_number: usize) -> String {
    character_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Dragon {fallback_number}"))
}

/// Shown when the player needs to pick a character before joining a workshop.
#[component]
pub fn PickCharacterView(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    reconnect_session_code: Signal<String>,
    reconnect_token: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
    workshop_code: String,
) -> Element {
    let mut loaded_workshop_code = use_signal(|| None::<String>);
    if loaded_workshop_code.read().as_deref() != Some(workshop_code.as_str()) {
        ops.with_mut(|o| begin_load_eligible_characters(o, &workshop_code));
        loaded_workshop_code.set(Some(workshop_code.clone()));
    }

    let pending = ops.read().pending_flow.is_some();
    let eligibility = {
        let current_ops = ops.read();
        let workshop_matches = current_ops.eligible_characters_workshop_code.as_deref()
            == Some(workshop_code.as_str());
        (
            current_ops.eligible_characters_loading && workshop_matches,
            current_ops.eligible_characters_loaded && workshop_matches,
            current_ops.eligible_characters_load_failed && workshop_matches,
            if workshop_matches {
                current_ops.eligible_characters.clone()
            } else {
                Vec::new()
            },
        )
    };
    let (characters_loading, characters_loaded, characters_load_failed, characters) = eligibility;
    let title = "Pick your dragon";
    let body = format!("Choose a character for workshop {workshop_code}");
    let empty_copy = "No dragons yet.";
    let primary_button = "Select";
    let starter_button = "Summon random";

    use_effect({
        let workshop_code = workshop_code.clone();
        move || {
            spawn(load_eligible_characters_flow(
                identity,
                ops,
                workshop_code.clone(),
            ));
        }
    });

    rsx! {
        article { class: "panel", "data-testid": "pick-character-panel",
            h1 { class: "panel__title", {title} }
            p { class: "panel__body", {body} }
            h2 { class: "panel__subtitle", "Your Characters" }
            div { class: "panel__stack",
                if characters_loading && characters.is_empty() {
                    p { class: "meta", role: "status", "aria-live": "polite", "aria-atomic": "true", "Loading dragons..." }
                } else if characters_load_failed && characters.is_empty() {
                    p { class: "meta", role: "alert", "Could not load eligible dragons right now." }
                } else if characters_loaded && characters.is_empty() {
                    p { class: "meta", role: "status", "aria-live": "polite", "aria-atomic": "true", {empty_copy} }
                } else if characters.is_empty() {
                    p { class: "meta", role: "status", "aria-live": "polite", "aria-atomic": "true", "Loading dragons..." }
                } else {
                    div { class: "roster roster--pick-character",
                        for (character_index, character) in characters.iter().enumerate() {
                            {
                                let char_id = character.id.clone();
                                let join_workshop_code = workshop_code.clone();
                                let character_number = character_index + 1;
                                let character_name = character_display_name(
                                    character.name.as_deref(),
                                    character_number,
                                );
                                rsx! {
                                    article { class: "roster__item pick-character-row", key: "{character.id}",
                                        div { class: "pick-character-row__body",
                                            div {
                                                class: "pick-character-row__sprites",
                                                "aria-label": "Dragon {character_number} sprites",
                                                for (sprite_index, label) in CHARACTER_SPRITE_LABELS.iter().enumerate() {
                                                    div { class: "pick-character-row__sprite-frame",
                                                        img {
                                                            class: "pick-character-row__sprite",
                                                            src: "data:image/png;base64,{sprite_for_index(&character.sprites, sprite_index)}",
                                                            alt: "{character_name}: {label} sprite",
                                                        }
                                                    }
                                                }
                                            }
                                            div { class: "pick-character-row__copy",
                                                p { class: "roster__name", "{character_name}" }
                                                p { class: "roster__meta", "Sprite set ready" }
                                            }
                                        }
                                        button {
                                            class: "button button--primary button--small",
                                            "data-testid": "select-character-button",
                                            disabled: pending,
                                            onclick: move |_| {
                                                let _ = start_join_with_character_flow(
                                                    identity,
                                                    game_state,
                                                    ops,
                                                    reconnect_session_code,
                                                    reconnect_token,
                                                    judge_bundle,
                                                    join_workshop_code.clone(),
                                                    Some(char_id.clone()),
                                                );
                                            },
                                            {primary_button}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "button-row",
                    button {
                        class: "button button--secondary",
                        "data-testid": "back-to-account-button",
                        disabled: pending,
                        onclick: move |_| {
                            identity.with_mut(|id| {
                                ops.with_mut(|o| {
                                    navigate_to_screen(id, o, ShellScreen::AccountHome);
                                });
                            });
                        },
                        "Back"
                    }
                    if characters_loaded && !characters_load_failed && characters.is_empty() {
                        button {
                            class: "button button--primary",
                            "data-testid": "use-starter-button",
                            disabled: pending || characters_loading,
                            onclick: move |_| {
                                let _ = start_join_with_character_flow(
                                    identity,
                                    game_state,
                                    ops,
                                    reconnect_session_code,
                                    reconnect_token,
                                    judge_bundle,
                                    workshop_code.clone(),
                                    None,
                                );
                            },
                            {starter_button}
                        }
                    }
                }
            }
        }
    }
}
