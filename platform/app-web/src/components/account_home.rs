use dioxus::prelude::*;

use crate::flows::{
    OpenWorkshopsPaging, begin_load_open_workshops, load_open_workshops_flow,
    request_open_workshops_flow_and_sync_paging, start_create_workshop_flow,
    start_delete_workshop_flow, start_resume_workshop_flow, start_review_workshop_flow,
    start_update_workshop_flow,
};
use crate::state::{IdentityState, OperationState, PendingFlow, ShellScreen, navigate_to_screen};
use protocol::{ClientGameState, JudgeBundle, UpdateWorkshopRequest, WorkshopCreateConfig};

fn parse_minutes_input(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<u32>().ok().filter(|minutes| *minutes > 0)
}

fn build_update_workshop_request(phase1_input: &str, phase2_input: &str) -> UpdateWorkshopRequest {
    UpdateWorkshopRequest {
        phase1_minutes: parse_minutes_input(phase1_input),
        phase2_minutes: parse_minutes_input(phase2_input),
    }
}

fn pager_button_disabled(open_workshops_loading: bool, has_cursor: bool) -> bool {
    open_workshops_loading || !has_cursor
}

#[cfg(target_arch = "wasm32")]
async fn sleep_open_workshops_poll(delay_ms: u32) {
    gloo_timers::future::TimeoutFuture::new(delay_ms).await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn sleep_open_workshops_poll(delay_ms: u32) {
    tokio::time::sleep(std::time::Duration::from_millis(delay_ms as u64)).await;
}

#[cfg(target_arch = "wasm32")]
fn open_workshops_poll_jitter_ms() -> u32 {
    (js_sys::Math::random() * 2_000.0) as u32
}

#[cfg(not(target_arch = "wasm32"))]
fn open_workshops_poll_jitter_ms() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::pager_button_disabled;

    #[test]
    fn pager_buttons_only_disable_for_loading_or_missing_cursor() {
        assert!(!pager_button_disabled(false, true));
        assert!(pager_button_disabled(true, true));
        assert!(pager_button_disabled(false, false));
    }
}

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
            sleep_open_workshops_poll(open_workshops_poll_jitter_ms()).await;
            let _ = load_open_workshops_flow(identity, ops, OpenWorkshopsPaging::First).await;
        });
    }

    // Account name is surfaced by the app bar's disclosure menu
    // trigger; no need to read it here any more.
    let pending = ops.read().pending_flow.is_some();
    let delete_workshop_pending = ops.read().pending_flow == Some(PendingFlow::DeleteWorkshop);
    let update_workshop_pending = ops.read().pending_flow == Some(PendingFlow::UpdateWorkshop);
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
    let mut pending_settings_workshop = use_signal(|| None::<String>);
    let mut phase1_minutes_input = use_signal(String::new);
    let mut phase2_minutes_input = use_signal(String::new);
    let defaults = WorkshopCreateConfig::default();

    // Tracks the paging direction of the last applied pager response. The 5s
    // poll reuses this instead of resetting to `First` so an already-paginated
    // view isn't yanked back to page 1 on every tick.
    let current_paging = use_signal(|| OpenWorkshopsPaging::First);

    // Poll open workshops every 5 seconds. Re-issues whichever paging
    // direction the user last selected so pagination isn't clobbered.
    use_future(move || {
        let identity = identity;
        let ops = ops;
        async move {
            let mut consecutive_failures = 0u32;
            loop {
                let backoff_ms = consecutive_failures.saturating_mul(5_000).min(20_000);
                sleep_open_workshops_poll(5_000 + backoff_ms + open_workshops_poll_jitter_ms())
                    .await;
                if matches!(
                    ops.read().pending_flow,
                    Some(
                        PendingFlow::DeleteWorkshop
                            | PendingFlow::Create
                            | PendingFlow::UpdateWorkshop
                    )
                ) || ops.read().open_workshops_loading
                {
                    continue;
                }
                let paging = current_paging.read().clone();
                if request_open_workshops_flow_and_sync_paging(
                    identity,
                    ops,
                    current_paging,
                    paging,
                )
                .await
                {
                    consecutive_failures = 0;
                } else {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                }
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

        if let Some(settings_code) = pending_settings_workshop.read().clone() {
            {
                let reset_settings_code = settings_code.clone();
                let save_settings_code = settings_code.clone();
                rsx! {
                    div {
                        class: "modal-backdrop",
                        role: "presentation",
                        onclick: move |_| pending_settings_workshop.set(None),
                        div {
                            class: "modal-card",
                            role: "dialog",
                            "aria-modal": "true",
                            "aria-labelledby": "workshop-settings-modal-title",
                            "aria-describedby": "workshop-settings-modal-body",
                            onclick: move |event| event.stop_propagation(),
                            h2 {
                                id: "workshop-settings-modal-title",
                                class: "panel__title modal-card__title",
                                "Workshop Settings"
                            }
                            p {
                                id: "workshop-settings-modal-body",
                                class: "panel__body modal-card__body",
                                "Set phase lengths in minutes. Leave a field blank to use the default."
                            }
                            div { class: "panel__stack",
                                label { class: "form-label", r#for: "phase1-minutes-input", "Phase 1 minutes" }
                                input {
                                    id: "phase1-minutes-input",
                                    class: "input",
                                    "data-testid": "phase1-minutes-input",
                                    r#type: "number",
                                    min: "1",
                                    step: "1",
                                    placeholder: "{defaults.phase1_minutes}",
                                    value: "{phase1_minutes_input.read()}",
                                    oninput: move |event| phase1_minutes_input.set(event.value()),
                                }
                                label { class: "form-label", r#for: "phase2-minutes-input", "Handover and Phase 2 minutes" }
                                input {
                                    id: "phase2-minutes-input",
                                    class: "input",
                                    "data-testid": "phase2-minutes-input",
                                    r#type: "number",
                                    min: "1",
                                    step: "1",
                                    placeholder: "{defaults.phase2_minutes}",
                                    value: "{phase2_minutes_input.read()}",
                                    oninput: move |event| phase2_minutes_input.set(event.value()),
                                }
                            }
                            div { class: "button-row modal-card__actions",
                                button {
                                    class: "button button--secondary",
                                    disabled: update_workshop_pending,
                                    onclick: move |_| pending_settings_workshop.set(None),
                                    "Cancel"
                                }
                                button {
                                    class: "button button--secondary",
                                    "data-testid": "reset-workshop-settings-button",
                                    disabled: update_workshop_pending,
                                    onclick: move |_| {
                                        let request = UpdateWorkshopRequest {
                                            phase1_minutes: None,
                                            phase2_minutes: None,
                                        };
                                        if start_update_workshop_flow(
                                            identity,
                                            ops,
                                            current_paging,
                                            reset_settings_code.clone(),
                                            request,
                                        ) {
                                            pending_settings_workshop.set(None);
                                        }
                                    },
                                    if update_workshop_pending { "Saving..." } else { "Reset to defaults" }
                                }
                                button {
                                    class: "button button--primary",
                                    "data-testid": "save-workshop-settings-button",
                                    disabled: update_workshop_pending,
                                    onclick: move |_| {
                                        let request = build_update_workshop_request(
                                            &phase1_minutes_input.read(),
                                            &phase2_minutes_input.read(),
                                        );
                                        if start_update_workshop_flow(
                                            identity,
                                            ops,
                                            current_paging,
                                            save_settings_code.clone(),
                                            request,
                                        ) {
                                            pending_settings_workshop.set(None);
                                        }
                                    },
                                    if update_workshop_pending { "Saving..." } else { "Save" }
                                }
                            }
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
                                let _ = start_create_workshop_flow(identity, ops, current_paging, None);
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
                                disabled: pager_button_disabled(open_workshops_loading, has_prev),
                                onclick: move |_| {
                                    if let Some(cursor) = pager_prev_cursor.clone() {
                                        let paging = OpenWorkshopsPaging::Before(cursor);
                                        spawn(async move {
                                            let _ = request_open_workshops_flow_and_sync_paging(
                                                identity,
                                                ops,
                                                current_paging,
                                                paging,
                                            )
                                            .await;
                                        });
                                    }
                                },
                                "Prev"
                            }
                            button {
                                class: "button button--secondary button--small",
                                "data-testid": "open-workshops-next-button",
                                disabled: pager_button_disabled(open_workshops_loading, has_next),
                                onclick: move |_| {
                                    if let Some(cursor) = pager_next_cursor.clone() {
                                        let paging = OpenWorkshopsPaging::After(cursor);
                                        spawn(async move {
                                            let _ = request_open_workshops_flow_and_sync_paging(
                                                identity,
                                                ops,
                                                current_paging,
                                                paging,
                                            )
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
                                     let settings_code = workshop.session_code.clone();
                                     let can_delete = workshop.can_delete;
                                     let is_archived = workshop.archived;
                                     let can_resume = workshop.can_resume;
                                     let phase1_minutes = workshop.phase1_minutes;
                                     let phase2_minutes = workshop.phase2_minutes;
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
                                                     button {
                                                         class: "button button--secondary button--small",
                                                         "data-testid": "workshop-settings-button",
                                                         disabled: pending,
                                                         onclick: move |_| {
                                                             phase1_minutes_input.set(phase1_minutes.to_string());
                                                             phase2_minutes_input.set(phase2_minutes.to_string());
                                                             pending_settings_workshop.set(Some(settings_code.clone()));
                                                         },
                                                         "Settings"
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
