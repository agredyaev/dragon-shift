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
    phase0_minutes: Signal<String>,
    phase1_minutes: Signal<String>,
    phase2_minutes: Signal<String>,
    join_session_code: Signal<String>,
    reconnect_session_code: Signal<String>,
    reconnect_token: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) -> Element {
    let _ = identity;
    let name = create_name.read().clone();
    let phase0_minutes_value = phase0_minutes.read().clone();
    let phase1_minutes_value = phase1_minutes.read().clone();
    let phase2_minutes_value = phase2_minutes.read().clone();
    let pending = ops.read().pending_flow.is_some();

    let mut create_name_w = create_name;
    let mut phase0_minutes_w = phase0_minutes;
    let mut phase1_minutes_w = phase1_minutes;
    let mut phase2_minutes_w = phase2_minutes;

    rsx! {
        article { class: "panel", "data-testid": "create-panel",
            h2 { class: "panel__title", "Create workshop" }
            p { class: "panel__body", "Start a new workshop and share the code with your group. Provider models and credentials are configured server-side through deployment values." }
            div { class: "panel__stack",
                input {
                    class: "input",
                    "data-testid": "create-name-input",
                    value: name,
                    placeholder: "Host name",
                    oninput: move |event| create_name_w.set(event.value())
                }
                div { class: "grid grid--compact",
                    input {
                        class: "input",
                        value: phase0_minutes_value,
                        placeholder: "Phase 0 minutes",
                        oninput: move |event| phase0_minutes_w.set(event.value())
                    }
                    input {
                        class: "input",
                        value: phase1_minutes_value,
                        placeholder: "Phase 1 minutes",
                        oninput: move |event| phase1_minutes_w.set(event.value())
                    }
                    input {
                        class: "input",
                        value: phase2_minutes_value,
                        placeholder: "Phase 2 minutes",
                        oninput: move |event| phase2_minutes_w.set(event.value())
                    }
                }
                div { class: "button-row",
                    button {
                        class: "button button--primary",
                        "data-testid": "create-workshop-button",
                        disabled: pending,
                        onclick: move |_| {
                            spawn(submit_create_flow(
                                identity,
                                game_state,
                                ops,
                                create_name,
                                phase0_minutes,
                                phase1_minutes,
                                phase2_minutes,
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
