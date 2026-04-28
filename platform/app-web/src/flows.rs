#![allow(clippy::too_many_arguments)]

use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use protocol::{
    AuthRequest, ClientGameState, JoinWorkshopRequest, JudgeBundle, ListOpenWorkshopsResponse,
    OpenWorkshopCursor, Phase, SessionCommand, SpriteSet, UpdateCharacterRequest,
    UpdateWorkshopRequest,
};

use crate::api::{
    AppWebApi, build_client_session_snapshot, build_command_request, build_judge_bundle_request,
};
use crate::helpers::{parse_tags_input, pending_command_label};
use crate::realtime::{bootstrap_realtime, disconnect_realtime};
use crate::state::{
    ConnectionStatus, IdentityState, NoticeScope, OperationState, PendingCommandTicket,
    PendingFlow, PendingFlowTicket, PendingJudgeBundleTicket, ShellNotice, ShellScreen,
    apply_command_error, apply_join_success, apply_judge_bundle_error, apply_judge_bundle_success,
    apply_realtime_bootstrap_error, apply_request_error, apply_successful_command,
    clear_account_identity, clear_pending_command_if_current, clear_pending_flow_if_current,
    clear_pending_judge_bundle_if_current, clear_pre_session_caches, clear_session_identity,
    error_notice, info_notice, navigate_to_screen, notice_scope_for_screen,
    pending_command_ticket_is_current, pending_flow_ticket_is_current,
    pending_judge_bundle_ticket_is_current, persist_browser_account_snapshot,
    persist_browser_session_game_state, persist_browser_session_snapshot, reserve_pending_command,
    reserve_pending_flow, reserve_pending_judge_bundle, scoped_notice, success_notice,
};

fn try_reserve_pending_flow(
    mut ops: Signal<OperationState>,
    flow: PendingFlow,
    notice: ShellNotice,
) -> Option<PendingFlowTicket> {
    let mut ticket = None;
    ops.with_mut(|o| {
        ticket = reserve_pending_flow(o, flow, notice);
    });
    ticket
}

fn try_reserve_pending_command(
    mut ops: Signal<OperationState>,
    command: SessionCommand,
) -> Option<PendingCommandTicket> {
    let mut ticket = None;
    ops.with_mut(|o| {
        ticket = reserve_pending_command(
            o,
            command,
            scoped_notice(
                NoticeScope::Session,
                info_notice(pending_command_label(command)),
            ),
        );
    });
    ticket
}

fn try_reserve_pending_judge_bundle(
    mut ops: Signal<OperationState>,
) -> Option<PendingJudgeBundleTicket> {
    let mut ticket = None;
    ops.with_mut(|o| {
        ticket = reserve_pending_judge_bundle(
            o,
            scoped_notice(
                NoticeScope::Session,
                info_notice("Building workshop archive…"),
            ),
        );
    });
    ticket
}

#[cfg_attr(not(test), allow(dead_code))]
pub async fn submit_reconnect_flow(
    mut identity: Signal<IdentityState>,
    mut game_state: Signal<Option<ClientGameState>>,
    mut ops: Signal<OperationState>,
    mut reconnect_session_code_input: Signal<String>,
    mut reconnect_token_input: Signal<String>,
    mut judge_bundle: Signal<Option<JudgeBundle>>,
) {
    let (base_url, session_code, reconnect_token) = {
        let id = identity.read();
        let session_code = reconnect_session_code_input.read();
        let reconnect_token = reconnect_token_input.read();
        (
            id.api_base_url.clone(),
            session_code.trim().to_string(),
            reconnect_token.trim().to_string(),
        )
    };

    if session_code.is_empty() || reconnect_token.is_empty() {
        ops.with_mut(|o| {
            o.notice = Some(scoped_notice(
                NoticeScope::Session,
                error_notice("Session code and reconnect token are required for reconnect."),
            ))
        });
        return;
    }

    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::Reconnect,
        scoped_notice(NoticeScope::Session, info_notice("Reconnecting…")),
    ) else {
        return;
    };

    identity.with_mut(|id| {
        id.connection_status = ConnectionStatus::Connecting;
    });

    let api = AppWebApi::new(base_url);
    match api.reconnect_workshop(session_code, reconnect_token).await {
        Ok(success) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            identity.with_mut(|id| {
                game_state.with_mut(|gs| {
                    ops.with_mut(|o| {
                        reconnect_session_code_input.with_mut(|reconnect_code| {
                            reconnect_token_input.with_mut(|token| {
                                judge_bundle.with_mut(|jb| {
                                    apply_join_success(
                                        id,
                                        gs,
                                        o,
                                        reconnect_code,
                                        token,
                                        jb,
                                        success,
                                        PendingFlow::Reconnect,
                                    );
                                });
                            });
                        });
                    });
                });
            });
            let persistence_warning = {
                let snapshot_warning =
                    { identity.read().session_snapshot.clone() }.and_then(|snapshot| {
                        persist_browser_session_snapshot(&snapshot)
                            .err()
                            .map(|error| {
                                format!("Reconnected, but session persistence failed: {error}")
                            })
                    });
                let game_state_warning = { game_state.read().clone() }.and_then(|state| {
                    persist_browser_session_game_state(&state)
                        .err()
                        .map(|error| {
                            format!("Reconnected, but session state persistence failed: {error}")
                        })
                });
                snapshot_warning.or(game_state_warning)
            };
            if let Err(error) = bootstrap_realtime(identity, game_state, ops, judge_bundle) {
                identity.with_mut(|id| {
                    game_state.with_mut(|gs| {
                        ops.with_mut(|o| {
                            apply_realtime_bootstrap_error(id, gs, o, error);
                        });
                    });
                });
            } else if let Some(warning) = persistence_warning {
                ops.with_mut(|o| {
                    o.pending_realtime_notice =
                        Some(scoped_notice(NoticeScope::Session, error_notice(&warning)));
                });
            }
        }
        Err(error) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    if !pending_flow_ticket_is_current(o, &ticket) {
                        return;
                    }
                    apply_request_error(id, o, error);
                });
            });
        }
    }
}

pub fn start_workshop_command(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    handover_tags_input: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
    command: SessionCommand,
    payload: Option<serde_json::Value>,
) -> bool {
    if identity.read().connection_status != ConnectionStatus::Connected {
        ops.with_mut(|o| {
            o.notice = Some(scoped_notice(
                NoticeScope::Session,
                error_notice("Wait for the session to finish syncing before sending commands."),
            ));
        });
        return false;
    }
    let Some(ticket) = try_reserve_pending_command(ops, command) else {
        return false;
    };
    spawn(submit_workshop_command_reserved(
        identity,
        ops,
        handover_tags_input,
        judge_bundle,
        command,
        payload,
        ticket,
    ));
    true
}

#[allow(dead_code)]
pub async fn submit_workshop_command(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    handover_tags_input: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
    command: SessionCommand,
    payload: Option<serde_json::Value>,
) {
    let Some(ticket) = try_reserve_pending_command(ops, command) else {
        return;
    };

    submit_workshop_command_reserved(
        identity,
        ops,
        handover_tags_input,
        judge_bundle,
        command,
        payload,
        ticket,
    )
    .await;
}

async fn submit_workshop_command_reserved(
    mut identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    mut handover_tags_input: Signal<String>,
    mut judge_bundle: Signal<Option<JudgeBundle>>,
    command: SessionCommand,
    payload: Option<serde_json::Value>,
    ticket: PendingCommandTicket,
) {
    let (base_url, snapshot) = {
        let id = identity.read();
        (id.api_base_url.clone(), id.session_snapshot.clone())
    };

    if identity.read().connection_status != ConnectionStatus::Connected {
        ops.with_mut(|o| {
            if !pending_command_ticket_is_current(o, &ticket) {
                return;
            }
            clear_pending_command_if_current(o, &ticket);
            o.notice = Some(scoped_notice(
                NoticeScope::Session,
                error_notice("Wait for the session to finish syncing before sending commands."),
            ));
        });
        return;
    }

    let Some(snapshot) = snapshot else {
        ops.with_mut(|o| {
            if !pending_command_ticket_is_current(o, &ticket) {
                return;
            }
            clear_pending_command_if_current(o, &ticket);
            o.notice = Some(scoped_notice(
                NoticeScope::Session,
                error_notice("Connect to a workshop before sending commands."),
            ))
        });
        return;
    };

    let api = AppWebApi::new(base_url);
    match api
        .send_command(build_command_request(&snapshot, command, payload))
        .await
    {
        Ok(()) => {
            if identity.read().session_snapshot.as_ref() != Some(&snapshot) {
                ops.with_mut(|o| clear_pending_command_if_current(o, &ticket));
                return;
            }
            if !pending_command_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    if !pending_command_ticket_is_current(o, &ticket) {
                        return;
                    }
                    handover_tags_input.with_mut(|tags| {
                        judge_bundle.with_mut(|jb| {
                            apply_successful_command(id, o, tags, jb, command);
                        });
                    });
                });
            });
        }
        Err(error) => {
            if identity.read().session_snapshot.as_ref() != Some(&snapshot) {
                ops.with_mut(|o| clear_pending_command_if_current(o, &ticket));
                return;
            }
            if !pending_command_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    if !pending_command_ticket_is_current(o, &ticket) {
                        return;
                    }
                    apply_command_error(id, o, error);
                });
            });
        }
    }
}

#[allow(dead_code)]
pub async fn submit_handover_tags_command(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    handover_tags_input: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) {
    let tags = {
        let tags_input = handover_tags_input.read();
        parse_tags_input(&tags_input)
    };

    submit_workshop_command(
        identity,
        ops,
        handover_tags_input,
        judge_bundle,
        SessionCommand::SubmitTags,
        Some(serde_json::json!(tags)),
    )
    .await;
}

#[allow(dead_code)]
pub fn start_judge_bundle_request(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) -> bool {
    let Some(ticket) = try_reserve_pending_judge_bundle(ops) else {
        return false;
    };
    spawn(submit_judge_bundle_request_reserved(
        identity,
        game_state,
        ops,
        judge_bundle,
        ticket,
    ));
    true
}

#[allow(dead_code)]
pub async fn submit_judge_bundle_request(
    identity: Signal<IdentityState>,
    _game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) {
    let Some(ticket) = try_reserve_pending_judge_bundle(ops) else {
        return;
    };

    submit_judge_bundle_request_reserved(identity, _game_state, ops, judge_bundle, ticket).await;
}

async fn submit_judge_bundle_request_reserved(
    mut identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    mut ops: Signal<OperationState>,
    mut judge_bundle: Signal<Option<JudgeBundle>>,
    ticket: PendingJudgeBundleTicket,
) {
    let (base_url, snapshot) = {
        let id = identity.read();
        (id.api_base_url.clone(), id.session_snapshot.clone())
    };

    let Some(snapshot) = snapshot else {
        ops.with_mut(|o| {
            if !pending_judge_bundle_ticket_is_current(o, &ticket) {
                return;
            }
            clear_pending_judge_bundle_if_current(o, &ticket);
            o.notice = Some(scoped_notice(
                NoticeScope::Session,
                error_notice("Connect to a workshop before building the archive."),
            ))
        });
        return;
    };

    let api = AppWebApi::new(base_url);
    let should_finalize_voting = {
        let gs = game_state.read();
        match gs.as_ref().map(|state| state.phase) {
            Some(Phase::Voting) => {
                let results_revealed = gs
                    .as_ref()
                    .and_then(|state| state.voting.as_ref())
                    .is_some_and(|voting| voting.results_revealed);
                if !results_revealed {
                    ops.with_mut(|o| {
                        if !pending_judge_bundle_ticket_is_current(o, &ticket) {
                            return;
                        }
                        clear_pending_judge_bundle_if_current(o, &ticket);
                        o.notice = Some(scoped_notice(
                            NoticeScope::Session,
                            error_notice("Finish voting before archiving the workshop."),
                        ));
                    });
                    return;
                }
                true
            }
            _ => false,
        }
    };

    if should_finalize_voting {
        match api
            .send_command(build_command_request(
                &snapshot,
                SessionCommand::EndSession,
                None,
            ))
            .await
        {
            Ok(()) => {}
            Err(error) => {
                if identity.read().session_snapshot.as_ref() != Some(&snapshot) {
                    ops.with_mut(|o| clear_pending_judge_bundle_if_current(o, &ticket));
                    return;
                }
                if !pending_judge_bundle_ticket_is_current(&ops.read(), &ticket) {
                    return;
                }
                identity.with_mut(|id| {
                    ops.with_mut(|o| {
                        if !pending_judge_bundle_ticket_is_current(o, &ticket) {
                            return;
                        }
                        apply_judge_bundle_error(id, o, error);
                    });
                });
                return;
            }
        }
    }

    match api
        .fetch_judge_bundle(build_judge_bundle_request(&snapshot))
        .await
    {
        Ok(bundle) => {
            if identity.read().session_snapshot.as_ref() != Some(&snapshot) {
                ops.with_mut(|o| clear_pending_judge_bundle_if_current(o, &ticket));
                return;
            }
            if !pending_judge_bundle_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            ops.with_mut(|o| {
                if !pending_judge_bundle_ticket_is_current(o, &ticket) {
                    return;
                }
                judge_bundle.with_mut(|jb| {
                    apply_judge_bundle_success(o, jb, bundle);
                });
            });
        }
        Err(error) => {
            if identity.read().session_snapshot.as_ref() != Some(&snapshot) {
                ops.with_mut(|o| clear_pending_judge_bundle_if_current(o, &ticket));
                return;
            }
            if !pending_judge_bundle_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    if !pending_judge_bundle_ticket_is_current(o, &ticket) {
                        return;
                    }
                    apply_judge_bundle_error(id, o, error);
                });
            });
        }
    }
}

// ---------------------------------------------------------------------------
// New cookie-auth flows
// ---------------------------------------------------------------------------

/// Sign in (or create account). On success, persists the account snapshot in
/// localStorage and navigates to AccountHome.
pub fn start_signin_flow(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    name: String,
    password: String,
    hero: String,
) -> bool {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::SignIn,
        scoped_notice(NoticeScope::SignIn, info_notice("Signing in…")),
    ) else {
        return false;
    };
    spawn(submit_signin_flow_reserved(
        identity, ops, name, password, hero, ticket,
    ));
    true
}

#[allow(dead_code)]
pub async fn submit_signin_flow(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    name: String,
    password: String,
    hero: String,
) {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::SignIn,
        scoped_notice(NoticeScope::SignIn, info_notice("Signing in…")),
    ) else {
        return;
    };

    submit_signin_flow_reserved(identity, ops, name, password, hero, ticket).await;
}

async fn submit_signin_flow_reserved(
    mut identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    name: String,
    password: String,
    hero: String,
    ticket: PendingFlowTicket,
) {
    let base_url = { identity.read().api_base_url.clone() };

    if name.trim().is_empty() || password.is_empty() {
        ops.with_mut(|o| {
            if !pending_flow_ticket_is_current(o, &ticket) {
                return;
            }
            clear_pending_flow_if_current(o, &ticket);
            o.notice = Some(scoped_notice(
                NoticeScope::SignIn,
                error_notice("Name and password are required."),
            ))
        });
        return;
    }

    let api = AppWebApi::new(base_url);
    let request = AuthRequest {
        hero: hero.clone(),
        name: name.trim().to_string(),
        password,
    };
    match api.signin(&request).await {
        Ok(response) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            let persistence_warning = persist_browser_account_snapshot(&response.account)
                .err()
                .map(|error| format!("Signed in, but local persistence failed: {error}"));
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    if !pending_flow_ticket_is_current(o, &ticket) {
                        return;
                    }
                    id.account = Some(response.account);
                    navigate_to_screen(id, o, ShellScreen::AccountHome);
                });
            });
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                o.pending_flow = None;
                let msg = if response.created {
                    "Account created."
                } else {
                    "Signed in."
                };
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    persistence_warning
                        .as_deref()
                        .map(error_notice)
                        .unwrap_or_else(|| success_notice(msg)),
                ));
            });
        }
        Err(error) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            // Map structured backend error codes (e.g.
            // `name_taken_wrong_password`) to the spec copy rendered in the
            // SignIn NoticeBar. See `components/sign_in.rs::map_signin_error`.
            let message = crate::components::sign_in::map_signin_error(&error);
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                o.pending_flow = None;
                o.notice = Some(scoped_notice(NoticeScope::SignIn, error_notice(&message)));
            });
        }
    }
}

/// Logout: clears cookie on server, clears localStorage, navigates to SignIn.
pub fn start_logout_flow(identity: Signal<IdentityState>, ops: Signal<OperationState>) -> bool {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::Logout,
        scoped_notice(NoticeScope::AccountHome, info_notice("Signing out…")),
    ) else {
        return false;
    };
    spawn(submit_logout_flow_reserved(identity, ops, ticket));
    true
}

#[allow(dead_code)]
pub async fn submit_logout_flow(identity: Signal<IdentityState>, ops: Signal<OperationState>) {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::Logout,
        scoped_notice(NoticeScope::AccountHome, info_notice("Signing out…")),
    ) else {
        return;
    };

    submit_logout_flow_reserved(identity, ops, ticket).await;
}

async fn submit_logout_flow_reserved(
    mut identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    ticket: PendingFlowTicket,
) {
    if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
        return;
    }

    let base_url = { identity.read().api_base_url.clone() };
    let api = AppWebApi::new(base_url);
    disconnect_realtime();

    identity.with_mut(|id| {
        clear_account_identity(id);
    });
    ops.with_mut(|o| {
        clear_pre_session_caches(o);
        o.pending_command = None;
        o.pending_judge_bundle = false;
        o.notice = Some(scoped_notice(
            NoticeScope::SignIn,
            info_notice("Signing out…"),
        ));
    });

    // Best-effort server logout; even if it fails we clear local state.
    let _ = api.logout().await;

    ops.with_mut(|o| {
        if pending_flow_ticket_is_current(o, &ticket) {
            o.pending_flow = None;
            o.notice = None;
        }
    });
}

/// Create an empty workshop lobby. The creator remains on AccountHome and must
/// explicitly join later from the open-workshops list.
pub fn start_create_workshop_flow(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    current_paging: Signal<OpenWorkshopsPaging>,
    config: Option<protocol::WorkshopCreateConfig>,
) -> bool {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::Create,
        scoped_notice(NoticeScope::AccountHome, info_notice("Creating workshop…")),
    ) else {
        return false;
    };
    spawn(submit_create_workshop_flow_reserved(
        identity,
        ops,
        current_paging,
        config,
        ticket,
    ));
    true
}

#[allow(dead_code)]
pub async fn submit_create_workshop_flow(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    current_paging: Signal<OpenWorkshopsPaging>,
    config: Option<protocol::WorkshopCreateConfig>,
) {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::Create,
        scoped_notice(NoticeScope::AccountHome, info_notice("Creating workshop…")),
    ) else {
        return;
    };

    submit_create_workshop_flow_reserved(identity, ops, current_paging, config, ticket).await;
}

async fn submit_create_workshop_flow_reserved(
    mut identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    mut current_paging: Signal<OpenWorkshopsPaging>,
    config: Option<protocol::WorkshopCreateConfig>,
    ticket: PendingFlowTicket,
) {
    let base_url = { identity.read().api_base_url.clone() };

    let api = AppWebApi::new(base_url);
    match api.create_workshop_lobby_with_config(config).await {
        Ok(success) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    if !pending_flow_ticket_is_current(o, &ticket) {
                        return;
                    }
                    id.connection_status = ConnectionStatus::Offline;
                    navigate_to_screen(id, o, ShellScreen::AccountHome);
                });
            });
            let requested_paging = { current_paging.read().clone() };
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    success_notice(&format!("Workshop {} created.", success.session_code)),
                ));
            });
            let applied =
                request_open_workshops_flow(identity, ops, OpenWorkshopsPaging::First).await;
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            let refresh_failed = ops.read().open_workshops_load_failed;
            if applied && *current_paging.read() == requested_paging {
                current_paging.set(OpenWorkshopsPaging::First);
            }
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                if refresh_failed {
                    o.notice = Some(scoped_notice(
                        NoticeScope::AccountHome,
                        error_notice(&format!(
                            "Workshop {} created. Workshop list refresh failed.",
                            success.session_code,
                        )),
                    ));
                }
                o.pending_flow = None;
            });
        }
        Err(error) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    if !pending_flow_ticket_is_current(o, &ticket) {
                        return;
                    }
                    apply_request_error(id, o, error);
                });
            });
        }
    }
}

/// Join a workshop with an optional character. Called from PickCharacter screen.
/// `character_id = None` means the server leases a random starter.
pub fn start_join_with_character_flow(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    reconnect_session_code: Signal<String>,
    reconnect_token: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
    workshop_code: String,
    character_id: Option<String>,
) -> bool {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::Join,
        scoped_notice(NoticeScope::PickCharacter, info_notice("Joining workshop…")),
    ) else {
        return false;
    };
    spawn(submit_join_with_character_flow_reserved(
        identity,
        game_state,
        ops,
        reconnect_session_code,
        reconnect_token,
        judge_bundle,
        workshop_code,
        character_id,
        ticket,
    ));
    true
}

/// Restore an already-participated workshop directly from AccountHome. Used for
/// archived workshops where the account owns an existing player slot and only
/// needs a fresh reconnect token to review the final screen.
pub fn start_review_workshop_flow(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    reconnect_session_code: Signal<String>,
    reconnect_token: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
    workshop_code: String,
) -> bool {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::Review,
        scoped_notice(
            NoticeScope::AccountHome,
            info_notice("Opening workshop review…"),
        ),
    ) else {
        return false;
    };
    spawn_forever(submit_review_workshop_flow_reserved(
        identity,
        game_state,
        ops,
        reconnect_session_code,
        reconnect_token,
        judge_bundle,
        workshop_code,
        ticket,
    ));
    true
}

/// Resume an in-progress workshop where this account already owns a player
/// slot. This skips character picking because non-lobby joins can only restore
/// an existing participant.
pub fn start_resume_workshop_flow(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    reconnect_session_code: Signal<String>,
    reconnect_token: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
    workshop_code: String,
) -> bool {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::Resume,
        scoped_notice(NoticeScope::AccountHome, info_notice("Resuming workshop…")),
    ) else {
        return false;
    };
    spawn(submit_resume_workshop_flow_reserved(
        identity,
        game_state,
        ops,
        reconnect_session_code,
        reconnect_token,
        judge_bundle,
        workshop_code,
        ticket,
    ));
    true
}

#[allow(dead_code)]
pub async fn submit_join_with_character_flow(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    reconnect_session_code: Signal<String>,
    reconnect_token: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
    workshop_code: String,
    character_id: Option<String>,
) {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::Join,
        scoped_notice(NoticeScope::PickCharacter, info_notice("Joining workshop…")),
    ) else {
        return;
    };

    submit_join_with_character_flow_reserved(
        identity,
        game_state,
        ops,
        reconnect_session_code,
        reconnect_token,
        judge_bundle,
        workshop_code,
        character_id,
        ticket,
    )
    .await;
}

async fn submit_join_with_character_flow_reserved(
    mut identity: Signal<IdentityState>,
    mut game_state: Signal<Option<ClientGameState>>,
    mut ops: Signal<OperationState>,
    mut reconnect_session_code: Signal<String>,
    mut reconnect_token: Signal<String>,
    mut judge_bundle: Signal<Option<JudgeBundle>>,
    workshop_code: String,
    character_id: Option<String>,
    ticket: PendingFlowTicket,
) {
    let base_url = { identity.read().api_base_url.clone() };

    identity.with_mut(|id| {
        id.connection_status = ConnectionStatus::Connecting;
    });

    let api = AppWebApi::new(base_url);
    let request = JoinWorkshopRequest {
        session_code: workshop_code,
        name: None, // server gets name from cookie
        character_id,
        reconnect_token: None,
    };
    match api.join_workshop_with_character(&request).await {
        Ok(success) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            apply_join_and_bootstrap(
                &mut identity,
                &mut game_state,
                &mut ops,
                &mut reconnect_session_code,
                &mut reconnect_token,
                &mut judge_bundle,
                success,
                PendingFlow::Join,
                "Joined workshop, but session persistence failed",
                &ticket,
            );
        }
        Err(error) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    if !pending_flow_ticket_is_current(o, &ticket) {
                        return;
                    }
                    apply_request_error(id, o, error);
                });
            });
        }
    }
}

async fn submit_review_workshop_flow_reserved(
    mut identity: Signal<IdentityState>,
    mut game_state: Signal<Option<ClientGameState>>,
    mut ops: Signal<OperationState>,
    mut reconnect_session_code: Signal<String>,
    mut reconnect_token: Signal<String>,
    mut judge_bundle: Signal<Option<JudgeBundle>>,
    workshop_code: String,
    ticket: PendingFlowTicket,
) {
    let base_url = { identity.read().api_base_url.clone() };
    identity.with_mut(|id| {
        id.connection_status = ConnectionStatus::Connecting;
    });

    let api = AppWebApi::new(base_url);
    let request = JoinWorkshopRequest {
        session_code: workshop_code,
        name: None,
        character_id: None,
        reconnect_token: None,
    };
    match api.join_workshop_with_character(&request).await {
        Ok(success) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            let review_snapshot = build_client_session_snapshot(&success);
            apply_join_and_bootstrap(
                &mut identity,
                &mut game_state,
                &mut ops,
                &mut reconnect_session_code,
                &mut reconnect_token,
                &mut judge_bundle,
                success,
                PendingFlow::Review,
                "Opened review, but session persistence failed",
                &ticket,
            );
            if let Ok(bundle) = api
                .fetch_judge_bundle(build_judge_bundle_request(&review_snapshot))
                .await
            {
                if identity.read().session_snapshot.as_ref() == Some(&review_snapshot) {
                    judge_bundle.set(Some(bundle));
                }
            }
        }
        Err(error) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    if !pending_flow_ticket_is_current(o, &ticket) {
                        return;
                    }
                    apply_request_error(id, o, error);
                });
            });
        }
    }
}

async fn submit_resume_workshop_flow_reserved(
    mut identity: Signal<IdentityState>,
    mut game_state: Signal<Option<ClientGameState>>,
    mut ops: Signal<OperationState>,
    mut reconnect_session_code: Signal<String>,
    mut reconnect_token: Signal<String>,
    mut judge_bundle: Signal<Option<JudgeBundle>>,
    workshop_code: String,
    ticket: PendingFlowTicket,
) {
    let base_url = { identity.read().api_base_url.clone() };
    identity.with_mut(|id| {
        id.connection_status = ConnectionStatus::Connecting;
    });

    let api = AppWebApi::new(base_url);
    let request = JoinWorkshopRequest {
        session_code: workshop_code,
        name: None,
        character_id: None,
        reconnect_token: None,
    };
    match api.join_workshop_with_character(&request).await {
        Ok(success) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            apply_join_and_bootstrap(
                &mut identity,
                &mut game_state,
                &mut ops,
                &mut reconnect_session_code,
                &mut reconnect_token,
                &mut judge_bundle,
                success,
                PendingFlow::Resume,
                "Resumed workshop, but session persistence failed",
                &ticket,
            );
        }
        Err(error) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    if !pending_flow_ticket_is_current(o, &ticket) {
                        return;
                    }
                    apply_request_error(id, o, error);
                });
            });
        }
    }
}

pub async fn load_my_characters_flow(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
) {
    let notice_scope = notice_scope_for_screen(&identity.read().screen);
    let request_generation = ops.read().my_characters_request_generation;
    let base_url = { identity.read().api_base_url.clone() };
    let api = AppWebApi::new(base_url);
    match api.list_my_characters().await {
        Ok(response) => {
            if ops.read().my_characters_request_generation != request_generation {
                return;
            }
            ops.with_mut(|o| {
                o.my_characters_loading = false;
                o.my_characters_loaded = true;
                o.my_characters_load_failed = false;
                o.my_characters = response.characters;
                o.my_characters_limit = response.limit;
            });
        }
        Err(error) => {
            if ops.read().my_characters_request_generation != request_generation {
                return;
            }
            ops.with_mut(|o| {
                o.my_characters_loading = false;
                o.my_characters_load_failed = true;
                o.notice = Some(scoped_notice(
                    notice_scope,
                    error_notice(&format!("Failed to load characters: {error}")),
                ));
            });
        }
    }
}

pub fn begin_load_my_characters(ops: &mut OperationState) {
    ops.my_characters_request_generation = ops
        .my_characters_request_generation
        .checked_add(1)
        .unwrap_or(1)
        .max(1);
    ops.my_characters_loading = true;
    ops.my_characters_loaded = false;
    ops.my_characters_load_failed = false;
}

/// Paging direction for `load_open_workshops_flow`. Mirrors the server's
/// keyset semantics: `First` for the initial / polled page, `After` for
/// the "Next" (older) button, `Before` for the "Prev" (newer) button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenWorkshopsPaging {
    First,
    After(OpenWorkshopCursor),
    Before(OpenWorkshopCursor),
}

/// Load open workshops into `ops.open_workshops` and refresh the paging
/// cursors on the operation state. AccountHome reuses the caller-provided
/// paging direction for both explicit pager clicks and its 5-second poll so
/// the current page is preserved until the user changes it.
pub async fn load_open_workshops_flow(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    paging: OpenWorkshopsPaging,
) -> bool {
    let request_generation = ops.read().open_workshops_request_generation;
    let base_url = { identity.read().api_base_url.clone() };
    let api = AppWebApi::new(base_url);
    match api.list_open_workshops(&paging).await {
        Ok(response) => {
            if ops.read().open_workshops_request_generation != request_generation {
                return false;
            }
            ops.with_mut(|o| {
                o.open_workshops_loading = false;
                o.open_workshops_loaded = true;
                o.open_workshops_load_failed = false;
                let _ = apply_open_workshops_response_if_current(o, request_generation, response);
            });
            true
        }
        Err(error) => {
            if ops.read().open_workshops_request_generation != request_generation {
                return false;
            }
            ops.with_mut(|o| {
                o.open_workshops_loading = false;
                o.open_workshops_load_failed = true;
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    error_notice(&format!("Failed to load workshops: {error}")),
                ));
            });
            true
        }
    }
}

pub fn begin_load_open_workshops(ops: &mut OperationState) {
    ops.open_workshops_request_generation = ops
        .open_workshops_request_generation
        .checked_add(1)
        .unwrap_or(1)
        .max(1);
    ops.open_workshops_loading = true;
    ops.open_workshops_load_failed = false;
}

pub async fn request_open_workshops_flow(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    paging: OpenWorkshopsPaging,
) -> bool {
    ops.with_mut(begin_load_open_workshops);
    load_open_workshops_flow(identity, ops, paging).await
}

pub async fn request_open_workshops_flow_and_sync_paging(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    mut current_paging: Signal<OpenWorkshopsPaging>,
    paging: OpenWorkshopsPaging,
) -> bool {
    let requested_paging = paging.clone();
    let applied = request_open_workshops_flow(identity, ops, paging).await;
    if !applied {
        return false;
    }

    let next_paging = {
        let current = ops.read();
        normalize_open_workshops_paging_after_response(
            &requested_paging,
            current.open_workshops_prev_cursor.as_ref(),
        )
    };
    current_paging.set(next_paging.clone());
    if next_paging != requested_paging {
        return request_open_workshops_flow(identity, ops, next_paging).await;
    }

    true
}

pub async fn refresh_open_workshops_after_delete(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    paging: OpenWorkshopsPaging,
) -> Option<OpenWorkshopsPaging> {
    let applied = request_open_workshops_flow(identity, ops, paging.clone()).await;
    if !applied {
        return None;
    }

    let next_paging = {
        let current = ops.read();
        normalize_open_workshops_paging_after_response(
            &paging,
            current.open_workshops_prev_cursor.as_ref(),
        )
    };
    let should_fallback = {
        let current = ops.read();
        !matches!(next_paging, OpenWorkshopsPaging::First) && current.open_workshops.is_empty()
    };
    if should_fallback {
        if request_open_workshops_flow(identity, ops, OpenWorkshopsPaging::First).await {
            Some(OpenWorkshopsPaging::First)
        } else {
            None
        }
    } else {
        Some(next_paging)
    }
}

pub async fn refresh_open_workshops_after_workshop_update(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    current_paging: Signal<OpenWorkshopsPaging>,
    paging: OpenWorkshopsPaging,
) -> bool {
    request_open_workshops_flow_and_sync_paging(identity, ops, current_paging, paging).await
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn normalize_open_workshops_paging_after_response(
    requested_paging: &OpenWorkshopsPaging,
    prev_cursor: Option<&OpenWorkshopCursor>,
) -> OpenWorkshopsPaging {
    match requested_paging {
        OpenWorkshopsPaging::Before(_) if prev_cursor.is_none() => OpenWorkshopsPaging::First,
        _ => requested_paging.clone(),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn apply_open_workshops_response_if_current(
    ops: &mut OperationState,
    request_generation: u64,
    response: ListOpenWorkshopsResponse,
) -> bool {
    if ops.open_workshops_request_generation != request_generation {
        return false;
    }
    ops.open_workshops = response.workshops;
    ops.open_workshops_next_cursor = response.next_cursor;
    ops.open_workshops_prev_cursor = response.prev_cursor;
    true
}

#[cfg_attr(not(test), allow(dead_code))]
fn apply_eligible_characters_response_if_current(
    ops: &mut OperationState,
    request_generation: u64,
    workshop_code: &str,
    characters: Vec<protocol::CharacterProfile>,
) -> bool {
    if ops.eligible_characters_request_generation != request_generation
        || ops.eligible_characters_workshop_code.as_deref() != Some(workshop_code)
    {
        return false;
    }
    ops.eligible_characters = characters;
    true
}

pub fn begin_load_eligible_characters(ops: &mut OperationState, workshop_code: &str) {
    ops.eligible_characters_request_generation = ops
        .eligible_characters_request_generation
        .checked_add(1)
        .unwrap_or(1)
        .max(1);
    ops.eligible_characters_loading = true;
    ops.eligible_characters_loaded = false;
    ops.eligible_characters_load_failed = false;
    ops.eligible_characters_workshop_code = Some(workshop_code.to_string());
    ops.eligible_characters.clear();
}

/// Load eligible characters for a workshop into `ops.eligible_characters`.
pub async fn load_eligible_characters_flow(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    workshop_code: String,
) {
    let request_generation = ops.read().eligible_characters_request_generation;
    let base_url = { identity.read().api_base_url.clone() };
    let api = AppWebApi::new(base_url);
    match api.eligible_characters(&workshop_code).await {
        Ok(response) => {
            if ops.read().eligible_characters_request_generation != request_generation {
                return;
            }
            ops.with_mut(|o| {
                if apply_eligible_characters_response_if_current(
                    o,
                    request_generation,
                    &workshop_code,
                    response.characters,
                ) {
                    o.eligible_characters_loading = false;
                    o.eligible_characters_loaded = true;
                    o.eligible_characters_load_failed = false;
                }
            });
        }
        Err(error) => {
            if ops.read().eligible_characters_request_generation != request_generation {
                return;
            }
            ops.with_mut(|o| {
                if o.eligible_characters_workshop_code.as_deref() != Some(workshop_code.as_str()) {
                    return;
                }
                o.eligible_characters_loading = false;
                o.eligible_characters_load_failed = true;
                o.notice = Some(scoped_notice(
                    NoticeScope::PickCharacter,
                    error_notice(&format!("Failed to load eligible characters: {error}")),
                ));
            });
        }
    }
}

/// Create a character (standalone, account-scoped). On success, navigates back
/// to AccountHome.
///
/// Superseded by the inline flow in `components/create_character.rs`, which
/// performs a two-step preview-then-save interaction. Kept temporarily for
/// any remaining consumer; safe to delete in a follow-up pass.
#[allow(dead_code)]
pub async fn submit_create_character_flow(
    mut identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    description: String,
    sprites: SpriteSet,
) {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::Create,
        scoped_notice(
            NoticeScope::CreateCharacter,
            info_notice("Creating character…"),
        ),
    ) else {
        return;
    };

    let base_url = { identity.read().api_base_url.clone() };

    if description.trim().is_empty() {
        ops.with_mut(|o| {
            if !pending_flow_ticket_is_current(o, &ticket) {
                return;
            }
            clear_pending_flow_if_current(o, &ticket);
            o.notice = Some(scoped_notice(
                NoticeScope::CreateCharacter,
                error_notice("Enter a character description."),
            ))
        });
        return;
    }

    let api = AppWebApi::new(base_url);
    let request = protocol::CreateCharacterRequest {
        description: description.trim().to_string(),
        sprites,
    };
    match api.create_character(&request).await {
        Ok(_profile) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    if !pending_flow_ticket_is_current(o, &ticket) {
                        return;
                    }
                    navigate_to_screen(id, o, ShellScreen::AccountHome);
                });
            });
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                o.pending_flow = None;
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    success_notice("Character created."),
                ));
            });
        }
        Err(error) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                o.pending_flow = None;
                o.notice = Some(scoped_notice(
                    NoticeScope::CreateCharacter,
                    error_notice(&error),
                ));
            });
        }
    }
}

pub fn start_delete_character_flow(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    character_id: String,
) -> bool {
    let notice_scope = notice_scope_for_screen(&identity.read().screen);
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::DeleteCharacter,
        scoped_notice(notice_scope, info_notice("Deleting character…")),
    ) else {
        return false;
    };
    spawn(submit_delete_character_flow_reserved(
        identity,
        ops,
        character_id,
        ticket,
    ));
    true
}

#[allow(dead_code)]
pub async fn submit_delete_character_flow(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    character_id: String,
) {
    let notice_scope = notice_scope_for_screen(&identity.read().screen);
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::DeleteCharacter,
        scoped_notice(notice_scope.clone(), info_notice("Deleting character…")),
    ) else {
        return;
    };

    submit_delete_character_flow_reserved(identity, ops, character_id, ticket).await;
}

async fn submit_delete_character_flow_reserved(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    character_id: String,
    ticket: PendingFlowTicket,
) {
    let notice_scope = notice_scope_for_screen(&identity.read().screen);
    let base_url = { identity.read().api_base_url.clone() };
    let api = AppWebApi::new(base_url);
    match api.delete_character(&character_id).await {
        Ok(()) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                o.pending_flow = None;
                o.my_characters.retain(|c| c.id != character_id);
                o.eligible_characters.retain(|c| c.id != character_id);
                o.notice = Some(scoped_notice(
                    notice_scope,
                    success_notice("Character deleted."),
                ));
            });
        }
        Err(error) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                o.pending_flow = None;
                o.notice = Some(scoped_notice(
                    notice_scope,
                    error_notice(&format!("Failed to delete character: {error}")),
                ));
            });
        }
    }
}

pub fn start_rename_character_flow(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    character_id: String,
    name: String,
) -> bool {
    let notice_scope = notice_scope_for_screen(&identity.read().screen);
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::RenameCharacter,
        scoped_notice(notice_scope, info_notice("Renaming character…")),
    ) else {
        return false;
    };
    spawn(submit_rename_character_flow_reserved(
        identity,
        ops,
        character_id,
        name,
        ticket,
    ));
    true
}

#[allow(dead_code)]
pub async fn submit_rename_character_flow(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    character_id: String,
    name: String,
) {
    let notice_scope = notice_scope_for_screen(&identity.read().screen);
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::RenameCharacter,
        scoped_notice(notice_scope.clone(), info_notice("Renaming character…")),
    ) else {
        return;
    };

    submit_rename_character_flow_reserved(identity, ops, character_id, name, ticket).await;
}

async fn submit_rename_character_flow_reserved(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    character_id: String,
    name: String,
    ticket: PendingFlowTicket,
) {
    let notice_scope = notice_scope_for_screen(&identity.read().screen);
    let trimmed_name = name.trim().to_string();
    if trimmed_name.is_empty() {
        ops.with_mut(|o| {
            if !pending_flow_ticket_is_current(o, &ticket) {
                return;
            }
            clear_pending_flow_if_current(o, &ticket);
            o.notice = Some(scoped_notice(
                notice_scope,
                error_notice("Enter a dragon name."),
            ));
        });
        return;
    }

    let base_url = { identity.read().api_base_url.clone() };
    let api = AppWebApi::new(base_url);
    let request = UpdateCharacterRequest { name: trimmed_name };
    match api.update_character(&character_id, &request).await {
        Ok(updated) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                o.pending_flow = None;
                if let Some(character) = o.my_characters.iter_mut().find(|c| c.id == character_id) {
                    *character = updated.clone();
                }
                if let Some(character) = o
                    .eligible_characters
                    .iter_mut()
                    .find(|c| c.id == character_id)
                {
                    *character = updated;
                }
                o.notice = Some(scoped_notice(
                    notice_scope,
                    success_notice("Character renamed."),
                ));
            });
        }
        Err(error) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                o.pending_flow = None;
                o.notice = Some(scoped_notice(
                    notice_scope,
                    error_notice(&format!("Failed to rename character: {error}")),
                ));
            });
        }
    }
}

pub fn start_delete_workshop_flow(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    current_paging: Signal<OpenWorkshopsPaging>,
    session_code: String,
    paging: OpenWorkshopsPaging,
) -> bool {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::DeleteWorkshop,
        scoped_notice(NoticeScope::AccountHome, info_notice("Deleting workshop…")),
    ) else {
        return false;
    };
    spawn(submit_delete_workshop_flow_reserved(
        identity,
        ops,
        current_paging,
        session_code,
        paging,
        ticket,
    ));
    true
}

pub fn start_update_workshop_flow(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    current_paging: Signal<OpenWorkshopsPaging>,
    session_code: String,
    request: UpdateWorkshopRequest,
) -> bool {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::UpdateWorkshop,
        scoped_notice(
            NoticeScope::AccountHome,
            info_notice("Saving workshop settings…"),
        ),
    ) else {
        return false;
    };
    spawn(submit_update_workshop_flow_reserved(
        identity,
        ops,
        current_paging,
        session_code,
        request,
        ticket,
    ));
    true
}

#[allow(dead_code)]
pub async fn submit_update_workshop_flow(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    current_paging: Signal<OpenWorkshopsPaging>,
    session_code: String,
    request: UpdateWorkshopRequest,
) {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::UpdateWorkshop,
        scoped_notice(
            NoticeScope::AccountHome,
            info_notice("Saving workshop settings…"),
        ),
    ) else {
        return;
    };

    submit_update_workshop_flow_reserved(
        identity,
        ops,
        current_paging,
        session_code,
        request,
        ticket,
    )
    .await;
}

async fn submit_update_workshop_flow_reserved(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    current_paging: Signal<OpenWorkshopsPaging>,
    session_code: String,
    request: UpdateWorkshopRequest,
    ticket: PendingFlowTicket,
) {
    let base_url = { identity.read().api_base_url.clone() };
    let api = AppWebApi::new(base_url);
    match api.update_workshop(&session_code, &request).await {
        Ok(()) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            let paging = current_paging.read().clone();
            let refresh_applied =
                refresh_open_workshops_after_workshop_update(identity, ops, current_paging, paging)
                    .await;
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            let refresh_failed = ops.read().open_workshops_load_failed || !refresh_applied;
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                o.pending_flow = None;
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    if refresh_failed {
                        error_notice(&format!(
                            "Workshop {} updated. Workshop list refresh failed.",
                            session_code,
                        ))
                    } else {
                        success_notice(&format!("Workshop {} updated.", session_code))
                    },
                ));
            });
        }
        Err(error) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                o.pending_flow = None;
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    error_notice(&format!("Failed to update workshop: {error}")),
                ));
            });
        }
    }
}

#[allow(dead_code)]
pub async fn submit_delete_workshop_flow(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    current_paging: Signal<OpenWorkshopsPaging>,
    session_code: String,
    paging: OpenWorkshopsPaging,
) {
    let Some(ticket) = try_reserve_pending_flow(
        ops,
        PendingFlow::DeleteWorkshop,
        scoped_notice(NoticeScope::AccountHome, info_notice("Deleting workshop…")),
    ) else {
        return;
    };

    submit_delete_workshop_flow_reserved(
        identity,
        ops,
        current_paging,
        session_code,
        paging,
        ticket,
    )
    .await;
}

async fn submit_delete_workshop_flow_reserved(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    mut current_paging: Signal<OpenWorkshopsPaging>,
    session_code: String,
    paging: OpenWorkshopsPaging,
    ticket: PendingFlowTicket,
) {
    let base_url = { identity.read().api_base_url.clone() };
    let api = AppWebApi::new(base_url);
    match api.delete_workshop(&session_code).await {
        Ok(()) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            let requested_paging = paging.clone();
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    success_notice(&format!("Workshop {} deleted.", session_code)),
                ));
            });
            let next_paging = refresh_open_workshops_after_delete(identity, ops, paging).await;
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            let refresh_failed = ops.read().open_workshops_load_failed;
            if let Some(next_paging) = next_paging
                && *current_paging.read() == requested_paging
            {
                current_paging.set(next_paging);
            }
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                if refresh_failed {
                    o.notice = Some(scoped_notice(
                        NoticeScope::AccountHome,
                        error_notice(&format!(
                            "Workshop {} deleted. Workshop list refresh failed.",
                            session_code,
                        )),
                    ));
                }
                o.pending_flow = None;
            });
        }
        Err(error) => {
            if !pending_flow_ticket_is_current(&ops.read(), &ticket) {
                return;
            }
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, &ticket) {
                    return;
                }
                o.pending_flow = None;
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    error_notice(&format!("Failed to delete workshop: {error}")),
                ));
            });
        }
    }
}

/// Leave the current workshop session and return to AccountHome.
pub fn leave_workshop(mut identity: Signal<IdentityState>, mut ops: Signal<OperationState>) {
    disconnect_realtime();
    identity.with_mut(|id| {
        clear_session_identity(id);
    });
    ops.with_mut(|o| {
        clear_pre_session_caches(o);
        o.pending_flow = None;
        o.pending_command = None;
        o.pending_judge_bundle = false;
        o.notice = None;
    });
}

// ---------------------------------------------------------------------------
// Shared helper for join+bootstrap
// ---------------------------------------------------------------------------

fn apply_join_and_bootstrap(
    identity: &mut Signal<IdentityState>,
    game_state: &mut Signal<Option<ClientGameState>>,
    ops: &mut Signal<OperationState>,
    reconnect_session_code: &mut Signal<String>,
    reconnect_token: &mut Signal<String>,
    judge_bundle: &mut Signal<Option<JudgeBundle>>,
    success: protocol::WorkshopJoinSuccess,
    flow: PendingFlow,
    persistence_error_prefix: &str,
    ticket: &PendingFlowTicket,
) {
    identity.with_mut(|id| {
        game_state.with_mut(|gs| {
            ops.with_mut(|o| {
                if !pending_flow_ticket_is_current(o, ticket) {
                    return;
                }
                reconnect_session_code.with_mut(|reconnect_code| {
                    reconnect_token.with_mut(|token| {
                        judge_bundle.with_mut(|jb| {
                            apply_join_success(id, gs, o, reconnect_code, token, jb, success, flow);
                        });
                    });
                });
            });
        });
    });
    let persistence_warning = {
        let snapshot_warning = { identity.read().session_snapshot.clone() }.and_then(|snapshot| {
            persist_browser_session_snapshot(&snapshot)
                .err()
                .map(|error| format!("{persistence_error_prefix}: {error}"))
        });
        let game_state_warning = { game_state.read().clone() }.and_then(|state| {
            persist_browser_session_game_state(&state)
                .err()
                .map(|error| format!("{persistence_error_prefix}: {error}"))
        });
        snapshot_warning.or(game_state_warning)
    };
    if let Err(error) = bootstrap_realtime(*identity, *game_state, *ops, *judge_bundle) {
        identity.with_mut(|id| {
            game_state.with_mut(|gs| {
                ops.with_mut(|o| {
                    apply_realtime_bootstrap_error(id, gs, o, error);
                });
            });
        });
    } else if let Some(warning) = persistence_warning {
        ops.with_mut(|o| {
            o.pending_realtime_notice =
                Some(scoped_notice(NoticeScope::Session, error_notice(&warning)));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        ConnectionStatus, ShellScreen, default_identity_state, default_operation_state,
    };
    use protocol::{
        CharacterProfile, ClientGameState, CoordinatorType, ListOpenWorkshopsResponse,
        OpenWorkshopCursor, OpenWorkshopSummary, Phase, Player, SessionMeta, SpriteSet,
        WorkshopJoinResult, WorkshopJoinSuccess, create_default_session_settings,
    };
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn mock_join_success() -> WorkshopJoinSuccess {
        let mut players = BTreeMap::new();
        players.insert(
            "player-1".to_string(),
            Player {
                id: "player-1".to_string(),
                name: "Alice".to_string(),
                is_host: true,
                score: 0,
                current_dragon_id: None,
                achievements: Vec::new(),
                is_ready: false,
                is_connected: true,
                character_id: None,
                pet_description: Some("Alice's workshop dragon".to_string()),
                custom_sprites: None,
                remaining_sprite_regenerations: 1,
            },
        );

        WorkshopJoinSuccess {
            ok: true,
            session_code: "123456".to_string(),
            player_id: "player-1".to_string(),
            reconnect_token: "reconnect-1".to_string(),
            coordinator_type: CoordinatorType::Rust,
            state: ClientGameState {
                session: SessionMeta {
                    id: "session-1".to_string(),
                    code: "123456".to_string(),
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                    updated_at: "2026-01-01T00:00:00Z".to_string(),
                    state_revision: 0,
                    phase_started_at: "2026-01-01T00:00:00Z".to_string(),
                    host_player_id: Some("player-1".to_string()),
                    settings: create_default_session_settings(),
                },
                phase: Phase::Lobby,
                time: 8,
                players,
                dragons: BTreeMap::new(),
                current_player_id: Some("player-1".to_string()),
                voting: None,
            },
        }
    }

    fn spawn_join_success_server(success: WorkshopJoinSuccess) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("read test server address");

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept reconnect request");
            let mut buffer = [0_u8; 8192];
            let _ = stream.read(&mut buffer).expect("read reconnect request");

            let body = serde_json::to_string(&WorkshopJoinResult::Success(success))
                .expect("encode reconnect response");
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write reconnect response");
        });

        (format!("http://{address}"), handle)
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut buffer = [0_u8; 8192];
        let bytes_read = stream.read(&mut buffer).expect("read http request");
        String::from_utf8_lossy(&buffer[..bytes_read]).into_owned()
    }

    fn json_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn mock_character_profile(id: &str, name: &str) -> CharacterProfile {
        CharacterProfile {
            id: id.to_string(),
            name: Some(name.to_string()),
            description: format!("{name} description"),
            sprites: SpriteSet {
                neutral: "neutral".to_string(),
                happy: "happy".to_string(),
                angry: "angry".to_string(),
                sleepy: "sleepy".to_string(),
            },
            remaining_sprite_regenerations: 1,
            creator_account_id: None,
            creator_name: None,
        }
    }

    fn spawn_delete_workshop_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind delete test server");
        let address = listener
            .local_addr()
            .expect("read delete test server address");

        let handle = thread::spawn(move || {
            let (mut delete_stream, _) = listener.accept().expect("accept delete request");
            let delete_request = read_http_request(&mut delete_stream);
            assert!(
                delete_request.starts_with("DELETE /api/workshops/654321 HTTP/1.1"),
                "unexpected delete request: {delete_request}"
            );
            delete_stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .expect("write delete response");

            let (mut stale_page_stream, _) = listener.accept().expect("accept stale page request");
            let stale_page_request = read_http_request(&mut stale_page_stream);
            assert!(
                stale_page_request.starts_with(
                    "GET /api/workshops/open?after_created_at=2026-01-02T00%3A00%3A00Z&after_session_code=654321 HTTP/1.1"
                ),
                "unexpected stale page request: {stale_page_request}"
            );
            let empty_page = serde_json::to_string(&ListOpenWorkshopsResponse {
                workshops: Vec::new(),
                next_cursor: None,
                prev_cursor: None,
            })
            .expect("encode empty page");
            stale_page_stream
                .write_all(json_response(&empty_page).as_bytes())
                .expect("write stale page response");

            let (mut first_page_stream, _) = listener.accept().expect("accept first page request");
            let first_page_request = read_http_request(&mut first_page_stream);
            assert!(
                first_page_request.starts_with("GET /api/workshops/open HTTP/1.1"),
                "unexpected first page request: {first_page_request}"
            );
            let refreshed_page = serde_json::to_string(&ListOpenWorkshopsResponse {
                workshops: vec![OpenWorkshopSummary {
                    session_code: "123456".to_string(),
                    host_name: "Alice".to_string(),
                    player_count: 0,
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                    phase1_minutes: 8,
                    phase2_minutes: 5,
                    archived: false,
                    can_delete: true,
                    can_resume: false,
                }],
                next_cursor: None,
                prev_cursor: None,
            })
            .expect("encode refreshed page");
            first_page_stream
                .write_all(json_response(&refreshed_page).as_bytes())
                .expect("write refreshed page response");
        });

        (format!("http://{address}"), handle)
    }

    fn spawn_create_workshop_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind create test server");
        let address = listener
            .local_addr()
            .expect("read create test server address");

        let handle = thread::spawn(move || {
            let (mut create_stream, _) = listener.accept().expect("accept create request");
            let create_request = read_http_request(&mut create_stream);
            assert!(
                create_request.starts_with("POST /api/workshops/lobby HTTP/1.1"),
                "unexpected create request: {create_request}"
            );
            assert!(
                !create_request.contains("phase1Minutes"),
                "default create request should not force custom phase settings: {create_request}"
            );
            let create_body = serde_json::to_string(&protocol::WorkshopCreateResult::Success(
                protocol::WorkshopCreateSuccess {
                    ok: true,
                    session_code: "222222".to_string(),
                    host_name: "Alice".to_string(),
                },
            ))
            .expect("encode create response");
            create_stream
                .write_all(json_response(&create_body).as_bytes())
                .expect("write create response");

            let (mut first_page_stream, _) = listener.accept().expect("accept first page request");
            let first_page_request = read_http_request(&mut first_page_stream);
            assert!(
                first_page_request.starts_with("GET /api/workshops/open HTTP/1.1"),
                "unexpected first page request: {first_page_request}"
            );
            let refreshed_page = serde_json::to_string(&ListOpenWorkshopsResponse {
                workshops: vec![OpenWorkshopSummary {
                    session_code: "222222".to_string(),
                    host_name: "Alice".to_string(),
                    player_count: 0,
                    created_at: "2026-01-03T00:00:00Z".to_string(),
                    phase1_minutes: 8,
                    phase2_minutes: 5,
                    archived: false,
                    can_delete: true,
                    can_resume: false,
                }],
                next_cursor: None,
                prev_cursor: None,
            })
            .expect("encode refreshed first page");
            first_page_stream
                .write_all(json_response(&refreshed_page).as_bytes())
                .expect("write refreshed first page");
        });

        (format!("http://{address}"), handle)
    }

    fn spawn_update_workshop_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind update test server");
        let address = listener
            .local_addr()
            .expect("read update test server address");

        let handle = thread::spawn(move || {
            let (mut update_stream, _) = listener.accept().expect("accept update request");
            let update_request = read_http_request(&mut update_stream);
            assert!(
                update_request.starts_with("PATCH /api/workshops/654321 HTTP/1.1"),
                "unexpected update request: {update_request}"
            );
            assert!(
                update_request.contains(r#""phase1Minutes":2"#),
                "expected phase1Minutes in update request: {update_request}"
            );
            assert!(
                update_request.contains(r#""phase2Minutes":3"#),
                "expected phase2Minutes in update request: {update_request}"
            );
            update_stream
                .write_all(json_response(r#"{"ok":true}"#).as_bytes())
                .expect("write update response");

            let (mut page_stream, _) = listener.accept().expect("accept refreshed page request");
            let page_request = read_http_request(&mut page_stream);
            assert!(
                page_request.starts_with(
                    "GET /api/workshops/open?after_created_at=2026-01-02T00%3A00%3A00Z&after_session_code=654321 HTTP/1.1"
                ),
                "unexpected refreshed page request: {page_request}"
            );
            let refreshed_page = serde_json::to_string(&ListOpenWorkshopsResponse {
                workshops: vec![OpenWorkshopSummary {
                    session_code: "654321".to_string(),
                    host_name: "Alice".to_string(),
                    player_count: 0,
                    created_at: "2026-01-02T00:00:00Z".to_string(),
                    phase1_minutes: 2,
                    phase2_minutes: 3,
                    archived: false,
                    can_delete: true,
                    can_resume: false,
                }],
                next_cursor: None,
                prev_cursor: None,
            })
            .expect("encode refreshed page after update");
            page_stream
                .write_all(json_response(&refreshed_page).as_bytes())
                .expect("write refreshed page after update");
        });

        (format!("http://{address}"), handle)
    }

    fn spawn_before_page_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind before-page test server");
        let address = listener
            .local_addr()
            .expect("read before-page test server address");

        let handle = thread::spawn(move || {
            let (mut page_stream, _) = listener.accept().expect("accept before page request");
            let page_request = read_http_request(&mut page_stream);
            assert!(
                page_request.starts_with(
                    "GET /api/workshops/open?before_created_at=2026-01-02T00%3A00%3A00Z&before_session_code=654321 HTTP/1.1"
                ),
                "unexpected before page request: {page_request}"
            );
            let refreshed_page = serde_json::to_string(&ListOpenWorkshopsResponse {
                workshops: vec![OpenWorkshopSummary {
                    session_code: "123456".to_string(),
                    host_name: "Alice".to_string(),
                    player_count: 0,
                    created_at: "2026-01-03T00:00:00Z".to_string(),
                    phase1_minutes: 8,
                    phase2_minutes: 5,
                    archived: false,
                    can_delete: true,
                    can_resume: false,
                }],
                next_cursor: Some(OpenWorkshopCursor {
                    created_at: "2026-01-03T00:00:00Z".to_string(),
                    session_code: "123456".to_string(),
                }),
                prev_cursor: None,
            })
            .expect("encode before page response");
            page_stream
                .write_all(json_response(&refreshed_page).as_bytes())
                .expect("write before page response");

            let (mut first_stream, _) = listener.accept().expect("accept canonical first request");
            let first_request = read_http_request(&mut first_stream);
            assert!(
                first_request.starts_with("GET /api/workshops/open HTTP/1.1"),
                "unexpected canonical first request: {first_request}"
            );
            first_stream
                .write_all(json_response(&refreshed_page).as_bytes())
                .expect("write canonical first response");
        });

        (format!("http://{address}"), handle)
    }

    fn spawn_shrunken_before_page_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind shrunken-page test server");
        let address = listener
            .local_addr()
            .expect("read shrunken-page test server address");

        let handle = thread::spawn(move || {
            let (mut before_stream, _) = listener.accept().expect("accept before page request");
            let before_request = read_http_request(&mut before_stream);
            assert!(
                before_request.starts_with(
                    "GET /api/workshops/open?before_created_at=2026-01-02T00%3A00%3A00Z&before_session_code=654321 HTTP/1.1"
                ),
                "unexpected before page request: {before_request}"
            );
            let shrunken_page = serde_json::to_string(&ListOpenWorkshopsResponse {
                workshops: vec![
                    mock_open_workshop("111111", "2026-01-05T00:00:00Z"),
                    mock_open_workshop("222222", "2026-01-04T00:00:00Z"),
                    mock_open_workshop("333333", "2026-01-03T00:00:00Z"),
                ],
                next_cursor: Some(OpenWorkshopCursor {
                    created_at: "2026-01-03T00:00:00Z".to_string(),
                    session_code: "333333".to_string(),
                }),
                prev_cursor: None,
            })
            .expect("encode shrunken before page");
            before_stream
                .write_all(json_response(&shrunken_page).as_bytes())
                .expect("write shrunken before page");

            let (mut first_stream, _) = listener.accept().expect("accept canonical first request");
            let first_request = read_http_request(&mut first_stream);
            assert!(
                first_request.starts_with("GET /api/workshops/open HTTP/1.1"),
                "unexpected canonical first request: {first_request}"
            );
            let first_page = serde_json::to_string(&ListOpenWorkshopsResponse {
                workshops: vec![
                    mock_open_workshop("111111", "2026-01-05T00:00:00Z"),
                    mock_open_workshop("222222", "2026-01-04T00:00:00Z"),
                    mock_open_workshop("333333", "2026-01-03T00:00:00Z"),
                    mock_open_workshop("444444", "2026-01-02T00:00:00Z"),
                ],
                next_cursor: Some(OpenWorkshopCursor {
                    created_at: "2026-01-02T00:00:00Z".to_string(),
                    session_code: "444444".to_string(),
                }),
                prev_cursor: None,
            })
            .expect("encode canonical first page");
            first_stream
                .write_all(json_response(&first_page).as_bytes())
                .expect("write canonical first page");
        });

        (format!("http://{address}"), handle)
    }

    fn mock_open_workshop(session_code: &str, created_at: &str) -> OpenWorkshopSummary {
        OpenWorkshopSummary {
            session_code: session_code.to_string(),
            host_name: "Alice".to_string(),
            player_count: 0,
            created_at: created_at.to_string(),
            phase1_minutes: 8,
            phase2_minutes: 5,
            archived: false,
            can_delete: true,
            can_resume: false,
        }
    }

    fn spawn_archive_from_voting_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind archive test server");
        let address = listener
            .local_addr()
            .expect("read archive test server address");

        let handle = thread::spawn(move || {
            let (mut end_stream, _) = listener.accept().expect("accept end session request");
            let end_request = read_http_request(&mut end_stream);
            assert!(
                end_request.starts_with("POST /api/workshops/command HTTP/1.1"),
                "unexpected end request: {end_request}"
            );
            assert!(
                end_request.contains(r#""command":"endSession""#),
                "expected endSession command: {end_request}"
            );
            let command_body = serde_json::to_string(&protocol::WorkshopCommandResult::Success(
                protocol::WorkshopCommandSuccess { ok: true },
            ))
            .expect("encode command response");
            end_stream
                .write_all(json_response(&command_body).as_bytes())
                .expect("write end session response");

            let (mut archive_stream, _) = listener.accept().expect("accept archive request");
            let archive_request = read_http_request(&mut archive_stream);
            assert!(
                archive_request.starts_with("POST /api/workshops/judge-bundle HTTP/1.1"),
                "unexpected archive request: {archive_request}"
            );
            let archive_body =
                serde_json::to_string(&protocol::WorkshopJudgeBundleResult::Success(
                    protocol::WorkshopJudgeBundleSuccess {
                        ok: true,
                        bundle: crate::helpers::tests::mock_judge_bundle(),
                    },
                ))
                .expect("encode archive response");
            archive_stream
                .write_all(json_response(&archive_body).as_bytes())
                .expect("write archive response");
        });

        (format!("http://{address}"), handle)
    }

    #[test]
    fn reconnect_success_bootstraps_realtime() {
        let (base_url, server) = spawn_join_success_server(mock_join_success());

        let mut dom = VirtualDom::new(|| rsx! { div {} });
        dom.rebuild_in_place();

        dom.in_scope(ScopeId::ROOT, || {
            let runtime = tokio::runtime::Runtime::new().expect("create tokio runtime");

            let mut initial_identity = default_identity_state();
            initial_identity.api_base_url = base_url;

            let identity = Signal::new(initial_identity);
            let game_state = Signal::new(None);
            let ops = Signal::new(default_operation_state());
            let reconnect_session_code = Signal::new("123456".to_string());
            let reconnect_token = Signal::new("reconnect-1".to_string());
            let judge_bundle = Signal::new(None);

            runtime.block_on(submit_reconnect_flow(
                identity,
                game_state,
                ops,
                reconnect_session_code,
                reconnect_token,
                judge_bundle,
            ));

            server.join().expect("join reconnect server thread");

            assert_eq!(identity.read().screen, ShellScreen::Session);
            assert_eq!(
                identity.read().connection_status,
                ConnectionStatus::Connected
            );
            assert!(identity.read().realtime_bootstrap_attempted);
            assert_eq!(
                identity
                    .read()
                    .session_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.session_code.as_str()),
                Some("123456")
            );
            assert!(game_state.read().is_some());
            assert_eq!(ops.read().pending_flow, None);
            assert_eq!(
                ops.read()
                    .notice
                    .as_ref()
                    .map(|notice| notice.message.as_str()),
                Some("Reconnected to workshop.")
            );
        });
    }

    // Compile-only guard for retained speculative flows (no runtime consumer).
    // Forces signature monomorphization so future API drift surfaces here.
    #[test]
    fn retained_flows_remain_linkable() {
        let _ = &load_my_characters_flow;
        let _ = &submit_delete_character_flow;
    }

    #[test]
    fn create_workshop_flow_stays_account_scoped() {
        let _f: fn(
            Signal<IdentityState>,
            Signal<OperationState>,
            Signal<OpenWorkshopsPaging>,
            Option<protocol::WorkshopCreateConfig>,
        ) -> _ = submit_create_workshop_flow;
    }

    #[test]
    fn create_workshop_flow_refreshes_first_page_and_updates_current_paging() {
        let (base_url, server) = spawn_create_workshop_server();

        let mut dom = VirtualDom::new(|| rsx! { div {} });
        dom.rebuild_in_place();

        dom.in_scope(ScopeId::ROOT, || {
            let runtime = tokio::runtime::Runtime::new().expect("create tokio runtime");

            let mut initial_identity = default_identity_state();
            initial_identity.api_base_url = base_url;
            initial_identity.account = Some(protocol::AccountProfile {
                id: "account-1".to_string(),
                hero: "knight".to_string(),
                name: "Alice".to_string(),
            });
            initial_identity.screen = ShellScreen::AccountHome;

            let identity = Signal::new(initial_identity);
            let ops = Signal::new(default_operation_state());
            let current_paging = Signal::new(OpenWorkshopsPaging::After(OpenWorkshopCursor {
                created_at: "2026-01-02T00:00:00Z".to_string(),
                session_code: "654321".to_string(),
            }));

            runtime.block_on(submit_create_workshop_flow(
                identity,
                ops,
                current_paging,
                None,
            ));

            server.join().expect("join create workshop server thread");

            assert_eq!(ops.read().pending_flow, None);
            assert_eq!(ops.read().open_workshops.len(), 1);
            assert_eq!(ops.read().open_workshops[0].session_code, "222222");
            assert_eq!(*current_paging.read(), OpenWorkshopsPaging::First);
            assert_eq!(identity.read().screen, ShellScreen::AccountHome);
            assert_eq!(identity.read().connection_status, ConnectionStatus::Offline);
        });
    }

    #[test]
    fn update_workshop_flow_refreshes_current_page() {
        let (base_url, server) = spawn_update_workshop_server();

        let mut dom = VirtualDom::new(|| rsx! { div {} });
        dom.rebuild_in_place();

        dom.in_scope(ScopeId::ROOT, || {
            let runtime = tokio::runtime::Runtime::new().expect("create tokio runtime");

            let mut initial_identity = default_identity_state();
            initial_identity.api_base_url = base_url;
            initial_identity.screen = ShellScreen::AccountHome;

            let identity = Signal::new(initial_identity);
            let ops = Signal::new(default_operation_state());
            let current_paging = Signal::new(OpenWorkshopsPaging::After(OpenWorkshopCursor {
                created_at: "2026-01-02T00:00:00Z".to_string(),
                session_code: "654321".to_string(),
            }));

            runtime.block_on(submit_update_workshop_flow(
                identity,
                ops,
                current_paging,
                "654321".to_string(),
                UpdateWorkshopRequest {
                    phase1_minutes: Some(2),
                    phase2_minutes: Some(3),
                },
            ));

            server.join().expect("join update workshop server thread");

            assert_eq!(ops.read().pending_flow, None);
            assert_eq!(ops.read().open_workshops.len(), 1);
            assert_eq!(ops.read().open_workshops[0].session_code, "654321");
            assert_eq!(ops.read().open_workshops[0].phase1_minutes, 2);
            assert_eq!(ops.read().open_workshops[0].phase2_minutes, 3);
            assert_eq!(
                ops.read()
                    .notice
                    .as_ref()
                    .map(|notice| notice.message.as_str()),
                Some("Workshop 654321 updated.")
            );
            assert_eq!(
                *current_paging.read(),
                OpenWorkshopsPaging::After(OpenWorkshopCursor {
                    created_at: "2026-01-02T00:00:00Z".to_string(),
                    session_code: "654321".to_string(),
                })
            );
        });
    }

    #[test]
    fn judge_bundle_request_finalizes_voting_before_archiving() {
        let (base_url, server) = spawn_archive_from_voting_server();

        let mut dom = VirtualDom::new(|| rsx! { div {} });
        dom.rebuild_in_place();

        dom.in_scope(ScopeId::ROOT, || {
            let runtime = tokio::runtime::Runtime::new().expect("create tokio runtime");

            let mut initial_identity = default_identity_state();
            initial_identity.api_base_url = base_url;
            initial_identity.session_snapshot = Some(protocol::ClientSessionSnapshot {
                session_code: "123456".to_string(),
                reconnect_token: "reconnect-1".to_string(),
                player_id: "player-1".to_string(),
                coordinator_type: CoordinatorType::Rust,
            });

            let mut state = mock_join_success().state;
            state.phase = Phase::Voting;
            state.voting = Some(protocol::ClientVotingState {
                eligible_count: 2,
                submitted_count: 2,
                current_player_vote_dragon_id: Some("dragon-2".to_string()),
                results_revealed: true,
                results: None,
            });

            let identity = Signal::new(initial_identity);
            let game_state = Signal::new(Some(state));
            let ops = Signal::new(default_operation_state());
            let judge_bundle = Signal::new(None);

            runtime.block_on(submit_judge_bundle_request(
                identity,
                game_state,
                ops,
                judge_bundle,
            ));

            server.join().expect("join archive server thread");

            assert!(!ops.read().pending_judge_bundle);
            assert!(judge_bundle.read().is_some());
            assert_eq!(
                ops.read()
                    .notice
                    .as_ref()
                    .map(|notice| notice.message.as_str()),
                Some("Workshop archive ready.")
            );
        });
    }

    #[test]
    fn stale_open_workshops_response_is_ignored() {
        let mut ops = default_operation_state();
        ops.open_workshops_request_generation = 2;
        ops.open_workshops = vec![OpenWorkshopSummary {
            session_code: "current".to_string(),
            host_name: "Alice".to_string(),
            player_count: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            phase1_minutes: 8,
            phase2_minutes: 5,
            archived: false,
            can_delete: true,
            can_resume: false,
        }];

        let applied = apply_open_workshops_response_if_current(
            &mut ops,
            1,
            ListOpenWorkshopsResponse {
                workshops: vec![OpenWorkshopSummary {
                    session_code: "stale".to_string(),
                    host_name: "Bob".to_string(),
                    player_count: 2,
                    created_at: "2026-01-02T00:00:00Z".to_string(),
                    phase1_minutes: 8,
                    phase2_minutes: 5,
                    archived: false,
                    can_delete: false,
                    can_resume: false,
                }],
                next_cursor: None,
                prev_cursor: None,
            },
        );

        assert!(!applied);
        assert_eq!(ops.open_workshops.len(), 1);
        assert_eq!(ops.open_workshops[0].session_code, "current");
    }

    #[test]
    fn stale_eligible_characters_response_is_ignored() {
        let mut ops = default_operation_state();
        ops.eligible_characters_request_generation = 2;
        ops.eligible_characters = vec![mock_character_profile("current", "Current Dragon")];

        let applied = apply_eligible_characters_response_if_current(
            &mut ops,
            1,
            "W1",
            vec![mock_character_profile("stale", "Stale Dragon")],
        );

        assert!(!applied);
        assert_eq!(ops.eligible_characters.len(), 1);
        assert_eq!(ops.eligible_characters[0].id, "current");
    }

    #[test]
    fn before_page_without_prev_cursor_normalizes_to_first() {
        let (base_url, server) = spawn_before_page_server();

        let mut dom = VirtualDom::new(|| rsx! { div {} });
        dom.rebuild_in_place();

        dom.in_scope(ScopeId::ROOT, || {
            let runtime = tokio::runtime::Runtime::new().expect("create tokio runtime");

            let mut initial_identity = default_identity_state();
            initial_identity.api_base_url = base_url;
            initial_identity.screen = ShellScreen::AccountHome;

            let identity = Signal::new(initial_identity);
            let ops = Signal::new(default_operation_state());
            let current_paging = Signal::new(OpenWorkshopsPaging::Before(OpenWorkshopCursor {
                created_at: "2026-01-02T00:00:00Z".to_string(),
                session_code: "654321".to_string(),
            }));

            runtime.block_on(async move {
                let requested_paging = current_paging.read().clone();
                let applied = request_open_workshops_flow_and_sync_paging(
                    identity,
                    ops,
                    current_paging,
                    requested_paging,
                )
                .await;
                assert!(applied, "paging request should apply");
            });

            server.join().expect("join before-page server thread");

            assert_eq!(ops.read().open_workshops.len(), 1);
            assert_eq!(ops.read().open_workshops[0].session_code, "123456");
            assert_eq!(*current_paging.read(), OpenWorkshopsPaging::First);
        });
    }

    #[test]
    fn before_page_without_prev_cursor_reloads_canonical_first_page() {
        let (base_url, server) = spawn_shrunken_before_page_server();

        let mut dom = VirtualDom::new(|| rsx! { div {} });
        dom.rebuild_in_place();

        dom.in_scope(ScopeId::ROOT, || {
            let runtime = tokio::runtime::Runtime::new().expect("create tokio runtime");

            let mut initial_identity = default_identity_state();
            initial_identity.api_base_url = base_url;
            initial_identity.screen = ShellScreen::AccountHome;

            let identity = Signal::new(initial_identity);
            let ops = Signal::new(default_operation_state());
            let current_paging = Signal::new(OpenWorkshopsPaging::Before(OpenWorkshopCursor {
                created_at: "2026-01-02T00:00:00Z".to_string(),
                session_code: "654321".to_string(),
            }));

            runtime.block_on(async move {
                let requested_paging = current_paging.read().clone();
                let applied = request_open_workshops_flow_and_sync_paging(
                    identity,
                    ops,
                    current_paging,
                    requested_paging,
                )
                .await;
                assert!(applied, "paging request should apply");
            });

            server.join().expect("join shrunken-page server thread");

            let codes = ops
                .read()
                .open_workshops
                .iter()
                .map(|workshop| workshop.session_code.clone())
                .collect::<Vec<_>>();
            assert_eq!(codes, ["111111", "222222", "333333", "444444"]);
            assert_eq!(*current_paging.read(), OpenWorkshopsPaging::First);
        });
    }

    #[test]
    fn delete_workshop_flow_falls_back_to_first_page_after_empty_non_first_reload() {
        let (base_url, server) = spawn_delete_workshop_server();

        let mut dom = VirtualDom::new(|| rsx! { div {} });
        dom.rebuild_in_place();

        dom.in_scope(ScopeId::ROOT, || {
            let runtime = tokio::runtime::Runtime::new().expect("create tokio runtime");

            let mut initial_identity = default_identity_state();
            initial_identity.api_base_url = base_url;

            let identity = Signal::new(initial_identity);
            let ops = Signal::new(default_operation_state());
            let current_paging = Signal::new(OpenWorkshopsPaging::After(OpenWorkshopCursor {
                created_at: "2026-01-02T00:00:00Z".to_string(),
                session_code: "654321".to_string(),
            }));

            runtime.block_on(submit_delete_workshop_flow(
                identity,
                ops,
                current_paging,
                "654321".to_string(),
                OpenWorkshopsPaging::After(OpenWorkshopCursor {
                    created_at: "2026-01-02T00:00:00Z".to_string(),
                    session_code: "654321".to_string(),
                }),
            ));

            server.join().expect("join delete workshop server thread");

            assert_eq!(ops.read().pending_flow, None);
            assert_eq!(ops.read().open_workshops.len(), 1);
            assert_eq!(ops.read().open_workshops[0].session_code, "123456");
            assert_eq!(*current_paging.read(), OpenWorkshopsPaging::First);
            assert_eq!(
                ops.read()
                    .notice
                    .as_ref()
                    .map(|notice| notice.message.as_str()),
                Some("Workshop 654321 deleted.")
            );
        });
    }
}
