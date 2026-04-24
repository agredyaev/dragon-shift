use dioxus::prelude::*;
use protocol::AUTH_ERR_NAME_TAKEN_WRONG_PASSWORD;

use crate::flows::submit_signin_flow;
use crate::state::{IdentityState, OperationState};

/// Map a backend signin error code (the `error` field of the JSON body,
/// surfaced by `api::extract_backend_error` as the `Err` string) to the
/// copy the SignIn screen renders in `NoticeBar`.
///
/// Unknown codes fall through so operators/tests still see the raw backend
/// string instead of a generic swallow.
pub fn map_signin_error(error: &str) -> String {
    match error {
        AUTH_ERR_NAME_TAKEN_WRONG_PASSWORD => {
            "That name is already registered. Enter the correct password or choose a different name."
                .to_string()
        }
        other => other.to_string(),
    }
}

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
                        class: "button button--primary button--cta",
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
