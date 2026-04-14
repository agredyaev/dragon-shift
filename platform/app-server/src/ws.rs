use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use chrono::Utc;
use persistence::{RealtimeConnectionRegistration, SessionUpdateNotification};
use protocol::{
    ClientWsMessage, NoticeLevel, ServerWsMessage, SessionArtifactKind, SessionArtifactRecord,
    SessionEnvelope,
};
use serde_json::json;
#[cfg(test)]
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::info;

use crate::app::AppState;
use crate::cache::{SessionWriteLease, ensure_session_cached, reload_cached_session};
use crate::helpers::{phase_label, phase_step, random_prefixed_id, to_client_game_state};
use crate::http::{
    MaybeConnectInfo, authorize_reconnect_identity, client_key, is_rate_limited,
    refresh_reconnect_identity,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct WsAttachOutcome {
    session_code: String,
    state_changed: bool,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum WsOutbound {
    Message(ServerWsMessage),
    Close,
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
    let registrations = match state.store.list_realtime_connections(session_code).await {
        Ok(registrations) => registrations,
        Err(_) => return,
    };
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
            .filter(|registration| registration.replica_id == state.replica_id)
            .filter_map(|registration| {
                senders.get(&registration.connection_id).and_then(|sender| {
                    sender
                        .send(WsOutbound::Message(notice.clone()))
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
    ConnectInfo(connect_info): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if !security::is_origin_allowed(
        headers.get("origin").and_then(|value| value.to_str().ok()),
        &state.config.origin_policy,
    ) {
        return (StatusCode::FORBIDDEN, "Origin is not allowed.").into_response();
    }
    let client_key = client_key(&state, MaybeConnectInfo(Some(connect_info)), &headers);
    if is_rate_limited(&state.websocket_limiter, &client_key).await {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests. Please slow down and try again.",
        )
            .into_response();
    }

    ws.on_upgrade(move |socket| handle_workshop_ws(state, socket, client_key))
        .into_response()
}

async fn handle_workshop_ws(state: AppState, mut socket: WebSocket, client_key: String) {
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel();
    let mut attached_connection_id: Option<String> = None;

    loop {
        tokio::select! {
            outbound_message = outbound_rx.recv() => {
                let Some(outbound_message) = outbound_message else {
                    break;
                };
                match outbound_message {
                    WsOutbound::Message(outbound_message) => {
                        if send_ws_message(&state, &mut socket, &outbound_message).await.is_err() {
                            break;
                        }
                    }
                    WsOutbound::Close => break,
                }
            }
            message_result = socket.recv() => {
                let Some(message_result) = message_result else {
                    break;
                };
                let Ok(message) = message_result else {
                    break;
                };

                if is_rate_limited(&state.websocket_limiter, &client_key).await {
                    if send_ws_message(
                        &state,
                        &mut socket,
                        &ServerWsMessage::Error {
                            message: "Too many requests. Please slow down and try again.".to_string(),
                        },
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                    continue;
                }

                match message {
                    Message::Text(text) => match serde_json::from_str::<ClientWsMessage>(&text) {
                        Ok(ClientWsMessage::AttachSession(envelope)) => {
                            let is_new_connection = attached_connection_id.is_none();
                            let connection_id = attached_connection_id
                                .clone()
                                .unwrap_or_else(|| random_prefixed_id("conn"));
                            if !is_new_connection {
                                let current_registration = state
                                    .realtime
                                    .lock()
                                    .await
                                    .connection_registration(&connection_id);
                                if let Some(current_registration) = current_registration
                                    && (current_registration.session_code != envelope.session_code.trim()
                                        || current_registration.player_id != envelope.player_id.trim())
                                {
                                if send_ws_message(&state, &mut socket, &ServerWsMessage::Error {
                                    message: "WebSocket is already attached to a different player.".to_string(),
                                })
                                .await
                                .is_err()
                                {
                                    break;
                                }
                                continue;
                            }
                            }
                            if state
                                .retired_realtime_connections
                                .lock()
                                .await
                                .contains_key(&connection_id)
                            {
                                if is_new_connection {
                                    unregister_ws_sender(&state, &connection_id).await;
                                }
                                if send_ws_message(&state, &mut socket, &ServerWsMessage::Error {
                                    message: "connection is closed".to_string(),
                                })
                                .await
                                .is_err()
                                {
                                    break;
                                }
                                let _ = outbound_tx.send(WsOutbound::Close);
                                continue;
                            }
                            if is_new_connection {
                                register_ws_sender(&state, &connection_id, outbound_tx.clone()).await;
                            }
                            match attach_ws_session(&state, &mut socket, &envelope, &connection_id, is_new_connection).await {
                                Ok(outcome) => {
                                      if outcome.state_changed {
                                          broadcast_session_state(&state, &outcome.session_code, Some(connection_id.as_str())).await;
                                      }
                                    attached_connection_id = Some(connection_id);
                                }
                                Err(error_message) => {
                                    if is_new_connection {
                                        unregister_ws_sender(&state, &connection_id).await;
                                    }
                                    if send_ws_message(&state, &mut socket, &ServerWsMessage::Error { message: error_message })
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(ClientWsMessage::Ping) => {
                            if send_ws_message(&state, &mut socket, &ServerWsMessage::Pong).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => {
                            if send_ws_message(
                                &state,
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
        stop_realtime_heartbeat(&state, &connection_id).await;
        unregister_ws_sender(&state, &connection_id).await;
        sync_ws_disconnect(&state, &connection_id).await;
        state
            .retired_realtime_connections
            .lock()
            .await
            .remove(&connection_id);
    }
}

async fn register_ws_sender(
    state: &AppState,
    connection_id: &str,
    sender: mpsc::UnboundedSender<WsOutbound>,
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

async fn replace_realtime_heartbeat(
    state: &AppState,
    connection_id: &str,
    heartbeat: JoinHandle<()>,
) {
    let previous = state
        .realtime_heartbeats
        .lock()
        .await
        .insert(connection_id.to_string(), heartbeat);
    if let Some(previous) = previous {
        previous.abort();
    }
}

async fn stop_realtime_heartbeat(state: &AppState, connection_id: &str) {
    if let Some(heartbeat) = state.realtime_heartbeats.lock().await.remove(connection_id) {
        heartbeat.abort();
    }
}

async fn retire_local_realtime_connection(state: &AppState, connection_id: &str) {
    state
        .retired_realtime_connections
        .lock()
        .await
        .insert(connection_id.to_string(), ());
    state.realtime.lock().await.detach(connection_id);
}

pub(crate) async fn clear_local_realtime_connection(state: &AppState, connection_id: &str) {
    stop_realtime_heartbeat(state, connection_id).await;
    retire_local_realtime_connection(state, connection_id).await;
}

async fn restore_replaced_registration(
    state: &AppState,
    replaced: &RealtimeConnectionRegistration,
) -> Result<(), String> {
    let restored = state
        .store
        .restore_realtime_connection(replaced)
        .await
        .map_err(|error| format!("failed to restore replaced realtime connection: {error}"))?;
    if restored.restored && replaced.replica_id == state.replica_id {
        state.realtime.lock().await.attach(
            &replaced.session_code,
            &replaced.player_id,
            &replaced.connection_id,
        );
        replace_realtime_heartbeat(
            state,
            &replaced.connection_id,
            spawn_realtime_heartbeat(state.clone(), replaced.connection_id.clone()),
        )
        .await;
    }
    Ok(())
}

fn spawn_realtime_heartbeat(state: AppState, connection_id: String) -> JoinHandle<()> {
    tokio::spawn(async move {
        let heartbeat_interval = persistence::REALTIME_CONNECTION_TTL / 2;
        loop {
            tokio::time::sleep(heartbeat_interval).await;
            match state
                .store
                .renew_realtime_connection(&connection_id, &state.replica_id)
                .await
            {
                Ok(true) => {}
                Ok(false) | Err(_) => {
                    retire_local_realtime_connection(&state, &connection_id).await;
                    close_local_connection(&state, &connection_id).await;
                    break;
                }
            }
        }
    })
}

pub(crate) async fn close_local_connection(state: &AppState, connection_id: &str) {
    if let Some(sender) = state
        .realtime_senders
        .lock()
        .await
        .get(connection_id)
        .cloned()
    {
        let _ = sender.send(WsOutbound::Close);
    }
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

    let registrations = match state.store.list_realtime_connections(session_code).await {
        Ok(registrations) => registrations,
        Err(_) => return,
    };
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
                registration.replica_id == state.replica_id
                    && Some(registration.connection_id.as_str()) != excluded_connection_id
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
                    Some(sender) => sender
                        .send(WsOutbound::Message(message))
                        .err()
                        .map(|_| connection_id),
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

pub(crate) async fn sync_ws_disconnect(state: &AppState, connection_id: &str) {
    let persisted_registration = state
        .store
        .release_realtime_connection(connection_id, &state.replica_id)
        .await
        .ok()
        .flatten();
    if persisted_registration.is_none()
        && state
            .store
            .take_retired_realtime_connection(connection_id, &state.replica_id)
            .await
            .ok()
            .flatten()
            .is_some()
    {
        state.realtime.lock().await.detach(connection_id);
        return;
    }
    let local_registration =
        state
            .realtime
            .lock()
            .await
            .detach(connection_id)
            .map(|registration| RealtimeConnectionRegistration {
                session_code: registration.session_code,
                player_id: registration.player_id,
                connection_id: registration.connection_id,
                replica_id: state.replica_id.clone(),
            });
    let Some(registration) = persisted_registration.or(local_registration) else {
        return;
    };

    let timestamp = Utc::now();
    let session_code = registration.session_code;
    let player_id = registration.player_id;
    let cached_session_before_reload = { state.sessions.lock().await.get(&session_code).cloned() };
    let was_connected_before_reload = cached_session_before_reload
        .as_ref()
        .and_then(|session| session.players.get(player_id.as_str()))
        .map(|player| player.is_connected)
        .unwrap_or(true);
    let (_, _write_guard, write_lease) = match SessionWriteLease::acquire(state, &session_code)
        .await
    {
        Ok(guard) => guard,
        Err(error) => {
            info!(session_code = %session_code, player_id = %player_id, error = %error, "failed to acquire websocket disconnect lease");
            return;
        }
    };
    if let Err(error) = write_lease.ensure_active() {
        info!(session_code = %session_code, player_id = %player_id, error = %error, "lost websocket disconnect lease before reload");
        return;
    }
    if !reload_cached_session(state, &session_code)
        .await
        .unwrap_or(false)
    {
        return;
    }
    if let Err(error) = write_lease.ensure_active() {
        info!(session_code = %session_code, player_id = %player_id, error = %error, "lost websocket disconnect lease before mutation");
        return;
    }
    let disconnect_payload = json!({
        "sessionCode": session_code.clone(),
        "playerId": player_id.clone(),
    });

    let disconnect_state = {
        let mut sessions = state.sessions.lock().await;
        match sessions.get_mut(session_code.as_str()) {
            Some(session) => {
                let Some(player) = session.players.get(player_id.as_str()) else {
                    return;
                };
                if !player.is_connected && !was_connected_before_reload {
                    return;
                }

                let session_before = session.clone();
                let step = phase_step(session.phase);

                if let Some(player) = session.players.get_mut(player_id.as_str()) {
                    player.is_connected = false;
                }
                session.ensure_host_assigned(true);
                session.updated_at = timestamp;
                Some((session_before, session.clone(), step))
            }
            None => None,
        }
    };

    let Some((session_before, session, step)) = disconnect_state else {
        return;
    };

    let disconnect_artifact = SessionArtifactRecord {
        id: random_prefixed_id("artifact"),
        session_id: session.id.to_string(),
        phase: session.phase,
        step,
        kind: SessionArtifactKind::PlayerLeft,
        player_id: Some(player_id.clone()),
        created_at: timestamp.to_rfc3339(),
        payload: disconnect_payload,
    };

    if let Err(error) = write_lease.ensure_active() {
        let mut sessions = state.sessions.lock().await;
        if let Some(previous_session) = cached_session_before_reload.clone() {
            sessions.insert(session.code.0.clone(), previous_session);
        } else {
            sessions.insert(session.code.0.clone(), session_before);
        }
        info!(session_code = %session.code.0, player_id = %player_id, error = %error, "lost websocket disconnect lease before persist");
        return;
    }

    if let Err(error) = state
        .store
        .save_session_with_artifact(&session, &disconnect_artifact)
        .await
    {
        let mut sessions = state.sessions.lock().await;
        if let Some(previous_session) = cached_session_before_reload {
            sessions.insert(session.code.0.clone(), previous_session);
        } else {
            sessions.insert(session.code.0.clone(), session_before);
        }
        info!(session_code = %session.code.0, player_id = %player_id, error = %error, "failed to persist websocket disconnect state");
        return;
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

    let identity = authorize_reconnect_identity(state, session_code, reconnect_token)
        .await
        .map_err(|error| format!("failed to lookup identity: {error}"))?
        .ok_or_else(|| "Session identity is invalid or expired.".to_string())?;
    if identity.player_id != player_id {
        return Err("Session identity is invalid or expired.".to_string());
    }

    if let Err(error) = refresh_reconnect_identity(state, reconnect_token, Utc::now()).await {
        return Err(format!("failed to touch player identity: {error}"));
    }

    let (_, _write_guard, write_lease) = SessionWriteLease::acquire(state, session_code)
        .await
        .map_err(|error| format!("failed to acquire session lease: {error}"))?;
    write_lease
        .ensure_active()
        .map_err(|error| format!("lost session lease before websocket load: {error}"))?;

    if !reload_cached_session(state, session_code).await? {
        return Err("Workshop not found.".to_string());
    }
    write_lease
        .ensure_active()
        .map_err(|error| format!("lost session lease before websocket mutation: {error}"))?;

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

    let state_changed = reconnect_artifact.is_some();

    let attach_result = match state
        .store
        .claim_realtime_connection(&RealtimeConnectionRegistration {
            session_code: session_code.to_string(),
            player_id: identity.player_id.clone(),
            connection_id: connection_id.to_string(),
            replica_id: state.replica_id.clone(),
        })
        .await
    {
        Ok(attach_result) => attach_result,
        Err(error) => {
            if let Some(session_before) = session_before {
                state
                    .sessions
                    .lock()
                    .await
                    .insert(session_code.to_string(), session_before);
            }
            if is_new_connection {
                unregister_ws_sender(state, connection_id).await;
            }
            return Err(format!("failed to claim realtime connection: {error}"));
        }
    };
    if let Some(replaced) = attach_result.replaced.as_ref()
        && replaced.replica_id == state.replica_id
    {
        // Stop the displaced local heartbeat before any further awaited work so
        // it cannot observe the temporary missing-row window and self-close.
        stop_realtime_heartbeat(state, &replaced.connection_id).await;
    }
    let local_attach_result =
        state
            .realtime
            .lock()
            .await
            .attach(session_code, &identity.player_id, connection_id);
    let local_replaced_registration =
        local_attach_result
            .replaced_connection_id
            .as_ref()
            .map(|connection_id| RealtimeConnectionRegistration {
                session_code: session_code.to_string(),
                player_id: identity.player_id.clone(),
                connection_id: connection_id.clone(),
                replica_id: state.replica_id.clone(),
            });

    if send_ws_message(state, socket, &ServerWsMessage::StateUpdate(client_state))
        .await
        .is_err()
    {
        let _ = state
            .store
            .release_realtime_connection(connection_id, &state.replica_id)
            .await;
        stop_realtime_heartbeat(state, connection_id).await;
        state.realtime.lock().await.detach(connection_id);
        if let Some(replaced) = attach_result
            .replaced
            .as_ref()
            .or(local_replaced_registration.as_ref())
            && let Err(error) = restore_replaced_registration(state, replaced).await
        {
            if is_new_connection {
                unregister_ws_sender(state, connection_id).await;
            }
            return Err(error);
        }

        if let Some(session_before) = session_before {
            state
                .sessions
                .lock()
                .await
                .insert(session_code.to_string(), session_before);
        }

        if is_new_connection {
            unregister_ws_sender(state, connection_id).await;
        }
        return Err("connection is closed".to_string());
    }

    if let (Some(session_clone), Some(artifact)) =
        (session_clone.as_ref(), reconnect_artifact.as_ref())
    {
        if let Err(error) = write_lease.ensure_active() {
            let _ = state
                .store
                .release_realtime_connection(connection_id, &state.replica_id)
                .await;
            stop_realtime_heartbeat(state, connection_id).await;
            state.realtime.lock().await.detach(connection_id);
            if let Some(replaced) = attach_result
                .replaced
                .as_ref()
                .or(local_replaced_registration.as_ref())
                && let Err(error) = restore_replaced_registration(state, replaced).await
            {
                if is_new_connection {
                    unregister_ws_sender(state, connection_id).await;
                }
                return Err(error);
            }
            if let Some(session_before) = session_before {
                state
                    .sessions
                    .lock()
                    .await
                    .insert(session_code.to_string(), session_before);
            }
            if is_new_connection {
                unregister_ws_sender(state, connection_id).await;
            }
            return Err(format!(
                "lost session lease before websocket persist: {error}"
            ));
        }
        if let Err(error) = state
            .store
            .save_session_with_artifact(session_clone, artifact)
            .await
        {
            let _ = state
                .store
                .release_realtime_connection(connection_id, &state.replica_id)
                .await;
            stop_realtime_heartbeat(state, connection_id).await;
            state.realtime.lock().await.detach(connection_id);
            if let Some(replaced) = attach_result
                .replaced
                .as_ref()
                .or(local_replaced_registration.as_ref())
                && let Err(error) = restore_replaced_registration(state, replaced).await
            {
                if is_new_connection {
                    unregister_ws_sender(state, connection_id).await;
                }
                return Err(error);
            }

            if let Some(session_before) = session_before {
                state
                    .sessions
                    .lock()
                    .await
                    .insert(session_code.to_string(), session_before);
            }
            if is_new_connection {
                unregister_ws_sender(state, connection_id).await;
            }
            return Err(format!("failed to persist websocket reconnect: {error}"));
        }
    }

    if let Some(replaced) = attach_result.replaced.as_ref() {
        if replaced.replica_id == state.replica_id {
            clear_local_realtime_connection(state, &replaced.connection_id).await;
        }
        let notification = SessionUpdateNotification::realtime_connection_replaced(replaced);
        let _ = state
            .store
            .publish_session_notification(&notification)
            .await;
        if replaced.replica_id == state.replica_id {
            close_local_connection(state, &replaced.connection_id).await;
        }
    }

    replace_realtime_heartbeat(
        state,
        connection_id,
        spawn_realtime_heartbeat(state.clone(), connection_id.to_string()),
    )
    .await;

    Ok(WsAttachOutcome {
        session_code: session_code.to_string(),
        state_changed,
    })
}

async fn send_ws_message(
    _state: &AppState,
    socket: &mut WebSocket,
    message: &ServerWsMessage,
) -> Result<(), ()> {
    #[cfg(test)]
    if matches!(message, ServerWsMessage::StateUpdate(_))
        && _state
            .fail_next_initial_state_send
            .swap(false, Ordering::SeqCst)
    {
        return Err(());
    }

    let encoded = serde_json::to_string(message).map_err(|_| ())?;
    socket
        .send(Message::Text(encoded.into()))
        .await
        .map_err(|_| ())
}

pub(crate) async fn advance_game_ticks(state: &AppState) {
    let session_codes: Vec<String> = {
        let sessions = state.sessions.lock().await;
        sessions
            .iter()
            .filter(|(_, session)| {
                session.phase == protocol::Phase::Phase1
                    || session.phase == protocol::Phase::Phase2
            })
            .map(|(code, _)| code.clone())
            .collect()
    };

    for session_code in session_codes {
        {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(&session_code) {
                session.advance_tick();
            }
        }

        // Persist asynchronously (best-effort; don't block the ticker)
        {
            let sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get(&session_code) {
                let _ = state.store.save_session(session).await;
            }
        }

        // Broadcast updated state to all connected players
        broadcast_session_state(state, &session_code, None).await;
    }
}
