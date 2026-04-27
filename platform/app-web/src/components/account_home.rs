use dioxus::prelude::*;

use crate::flows::{
    OpenWorkshopsPaging, begin_load_open_workshops, load_open_workshops_flow,
    request_open_workshops_flow, start_create_workshop_flow, start_delete_workshop_flow,
    start_resume_workshop_flow, start_review_workshop_flow,
};
use crate::state::{IdentityState, OperationState, PendingFlow, ShellScreen, navigate_to_screen};
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
    let mut refreshed_on_mount = use_signal(|| false);
    if !*refreshed_on_mount.read() {
        ops.with_mut(begin_load_open_workshops);
        refreshed_on_mount.set(true);
        spawn(async move {
            let _ = load_open_workshops_flow(identity, ops, OpenWorkshopsPaging::First).await;
        });
    }

    // Account name is surfaced by the app bar's disclosure menu
    // trigger; no need to read it here any more.
    let pending = ops.read().pending_flow.is_some();
    let delete_workshop_pending = ops.read().pending_flow == Some(PendingFlow::DeleteWorkshop);
    let open_workshops_loading = ops.read().open_workshops_loading;
    let open_workshops_loaded = ops.read().open_workshops_loaded;
    let open_workshops_load_failed = ops.read().open_workshops_load_failed;
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
                if matches!(
                    ops.read().pending_flow,
                    Some(PendingFlow::DeleteWorkshop | PendingFlow::Create)
                ) || ops.read().open_workshops_loading
                {
                    continue;
                }
                let paging = current_paging.read().clone();
                let _ = request_open_workshops_flow(identity, ops, paging).await;
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
                            disabled: delete_workshop_pending,
                            onclick: move |_| pending_delete_workshop_code.set(None),
                            "Cancel"
                        }
                        button {
                            class: "button button--danger",
                            disabled: delete_workshop_pending,
                            onclick: move |_| {
                                let paging = current_paging.read().clone();
                                if start_delete_workshop_flow(
                                    identity,
                                    ops,
                                    current_paging,
                                    delete_code.clone(),
                                    paging,
                                ) {
                                    pending_delete_workshop_code.set(None);
                                }
                            },
                            if delete_workshop_pending { "Deleting..." } else { "Delete" }
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
                                let _ = start_create_workshop_flow(identity, ops, current_paging);
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
                    h2 { class: "panel__title", "Workshops" }
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
                                        spawn(async move {
                                            let _ = request_open_workshops_flow(identity, ops, paging)
                                                .await;
                                        });
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
                                        spawn(async move {
                                            let _ = request_open_workshops_flow(identity, ops, paging)
                                                .await;
                                        });
                                    }
                                },
                                "Next"
                            }
                        }
                    }
                }
                div { class: "panel__stack",
                    if open_workshops_loading && open_workshops.is_empty() {
                        p { class: "meta", role: "status", "aria-live": "polite", "aria-atomic": "true", "Loading workshops..." }
                    } else if open_workshops_load_failed && open_workshops.is_empty() {
                        p { class: "meta", role: "alert", "Could not load workshops right now." }
                    } else if open_workshops.is_empty() && open_workshops_loaded {
                        p { class: "meta", role: "status", "aria-live": "polite", "aria-atomic": "true", "No workshops at the moment." }
                    } else {
                        div { class: "roster",
                            for workshop in open_workshops.iter() {
                                {
                                    let code = workshop.session_code.clone();
                                    let resume_code = workshop.session_code.clone();
                                    let review_code = workshop.session_code.clone();
                                    let delete_code = workshop.session_code.clone();
                                    let can_delete = workshop.can_delete;
                                    let is_archived = workshop.archived;
                                    let can_resume = workshop.can_resume;
                                    rsx! {
                                        article { class: "roster__item", key: "{workshop.session_code}",
                                            div {
                                                p { class: "roster__name", "{workshop.host_name}'s workshop" }
                                                p { class: "roster__meta",
                                                    "{workshop.player_count} player(s) \u{2014} Code: {workshop.session_code}"
                                                    if is_archived {
                                                        span { class: "workshop-status-badge workshop-status-badge--archived", "Archived" }
                                                    } else if can_resume {
                                                        span { class: "workshop-status-badge workshop-status-badge--active", "In progress" }
                                                    }
                                                }
                                            }
                                            div { class: "button-row button-row--workshop-actions",
                                                if is_archived {
                                                    button {
                                                        class: "button button--secondary button--small",
                                                        "data-testid": "review-workshop-button",
                                                        disabled: pending,
                                                        onclick: move |_| {
                                                            let _ = start_review_workshop_flow(
                                                                identity,
                                                                game_state,
                                                                ops,
                                                                reconnect_session_code,
                                                                reconnect_token,
                                                                judge_bundle,
                                                                review_code.clone(),
                                                            );
                                                        },
                                                        "Review"
                                                    }
                                                } else if can_resume {
                                                    button {
                                                        class: "button button--primary button--small",
                                                        "data-testid": "resume-workshop-button",
                                                        disabled: pending,
                                                        onclick: move |_| {
                                                            let _ = start_resume_workshop_flow(
                                                                identity,
                                                                game_state,
                                                                ops,
                                                                reconnect_session_code,
                                                                reconnect_token,
                                                                judge_bundle,
                                                                resume_code.clone(),
                                                            );
                                                        },
                                                        "Resume"
                                                    }
                                                } else if can_delete {
                                                    button {
                                                        class: "button button--danger button--small",
                                                        "data-testid": "delete-workshop-button",
                                                        disabled: pending,
                                                        onclick: move |_| {
                                                            pending_delete_workshop_code
                                                                .set(Some(delete_code.clone()));
                                                        },
                                                        if delete_workshop_pending { "Deleting..." } else { "Delete" }
                                                    }
                                                }
                                                if !is_archived && !can_resume {
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
}
