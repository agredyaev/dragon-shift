mod app;
mod cache;
mod helpers;
mod http;
mod llm;
#[cfg(test)]
mod tests;
mod ws;

use std::sync::Arc;

use app::{
    AppState, build_app, build_session_store, load_config, load_fallback_companion_sprites,
};
use persistence::SessionUpdateNotification;
use tracing::info;
use ws::{
    advance_game_ticks, broadcast_session_state, clear_local_realtime_connection,
    close_local_connection, emit_phase_warning_notices,
};

pub(crate) fn parse_session_update_notification(
    payload: &str,
) -> Option<SessionUpdateNotification> {
    let payload = payload.trim();
    if payload.is_empty() {
        return None;
    }

    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| {
            let session_code = value.get("sessionCode")?.as_str()?.trim();
            if session_code.is_empty() {
                return None;
            }
            Some(SessionUpdateNotification {
                kind: value
                    .get("kind")
                    .and_then(|value| value.as_str())
                    .unwrap_or("session_state_changed")
                    .to_string(),
                session_code: session_code.to_string(),
                updated_at: value
                    .get("updatedAt")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                payload_fingerprint: value
                    .get("payloadFingerprint")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                connection_id: value
                    .get("connectionId")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                replica_id: value
                    .get("replicaId")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
            })
        })
        .or_else(|| {
            Some(SessionUpdateNotification {
                kind: "session_state_changed".to_string(),
                session_code: payload.to_string(),
                updated_at: None,
                payload_fingerprint: None,
                connection_id: None,
                replica_id: None,
            })
        })
}

pub(crate) async fn handle_session_update_notification(
    state: &AppState,
    notification: &SessionUpdateNotification,
) {
    if notification.kind == "realtime_connection_replaced" {
        if notification.replica_id.as_deref() == Some(state.replica_id.as_str())
            && let Some(connection_id) = notification.connection_id.as_deref()
        {
            clear_local_realtime_connection(state, connection_id).await;
            close_local_connection(state, connection_id).await;
        }
        return;
    }

    let typed_payload_fingerprint = notification.payload_fingerprint.clone();
    let (is_cached, has_registrations, should_skip) = {
        let realtime = state.realtime.lock().await;
        let has_registrations = !realtime
            .session_registrations(&notification.session_code)
            .is_empty();
        drop(realtime);

        let sessions = state.sessions.lock().await;
        let is_cached = sessions.contains_key(&notification.session_code);
        let should_skip = sessions
            .get(&notification.session_code)
            .and_then(|session| {
                notification.updated_at.as_deref().map(|updated_at| {
                    session.updated_at.to_rfc3339() == updated_at
                        && notification.payload_fingerprint.as_deref().is_none_or(
                            |payload_fingerprint| {
                                SessionUpdateNotification::session_state_changed(session)
                                    .payload_fingerprint
                                    .as_deref()
                                    == Some(payload_fingerprint)
                            },
                        )
                })
            })
            .unwrap_or(false);
        (is_cached, has_registrations, should_skip)
    };

    if notification.updated_at.is_none() {
        let expected_fingerprint = {
            state
                .recent_session_notifications
                .lock()
                .await
                .remove(&notification.session_code)
        };
        if let Some(expected_fingerprint) = expected_fingerprint {
            let persisted_matches = state
                .store
                .load_session_by_code(&notification.session_code)
                .await
                .ok()
                .flatten()
                .map(|session| {
                    SessionUpdateNotification::session_state_changed(&session)
                        .payload_fingerprint
                        .expect("typed notification fingerprint")
                        == expected_fingerprint
                })
                .unwrap_or(false);
            if persisted_matches {
                return;
            }
        }
    }

    if !is_cached && !has_registrations {
        return;
    }

    if should_skip {
        if let Some(payload_fingerprint) = typed_payload_fingerprint {
            state
                .recent_session_notifications
                .lock()
                .await
                .insert(notification.session_code.clone(), payload_fingerprint);
        }
        return;
    }

    state
        .sessions
        .lock()
        .await
        .remove(&notification.session_code);

    if !has_registrations {
        return;
    }

    broadcast_session_state(state, &notification.session_code, None).await;

    if let Some(payload_fingerprint) = typed_payload_fingerprint {
        state
            .recent_session_notifications
            .lock()
            .await
            .insert(notification.session_code.clone(), payload_fingerprint);
    } else {
        state
            .recent_session_notifications
            .lock()
            .await
            .remove(&notification.session_code);
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Arc::new(load_config().expect("load app config"));
    let store = build_session_store(&config)
        .await
        .expect("build session store");
    store.init().await.expect("init session store");
    let fallback_companion_sprites = load_fallback_companion_sprites(&store)
        .await
        .expect("load fallback companion sprites");

    let state = AppState::new(config.clone(), store, fallback_companion_sprites);

    if let Some(database_url) = state.config.database_url.as_deref() {
        let listener_state = state.clone();
        let database_url = database_url.to_string();
        tokio::spawn(async move {
            loop {
                match sqlx::postgres::PgListener::connect(&database_url).await {
                    Ok(mut listener) => {
                        if let Err(error) = listener.listen("session_updates").await {
                            tracing::warn!(%error, "PgListener failed to subscribe, retrying in 5s");
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            continue;
                        }
                        loop {
                            match listener.recv().await {
                                Ok(notification) => {
                                    let Some(notification) =
                                        parse_session_update_notification(notification.payload())
                                    else {
                                        continue;
                                    };
                                    handle_session_update_notification(
                                        &listener_state,
                                        &notification,
                                    )
                                    .await;
                                }
                                Err(error) => {
                                    tracing::warn!(%error, "PgListener recv error, reconnecting in 5s");
                                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                    break;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "PgListener connect failed, retrying in 5s");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        });
    }

    let ticker_state = state.clone();
    let app = build_app(state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            advance_game_ticks(&ticker_state).await;
            emit_phase_warning_notices(&ticker_state).await;
        }
    });

    info!(bind_addr = %config.bind_addr, "starting platform app-server");

    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .expect("bind listener");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .expect("serve app");
}
