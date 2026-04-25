use dioxus::prelude::*;

use crate::flows::{
    OpenWorkshopsPaging, load_open_workshops_flow, submit_create_workshop_flow,
    submit_delete_workshop_flow,
};
use crate::state::{
    IdentityState, OperationState, PendingFlow, ShellScreen, navigate_to_screen,
};

#[component]
pub fn AccountHomeView(identity: Signal<IdentityState>, ops: Signal<OperationState>) -> Element {
    // Account name is surfaced by the app bar's disclosure menu
    // trigger; no need to read it here any more.
    let pending = ops.read().pending_flow.is_some();
    let delete_pending = ops.read().pending_flow == Some(PendingFlow::DeleteWorkshop);
    let open_workshops = ops.read().open_workshops.clone();
    let next_cursor = ops.read().open_workshops_next_cursor.clone();
    let prev_cursor = ops.read().open_workshops_prev_cursor.clone();
    let has_next = next_cursor.is_some();
    let has_prev = prev_cursor.is_some();
    let show_pager = has_prev || has_next;
    let pager_prev_cursor = prev_cursor.clone();
    let pager_next_cursor = next_cursor.clone();
    let mut pending_delete_workshop_code = use_signal(|| None::<String>);

    // Tracks the paging direction of the last user-initiated pager click
    // (or the initial mount). The 5s poll re-uses this instead of
    // resetting to `First` so an already-paginated view isn't yanked
    // back to page 1 on every tick.
    let mut current_paging = use_signal(|| OpenWorkshopsPaging::First);

    // Load open workshops on mount.
    let mut loaded = use_signal(|| false);
    let initial_open_workshops_loaded = loaded.read().clone();
    if !*loaded.read() {
        loaded.set(true);
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
        // Page-level <h1> is visually hidden so screen readers still
        // announce the landmark title; the wordmark lives in the app
        // bar (see UX_RECOMPOSE_v2 §4.A line 173).
        h1 { class: "sr-only", "Your workshops" }

        if let Some(delete_code) = pending_delete_workshop_code.read().clone() {
            div {
                class: "modal-backdrop",
                role: "presentation",
                onclick: move |_| pending_delete_workshop_code.set(None),
                div {
                    class: "modal-card",
                    role: "dialog",
                    "aria-modal": "true",
                    "aria-labelledby": "delete-workshop-modal-title",
                    "aria-describedby": "delete-workshop-modal-body",
                    onclick: move |event| event.stop_propagation(),
                    h2 {
                        id: "delete-workshop-modal-title",
                        class: "panel__title modal-card__title",
                        "Delete Workshop"
                    }
                    p {
                        id: "delete-workshop-modal-body",
                        class: "panel__body modal-card__body",
                        "Delete this empty lobby workshop? This action cannot be undone."
                    }
                    div { class: "button-row modal-card__actions",
                        button {
                            class: "button button--secondary",
                            disabled: delete_pending,
                            onclick: move |_| pending_delete_workshop_code.set(None),
                            "Cancel"
                        }
                        button {
                            class: "button button--danger",
                            disabled: delete_pending,
                            onclick: move |_| {
                                let paging = current_paging.read().clone();
                                pending_delete_workshop_code.set(None);
                                spawn(submit_delete_workshop_flow(
                                    identity,
                                    ops,
                                    current_paging,
                                    delete_code.clone(),
                                    paging,
                                ));
                            },
                            if delete_pending { "Deleting..." } else { "Delete" }
                        }
                    }
                }
            }
        }

        section { class: "grid",
            // ---- Create Workshop ----
            article { class: "panel",
                h2 { class: "panel__title", "Create Workshop" }
                p { class: "panel__body",
                    "Create a lobby now and join it later when you're ready to play."
                }
                div { class: "panel__stack panel__stack--home-action",
                    div { class: "button-row button-row--home-action",
                        button {
                            class: "button button--primary",
                            "data-testid": "create-workshop-button",
                            disabled: pending,
                            onclick: move |_| {
                                current_paging.set(OpenWorkshopsPaging::First);
                                spawn(submit_create_workshop_flow(identity, ops));
                            },
                            "Create Workshop"
                        }
                    }
                }
            }

            // ---- Create Character ----
            article { class: "panel",
                h2 { class: "panel__title", "Create a dragon" }
                p { class: "panel__body",
                    "Create your dragon now so you can jump into the game with a ready-to-play companion."
                }
                div { class: "panel__stack panel__stack--home-action",
                    div { class: "button-row button-row--home-action",
                        button {
                            class: "button button--secondary",
                            "data-testid": "create-character-button",
                            disabled: pending,
                            onclick: move |_| {
                                identity.with_mut(|id| {
                                    ops.with_mut(|o| {
                                        navigate_to_screen(id, o, ShellScreen::CreateCharacter);
                                    });
                                });
                            },
                            "Create a dragon"
                        }
                    }
                }
            }
        }

        section { class: "panel panel--wide", "data-testid": "open-workshops-panel",
                div { class: "panel__header",
                    h2 { class: "panel__title", "Open Workshops" }
                    if show_pager {
                        div { class: "button-row button-row--workshop-pager",
                            button {
                                class: "button button--secondary button--small",
                                "data-testid": "open-workshops-prev-button",
                                disabled: pending || !has_prev,
                                onclick: move |_| {
                                    if let Some(cursor) = pager_prev_cursor.clone() {
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
                                    if let Some(cursor) = pager_next_cursor.clone() {
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
                div { class: "panel__stack",
                    if open_workshops.is_empty() && initial_open_workshops_loaded {
                        p { class: "meta", "No open workshops at the moment." }
                    } else {
                        div { class: "roster",
                            for workshop in open_workshops.iter() {
                                {
                                    let code = workshop.session_code.clone();
                                    let delete_code = workshop.session_code.clone();
                                    let can_delete = workshop.can_delete;
                                    rsx! {
                                        article { class: "roster__item",
                                            div {
                                                p { class: "roster__name", "{workshop.host_name}'s workshop" }
                                                p { class: "roster__meta",
                                                    "{workshop.player_count} player(s) \u{2014} Code: {workshop.session_code}"
                                                }
                                            }
                                            div { class: "button-row button-row--workshop-actions",
                                                if can_delete {
                                                    button {
                                                        class: "button button--danger button--small",
                                                        "data-testid": "delete-workshop-button",
                                                        disabled: pending,
                                                        onclick: move |_| {
                                                            pending_delete_workshop_code
                                                                .set(Some(delete_code.clone()));
                                                        },
                                                        if delete_pending { "Deleting..." } else { "Delete" }
                                                    }
                                                }
                                                button {
                                                    class: "button button--primary button--small",
                                                    "data-testid": "join-workshop-button",
                                                    disabled: pending,
                                                    onclick: move |_| {
                                                        let c = code.clone();
                                                        identity.with_mut(|id| {
                                                            ops.with_mut(|o| {
                                                                navigate_to_screen(
                                                                    id,
                                                                    o,
                                                                    ShellScreen::PickCharacter {
                                                                        workshop_code: c,
                                                                    },
                                                                );
                                                            });
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
