use dioxus::prelude::*;
use protocol::{ClientGameState, JoinWorkshopRequest, JudgeBundle, SessionCommand};

use crate::api::{AppWebApi, build_command_request, build_judge_bundle_request};
use crate::helpers::{parse_tags_input, pending_command_label};
use crate::realtime::bootstrap_realtime;
use crate::state::{
    ConnectionStatus, IdentityState, OperationState, PendingFlow, apply_command_error,
    apply_join_success, apply_judge_bundle_error, apply_judge_bundle_success,
    apply_realtime_bootstrap_error, apply_request_error, apply_successful_command, error_notice,
    info_notice, persist_browser_session_snapshot,
};
use protocol::WorkshopCreateConfig;

pub async fn submit_create_flow(
    mut identity: Signal<IdentityState>,
    mut game_state: Signal<Option<ClientGameState>>,
    mut ops: Signal<OperationState>,
    create_name: Signal<String>,
    phase0_minutes: Signal<String>,
    phase1_minutes: Signal<String>,
    phase2_minutes: Signal<String>,
    image_generator_token: Signal<String>,
    image_generator_model: Signal<String>,
    judge_token: Signal<String>,
    judge_model: Signal<String>,
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
        let image_token = image_generator_token.read().trim().to_string();
        let image_model = image_generator_model.read().trim().to_string();
        let judge_token = judge_token.read().trim().to_string();
        let judge_model = judge_model.read().trim().to_string();

        WorkshopCreateConfig {
            phase0_minutes: phase0,
            phase1_minutes: phase1,
            phase2_minutes: phase2,
            image_generator_token: if image_token.is_empty() {
                None
            } else {
                Some(image_token)
            },
            image_generator_model: if image_model.is_empty() {
                None
            } else {
                Some(image_model)
            },
            judge_token: if judge_token.is_empty() {
                None
            } else {
                Some(judge_token)
            },
            judge_model: if judge_model.is_empty() {
                None
            } else {
                Some(judge_model)
            },
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
            if let Some(snapshot) = persisted_snapshot {
                if let Err(error) = persist_browser_session_snapshot(&snapshot) {
                    ops.with_mut(|o| {
                        o.notice = Some(error_notice(&format!(
                            "Workshop created, but session persistence failed: {error}"
                        )))
                    });
                }
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
            if let Some(snapshot) = persisted_snapshot {
                if let Err(error) = persist_browser_session_snapshot(&snapshot) {
                    ops.with_mut(|o| {
                        o.notice = Some(error_notice(&format!(
                            "Joined workshop, but session persistence failed: {error}"
                        )))
                    });
                }
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
            if let Some(snapshot) = persisted_snapshot {
                if let Err(error) = persist_browser_session_snapshot(&snapshot) {
                    ops.with_mut(|o| {
                        o.notice = Some(error_notice(&format!(
                            "Reconnected, but session persistence failed: {error}"
                        )))
                    });
                }
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
            ops.with_mut(|o| {
                apply_command_error(o, error);
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
    identity: Signal<IdentityState>,
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
            ops.with_mut(|o| {
                apply_judge_bundle_error(o, error);
            });
        }
    }
}
