use dioxus::prelude::*;

use crate::flows::{
    OpenWorkshopsPaging, load_my_characters_flow, load_open_workshops_flow, submit_logout_flow,
};
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
    let my_characters_empty = ops.read().my_characters.is_empty();
    let next_cursor = ops.read().open_workshops_next_cursor.clone();
    let prev_cursor = ops.read().open_workshops_prev_cursor.clone();
    let has_next = next_cursor.is_some();
    let has_prev = prev_cursor.is_some();
    // Zero-state: no characters AND no visible open workshops. Tier-1
    // tradeoff (per §10 step 8): we don't yet distinguish "loading" from
    // "truly empty" — ListLoadState arrives in Tier 4. Until the initial
    // `load_my_characters_flow` resolves we keep rendering the 3-card
    // layout so returning users never see a misleading "create your
    // first dragon" flash when `state.rs` clears `my_characters` on a
    // non-reconnect session reset.
    let mut characters_loaded = use_signal(|| false);
    let is_first_visit =
        *characters_loaded.read() && my_characters_empty && open_workshops.is_empty();

    // Tracks the paging direction of the last user-initiated pager click
    // (or the initial mount). The 5s poll re-uses this instead of
    // resetting to `First` so an already-paginated view isn't yanked
    // back to page 1 on every tick.
    let mut current_paging = use_signal(|| OpenWorkshopsPaging::First);

    // Load characters + workshops on mount.
    let mut loaded = use_signal(|| false);
    if !*loaded.read() {
        loaded.set(true);
        spawn(async move {
            load_my_characters_flow(identity, ops).await;
            characters_loaded.set(true);
        });
        spawn(load_open_workshops_flow(
            identity,
            ops,
            OpenWorkshopsPaging::First,
        ));
    }

    // Poll open workshops every 5 seconds. Re-issues whichever paging
    // direction the user last selected so pagination isn't clobbered.
    use_future(move || {
        let identity = identity;
        let ops = ops;
        async move {
            loop {
                #[cfg(target_arch = "wasm32")]
                gloo_timers::future::TimeoutFuture::new(5_000).await;
                #[cfg(not(target_arch = "wasm32"))]
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let paging = current_paging.read().clone();
                load_open_workshops_flow(identity, ops, paging).await;
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

        if is_first_visit {
            // Zero-state: single dominant CTA routing straight to
            // character creation. No secondary tiles while the user has
            // nothing to pick up.
            section { class: "panel", "data-testid": "account-home-zero-state",
                h2 { class: "panel__title", "Create your first dragon" }
                p { class: "panel__body",
                    "You'll need a dragon character before you can create or join a workshop."
                }
                div { class: "button-row",
                    button {
                        class: "button button--primary",
                        "data-testid": "create-character-button",
                        disabled: pending,
                        onclick: move |_| {
                            identity.with_mut(|id| {
                                id.screen = ShellScreen::CreateCharacter;
                            });
                        },
                        "Create a dragon"
                    }
                }
            }
        } else {
            section { class: "grid",
                // ---- Create Workshop ----
                article { class: "panel",
                    h2 { class: "panel__title", "Create Workshop" }
                    div { class: "panel__stack",
                        div { class: "button-row",
                            button {
                                class: "button button--primary",
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
                        // Prev / Next pager — rendered only when the
                        // server returned at least one cursor. When both
                        // are absent the single-page case skips the
                        // wrapper entirely (M-5).
                        if has_prev || has_next {
                            div { class: "button-row",
                                button {
                                    class: "button button--secondary button--small",
                                    "data-testid": "open-workshops-prev-button",
                                    disabled: pending || !has_prev,
                                    onclick: move |_| {
                                        if let Some(cursor) = prev_cursor.clone() {
                                            let paging = OpenWorkshopsPaging::Before(cursor);
                                            current_paging.set(paging.clone());
                                            spawn(load_open_workshops_flow(identity, ops, paging));
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
                                            let paging = OpenWorkshopsPaging::After(cursor);
                                            current_paging.set(paging.clone());
                                            spawn(load_open_workshops_flow(identity, ops, paging));
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
    }
}
