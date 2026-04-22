use dioxus::prelude::*;

use crate::flows::{
    load_open_workshops_flow, submit_create_workshop_flow, submit_logout_flow,
};
use crate::state::{IdentityState, OperationState, ShellScreen};
use protocol::{ClientGameState, JudgeBundle};

#[component]
pub fn AccountHomeView(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    reconnect_session_code: Signal<String>,
    reconnect_token: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) -> Element {
    let account_name = identity
        .read()
        .account
        .as_ref()
        .map(|a| a.name.clone())
        .unwrap_or_default();
    let pending = ops.read().pending_flow.is_some();
    let open_workshops = ops.read().open_workshops.clone();

    // Load characters + workshops on mount.
    let mut loaded = use_signal(|| false);
    if !*loaded.read() {
        loaded.set(true);
        spawn(load_open_workshops_flow(identity, ops));
    }

    // Poll open workshops every 5 seconds.
    use_future(move || {
        let identity = identity;
        let ops = ops;
        async move {
            loop {
                #[cfg(target_arch = "wasm32")]
                gloo_timers::future::TimeoutFuture::new(5_000).await;
                #[cfg(not(target_arch = "wasm32"))]
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                load_open_workshops_flow(identity, ops).await;
            }
        }
    });

    rsx! {
        section { class: "hero", "data-testid": "account-home-panel",
            h1 { class: "hero__title", "Dragon Shift" }
            p { class: "hero__body", "Welcome, {account_name}" }
            div { class: "hero__meta",
                button {
                    class: "button button--secondary",
                    "data-testid": "logout-button",
                    disabled: pending,
                    onclick: move |_| {
                        spawn(submit_logout_flow(identity, ops));
                    },
                    "Logout"
                }
            }
        }

        section { class: "grid",
            // ---- Create Workshop ----
            article { class: "panel",
                h2 { class: "panel__title", "Create Workshop" }
                div { class: "panel__stack",
                    div { class: "button-row",
                        button {
                            class: "button button--primary",
                            style: "width:100%;",
                            "data-testid": "create-workshop-button",
                            disabled: pending,
                            onclick: move |_| {
                                spawn(submit_create_workshop_flow(
                                    identity,
                                    game_state,
                                    ops,
                                    reconnect_session_code,
                                    reconnect_token,
                                    judge_bundle,
                                ));
                            },
                            "Create Workshop"
                        }
                    }
                }
            }

            // ---- Create Character ----
            article { class: "panel",
                h2 { class: "panel__title", "Create Character" }
                div { class: "panel__stack",
                    div { class: "button-row",
                        button {
                            class: "button button--secondary",
                            style: "width:100%;",
                            "data-testid": "create-character-button",
                            disabled: pending,
                            onclick: move |_| {
                                identity.with_mut(|id| {
                                    id.screen = ShellScreen::CreateCharacter;
                                });
                            },
                            "Create Character"
                        }
                    }
                }
            }

            // ---- Open Workshops ----
            article { class: "panel",
                h2 { class: "panel__title", "Open Workshops" }
                div { class: "panel__stack",
                    if open_workshops.is_empty() {
                        p { class: "meta", "No open workshops at the moment." }
                    } else {
                        div { class: "roster",
                            for workshop in open_workshops.iter() {
                                {
                                    let code = workshop.session_code.clone();
                                    rsx! {
                                        article { class: "roster__item",
                                            div {
                                                p { class: "roster__name", "{workshop.host_name}'s workshop" }
                                                p { class: "roster__meta",
                                                    "{workshop.player_count} player(s) \u{2014} Code: {workshop.session_code}"
                                                }
                                            }
                                            button {
                                                class: "button button--primary button--small",
                                                "data-testid": "join-workshop-button",
                                                disabled: pending,
                                                onclick: move |_| {
                                                    let c = code.clone();
                                                    identity.with_mut(|id| {
                                                        id.screen = ShellScreen::PickCharacter { workshop_code: c };
                                                    });
                                                },
                                                "Join"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
