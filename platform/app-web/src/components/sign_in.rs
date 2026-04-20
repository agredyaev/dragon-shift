use dioxus::prelude::*;

use crate::flows::submit_signin_flow;
use crate::state::{IdentityState, OperationState};

#[component]
pub fn SignInView(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
) -> Element {
    let mut name = use_signal(String::new);
    let mut password = use_signal(String::new);
    let pending = ops.read().pending_flow.is_some();

    rsx! {
        section { class: "hero", "data-testid": "signin-panel",
            h1 { class: "hero__title", "Dragon Shift" }
            p { class: "hero__body", "Sign in or create a new account" }
        }
        article { class: "panel",
            h2 { class: "panel__title", "Sign In" }
            div { class: "panel__stack",
                input {
                    class: "input",
                    "data-testid": "signin-name-input",
                    r#type: "text",
                    placeholder: "Name",
                    value: "{name.read()}",
                    disabled: pending,
                    oninput: move |event| name.set(event.value()),
                }
                input {
                    class: "input",
                    "data-testid": "signin-password-input",
                    r#type: "password",
                    placeholder: "Password",
                    value: "{password.read()}",
                    disabled: pending,
                    oninput: move |event| password.set(event.value()),
                }
                div { class: "button-row",
                    button {
                        class: "button button--primary",
                        style: "width:100%;",
                        "data-testid": "signin-submit-button",
                        disabled: pending,
                        onclick: move |_| {
                            let n = name.read().clone();
                            let p = password.read().clone();
                            spawn(submit_signin_flow(identity, ops, n, p, String::new()));
                        },
                        "Sign In"
                    }
                }
            }
        }
    }
}
