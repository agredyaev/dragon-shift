use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use chrono::Utc;
use futures_util::{Sink, SinkExt, StreamExt};
use persistence::{RealtimeConnectionRegistration, SessionUpdateNotification};
use protocol::{
    ClientWsMessage, NoticeLevel, ServerWsMessage, SessionArtifactKind, SessionArtifactRecord,
    SessionEnvelope, SessionNoticeCode,
};
use serde_json::json;
#[cfg(test)]
use std::sync::atomic::Ordering;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::info;

use crate::app::AppState;
use crate::auth::SESSION_COOKIE_NAME;
use crate::cache::{
    SessionWriteLease, ensure_session_cached, reload_cached_session, session_write_lock,
};
use crate::helpers::{phase_label, phase_step, random_prefixed_id, to_client_game_state};
use crate::http::{
    MaybeConnectInfo, authorize_reconnect_identity, client_key, is_rate_limited,
    refresh_reconnect_identity, run_judge_for_session,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct WsAttachOutcome {
    session_code: String,
    state_changed: bool,
}

const WS_OUTBOUND_BUFFER: usize = 64;

#[allow(clippy::large_enum_variant)]
pub(crate) enum WsOutbound {
    Message(ServerWsMessage),
    MessageWithAck(ServerWsMessage, oneshot::Sender<Result<(), ()>>),
    MessageThenClose(ServerWsMessage),
    EncodedMessage(std::sync::Arc<str>),
    Raw(Message),
    Close,
}

impl std::fmt::Debug for WsOutbound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(message) => f.debug_tuple("Message").field(message).finish(),
            Self::MessageWithAck(message, _) => f
                .debug_tuple("MessageWithAck")
                .field(message)
                .field(&"<ack>")
                .finish(),
            Self::MessageThenClose(message) => {
                f.debug_tuple("MessageThenClose").field(message).finish()
            }
            Self::EncodedMessage(encoded) => {
                f.debug_tuple("EncodedMessage").field(encoded).finish()
            }
            Self::Raw(message) => f.debug_tuple("Raw").field(message).finish(),
            Self::Close => f.write_str("Close"),
        }
    }
}

fn try_send_ws(sender: &mpsc::Sender<WsOutbound>, message: WsOutbound) -> Result<(), ()> {
    sender.try_send(message).map_err(|_| ())
}

fn enqueue_initial_state(
    sender: &mpsc::Sender<WsOutbound>,
    client_state: protocol::ClientGameState,
) -> Result<oneshot::Receiver<Result<(), ()>>, ()> {
    let (ack_tx, ack_rx) = oneshot::channel();
    try_send_ws(
        sender,
        WsOutbound::MessageWithAck(ServerWsMessage::StateUpdate(client_state), ack_tx),
    )?;
    Ok(ack_rx)
}

async fn await_initial_state(
    state: &AppState,
    connection_id: &str,
    ack_rx: oneshot::Receiver<Result<(), ()>>,
) -> Result<(), ()> {
    ack_rx.await.map_err(|_| ())??;
    if state
        .realtime
        .lock()
        .await
        .contains_connection(connection_id)
    {
        Ok(())
    } else {
        Err(())
    }
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

pub(crate) async fn advance_overdue_phases(state: &AppState) {
    let now = Utc::now();
    let session_codes = {
        let sessions = state.sessions.lock().await;
        sessions
            .iter()
            .filter(|(_, session)| session.remaining_phase_seconds(now) == Some(0))
            .map(|(code, _)| code.clone())
            .collect::<Vec<_>>()
    };

    for session_code in session_codes {
        if let Err(error) = advance_overdue_phase(state, &session_code, now).await {
            tracing::warn!(session_code = %session_code, %error, "failed to auto-advance overdue phase");
        }
    }
}

async fn advance_overdue_phase(
    state: &AppState,
    session_code: &str,
    now: chrono::DateTime<Utc>,
) -> Result<(), String> {
    let (_, _write_guard, write_lease) = SessionWriteLease::acquire(state, session_code)
        .await
        .map_err(|error| format!("failed to acquire session lease: {error}"))?;
    write_lease
        .ensure_active()
        .map_err(|error| format!("lost session lease before auto-advance load: {error}"))?;
    if !ensure_session_cached(state, session_code).await? {
        return Ok(());
    }
    write_lease
        .ensure_active()
        .map_err(|error| format!("lost session lease before auto-advance mutation: {error}"))?;

    let (session_before, session_snapshot, artifact, from_phase, host_player_id) = {
        let mut sessions = state.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_code) else {
            return Ok(());
        };
        if session.remaining_phase_seconds(now) != Some(0) {
            return Ok(());
        }

        let session_before = session.clone();
        let from_phase = session.phase;
        match session.phase {
            protocol::Phase::Phase1 => session
                .transition_to(protocol::Phase::Handover)
                .map_err(|error| error.to_string())?,
            protocol::Phase::Handover => {
                session
                    .enter_phase2_after_deadline()
                    .map_err(|error| error.to_string())?;
            }
            protocol::Phase::Phase2 => {
                session.award_phase_end_achievements();
                session.enter_voting().map_err(|error| error.to_string())?;
            }
            _ => return Ok(()),
        }

        let session_snapshot = session.clone();
        let host_player_id = session.host_player_id.clone();
        let artifact = SessionArtifactRecord {
            id: random_prefixed_id("artifact"),
            session_id: session.id.to_string(),
            phase: session.phase,
            step: phase_step(session.phase),
            kind: SessionArtifactKind::PhaseChanged,
            player_id: None,
            created_at: Utc::now().to_rfc3339(),
            payload: json!({
                "auto": true,
                "fromPhase": from_phase,
                "toPhase": session.phase,
            }),
        };
        (
            session_before,
            session_snapshot,
            artifact,
            from_phase,
            host_player_id,
        )
    };

    if let Err(error) = write_lease.ensure_active() {
        state
            .sessions
            .lock()
            .await
            .insert(session_code.to_string(), session_before);
        return Err(format!(
            "lost session lease before auto-advance persist: {error}"
        ));
    }
    if let Err(error) = state
        .store
        .save_session_with_artifact(&session_snapshot, &artifact)
        .await
    {
        state
            .sessions
            .lock()
            .await
            .insert(session_code.to_string(), session_before);
        return Err(format!("failed to persist auto-advanced phase: {error}"));
    }

    drop(_write_guard);
    drop(write_lease);
    mark_session_dirty(state, session_code).await;
    if from_phase == protocol::Phase::Phase2 {
        let background_state = state.clone();
        let background_session_code = session_code.to_string();
        let background_player_id = host_player_id.unwrap_or_else(|| "server".to_string());
        tokio::spawn(async move {
            if let Err(error) = run_judge_for_session(
                &background_state,
                &background_session_code,
                &background_player_id,
            )
            .await
            {
                tracing::error!(
                    session_code = %background_session_code,
                    player_id = %background_player_id,
                    %error,
                    "background judge run failed after auto phase advance"
                );
            }
        });
    }
    Ok(())
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
        code: None,
    });

    let failed_connection_ids = {
        let senders = state.realtime_senders.lock().await;
        registrations
            .into_iter()
            .filter_map(
                |registration| match senders.get(&registration.connection_id) {
                    Some(sender) => try_send_ws(sender, WsOutbound::Message(notice.clone()))
                        .err()
                        .map(|_| registration.connection_id),
                    None => Some(registration.connection_id),
                },
            )
            .collect::<Vec<_>>()
    };

    force_close_failed_connections(state, failed_connection_ids).await;
}

pub(crate) async fn send_player_notice_with_code(
    state: &AppState,
    session_code: &str,
    player_id: &str,
    level: protocol::NoticeLevel,
    title: &str,
    message: &str,
    code: Option<SessionNoticeCode>,
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
        code,
    });

    let failed_connection_ids = {
        let senders = state.realtime_senders.lock().await;
        registrations
            .into_iter()
            .filter(|registration| registration.player_id == player_id)
            .filter_map(
                |registration| match senders.get(&registration.connection_id) {
                    Some(sender) => try_send_ws(sender, WsOutbound::Message(notice.clone()))
                        .err()
                        .map(|_| registration.connection_id),
                    None => Some(registration.connection_id),
                },
            )
            .collect::<Vec<_>>()
    };

    force_close_failed_connections(state, failed_connection_ids).await;
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

    // Soft cookie check: if a session cookie is present in the upgrade
    // request, verify it is not tampered. Reject tampered cookies but allow
    // upgrade when no cookie is present (the client will auth via
    // reconnect_token after the WebSocket is established).
    //
    // Also extract the authenticated account id from the signed cookie (if
    // any) so the post-upgrade attach path can assert `session.player
    // .account_id == cookie.account_id` for account-owned sessions. The
    // signed cookie value is the account id (see `build_session_cookie`), so
    // no store lookup is required here — equality of the signed value is
    // sufficient to bind the WS to the cookie holder.
    let cookie_account_id: Option<String> = {
        use axum_extra::extract::cookie::SignedCookieJar;
        let jar = SignedCookieJar::<axum_extra::extract::cookie::Key>::from_headers(
            &headers,
            state.config.cookie_key.clone(),
        );
        // Only check if the raw Cookie header actually contains our cookie
        // name — avoids rejecting requests that carry no cookie at all.
        let raw_cookies = headers
            .get(axum::http::header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if raw_cookies.contains(SESSION_COOKIE_NAME) && jar.get(SESSION_COOKIE_NAME).is_none() {
            return (StatusCode::UNAUTHORIZED, "Invalid session cookie.").into_response();
        }
        jar.get(SESSION_COOKIE_NAME).and_then(|cookie| {
            let value = cookie.value().to_string();
            if value.is_empty() { None } else { Some(value) }
        })
    };

    ws.on_upgrade(move |socket| handle_workshop_ws(state, socket, client_key, cookie_account_id))
        .into_response()
}

async fn handle_workshop_ws(
    state: AppState,
    socket: WebSocket,
    client_key: String,
    cookie_account_id: Option<String>,
) {
    let (mut socket_tx, mut socket_rx) = socket.split();
    let (outbound_tx, mut outbound_rx) = mpsc::channel(WS_OUTBOUND_BUFFER);
    let (force_close_tx, force_close_rx) = oneshot::channel::<()>();
    let mut force_close_rx = Box::pin(force_close_rx);
    let mut force_close_tx = Some(force_close_tx);
    let writer_state = state.clone();
    let mut writer_task = tokio::spawn(async move {
        while let Some(outbound_message) = outbound_rx.recv().await {
            match outbound_message {
                WsOutbound::Message(outbound_message) => {
                    if send_ws_message(&writer_state, &mut socket_tx, &outbound_message)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                WsOutbound::MessageWithAck(outbound_message, ack) => {
                    let result =
                        send_ws_message(&writer_state, &mut socket_tx, &outbound_message).await;
                    let _ = ack.send(result);
                }
                WsOutbound::MessageThenClose(outbound_message) => {
                    let _ = send_ws_message(&writer_state, &mut socket_tx, &outbound_message).await;
                    break;
                }
                WsOutbound::EncodedMessage(encoded) => {
                    if send_encoded_ws_message(&mut socket_tx, encoded)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                WsOutbound::Raw(message) => {
                    if socket_tx.send(message).await.is_err() {
                        break;
                    }
                }
                WsOutbound::Close => break,
            }
        }
    });
    let mut attached_connection_id: Option<String> = None;
    let mut registered_connection_id: Option<String> = None;
    let mut writer_finished = false;
    let mut wait_for_writer_close = false;

    loop {
        tokio::select! {
            _ = &mut force_close_rx => {
                break;
            }
            _ = &mut writer_task => {
                writer_finished = true;
                break;
            }
            message_result = socket_rx.next() => {
                let Some(message_result) = message_result else {
                    break;
                };
                let Ok(message) = message_result else {
                    break;
                };

                if is_rate_limited(&state.websocket_limiter, &client_key).await {
                    if try_send_ws(
                        &outbound_tx,
                        WsOutbound::Message(ServerWsMessage::Error {
                            message: "Too many requests. Please slow down and try again.".to_string(),
                        }),
                    )
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
                                    if try_send_ws(&outbound_tx, WsOutbound::Message(ServerWsMessage::Error {
                                        message: "WebSocket is already attached to a different player.".to_string(),
                                    }))
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
                                if try_send_ws(
                                    &outbound_tx,
                                    WsOutbound::MessageThenClose(ServerWsMessage::Error {
                                        message: "connection is closed".to_string(),
                                    }),
                                )
                                .is_err()
                                {
                                    break;
                                }
                                wait_for_writer_close = true;
                                break;
                            }
                            if is_new_connection {
                                register_ws_sender(&state, &connection_id, outbound_tx.clone())
                                    .await;
                                register_ws_close_signal(
                                    &state,
                                    &connection_id,
                                    force_close_tx.take(),
                                )
                                .await;
                                registered_connection_id = Some(connection_id.clone());
                            }
                            match attach_ws_session(
                                &state,
                                &outbound_tx,
                                &envelope,
                                &connection_id,
                                is_new_connection,
                                cookie_account_id.as_deref(),
                            )
                            .await
                            {
                                Ok(outcome) => {
                                    attached_connection_id = Some(connection_id.clone());
                                    if outcome.state_changed {
                                        broadcast_session_state(
                                            &state,
                                            &outcome.session_code,
                                            Some(connection_id.as_str()),
                                        )
                                        .await;
                                    }
                                }
                                Err(error_message) => {
                                    if is_new_connection {
                                        unregister_ws_sender(&state, &connection_id).await;
                                    }
                                    if try_send_ws(
                                        &outbound_tx,
                                        WsOutbound::MessageThenClose(ServerWsMessage::Error {
                                            message: error_message,
                                        }),
                                    )
                                    .is_err()
                                    {
                                        break;
                                    }
                                    wait_for_writer_close = true;
                                    break;
                                }
                            }
                        }
                        Ok(ClientWsMessage::Ping) => {
                            if try_send_ws(&outbound_tx, WsOutbound::Message(ServerWsMessage::Pong)).is_err() {
                                break;
                            }
                        }
                        Err(_) => {
                            if try_send_ws(
                                &outbound_tx,
                                WsOutbound::Message(ServerWsMessage::Error {
                                    message: "Invalid WebSocket payload.".to_string(),
                                }),
                            )
                            .is_err()
                            {
                                break;
                            }
                        }
                    },
                    Message::Ping(payload) => {
                        if try_send_ws(&outbound_tx, WsOutbound::Raw(Message::Pong(payload))).is_err() {
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
        unregister_close_signal(&state, &connection_id).await;
        sync_ws_disconnect(&state, &connection_id).await;
        state
            .retired_realtime_connections
            .lock()
            .await
            .remove(&connection_id);
    } else if let Some(connection_id) = registered_connection_id {
        unregister_ws_sender(&state, &connection_id).await;
        unregister_close_signal(&state, &connection_id).await;
    }
    if let Some(force_close_tx) = force_close_tx.take() {
        drop(force_close_tx);
    }

    if !writer_finished && wait_for_writer_close {
        let _ = writer_task.await;
    } else if !writer_finished {
        writer_task.abort();
        let _ = writer_task.await;
    }
}

async fn register_ws_sender(
    state: &AppState,
    connection_id: &str,
    sender: mpsc::Sender<WsOutbound>,
) {
    state
        .realtime_senders
        .lock()
        .await
        .insert(connection_id.to_string(), sender);
}

async fn register_ws_close_signal(
    state: &AppState,
    connection_id: &str,
    close_signal: Option<oneshot::Sender<()>>,
) {
    if let Some(close_signal) = close_signal {
        state
            .realtime_close_signals
            .lock()
            .await
            .insert(connection_id.to_string(), close_signal);
    }
}

async fn unregister_ws_sender(state: &AppState, connection_id: &str) {
    state.realtime_senders.lock().await.remove(connection_id);
}

async fn unregister_close_signal(state: &AppState, connection_id: &str) {
    state
        .realtime_close_signals
        .lock()
        .await
        .remove(connection_id);
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

pub(crate) async fn restore_replaced_registration(
    state: &AppState,
    replaced: &RealtimeConnectionRegistration,
) -> Result<(), String> {
    let is_replaced_connection_local = replaced.replica_id == state.replica_id;
    if is_replaced_connection_local
        && !state
            .realtime_senders
            .lock()
            .await
            .contains_key(&replaced.connection_id)
    {
        return Ok(());
    }

    let restored = state
        .store
        .restore_realtime_connection(replaced)
        .await
        .map_err(|error| format!("failed to restore replaced realtime connection: {error}"))?;
    if restored.restored && is_replaced_connection_local {
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
        if !state
            .realtime_senders
            .lock()
            .await
            .contains_key(&replaced.connection_id)
        {
            stop_realtime_heartbeat(state, &replaced.connection_id).await;
            state.realtime.lock().await.detach(&replaced.connection_id);
            let _ = state
                .store
                .release_realtime_connection(&replaced.connection_id, &state.replica_id)
                .await;
        }
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
        if try_send_ws(&sender, WsOutbound::Close).is_err() {
            force_close_local_connection(state, connection_id).await;
        }
    }
}

async fn force_close_local_connection(state: &AppState, connection_id: &str) {
    state.realtime_senders.lock().await.remove(connection_id);
    state.realtime.lock().await.detach(connection_id);
    stop_realtime_heartbeat(state, connection_id).await;
    if let Some(close_signal) = state
        .realtime_close_signals
        .lock()
        .await
        .remove(connection_id)
    {
        let _ = close_signal.send(());
    }
}

async fn force_close_failed_connections(state: &AppState, connection_ids: Vec<String>) {
    for connection_id in connection_ids {
        force_close_local_connection(state, &connection_id).await;
    }
}

pub(crate) async fn close_local_workshop_connections(
    state: &AppState,
    session_code: &str,
    error_message: Option<&str>,
) {
    let registrations = state
        .realtime
        .lock()
        .await
        .session_registrations(session_code);
    for registration in registrations {
        clear_local_realtime_connection(state, &registration.connection_id).await;
        if let Some(message) = error_message {
            if let Some(sender) = state
                .realtime_senders
                .lock()
                .await
                .get(&registration.connection_id)
                .cloned()
            {
                if try_send_ws(
                    &sender,
                    WsOutbound::Message(ServerWsMessage::Error {
                        message: message.to_string(),
                    }),
                )
                .is_err()
                {
                    force_close_local_connection(state, &registration.connection_id).await;
                }
            }
        }
        close_local_connection(state, &registration.connection_id).await;
    }
}

/// Mark a session as needing a client state broadcast. The actual broadcast
/// is delayed by ~150ms and collapsed with other pending marks for the same
/// session. Use this for post-mutation broadcasts where `excluded_connection_id`
/// is `None`.
pub(crate) async fn mark_session_dirty(state: &AppState, session_code: &str) {
    let should_schedule_drain = {
        let mut dirty = state.dirty_sessions.lock().await;
        let should_schedule_drain = dirty.is_empty();
        dirty.insert(session_code.to_string());
        should_schedule_drain
    };

    if should_schedule_drain {
        let drain_state = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            drain_dirty_sessions_once(&drain_state).await;
        });
    }
}

pub(crate) async fn drain_dirty_sessions_once(state: &AppState) {
    let codes: Vec<String> = {
        let mut dirty = state.dirty_sessions.lock().await;
        std::mem::take(&mut *dirty).into_iter().collect()
    };
    for code in codes {
        broadcast_session_state(state, &code, None).await;
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

    let registrations = state
        .realtime
        .lock()
        .await
        .session_registrations(session_code);
    if registrations.is_empty() {
        return;
    }

    let Some(session) = ({
        let sessions = state.sessions.lock().await;
        sessions.get(session_code).cloned()
    }) else {
        return;
    };

    let messages = registrations
        .into_iter()
        .filter(|registration| Some(registration.connection_id.as_str()) != excluded_connection_id)
        .filter_map(|registration| {
            let message = ServerWsMessage::StateUpdate(to_client_game_state(
                &session,
                &registration.player_id,
            ));
            encode_ws_message(&message)
                .ok()
                .map(|encoded| (registration.connection_id, encoded))
        })
        .collect::<Vec<_>>();

    if messages.is_empty() {
        return;
    }

    let failed_connection_ids = {
        let senders = state.realtime_senders.lock().await;
        messages
            .into_iter()
            .filter_map(
                |(connection_id, encoded)| match senders.get(&connection_id) {
                    Some(sender) => try_send_ws(sender, WsOutbound::EncodedMessage(encoded))
                        .err()
                        .map(|_| connection_id),
                    None => Some(connection_id),
                },
            )
            .collect::<Vec<_>>()
    };

    force_close_failed_connections(state, failed_connection_ids).await;
}

pub(crate) async fn broadcast_player_upsert(state: &AppState, session_code: &str, player_id: &str) {
    broadcast_session_delta(state, session_code, |session, registration| {
        let client_state = to_client_game_state(session, &registration.player_id);
        client_state
            .players
            .get(player_id)
            .cloned()
            .map(|player| ServerWsMessage::PlayerUpsert {
                state_revision: client_state.session.state_revision,
                player,
            })
            .into_iter()
            .collect()
    })
    .await;
}

pub(crate) async fn broadcast_dragon_patch(state: &AppState, session_code: &str, dragon_id: &str) {
    broadcast_session_delta(state, session_code, |session, registration| {
        let client_state = to_client_game_state(session, &registration.player_id);
        client_state
            .dragons
            .get(dragon_id)
            .cloned()
            .map(|dragon| ServerWsMessage::DragonPatch {
                state_revision: client_state.session.state_revision,
                dragon,
            })
            .into_iter()
            .collect()
    })
    .await;
}

pub(crate) async fn broadcast_player_and_dragon_patch(
    state: &AppState,
    session_code: &str,
    player_id: &str,
    dragon_id: &str,
) {
    broadcast_session_delta(state, session_code, |session, registration| {
        let client_state = to_client_game_state(session, &registration.player_id);
        let mut messages = Vec::new();
        if let Some(player) = client_state.players.get(player_id).cloned() {
            messages.push(ServerWsMessage::PlayerUpsert {
                state_revision: client_state.session.state_revision,
                player,
            });
        }
        if let Some(dragon) = client_state.dragons.get(dragon_id).cloned() {
            messages.push(ServerWsMessage::DragonPatch {
                state_revision: client_state.session.state_revision,
                dragon,
            });
        }
        messages
    })
    .await;
}

async fn broadcast_session_delta(
    state: &AppState,
    session_code: &str,
    message_for: impl Fn(
        &domain::WorkshopSession,
        &realtime::ConnectionRegistration,
    ) -> Vec<ServerWsMessage>,
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

    let Some(session) = ({
        let sessions = state.sessions.lock().await;
        sessions.get(session_code).cloned()
    }) else {
        return;
    };

    let messages = registrations
        .into_iter()
        .flat_map(|registration| {
            let connection_id = registration.connection_id.clone();
            message_for(&session, &registration)
                .into_iter()
                .map(move |message| (connection_id.clone(), message))
        })
        .collect::<Vec<_>>();
    if messages.is_empty() {
        return;
    }

    let failed_connection_ids = {
        let senders = state.realtime_senders.lock().await;
        messages
            .into_iter()
            .filter_map(
                |(connection_id, message)| match senders.get(&connection_id) {
                    Some(sender) => try_send_ws(sender, WsOutbound::Message(message))
                        .err()
                        .map(|_| connection_id),
                    None => Some(connection_id),
                },
            )
            .collect::<Vec<_>>()
    };

    force_close_failed_connections(state, failed_connection_ids).await;
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
                session.touch();
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

    mark_session_dirty(state, &session.code.0).await;
}

async fn attach_ws_session(
    state: &AppState,
    outbound_tx: &mpsc::Sender<WsOutbound>,
    envelope: &SessionEnvelope,
    connection_id: &str,
    is_new_connection: bool,
    cookie_account_id: Option<&str>,
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
        // Bind the WS identity to the authenticated account cookie. If the
        // session player is account-owned (post-session-4 signed-in flow),
        // the upgrade request's signed `ds_session` cookie must carry that
        // same account id. Legacy anonymous players (`account_id: None`,
        // e.g. seeded test fixtures) skip the check so we don't regress
        // cookie-less flows. Mismatches fail closed — the client should
        // sign in with the correct account and reconnect.
        {
            let player = session
                .players
                .get(&identity.player_id)
                .ok_or_else(|| "Session identity is invalid or expired.".to_string())?;
            if let Some(expected_account_id) = player.account_id.as_deref() {
                let matches = cookie_account_id
                    .map(|observed| observed == expected_account_id)
                    .unwrap_or(false);
                if !matches {
                    // Correlate without leaking signed cookie bytes: log the
                    // expected account id and whether a cookie was present
                    // (but never its value).
                    let observed_repr = match cookie_account_id {
                        Some(_) => "mismatch",
                        None => "none",
                    };
                    tracing::warn!(
                        session_code = %session_code,
                        player_id = %identity.player_id,
                        expected_account_id = %expected_account_id,
                        observed_account = observed_repr,
                        "ws attach rejected: session identity does not match authenticated account cookie"
                    );
                    return Err(
                        "Session identity does not match authenticated account.".to_string()
                    );
                }
            }
        }
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
            session.touch();
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
    let initial_state_ack = match enqueue_initial_state(outbound_tx, client_state) {
        Ok(ack) => ack,
        Err(_) => {
            let _ = state
                .store
                .release_realtime_connection(connection_id, &state.replica_id)
                .await;
            if let Some(replaced) = attach_result.replaced.as_ref()
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
    };
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

    if await_initial_state(state, connection_id, initial_state_ack)
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

async fn send_ws_message<S>(
    _state: &AppState,
    socket: &mut S,
    message: &ServerWsMessage,
) -> Result<(), ()>
where
    S: Sink<Message> + Unpin,
{
    #[cfg(test)]
    if matches!(message, ServerWsMessage::StateUpdate(_))
        && _state
            .fail_next_initial_state_send
            .swap(false, Ordering::SeqCst)
    {
        return Err(());
    }

    let encoded = encode_ws_message(message)?;
    send_encoded_ws_message(socket, encoded).await
}

fn encode_ws_message(message: &ServerWsMessage) -> Result<std::sync::Arc<str>, ()> {
    serde_json::to_string(message)
        .map(std::sync::Arc::<str>::from)
        .map_err(|_| ())
}

async fn send_encoded_ws_message<S>(socket: &mut S, encoded: std::sync::Arc<str>) -> Result<(), ()>
where
    S: Sink<Message> + Unpin,
{
    socket
        .send(Message::Text(encoded.to_string().into()))
        .await
        .map_err(|_| ())
}

pub(crate) async fn advance_game_ticks(state: &AppState) {
    let session_codes: Vec<String> = {
        let now = Utc::now();
        let sessions = state.sessions.lock().await;
        sessions
            .iter()
            .filter(|(_, session)| {
                (session.phase == protocol::Phase::Phase1
                    || session.phase == protocol::Phase::Phase2)
                    && session.remaining_phase_seconds(now) != Some(0)
            })
            .map(|(code, _)| code.clone())
            .collect()
    };

    for session_code in session_codes {
        let write_lock = session_write_lock(state, &session_code).await;
        let _write_guard = write_lock.lock().await;
        let session_snapshot = {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(&session_code) {
                let awarded_achievements = session.advance_tick();
                Some((session.clone(), !awarded_achievements.is_empty()))
            } else {
                None
            }
        };

        // Persist tick-decayed state periodically so that command-handler reloads
        // (reload_cached_session) pick up recent stats. Throttled to every 5 ticks
        // to reduce write pressure on Postgres (was OOM-killed at 1 write/sec).
        if let Some((snapshot, has_awards)) = &session_snapshot {
            if *has_awards || snapshot.time % 5 == 0 {
                if let Err(error) = state.store.save_session(snapshot).await {
                    info!(session_code = %session_code, error = %error, "failed to persist tick state");
                }
            }
        }

        drop(_write_guard);

        // Broadcast updated state to all connected players
        mark_session_dirty(state, &session_code).await;
    }
}
