use dioxus::prelude::*;

use crate::flows::{
    load_eligible_characters_flow, load_my_characters_flow, submit_create_workshop_flow,
    submit_join_with_character_flow,
};
use crate::state::{IdentityState, OperationState, ShellScreen};
use protocol::{ClientGameState, JudgeBundle};

/// Shown when the player needs to pick a character before entering a workshop.
/// `workshop_code = Some(...)` is the join flow; `None` is host-side workshop
/// creation.
#[component]
pub fn PickCharacterView(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    reconnect_session_code: Signal<String>,
    reconnect_token: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
    workshop_code: Option<String>,
) -> Element {
    let pending = ops.read().pending_flow.is_some();
    let characters = if workshop_code.is_some() {
        ops.read().eligible_characters.clone()
    } else {
        ops.read().my_characters.clone()
    };
    let title = if workshop_code.is_some() {
        "Pick a host dragon"
    } else {
        "Pick your dragon"
    };
    let body = match workshop_code.as_deref() {
        Some(code) => format!("Choose a character for workshop {code}"),
        None => "Choose a character before creating your workshop".to_string(),
    };
    let empty_copy = if workshop_code.is_some() {
        "No eligible characters. Use a starter instead."
    } else {
        "No saved characters yet. Use a starter instead."
    };
    let primary_button = if workshop_code.is_some() {
        "Select"
    } else {
        "Create Workshop"
    };
    let starter_button = if workshop_code.is_some() {
        "Use Starter Character"
    } else {
        "Create With Starter Character"
    };

    // Load join-eligible characters or owned characters on mount.
    let mut loaded = use_signal(|| false);
    if !*loaded.read() {
        loaded.set(true);
        if let Some(code) = workshop_code.clone() {
            spawn(load_eligible_characters_flow(identity, ops, code));
        } else {
            spawn(load_my_characters_flow(identity, ops));
        }
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
                        for character in characters.iter() {
                            {
                                let char_id = character.id.clone();
                                let maybe_wcode = workshop_code.clone();
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
                                                if let Some(wc) = maybe_wcode.clone() {
                                                    spawn(submit_join_with_character_flow(
                                                        identity,
                                                        game_state,
                                                        ops,
                                                        reconnect_session_code,
                                                        reconnect_token,
                                                        judge_bundle,
                                                        wc,
                                                        Some(cid),
                                                    ));
                                                } else {
                                                    spawn(submit_create_workshop_flow(
                                                        identity,
                                                        game_state,
                                                        ops,
                                                        reconnect_session_code,
                                                        reconnect_token,
                                                        judge_bundle,
                                                        Some(cid),
                                                    ));
                                                }
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
                                id.screen = ShellScreen::AccountHome;
                            });
                        },
                        "Back"
                    }
                    {
                        let maybe_wcode = workshop_code.clone();
                        rsx! {
                            button {
                                class: "button button--primary button--cta",
                                "data-testid": "use-starter-button",
                                disabled: pending,
                                onclick: move |_| {
                                    if let Some(wc) = maybe_wcode.clone() {
                                        spawn(submit_join_with_character_flow(
                                            identity,
                                            game_state,
                                            ops,
                                            reconnect_session_code,
                                            reconnect_token,
                                            judge_bundle,
                                            wc,
                                            None,
                                        ));
                                    } else {
                                        spawn(submit_create_workshop_flow(
                                            identity,
                                            game_state,
                                            ops,
                                            reconnect_session_code,
                                            reconnect_token,
                                            judge_bundle,
                                            None,
                                        ));
                                    }
                                },
                                {starter_button}
                            }
                        }
                    }
                }
            }
        }
    }
}
