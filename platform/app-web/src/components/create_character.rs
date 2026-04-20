use dioxus::prelude::*;

use crate::flows::submit_create_character_flow;
use crate::state::{IdentityState, OperationState, ShellScreen};
use protocol::SpriteSet;

/// Standalone character creation screen (account-scoped, not workshop-scoped).
/// This is a simplified version of the old Phase0 character editor. The sprite
/// generation flow is deferred — for now the player enters a description and
/// the server will assign default/placeholder sprites.
#[component]
pub fn CreateCharacterView(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
) -> Element {
    let mut description = use_signal(String::new);
    let pending = ops.read().pending_flow.is_some();
    let desc_value = description.read().clone();

    rsx! {
        section { class: "hero",
            h1 { class: "hero__title", "Create Character" }
            p { class: "hero__body", "Describe your dragon character" }
        }
        article { class: "panel", "data-testid": "create-character-panel",
            h2 { class: "panel__title", "Character Description" }
            div { class: "panel__stack",
                textarea {
                    class: "input",
                    "data-testid": "character-description-input",
                    placeholder: "Describe your dragon (max 512 characters)…",
                    rows: 4,
                    maxlength: 512,
                    disabled: pending,
                    value: "{desc_value}",
                    oninput: move |event| description.set(event.value()),
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
                    button {
                        class: "button button--primary",
                        "data-testid": "save-character-button",
                        disabled: pending || desc_value.trim().is_empty(),
                        onclick: move |_| {
                            let desc = description.read().clone();
                            // Placeholder sprites — the real sprite generation
                            // flow will be integrated in a future pass.
                            let sprites = SpriteSet {
                                neutral: String::new(),
                                happy: String::new(),
                                angry: String::new(),
                                sleepy: String::new(),
                            };
                            spawn(submit_create_character_flow(identity, ops, desc, sprites));
                        },
                        "Save Character"
                    }
                }
            }
        }
    }
}
