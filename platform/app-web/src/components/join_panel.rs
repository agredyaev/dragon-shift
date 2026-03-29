use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle};

use crate::flows::{submit_join_flow, submit_reconnect_flow};
use crate::state::{IdentityState, OperationState};

#[component]
pub fn JoinPanel(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    join_session_code: Signal<String>,
    join_name: Signal<String>,
    reconnect_session_code: Signal<String>,
    reconnect_token: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) -> Element {
    let join_session_code_value = join_session_code.read().clone();
    let join_name_value = join_name.read().clone();
    let reconnect_session_code_value = reconnect_session_code.read().clone();
    let reconnect_token_value = reconnect_token.read().clone();

    let pending = ops.read().pending_flow.is_some();

    let mut join_session_code_w = join_session_code;
    let mut join_name_w = join_name;
    let mut reconnect_session_code_w = reconnect_session_code;
    let mut reconnect_token_w = reconnect_token;

    rsx! {
        article { class: "panel",
            h2 { class: "panel__title", "Join workshop" }
            p { class: "panel__body", "Join with a workshop code or reopen the last saved session from this browser." }
            div { class: "panel__stack",
                input {
                    class: "input",
                    value: join_session_code_value,
                    placeholder: "Workshop code",
                    oninput: move |event| join_session_code_w.set(event.value())
                }
                input {
                    class: "input",
                    value: join_name_value,
                    placeholder: "Player name",
                    oninput: move |event| join_name_w.set(event.value())
                }
                div { class: "button-row",
                    button {
                        class: "button button--primary",
                        disabled: pending,
                        onclick: move |_| {
                            spawn(submit_join_flow(
                                identity,
                                game_state,
                                ops,
                                join_session_code,
                                join_name,
                                reconnect_session_code,
                                reconnect_token,
                                judge_bundle,
                            ));
                        },
                        "Join workshop"
                    }
                }
                input {
                    class: "input",
                    value: reconnect_session_code_value,
                    placeholder: "Reconnect session code",
                    oninput: move |event| reconnect_session_code_w.set(event.value())
                }
                input {
                    class: "input",
                    value: reconnect_token_value,
                    placeholder: "Reconnect token",
                    oninput: move |event| reconnect_token_w.set(event.value())
                }
                div { class: "button-row",
                    button {
                        class: "button button--secondary",
                        disabled: pending,
                        onclick: move |_| {
                            spawn(submit_reconnect_flow(
                                identity,
                                game_state,
                                ops,
                                join_session_code,
                                reconnect_session_code,
                                reconnect_token,
                                judge_bundle,
                            ));
                        },
                        "Reconnect"
                    }
                }
            }
        }
    }
}
