#![allow(clippy::too_many_arguments)]

use dioxus::prelude::*;
use protocol::{ClientGameState, JoinWorkshopRequest, JudgeBundle, SessionCommand, SpriteSet};

use crate::api::{
    AppWebApi, build_command_request, build_judge_bundle_request, build_sprite_sheet_request,
};
use crate::helpers::{parse_tags_input, pending_command_label};
use crate::realtime::bootstrap_realtime;
use crate::state::{
    ConnectionStatus, IdentityState, OperationState, PendingFlow, apply_command_error,
    apply_join_success, apply_judge_bundle_error, apply_judge_bundle_success,
    apply_realtime_bootstrap_error, apply_request_error, apply_successful_command, error_notice,
    info_notice, persist_browser_session_snapshot,
};
use protocol::WorkshopCreateConfig;

pub enum SpriteSheetSubmitError {
    Preflight(String),
    Request(String),
}

pub async fn submit_create_flow(
    mut identity: Signal<IdentityState>,
    mut game_state: Signal<Option<ClientGameState>>,
    mut ops: Signal<OperationState>,
    create_name: Signal<String>,
    phase0_minutes: Signal<String>,
    phase1_minutes: Signal<String>,
    phase2_minutes: Signal<String>,
    mut join_session_code: Signal<String>,
    mut reconnect_session_code: Signal<String>,
    mut reconnect_token: Signal<String>,
    mut judge_bundle: Signal<Option<JudgeBundle>>,
) {
    let (base_url, name) = {
        let id = identity.read();
        let name = create_name.read();
        (id.api_base_url.clone(), name.trim().to_string())
    };
    let config = {
        let phase0 = phase0_minutes.read().trim().parse::<u32>().unwrap_or(8);
        let phase1 = phase1_minutes.read().trim().parse::<u32>().unwrap_or(8);
        let phase2 = phase2_minutes.read().trim().parse::<u32>().unwrap_or(8);

        WorkshopCreateConfig {
            phase0_minutes: phase0,
            phase1_minutes: phase1,
            phase2_minutes: phase2,
        }
    };

    if name.is_empty() {
        ops.with_mut(|o| o.notice = Some(error_notice("Please enter a host name.")));
        return;
    }

    identity.with_mut(|id| {
        id.connection_status = ConnectionStatus::Connecting;
    });
    ops.with_mut(|o| {
        o.pending_flow = Some(PendingFlow::Create);
        o.notice = Some(info_notice("Creating workshop…"));
    });

    let api = AppWebApi::new(base_url);
    match api.create_workshop(name, config).await {
        Ok(success) => {
            identity.with_mut(|id| {
                game_state.with_mut(|gs| {
                    ops.with_mut(|o| {
                        join_session_code.with_mut(|join_code| {
                            reconnect_session_code.with_mut(|reconnect_code| {
                                reconnect_token.with_mut(|token| {
                                    judge_bundle.with_mut(|jb| {
                                        apply_join_success(
                                            id,
                                            gs,
                                            o,
                                            join_code,
                                            reconnect_code,
                                            token,
                                            jb,
                                            success,
                                            PendingFlow::Create,
                                        );
                                    });
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
                    o.notice = Some(error_notice(&format!(
                        "Workshop created, but session persistence failed: {error}"
                    )))
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

pub async fn submit_join_flow(
    mut identity: Signal<IdentityState>,
    mut game_state: Signal<Option<ClientGameState>>,
    mut ops: Signal<OperationState>,
    mut join_session_code_input: Signal<String>,
    join_name_input: Signal<String>,
    mut reconnect_session_code: Signal<String>,
    mut reconnect_token: Signal<String>,
    mut judge_bundle: Signal<Option<JudgeBundle>>,
) {
    let (base_url, session_code, name) = {
        let id = identity.read();
        let session_code = join_session_code_input.read();
        let name = join_name_input.read();
        (
            id.api_base_url.clone(),
            session_code.trim().to_string(),
            name.trim().to_string(),
        )
    };

    if session_code.is_empty() {
        ops.with_mut(|o| o.notice = Some(error_notice("Enter a workshop code.")));
        return;
    }
    if name.is_empty() {
        ops.with_mut(|o| o.notice = Some(error_notice("Please enter a player name.")));
        return;
    }

    identity.with_mut(|id| {
        id.connection_status = ConnectionStatus::Connecting;
    });
    ops.with_mut(|o| {
        o.pending_flow = Some(PendingFlow::Join);
        o.notice = Some(info_notice("Joining workshop…"));
    });

    let api = AppWebApi::new(base_url);
    let request = JoinWorkshopRequest {
        session_code,
        name: Some(name),
        reconnect_token: None,
    };
    match api.join_workshop(request).await {
        Ok(success) => {
            identity.with_mut(|id| {
                game_state.with_mut(|gs| {
                    ops.with_mut(|o| {
                        join_session_code_input.with_mut(|join_code| {
                            reconnect_session_code.with_mut(|reconnect_code| {
                                reconnect_token.with_mut(|token| {
                                    judge_bundle.with_mut(|jb| {
                                        apply_join_success(
                                            id,
                                            gs,
                                            o,
                                            join_code,
                                            reconnect_code,
                                            token,
                                            jb,
                                            success,
                                            PendingFlow::Join,
                                        );
                                    });
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
                    o.notice = Some(error_notice(&format!(
                        "Joined workshop, but session persistence failed: {error}"
                    )))
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

pub async fn submit_reconnect_flow(
    mut identity: Signal<IdentityState>,
    mut game_state: Signal<Option<ClientGameState>>,
    mut ops: Signal<OperationState>,
    mut join_session_code: Signal<String>,
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
            o.notice = Some(error_notice(
                "Session code and reconnect token are required for reconnect.",
            ))
        });
        return;
    }

    identity.with_mut(|id| {
        id.connection_status = ConnectionStatus::Connecting;
    });
    ops.with_mut(|o| {
        o.pending_flow = Some(PendingFlow::Reconnect);
        o.notice = Some(info_notice("Reconnecting…"));
    });

    let api = AppWebApi::new(base_url);
    match api.reconnect_workshop(session_code, reconnect_token).await {
        Ok(success) => {
            identity.with_mut(|id| {
                game_state.with_mut(|gs| {
                    ops.with_mut(|o| {
                        join_session_code.with_mut(|join_code| {
                            reconnect_session_code_input.with_mut(|reconnect_code| {
                                reconnect_token_input.with_mut(|token| {
                                    judge_bundle.with_mut(|jb| {
                                        apply_join_success(
                                            id,
                                            gs,
                                            o,
                                            join_code,
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
            });
            let persisted_snapshot = { identity.read().session_snapshot.clone() };
            if let Some(snapshot) = persisted_snapshot
                && let Err(error) = persist_browser_session_snapshot(&snapshot)
            {
                ops.with_mut(|o| {
                    o.notice = Some(error_notice(&format!(
                        "Reconnected, but session persistence failed: {error}"
                    )))
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
            o.notice = Some(error_notice(
                "Connect to a workshop before sending commands.",
            ))
        });
        return;
    };

    ops.with_mut(|o| {
        o.pending_command = Some(command);
        o.notice = Some(info_notice(pending_command_label(command)));
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
    mut ops: Signal<OperationState>,
    handover_tags_input: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) {
    let tags = {
        let tags_input = handover_tags_input.read();
        parse_tags_input(&tags_input)
    };

    if tags.is_empty() {
        ops.with_mut(|o| o.notice = Some(error_notice("Enter at least one handover tag.")));
        return;
    }

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
            o.notice = Some(error_notice(
                "Connect to a workshop before building the archive.",
            ))
        });
        return;
    };

    ops.with_mut(|o| {
        o.pending_judge_bundle = true;
        o.notice = Some(info_notice("Building workshop archive…"));
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

pub async fn submit_sprite_sheet_request(
    identity: Signal<IdentityState>,
    mut ops: Signal<OperationState>,
    mut sprite_result: Signal<Option<SpriteSet>>,
    description: String,
) -> Result<(), SpriteSheetSubmitError> {
    let (base_url, snapshot) = {
        let id = identity.read();
        (id.api_base_url.clone(), id.session_snapshot.clone())
    };

    let Some(snapshot) = snapshot else {
        let message = "Connect to a workshop before generating sprites.".to_string();
        ops.with_mut(|o| {
            o.notice = Some(error_notice(&message))
        });
        return Err(SpriteSheetSubmitError::Preflight(message));
    };

    if description.trim().is_empty() {
        let message = "Enter a dragon description.".to_string();
        ops.with_mut(|o| o.notice = Some(error_notice(&message)));
        return Err(SpriteSheetSubmitError::Preflight(message));
    }

    ops.with_mut(|o| {
        o.notice = Some(info_notice("Generating dragon sprites…"));
    });

    let api = AppWebApi::new(base_url);
    match api
        .generate_sprite_sheet(build_sprite_sheet_request(&snapshot, &description))
        .await
    {
        Ok(sprites) => {
            sprite_result.set(Some(sprites));
            ops.with_mut(|o| {
                o.notice = Some(info_notice("Dragon sprites generated!"));
            });
            Ok(())
        }
        Err(error) => {
            ops.with_mut(|o| {
                o.notice = Some(error_notice(&format!("Sprite generation failed: {error}")));
            });
            Err(SpriteSheetSubmitError::Request(error))
        }
    }
}

pub async fn submit_update_player_pet(
    identity: Signal<IdentityState>,
    ops: Signal<OperationState>,
    handover_tags_input: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
    description: String,
    sprites: Option<SpriteSet>,
) {
    let payload = serde_json::json!({
        "description": description,
        "sprites": sprites,
    });

    submit_workshop_command(
        identity,
        ops,
        handover_tags_input,
        judge_bundle,
        SessionCommand::UpdatePlayerPet,
        Some(payload),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        ConnectionStatus, ShellScreen, default_identity_state, default_operation_state,
    };
    use protocol::{
        ClientGameState, CoordinatorType, Phase, Player, SessionMeta, WorkshopJoinResult,
        WorkshopJoinSuccess, create_default_session_settings,
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
                pet_description: Some("Alice's workshop dragon".to_string()),
                custom_sprites: None,
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
            let join_session_code = Signal::new(String::new());
            let reconnect_session_code = Signal::new("123456".to_string());
            let reconnect_token = Signal::new("reconnect-1".to_string());
            let judge_bundle = Signal::new(None);

            runtime.block_on(submit_reconnect_flow(
                identity,
                game_state,
                ops,
                join_session_code,
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
}
