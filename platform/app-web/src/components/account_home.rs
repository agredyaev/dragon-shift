use dioxus::prelude::*;

use crate::flows::{OpenWorkshopsPaging, load_open_workshops_flow, submit_logout_flow};
use crate::state::{IdentityState, OperationState, ShellScreen};

#[component]
pub fn AccountHomeView(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
) -> Element {
    let account_name = identity
        .read()
        .account
        .as_ref()
        .map(|a| a.name.clone())
        .unwrap_or_default();
    let pending = ops.read().pending_flow.is_some();
    let open_workshops = ops.read().open_workshops.clone();
    let next_cursor = ops.read().open_workshops_next_cursor.clone();
    let prev_cursor = ops.read().open_workshops_prev_cursor.clone();
    let has_next = next_cursor.is_some();
    let has_prev = prev_cursor.is_some();

    // Load characters + workshops on mount.
    let mut loaded = use_signal(|| false);
    if !*loaded.read() {
        loaded.set(true);
        spawn(load_open_workshops_flow(
            identity,
            ops,
            OpenWorkshopsPaging::First,
        ));
    }

    // Poll open workshops every 5 seconds. The poll always resets to the
    // first page so AccountHome keeps surfacing the freshest lobbies at the
    // top — paging through older lobbies is an explicit user gesture via
    // the Prev / Next buttons below.
    use_future(move || {
        let identity = identity;
        let ops = ops;
        async move {
            loop {
                #[cfg(target_arch = "wasm32")]
                gloo_timers::future::TimeoutFuture::new(5_000).await;
                #[cfg(not(target_arch = "wasm32"))]
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                load_open_workshops_flow(identity, ops, OpenWorkshopsPaging::First).await;
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
                                identity.with_mut(|id| {
                                    id.screen = ShellScreen::PickCharacter {
                                        workshop_code: None,
                                    };
                                });
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
                                                        id.screen = ShellScreen::PickCharacter {
                                                            workshop_code: Some(c),
                                                        };
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
                    // Prev / Next pager. Buttons are disabled when the
                    // respective cursor is absent. No page numbers: the
                    // keyset cursor is one-way on each side, so an "N of M"
                    // indicator isn't cheap to compute and isn't part of
                    // this pass's scope.
                    div { class: "button-row",
                        button {
                            class: "button button--secondary button--small",
                            "data-testid": "open-workshops-prev-button",
                            disabled: pending || !has_prev,
                            onclick: move |_| {
                                if let Some(cursor) = prev_cursor.clone() {
                                    spawn(load_open_workshops_flow(
                                        identity,
                                        ops,
                                        OpenWorkshopsPaging::Before(cursor),
                                    ));
                                }
                            },
                            "Prev"
                        }
                        button {
                            class: "button button--secondary button--small",
                            "data-testid": "open-workshops-next-button",
                            disabled: pending || !has_next,
                            onclick: move |_| {
                                if let Some(cursor) = next_cursor.clone() {
                                    spawn(load_open_workshops_flow(
                                        identity,
                                        ops,
                                        OpenWorkshopsPaging::After(cursor),
                                    ));
                                }
                            },
                            "Next"
                        }
                    }
                }
            }
        }
    }
}
