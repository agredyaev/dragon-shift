use dioxus::prelude::*;

use crate::flows::{load_eligible_characters_flow, submit_join_with_character_flow};
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
    let pending = ops.read().pending_flow.is_some();
    let characters = ops.read().eligible_characters.clone();
    let title = "Pick your dragon";
    let body = format!("Choose a character for workshop {workshop_code}");
    let empty_copy = "No dragons yet.";
    let primary_button = "Select";
    let starter_button = "Summon random";

    // Load join-eligible characters on mount.
    let mut loaded = use_signal(|| false);
    if !*loaded.read() {
        loaded.set(true);
        spawn(load_eligible_characters_flow(
            identity,
            ops,
            workshop_code.clone(),
        ));
    }

    rsx! {
        article { class: "panel", "data-testid": "pick-character-panel",
            h1 { class: "panel__title", {title} }
            p { class: "panel__body", {body} }
            h2 { class: "panel__subtitle", "Your Characters" }
            div { class: "panel__stack",
                if characters.is_empty() {
                    p { class: "meta", {empty_copy} }
                } else {
                    div { class: "roster",
                        for (character_index, character) in characters.iter().enumerate() {
                            {
                                let char_id = character.id.clone();
                                let join_workshop_code = workshop_code.clone();
                                let character_number = character_index + 1;
                                rsx! {
                                    article { class: "roster__item pick-character-row",
                                        div { class: "pick-character-row__body",
                                            div {
                                                class: "pick-character-row__sprites",
                                                "aria-label": "Dragon {character_number} sprites",
                                                for (sprite_index, label) in CHARACTER_SPRITE_LABELS.iter().enumerate() {
                                                    div { class: "pick-character-row__sprite-frame",
                                                        img {
                                                            class: "pick-character-row__sprite",
                                                            src: "data:image/png;base64,{sprite_for_index(&character.sprites, sprite_index)}",
                                                            alt: "Dragon {character_number}: {label} sprite",
                                                        }
                                                    }
                                                }
                                            }
                                            div { class: "pick-character-row__copy",
                                                p { class: "roster__name", "Dragon {character_number}" }
                                                p { class: "roster__meta", "Sprite set ready" }
                                            }
                                        }
                                        button {
                                            class: "button button--primary button--small",
                                            "data-testid": "select-character-button",
                                            disabled: pending,
                                            onclick: move |_| {
                                                spawn(submit_join_with_character_flow(
                                                    identity,
                                                    game_state,
                                                    ops,
                                                    reconnect_session_code,
                                                    reconnect_token,
                                                    judge_bundle,
                                                    join_workshop_code.clone(),
                                                    Some(char_id.clone()),
                                                ));
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
                    if characters.is_empty() {
                        button {
                            class: "button button--primary",
                            "data-testid": "use-starter-button",
                            disabled: pending,
                            onclick: move |_| {
                                spawn(submit_join_with_character_flow(
                                    identity,
                                    game_state,
                                    ops,
                                    reconnect_session_code,
                                    reconnect_token,
                                    judge_bundle,
                                    workshop_code.clone(),
                                    None,
                                ));
                            },
                            {starter_button}
                        }
                    }
                }
            }
        }
    }
}
