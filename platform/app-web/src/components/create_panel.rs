use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle};

use crate::flows::submit_create_flow;
use crate::state::{IdentityState, OperationState};

#[component]
pub fn CreatePanel(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    create_name: Signal<String>,
    join_session_code: Signal<String>,
    reconnect_session_code: Signal<String>,
    reconnect_token: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) -> Element {
    let name = create_name.read().clone();
    let pending = ops.read().pending_flow.is_some();

    let mut create_name_w = create_name;

    rsx! {
        article { class: "panel",
            h2 { class: "panel__title", "Create workshop" }
            p { class: "panel__body", "Start a new workshop and share the code with your group." }
            div { class: "panel__stack",
                input {
                    class: "input",
                    value: name,
                    placeholder: "Host name",
                    oninput: move |event| create_name_w.set(event.value())
                }
                div { class: "button-row",
                    button {
                        class: "button button--primary",
                        disabled: pending,
                        onclick: move |_| {
                            spawn(submit_create_flow(
                                identity,
                                game_state,
                                ops,
                                create_name,
                                join_session_code,
                                reconnect_session_code,
                                reconnect_token,
                                judge_bundle,
                            ));
                        },
                        "Create workshop"
                    }
                }
            }
        }
    }
}
