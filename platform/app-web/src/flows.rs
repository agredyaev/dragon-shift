#![allow(clippy::too_many_arguments)]

use dioxus::prelude::*;
use protocol::{
    AuthRequest, ClientGameState, JoinWorkshopRequest, JudgeBundle, OpenWorkshopCursor,
    SessionCommand, SpriteSet,
};

use crate::api::{AppWebApi, build_command_request, build_judge_bundle_request};
use crate::helpers::{parse_tags_input, pending_command_label};
use crate::realtime::bootstrap_realtime;
use crate::state::{
    ConnectionStatus, IdentityState, NoticeScope, OperationState, PendingFlow, ShellScreen,
    apply_command_error, apply_join_success, apply_judge_bundle_error, apply_judge_bundle_success,
    apply_realtime_bootstrap_error, apply_request_error, apply_successful_command,
    clear_account_identity, clear_session_identity, error_notice, info_notice, navigate_to_screen,
    persist_browser_account_snapshot, persist_browser_session_snapshot, scoped_notice,
    success_notice,
};

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

    identity.with_mut(|id| {
        id.connection_status = ConnectionStatus::Connecting;
    });
    ops.with_mut(|o| {
        o.pending_flow = Some(PendingFlow::Reconnect);
        o.notice = Some(scoped_notice(
            NoticeScope::Session,
            info_notice("Reconnecting…"),
        ));
    });

    let api = AppWebApi::new(base_url);
    match api.reconnect_workshop(session_code, reconnect_token).await {
        Ok(success) => {
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
            let persisted_snapshot = { identity.read().session_snapshot.clone() };
            if let Some(snapshot) = persisted_snapshot
                && let Err(error) = persist_browser_session_snapshot(&snapshot)
            {
                ops.with_mut(|o| {
                    o.notice = Some(scoped_notice(
                        NoticeScope::Session,
                        error_notice(&format!(
                            "Reconnected, but session persistence failed: {error}"
                        )),
                    ))
                });
            }
            if let Err(error) = bootstrap_realtime(identity, game_state, ops, judge_bundle) {
                identity.with_mut(|id| {
                    ops.with_mut(|o| {
                        apply_realtime_bootstrap_error(id, o, error);
                    });
                });
            }
        }
        Err(error) => {
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    apply_request_error(id, o, error);
                });
            });
        }
    }
}

pub async fn submit_workshop_command(
    mut identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    mut handover_tags_input: Signal<String>,
    mut judge_bundle: Signal<Option<JudgeBundle>>,
    command: SessionCommand,
    payload: Option<serde_json::Value>,
) {
    let (base_url, snapshot) = {
        let id = identity.read();
        (id.api_base_url.clone(), id.session_snapshot.clone())
    };

    let Some(snapshot) = snapshot else {
        ops.with_mut(|o| {
            o.notice = Some(scoped_notice(
                NoticeScope::Session,
                error_notice("Connect to a workshop before sending commands."),
            ))
        });
        return;
    };

    ops.with_mut(|o| {
        o.pending_command = Some(command);
        o.notice = Some(scoped_notice(
            NoticeScope::Session,
            info_notice(pending_command_label(command)),
        ));
    });

    let api = AppWebApi::new(base_url);
    match api
        .send_command(build_command_request(&snapshot, command, payload))
        .await
    {
        Ok(()) => {
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    handover_tags_input.with_mut(|tags| {
                        judge_bundle.with_mut(|jb| {
                            apply_successful_command(id, o, tags, jb, command);
                        });
                    });
                });
            });
        }
        Err(error) => {
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    apply_command_error(id, o, error);
                });
            });
        }
    }
}

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
pub async fn submit_judge_bundle_request(
    mut identity: Signal<IdentityState>,
    _game_state: Signal<Option<ClientGameState>>,
    mut ops: Signal<OperationState>,
    mut judge_bundle: Signal<Option<JudgeBundle>>,
) {
    let (base_url, snapshot) = {
        let id = identity.read();
        (id.api_base_url.clone(), id.session_snapshot.clone())
    };

    let Some(snapshot) = snapshot else {
        ops.with_mut(|o| {
            o.notice = Some(scoped_notice(
                NoticeScope::Session,
                error_notice("Connect to a workshop before building the archive."),
            ))
        });
        return;
    };

    ops.with_mut(|o| {
        o.pending_judge_bundle = true;
        o.notice = Some(scoped_notice(
            NoticeScope::Session,
            info_notice("Building workshop archive…"),
        ));
    });

    let api = AppWebApi::new(base_url);
    match api
        .fetch_judge_bundle(build_judge_bundle_request(&snapshot))
        .await
    {
        Ok(bundle) => {
            ops.with_mut(|o| {
                judge_bundle.with_mut(|jb| {
                    apply_judge_bundle_success(o, jb, bundle);
                });
            });
        }
        Err(error) => {
            identity.with_mut(|id| {
                ops.with_mut(|o| {
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
pub async fn submit_signin_flow(
    mut identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    name: String,
    password: String,
    hero: String,
) {
    let base_url = { identity.read().api_base_url.clone() };

    if name.trim().is_empty() || password.is_empty() {
        ops.with_mut(|o| {
            o.notice = Some(scoped_notice(
                NoticeScope::SignIn,
                error_notice("Name and password are required."),
            ))
        });
        return;
    }

    ops.with_mut(|o| {
        o.pending_flow = Some(PendingFlow::SignIn);
        o.notice = Some(scoped_notice(
            NoticeScope::SignIn,
            info_notice("Signing in…"),
        ));
    });

    let api = AppWebApi::new(base_url);
    let request = AuthRequest {
        hero: hero.clone(),
        name: name.trim().to_string(),
        password,
    };
    match api.signin(&request).await {
        Ok(response) => {
            if let Err(error) = persist_browser_account_snapshot(&response.account) {
                ops.with_mut(|o| {
                    o.notice = Some(scoped_notice(
                        NoticeScope::AccountHome,
                        error_notice(&format!("Signed in, but local persistence failed: {error}")),
                    ));
                });
            }
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    id.account = Some(response.account);
                    navigate_to_screen(id, o, ShellScreen::AccountHome);
                });
            });
            ops.with_mut(|o| {
                o.pending_flow = None;
                let msg = if response.created {
                    "Account created."
                } else {
                    "Signed in."
                };
                o.notice = Some(scoped_notice(NoticeScope::AccountHome, success_notice(msg)));
            });
        }
        Err(error) => {
            // Map structured backend error codes (e.g.
            // `name_taken_wrong_password`) to the spec copy rendered in the
            // SignIn NoticeBar. See `components/sign_in.rs::map_signin_error`.
            let message = crate::components::sign_in::map_signin_error(&error);
            ops.with_mut(|o| {
                o.pending_flow = None;
                o.notice = Some(scoped_notice(NoticeScope::SignIn, error_notice(&message)));
            });
        }
    }
}

/// Logout: clears cookie on server, clears localStorage, navigates to SignIn.
pub async fn submit_logout_flow(
    mut identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
) {
    let base_url = { identity.read().api_base_url.clone() };
    let api = AppWebApi::new(base_url);

    // Best-effort server logout; even if it fails we clear local state.
    let _ = api.logout().await;

    identity.with_mut(|id| {
        clear_account_identity(id);
    });
    ops.with_mut(|o| {
        o.pending_flow = None;
        o.notice = None;
    });
}

/// Create an empty workshop lobby. The creator remains on AccountHome and must
/// explicitly join later from the open-workshops list.
pub async fn submit_create_workshop_flow(
    mut identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
) {
    let base_url = { identity.read().api_base_url.clone() };

    ops.with_mut(|o| {
        o.pending_flow = Some(PendingFlow::Create);
        o.notice = Some(scoped_notice(
            NoticeScope::AccountHome,
            info_notice("Creating workshop…"),
        ));
    });

    let api = AppWebApi::new(base_url);
    match api.create_workshop_lobby().await {
        Ok(success) => {
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    id.connection_status = ConnectionStatus::Offline;
                    navigate_to_screen(id, o, ShellScreen::AccountHome);
                });
            });
            ops.with_mut(|o| {
                o.pending_flow = None;
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    success_notice(&format!("Workshop {} created.", success.session_code)),
                ));
            });
            load_open_workshops_flow(identity, ops, OpenWorkshopsPaging::First).await;
        }
        Err(error) => {
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    apply_request_error(id, o, error);
                });
            });
        }
    }
}

/// Join a workshop with an optional character. Called from PickCharacter screen.
/// `character_id = None` means the server leases a random starter.
pub async fn submit_join_with_character_flow(
    mut identity: Signal<IdentityState>,
    mut game_state: Signal<Option<ClientGameState>>,
    mut ops: Signal<OperationState>,
    mut reconnect_session_code: Signal<String>,
    mut reconnect_token: Signal<String>,
    mut judge_bundle: Signal<Option<JudgeBundle>>,
    workshop_code: String,
    character_id: Option<String>,
) {
    let base_url = { identity.read().api_base_url.clone() };

    identity.with_mut(|id| {
        id.connection_status = ConnectionStatus::Connecting;
    });
    ops.with_mut(|o| {
        o.pending_flow = Some(PendingFlow::Join);
        o.notice = Some(scoped_notice(
            NoticeScope::PickCharacter,
            info_notice("Joining workshop…"),
        ));
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
            );
        }
        Err(error) => {
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    apply_request_error(id, o, error);
                });
            });
        }
    }
}

/// Load the player's characters into `ops.my_characters`.
// Retained without a current consumer; no plan2 item schedules reuse. Remove if still unused after plan2 reintroduces per-account character management UI.
#[allow(dead_code)]
pub async fn load_my_characters_flow(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
) {
    let base_url = { identity.read().api_base_url.clone() };
    let api = AppWebApi::new(base_url);
    match api.list_my_characters().await {
        Ok(response) => {
            ops.with_mut(|o| {
                o.my_characters = response.characters;
                o.my_characters_limit = response.limit;
            });
        }
        Err(error) => {
            ops.with_mut(|o| {
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    error_notice(&format!("Failed to load characters: {error}")),
                ));
            });
        }
    }
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
) {
    let base_url = { identity.read().api_base_url.clone() };
    let api = AppWebApi::new(base_url);
    match api.list_open_workshops(&paging).await {
        Ok(response) => {
            ops.with_mut(|o| {
                o.open_workshops = response.workshops;
                o.open_workshops_next_cursor = response.next_cursor;
                o.open_workshops_prev_cursor = response.prev_cursor;
            });
        }
        Err(error) => {
            ops.with_mut(|o| {
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    error_notice(&format!("Failed to load workshops: {error}")),
                ));
            });
        }
    }
}

pub async fn refresh_open_workshops_after_delete(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    paging: OpenWorkshopsPaging,
) -> OpenWorkshopsPaging {
    load_open_workshops_flow(identity, ops, paging.clone()).await;
    let should_fallback = {
        let current = ops.read();
        !matches!(paging, OpenWorkshopsPaging::First) && current.open_workshops.is_empty()
    };
    if should_fallback {
        load_open_workshops_flow(identity, ops, OpenWorkshopsPaging::First).await;
        OpenWorkshopsPaging::First
    } else {
        paging
    }
}

/// Load eligible characters for a workshop into `ops.eligible_characters`.
pub async fn load_eligible_characters_flow(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    workshop_code: String,
) {
    ops.with_mut(|o| {
        o.eligible_characters.clear();
    });
    let base_url = { identity.read().api_base_url.clone() };
    let api = AppWebApi::new(base_url);
    match api.eligible_characters(&workshop_code).await {
        Ok(response) => {
            ops.with_mut(|o| {
                o.eligible_characters = response.characters;
            });
        }
        Err(error) => {
            ops.with_mut(|o| {
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
    let base_url = { identity.read().api_base_url.clone() };

    if description.trim().is_empty() {
        ops.with_mut(|o| {
            o.notice = Some(scoped_notice(
                NoticeScope::CreateCharacter,
                error_notice("Enter a character description."),
            ))
        });
        return;
    }

    ops.with_mut(|o| {
        o.pending_flow = Some(PendingFlow::Create);
        o.notice = Some(scoped_notice(
            NoticeScope::CreateCharacter,
            info_notice("Creating character…"),
        ));
    });

    let api = AppWebApi::new(base_url);
    let request = protocol::CreateCharacterRequest {
        description: description.trim().to_string(),
        sprites,
    };
    match api.create_character(&request).await {
        Ok(_profile) => {
            identity.with_mut(|id| {
                ops.with_mut(|o| {
                    navigate_to_screen(id, o, ShellScreen::AccountHome);
                });
            });
            ops.with_mut(|o| {
                o.pending_flow = None;
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    success_notice("Character created."),
                ));
            });
        }
        Err(error) => {
            ops.with_mut(|o| {
                o.pending_flow = None;
                o.notice = Some(scoped_notice(
                    NoticeScope::CreateCharacter,
                    error_notice(&error),
                ));
            });
        }
    }
}

/// Delete a character and refresh the character list.
// Retained without a current consumer; no plan2 item schedules reuse. Remove if still unused after plan2 reintroduces per-account character management UI.
#[allow(dead_code)]
pub async fn submit_delete_character_flow(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    character_id: String,
) {
    let base_url = { identity.read().api_base_url.clone() };
    let api = AppWebApi::new(base_url);
    match api.delete_character(&character_id).await {
        Ok(()) => {
            ops.with_mut(|o| {
                o.my_characters.retain(|c| c.id != character_id);
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    success_notice("Character deleted."),
                ));
            });
        }
        Err(error) => {
            ops.with_mut(|o| {
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    error_notice(&format!("Failed to delete character: {error}")),
                ));
            });
        }
    }
}

pub async fn submit_delete_workshop_flow(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    mut current_paging: Signal<OpenWorkshopsPaging>,
    session_code: String,
    paging: OpenWorkshopsPaging,
) {
    let base_url = { identity.read().api_base_url.clone() };
    ops.with_mut(|o| {
        o.pending_flow = Some(PendingFlow::DeleteWorkshop);
        o.notice = Some(scoped_notice(
            NoticeScope::AccountHome,
            info_notice("Deleting workshop…"),
        ));
    });
    let api = AppWebApi::new(base_url);
    match api.delete_workshop(&session_code).await {
        Ok(()) => {
            ops.with_mut(|o| {
                o.pending_flow = None;
                o.notice = Some(scoped_notice(
                    NoticeScope::AccountHome,
                    success_notice(&format!("Workshop {} deleted.", session_code)),
                ));
            });
            let next_paging = refresh_open_workshops_after_delete(identity, ops, paging).await;
            current_paging.set(next_paging);
        }
        Err(error) => {
            ops.with_mut(|o| {
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
    identity.with_mut(|id| {
        clear_session_identity(id);
    });
    ops.with_mut(|o| {
        o.pending_flow = None;
        o.pending_command = None;
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
) {
    identity.with_mut(|id| {
        game_state.with_mut(|gs| {
            ops.with_mut(|o| {
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
    let persisted_snapshot = { identity.read().session_snapshot.clone() };
    if let Some(snapshot) = persisted_snapshot
        && let Err(error) = persist_browser_session_snapshot(&snapshot)
    {
        ops.with_mut(|o| {
            o.notice = Some(scoped_notice(
                NoticeScope::Session,
                error_notice(&format!("{persistence_error_prefix}: {error}")),
            ));
        });
    }
    if let Err(error) = bootstrap_realtime(*identity, *game_state, *ops, *judge_bundle) {
        identity.with_mut(|id| {
            ops.with_mut(|o| {
                apply_realtime_bootstrap_error(id, o, error);
            });
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
        ClientGameState, CoordinatorType, ListOpenWorkshopsResponse, OpenWorkshopCursor,
        OpenWorkshopSummary, Phase, Player, SessionMeta, WorkshopJoinResult, WorkshopJoinSuccess,
        create_default_session_settings,
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
                    can_delete: true,
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
        let _f: fn(Signal<IdentityState>, Signal<OperationState>) -> _ =
            submit_create_workshop_flow;
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
