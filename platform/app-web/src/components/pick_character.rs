use dioxus::prelude::*;

use crate::flows::{load_eligible_characters_flow, submit_join_with_character_flow};
use crate::state::{IdentityState, OperationState, ShellScreen};
use protocol::{ClientGameState, JudgeBundle};

/// Shown after clicking "Join" on an open workshop. Lists the player's
/// characters eligible for this workshop. Selecting one (or choosing "Use a
/// starter") dispatches `POST /api/workshops/join`.
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
    let eligible = ops.read().eligible_characters.clone();

    // Load eligible characters on mount.
    let mut loaded = use_signal(|| false);
    if !*loaded.read() {
        loaded.set(true);
        let code = workshop_code.clone();
        spawn(load_eligible_characters_flow(identity, ops, code));
    }

    rsx! {
        section { class: "hero",
            h1 { class: "hero__title", "Pick Character" }
            p { class: "hero__body", "Choose a character for workshop {workshop_code}" }
        }
        article { class: "panel", "data-testid": "pick-character-panel",
            h2 { class: "panel__title", "Your Characters" }
            div { class: "panel__stack",
                if eligible.is_empty() {
                    p { class: "meta", "No eligible characters. Use a starter instead." }
                } else {
                    div { class: "roster",
                        for character in eligible.iter() {
                            {
                                let char_id = character.id.clone();
                                let wcode = workshop_code.clone();
                                rsx! {
                                    article { class: "roster__item",
                                        div {
                                            p { class: "roster__name", "{character.description}" }
                                        }
                                        button {
                                            class: "button button--primary button--small",
                                            "data-testid": "select-character-button",
                                            disabled: pending,
                                            onclick: move |_| {
                                                let cid = char_id.clone();
                                                let wc = wcode.clone();
                                                spawn(submit_join_with_character_flow(
                                                    identity, game_state, ops,
                                                    reconnect_session_code, reconnect_token,
                                                    judge_bundle, wc, Some(cid),
                                                ));
                                            },
                                            "Select"
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
                                id.screen = ShellScreen::AccountHome;
                            });
                        },
                        "Back"
                    }
                    {
                        let wcode = workshop_code.clone();
                        rsx! {
                            button {
                                class: "button button--primary",
                                "data-testid": "use-starter-button",
                                disabled: pending,
                                onclick: move |_| {
                                    let wc = wcode.clone();
                                    spawn(submit_join_with_character_flow(
                                        identity, game_state, ops,
                                        reconnect_session_code, reconnect_token,
                                        judge_bundle, wc, None,
                                    ));
                                },
                                "Use Starter Character"
                            }
                        }
                    }
                }
            }
        }
    }
}
