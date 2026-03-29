use axum::{
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use chrono::Utc;
use protocol::{
    ClientWsMessage, NoticeLevel, ServerWsMessage, SessionArtifactKind, SessionArtifactRecord,
    SessionEnvelope,
};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::info;

use crate::app::AppState;
use crate::cache::{ensure_session_cached, session_write_lock};
use crate::helpers::{phase_label, phase_step, random_prefixed_id, to_client_game_state};

#[derive(Debug, Clone, PartialEq, Eq)]
struct WsAttachOutcome {
    session_code: String,
    replaced_connection_id: Option<String>,
    state_changed: bool,
}

pub(crate) async fn emit_phase_warning_notices(state: &AppState) {
    let now = Utc::now();
    let mut sessions_to_warn = Vec::new();

    {
        let mut sessions = state.sessions.lock().await;
        for session in sessions.values_mut() {
            let Some(remaining_seconds) = session.remaining_phase_seconds(now) else {
                continue;
            };
            if session.warned_for_current_phase {
                continue;
            }
            if remaining_seconds <= session.phase_warning_threshold_seconds() {
                session.warned_for_current_phase = true;
                sessions_to_warn.push((session.code.0.clone(), session.phase, remaining_seconds));
            }
        }
    }

    for (session_code, phase, remaining_seconds) in sessions_to_warn {
        broadcast_notice(
            state,
            &session_code,
            NoticeLevel::Warning,
            "Phase ending soon",
            &format!(
                "{} ends in {} seconds.",
                phase_label(phase),
                remaining_seconds
            ),
        )
        .await;
    }
}

async fn broadcast_notice(
    state: &AppState,
    session_code: &str,
    level: protocol::NoticeLevel,
    title: &str,
    message: &str,
) {
    let registrations = state
        .realtime
        .lock()
        .await
        .session_registrations(session_code);
    if registrations.is_empty() {
        return;
    }

    let notice = ServerWsMessage::Notice(protocol::SessionNotice {
        level,
        title: title.to_string(),
        message: message.to_string(),
    });

    let failed_connection_ids = {
        let senders = state.realtime_senders.lock().await;
        registrations
            .into_iter()
            .filter_map(|registration| {
                senders.get(&registration.connection_id).and_then(|sender| {
                    sender
                        .send(notice.clone())
                        .err()
                        .map(|_| registration.connection_id)
                })
            })
            .collect::<Vec<_>>()
    };

    if !failed_connection_ids.is_empty() {
        let mut senders = state.realtime_senders.lock().await;
        for connection_id in failed_connection_ids {
            senders.remove(&connection_id);
        }
    }
}

pub(crate) async fn workshop_ws(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if !security::is_origin_allowed(
        headers.get("origin").and_then(|value| value.to_str().ok()),
        &state.config.origin_policy,
    ) {
        return (StatusCode::FORBIDDEN, "Origin is not allowed.").into_response();
    }

    ws.on_upgrade(move |socket| handle_workshop_ws(state, socket))
        .into_response()
}

async fn handle_workshop_ws(state: AppState, mut socket: WebSocket) {
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel();
    let mut attached_connection_id: Option<String> = None;

    loop {
        tokio::select! {
            outbound_message = outbound_rx.recv() => {
                let Some(outbound_message) = outbound_message else {
                    break;
                };
                if send_ws_message(&mut socket, &outbound_message).await.is_err() {
                    break;
                }
            }
            message_result = socket.recv() => {
                let Some(message_result) = message_result else {
                    break;
                };
                let Ok(message) = message_result else {
                    break;
                };

                match message {
                    Message::Text(text) => match serde_json::from_str::<ClientWsMessage>(&text) {
                        Ok(ClientWsMessage::AttachSession(envelope)) => {
                            let is_new_connection = attached_connection_id.is_none();
                            let connection_id = attached_connection_id
                                .clone()
                                .unwrap_or_else(|| random_prefixed_id("conn"));
                            if is_new_connection {
                                register_ws_sender(&state, &connection_id, outbound_tx.clone()).await;
                            }
                            match attach_ws_session(&state, &mut socket, &envelope, &connection_id, is_new_connection).await {
                                Ok(outcome) => {
                                    if let Some(replaced_connection_id) = outcome.replaced_connection_id.as_deref() {
                                        if replaced_connection_id != connection_id.as_str() {
                                            unregister_ws_sender(&state, replaced_connection_id).await;
                                        }
                                    }
                                    if outcome.state_changed {
                                        broadcast_session_state(&state, &outcome.session_code, Some(connection_id.as_str())).await;
                                    }
                                    attached_connection_id = Some(connection_id);
                                }
                                Err(error_message) => {
                                    if is_new_connection {
                                        unregister_ws_sender(&state, &connection_id).await;
                                    }
                                    if send_ws_message(&mut socket, &ServerWsMessage::Error { message: error_message })
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(ClientWsMessage::Ping) => {
                            if send_ws_message(&mut socket, &ServerWsMessage::Pong).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => {
                            if send_ws_message(
                                &mut socket,
                                &ServerWsMessage::Error {
                                    message: "Invalid WebSocket payload.".to_string(),
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }
                    },
                    Message::Ping(payload) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }

    if let Some(connection_id) = attached_connection_id {
        unregister_ws_sender(&state, &connection_id).await;
        sync_ws_disconnect(&state, &connection_id).await;
    }
}

async fn register_ws_sender(
    state: &AppState,
    connection_id: &str,
    sender: mpsc::UnboundedSender<ServerWsMessage>,
) {
    state
        .realtime_senders
        .lock()
        .await
        .insert(connection_id.to_string(), sender);
}

async fn unregister_ws_sender(state: &AppState, connection_id: &str) {
    state.realtime_senders.lock().await.remove(connection_id);
}

pub(crate) async fn broadcast_session_state(
    state: &AppState,
    session_code: &str,
    excluded_connection_id: Option<&str>,
) {
    let Ok(is_cached) = ensure_session_cached(state, session_code).await else {
        return;
    };
    if !is_cached {
        return;
    }

    let registrations = state
        .realtime
        .lock()
        .await
        .session_registrations(session_code);
    if registrations.is_empty() {
        return;
    }

    let messages = {
        let sessions = state.sessions.lock().await;
        let Some(session) = sessions.get(session_code) else {
            return;
        };

        registrations
            .into_iter()
            .filter(|registration| {
                Some(registration.connection_id.as_str()) != excluded_connection_id
            })
            .map(|registration| {
                (
                    registration.connection_id,
                    ServerWsMessage::StateUpdate(to_client_game_state(
                        session,
                        &registration.player_id,
                    )),
                )
            })
            .collect::<Vec<_>>()
    };

    if messages.is_empty() {
        return;
    }

    let failed_connection_ids = {
        let senders = state.realtime_senders.lock().await;
        messages
            .into_iter()
            .filter_map(
                |(connection_id, message)| match senders.get(&connection_id) {
                    Some(sender) => sender.send(message).err().map(|_| connection_id),
                    None => None,
                },
            )
            .collect::<Vec<_>>()
    };

    if !failed_connection_ids.is_empty() {
        let mut senders = state.realtime_senders.lock().await;
        for connection_id in failed_connection_ids {
            senders.remove(&connection_id);
        }
    }
}

async fn sync_ws_disconnect(state: &AppState, connection_id: &str) {
    let Some(registration) = state.realtime.lock().await.detach(connection_id) else {
        return;
    };

    let timestamp = Utc::now();
    let session_code = registration.session_code;
    let player_id = registration.player_id;
    let write_lock = session_write_lock(state, &session_code).await;
    let _write_guard = write_lock.lock().await;
    let disconnect_payload = json!({
        "sessionCode": session_code.clone(),
        "playerId": player_id.clone(),
    });

    let disconnect_state = {
        let mut sessions = state.sessions.lock().await;
        match sessions.get_mut(session_code.as_str()) {
            Some(session) => match session.players.get_mut(player_id.as_str()) {
                Some(player) if player.is_connected => {
                    player.is_connected = false;
                    session.ensure_host_assigned(true);
                    session.updated_at = timestamp;
                    Some((session.clone(), phase_step(session.phase)))
                }
                _ => None,
            },
            None => None,
        }
    };

    let Some((session, step)) = disconnect_state else {
        return;
    };

    if let Err(error) = state.store.save_session(&session).await {
        info!(session_code = %session.code.0, player_id = %player_id, error = %error, "failed to persist websocket disconnect session state");
    }

    if let Err(error) = state
        .store
        .append_session_artifact(&SessionArtifactRecord {
            id: random_prefixed_id("artifact"),
            session_id: session.id.to_string(),
            phase: session.phase,
            step,
            kind: SessionArtifactKind::PlayerLeft,
            player_id: Some(player_id.clone()),
            created_at: timestamp.to_rfc3339(),
            payload: disconnect_payload,
        })
        .await
    {
        info!(session_code = %session.code.0, player_id = %player_id, error = %error, "failed to append websocket disconnect artifact");
    }

    broadcast_session_state(state, &session.code.0, None).await;
}

async fn attach_ws_session(
    state: &AppState,
    socket: &mut WebSocket,
    envelope: &SessionEnvelope,
    connection_id: &str,
    is_new_connection: bool,
) -> Result<WsAttachOutcome, String> {
    let session_code = envelope.session_code.trim();
    let reconnect_token = envelope.reconnect_token.trim();
    let player_id = envelope.player_id.trim();
    if session_code.is_empty()
        || reconnect_token.is_empty()
        || player_id.is_empty()
        || security::validate_session_code(session_code).is_err()
    {
        return Err("Missing workshop credentials.".to_string());
    }

    let identity = state
        .store
        .find_player_identity(session_code, reconnect_token)
        .await
        .map_err(|error| format!("failed to lookup identity: {error}"))?
        .ok_or_else(|| "Session identity is invalid or expired.".to_string())?;
    if identity.player_id != player_id {
        return Err("Session identity is invalid or expired.".to_string());
    }

    let write_lock = session_write_lock(state, session_code).await;
    let _write_guard = write_lock.lock().await;

    if !ensure_session_cached(state, session_code).await? {
        return Err("Workshop not found.".to_string());
    }

    let mut reconnect_artifact: Option<SessionArtifactRecord> = None;

    let (client_state, session_before, session_clone) = {
        let mut sessions = state.sessions.lock().await;
        let session = sessions
            .get_mut(session_code)
            .ok_or_else(|| "Workshop not found.".to_string())?;
        let was_connected = session
            .players
            .get(&identity.player_id)
            .ok_or_else(|| "Session identity is invalid or expired.".to_string())?
            .is_connected;
        let session_before = if !was_connected {
            Some(session.clone())
        } else {
            None
        };
        if !was_connected {
            session
                .players
                .get_mut(&identity.player_id)
                .expect("player checked above")
                .is_connected = true;
            session.ensure_host_assigned(true);
            session.updated_at = Utc::now();
            reconnect_artifact = Some(SessionArtifactRecord {
                id: random_prefixed_id("artifact"),
                session_id: session.id.to_string(),
                phase: session.phase,
                step: phase_step(session.phase),
                kind: SessionArtifactKind::PlayerReconnected,
                player_id: Some(identity.player_id.clone()),
                created_at: session.updated_at.to_rfc3339(),
                payload: json!({
                    "sessionCode": session_code,
                    "playerId": identity.player_id.clone(),
                    "transport": "websocket",
                }),
            });
        }
        let client_state = to_client_game_state(session, &identity.player_id);
        let session_clone = if reconnect_artifact.is_some() {
            Some(session.clone())
        } else {
            None
        };
        (client_state, session_before, session_clone)
    };

    if let Some(ref session_clone) = session_clone {
        if let Err(error) = state.store.save_session(session_clone).await {
            if let Some(session_before) = session_before {
                state
                    .sessions
                    .lock()
                    .await
                    .insert(session_code.to_string(), session_before);
            }
            return Err(format!("failed to save session: {error}"));
        }
    }

    let state_changed = reconnect_artifact.is_some();

    if let Some(artifact) = reconnect_artifact {
        state
            .store
            .append_session_artifact(&artifact)
            .await
            .map_err(|error| format!("failed to append reconnect artifact: {error}"))?;
    }

    let attach_result =
        state
            .realtime
            .lock()
            .await
            .attach(session_code, &identity.player_id, connection_id);

    drop(_write_guard);

    if send_ws_message(socket, &ServerWsMessage::StateUpdate(client_state))
        .await
        .is_err()
    {
        let mut realtime = state.realtime.lock().await;
        realtime.detach(connection_id);
        if let Some(replaced_connection_id) = attach_result.replaced_connection_id.as_deref() {
            if replaced_connection_id != connection_id {
                realtime.attach(session_code, &identity.player_id, replaced_connection_id);
            }
        }
        drop(realtime);

        if is_new_connection {
            unregister_ws_sender(state, connection_id).await;
        }
        return Err("connection is closed".to_string());
    }

    Ok(WsAttachOutcome {
        session_code: session_code.to_string(),
        replaced_connection_id: attach_result.replaced_connection_id,
        state_changed,
    })
}

async fn send_ws_message(socket: &mut WebSocket, message: &ServerWsMessage) -> Result<(), ()> {
    let encoded = serde_json::to_string(message).map_err(|_| ())?;
    socket
        .send(Message::Text(encoded.into()))
        .await
        .map_err(|_| ())
}
