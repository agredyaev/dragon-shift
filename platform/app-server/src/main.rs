use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    http::HeaderMap,
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    routing::get,
    Json, Router,
};
use chrono::Utc;
use domain::{DomainError, Phase1Assignment, PlayerAction, SessionCode, SessionPlayer, WorkshopSession};
use persistence::{InMemorySessionStore, PostgresSessionStore, SessionStore};
use protocol::{
    create_default_session_settings, ActionPayload, ClientGameState, ClientWsMessage, CoordinatorType,
    CreateWorkshopRequest, DiscoveryObservationRequest, DragonStats, FoodType, JudgeActionTrace,
    JudgeBundle, JudgeDragonBundle, JudgeHandoverChain, JudgePlayerSummary, JoinWorkshopRequest,
    PlayType, Player, SessionArtifactKind, SessionArtifactRecord, SessionMeta, ServerWsMessage,
    SessionCommand, SessionEnvelope, VotePayload, WorkshopCommandRequest, WorkshopCommandResult,
    WorkshopCommandSuccess, WorkshopError, WorkshopJoinResult, WorkshopJoinSuccess,
    WorkshopJudgeBundleRequest, WorkshopJudgeBundleResult, WorkshopJudgeBundleSuccess,
};
use realtime::SessionRegistry;
use security::{
    create_origin_policy, validate_session_code, FixedWindowRateLimiter, OriginPolicy, OriginPolicyOptions,
    DEFAULT_RUST_SESSION_CODE_PREFIX,
};
use serde::Serialize;
use serde_json::json;
use std::{
    collections::BTreeMap, env, net::SocketAddr, path::PathBuf, str::FromStr, sync::Arc,
};
use tokio::sync::{mpsc, Mutex};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing::info;
use uuid::Uuid;

#[derive(Debug, Clone)]
struct AppConfig {
    bind_addr: SocketAddr,
    is_production: bool,
    rust_session_code_prefix: String,
    origin_policy: OriginPolicy,
    static_assets_dir: PathBuf,
    database_url: Option<String>,
    persistence_backend: String,
}

#[derive(Clone)]
struct AppState {
    config: Arc<AppConfig>,
    store: Arc<dyn SessionStore>,
    sessions: Arc<Mutex<BTreeMap<String, WorkshopSession>>>,
    create_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    join_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    realtime: Arc<Mutex<SessionRegistry>>,
    realtime_senders: Arc<Mutex<BTreeMap<String, mpsc::UnboundedSender<ServerWsMessage>>>>,
}

#[derive(Debug, Serialize)]
struct RuntimeSnapshot {
    bind_addr: String,
    is_production: bool,
    rust_session_code_prefix: String,
    persistence_backend: String,
    allow_any_origin: bool,
    require_origin: bool,
    allowed_origins: Vec<String>,
    active_realtime_sessions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WsAttachOutcome {
    session_code: String,
    replaced_connection_id: Option<String>,
    state_changed: bool,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Arc::new(load_config().expect("load app config"));
    let store = build_session_store(&config).expect("build session store");
    store.init().expect("init session store");

    let state = AppState {
        config: config.clone(),
        store,
        sessions: Arc::new(Mutex::new(BTreeMap::new())),
        create_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(20, 60_000))),
        join_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(40, 60_000))),
        realtime: Arc::new(Mutex::new(SessionRegistry::new())),
        realtime_senders: Arc::new(Mutex::new(BTreeMap::new())),
    };

    let app = build_app(state);

    info!(bind_addr = %config.bind_addr, "starting platform app-server");

    let bind_addr = config.bind_addr;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(bind_addr)
            .await
            .expect("bind listener");
        axum::serve(listener, app).await.expect("serve app");
    });
}

fn build_app(state: AppState) -> Router {
    let static_assets_dir = state.config.static_assets_dir.clone();
    let index_file = static_assets_dir.join("index.html");

    Router::new()
        .route("/api/workshops", post(create_workshop))
        .route("/api/workshops/join", post(join_workshop))
        .route("/api/workshops/command", post(workshop_command))
        .route("/api/workshops/ws", get(workshop_ws))
        .route("/api/workshops/judge-bundle", post(workshop_judge_bundle))
        .route("/api/live", get(live))
        .route("/api/ready", get(ready))
        .route("/api/runtime", get(runtime_snapshot))
        .fallback_service(ServeDir::new(static_assets_dir).not_found_service(ServeFile::new(index_file)))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

fn build_session_store(config: &AppConfig) -> Result<Arc<dyn SessionStore>, String> {
    if let Some(database_url) = config.database_url.as_deref() {
        let store = PostgresSessionStore::connect(database_url)
            .map_err(|error| format!("connect postgres session store: {error}"))?;
        Ok(Arc::new(store))
    } else {
        Ok(Arc::new(InMemorySessionStore::new()))
    }
}

fn load_config() -> Result<AppConfig, String> {
    let bind_addr = env::var("APP_SERVER_BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:4100".to_string());
    let bind_addr = SocketAddr::from_str(&bind_addr)
        .map_err(|error| format!("invalid APP_SERVER_BIND_ADDR: {error}"))?;

    let is_production = env::var("NODE_ENV")
        .map(|value| value == "production")
        .unwrap_or(false);
    let rust_session_code_prefix = env::var("RUST_SESSION_CODE_PREFIX")
        .ok()
        .filter(|value| value.len() == 1 && value.chars().all(|ch| ch.is_ascii_digit()))
        .unwrap_or_else(|| DEFAULT_RUST_SESSION_CODE_PREFIX.to_string());
    let origin_policy = create_origin_policy(OriginPolicyOptions {
        allowed_origins: env::var("ALLOWED_ORIGINS").ok().as_deref(),
        app_origin: env::var("VITE_APP_URL").ok().as_deref(),
        is_production,
    })
    .map_err(|error| error.to_string())?;
    let static_assets_dir = env::var("APP_SERVER_STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("workspace root")
                .join("app-web/dist")
        });
    let database_url = env::var("DATABASE_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if is_production && database_url.is_none() {
        return Err("DATABASE_URL is required when NODE_ENV=production".to_string());
    }
    let persistence_backend = if database_url.is_some() {
        "postgres".to_string()
    } else {
        "memory".to_string()
    };

    Ok(AppConfig {
        bind_addr,
        is_production,
        rust_session_code_prefix,
        origin_policy,
        static_assets_dir,
        database_url,
        persistence_backend,
    })
}

async fn ensure_session_cached(state: &AppState, session_code: &str) -> Result<bool, String> {
    {
        let sessions = state.sessions.lock().await;
        if sessions.contains_key(session_code) {
            return Ok(true);
        }
    }

    let Some(session) = state
        .store
        .load_session_by_code(session_code)
        .map_err(|error| format!("failed to load session: {error}"))?
    else {
        return Ok(false);
    };

    let mut sessions = state.sessions.lock().await;
    sessions.entry(session.code.0.clone()).or_insert(session);
    Ok(true)
}

async fn workshop_ws(
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
                            let connection_id = attached_connection_id
                                .clone()
                                .unwrap_or_else(|| random_prefixed_id("conn"));
                            match attach_ws_session(&state, &mut socket, &envelope, &connection_id).await {
                                Ok(outcome) => {
                                    register_ws_sender(&state, &connection_id, outbound_tx.clone()).await;
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

async fn broadcast_session_state(
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

    let registrations = state.realtime.lock().await.session_registrations(session_code);
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
            .filter(|registration| Some(registration.connection_id.as_str()) != excluded_connection_id)
            .map(|registration| {
                (
                    registration.connection_id,
                    ServerWsMessage::StateUpdate(to_client_game_state(session, &registration.player_id)),
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
            .filter_map(|(connection_id, message)| match senders.get(&connection_id) {
                Some(sender) => sender.send(message).err().map(|_| connection_id),
                None => None,
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

async fn sync_ws_disconnect(state: &AppState, connection_id: &str) {
    let Some(registration) = state.realtime.lock().await.detach(connection_id) else {
        return;
    };

    let timestamp = Utc::now();
    let session_code = registration.session_code;
    let player_id = registration.player_id;
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

    if let Err(error) = state.store.save_session(&session) {
        info!(session_code = %session.code.0, player_id = %player_id, error = %error, "failed to persist websocket disconnect session state");
    }

    if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
        id: random_prefixed_id("artifact"),
        session_id: session.id.to_string(),
        phase: session.phase,
        step,
        kind: SessionArtifactKind::PlayerLeft,
        player_id: Some(player_id.clone()),
        created_at: timestamp.to_rfc3339(),
        payload: disconnect_payload,
    }) {
        info!(session_code = %session.code.0, player_id = %player_id, error = %error, "failed to append websocket disconnect artifact");
    }

    broadcast_session_state(state, &session.code.0, None).await;
}

async fn attach_ws_session(
    state: &AppState,
    socket: &mut WebSocket,
    envelope: &SessionEnvelope,
    connection_id: &str,
) -> Result<WsAttachOutcome, String> {
    let session_code = envelope.session_code.trim();
    let reconnect_token = envelope.reconnect_token.trim();
    let player_id = envelope.player_id.trim();
    if session_code.is_empty()
        || reconnect_token.is_empty()
        || player_id.is_empty()
        || validate_session_code(session_code).is_err()
    {
        return Err("Missing workshop credentials.".to_string());
    }

    let identity = state
        .store
        .find_player_identity(session_code, reconnect_token)
        .map_err(|error| format!("failed to lookup identity: {error}"))?
        .ok_or_else(|| "Session identity is invalid or expired.".to_string())?;
    if identity.player_id != player_id {
        return Err("Session identity is invalid or expired.".to_string());
    }
    if !ensure_session_cached(state, session_code).await? {
        return Err("Workshop not found.".to_string());
    }

    let mut reconnect_artifact: Option<SessionArtifactRecord> = None;

    let client_state = {
        let mut sessions = state.sessions.lock().await;
        let session = sessions
            .get_mut(session_code)
            .ok_or_else(|| "Workshop not found.".to_string())?;
        let player = session
            .players
            .get_mut(&identity.player_id)
            .ok_or_else(|| "Session identity is invalid or expired.".to_string())?;
        if !player.is_connected {
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
        if reconnect_artifact.is_some() {
            if let Err(error) = state.store.save_session(session) {
                return Err(format!("failed to save session: {error}"));
            }
        }
        client_state
    };

    let state_changed = reconnect_artifact.is_some();

    if let Some(artifact) = reconnect_artifact {
        state
            .store
            .append_session_artifact(&artifact)
            .map_err(|error| format!("failed to append reconnect artifact: {error}"))?;
    }

    let attach_result = state
        .realtime
        .lock()
        .await
        .attach(session_code, &identity.player_id, connection_id);

    send_ws_message(socket, &ServerWsMessage::StateUpdate(client_state))
        .await
        .map_err(|_| "connection is closed".to_string())?;

    Ok(WsAttachOutcome {
        session_code: session_code.to_string(),
        replaced_connection_id: attach_result.replaced_connection_id,
        state_changed,
    })
}

async fn send_ws_message(socket: &mut WebSocket, message: &ServerWsMessage) -> Result<(), ()> {
    let encoded = serde_json::to_string(message).map_err(|_| ())?;
    socket.send(Message::Text(encoded.into())).await.map_err(|_| ())
}

async fn live() -> Json<serde_json::Value> {
    Json(json!({ "ok": true, "service": "app-server", "status": "live" }))
}

async fn create_workshop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateWorkshopRequest>,
) -> (StatusCode, Json<WorkshopJoinResult>) {
    if let Some(response) = reject_disallowed_origin(&headers, &state.config.origin_policy) {
        return response;
    }
    if let Some(response) = reject_rate_limited(&state.create_limiter, client_key_from_headers(&headers)).await {
        return response;
    }
    let normalized_name = payload.name.trim();
    if normalized_name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(WorkshopJoinResult::Error(WorkshopError {
                ok: false,
                error: "Please enter a host name.".to_string(),
            })),
        );
    }

    let timestamp = Utc::now();
    let session_code = allocate_session_code(&state).await;
    let player_id = random_prefixed_id("player");
    let reconnect_token = random_prefixed_id("reconnect");
    let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode(session_code.clone()), timestamp);
    let host_player = SessionPlayer {
        id: player_id.clone(),
        name: normalized_name.to_string(),
        pet_description: Some(format!("{}'s workshop dragon", normalized_name)),
        is_host: true,
        is_connected: true,
        is_ready: false,
        score: 0,
        current_dragon_id: None,
        achievements: Vec::new(),
        joined_at: timestamp,
    };
    session.add_player(host_player.clone());

    if let Err(error) = state.store.save_session(&session) {
        return internal_join_error(format!("failed to save session: {error}"));
    }
    if let Err(error) = state.store.create_player_identity(&persistence::PlayerIdentity {
        session_id: session.id.to_string(),
        player_id: player_id.clone(),
        reconnect_token: reconnect_token.clone(),
        created_at: timestamp.to_rfc3339(),
        last_seen_at: timestamp.to_rfc3339(),
    }) {
        return internal_join_error(format!("failed to save player identity: {error}"));
    }
    if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
        id: random_prefixed_id("artifact"),
        session_id: session.id.to_string(),
        phase: protocol::Phase::Lobby,
        step: 0,
        kind: SessionArtifactKind::SessionCreated,
        player_id: Some(player_id.clone()),
        created_at: timestamp.to_rfc3339(),
        payload: json!({ "sessionCode": session_code, "hostName": normalized_name }),
    }) {
        return internal_join_error(format!("failed to append session artifact: {error}"));
    }

    let response = WorkshopJoinSuccess {
        ok: true,
        session_code: session.code.0.clone(),
        player_id: player_id.clone(),
        reconnect_token,
        coordinator_type: CoordinatorType::Rust,
        state: to_client_game_state(&session, &player_id),
    };

    state
        .sessions
        .lock()
        .await
        .insert(session.code.0.clone(), session);

    (StatusCode::OK, Json(WorkshopJoinResult::Success(response)))
}

async fn join_workshop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<JoinWorkshopRequest>,
) -> (StatusCode, Json<WorkshopJoinResult>) {
    if let Some(response) = reject_disallowed_origin(&headers, &state.config.origin_policy) {
        return response;
    }
    if let Some(response) = reject_rate_limited(&state.join_limiter, client_key_from_headers(&headers)).await {
        return response;
    }
    let session_code = payload.session_code.trim();
    if session_code.is_empty() {
        return bad_join_request("Enter a workshop code.");
    }
    if validate_session_code(session_code).is_err() {
        return bad_join_request("Workshop codes must be 6 digits.");
    }

    if let Some(reconnect_token) = payload
        .reconnect_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let identity = match state.store.find_player_identity(session_code, reconnect_token) {
            Ok(Some(identity)) => identity,
            Ok(None) => return bad_join_request("Session identity is invalid or expired."),
            Err(error) => return internal_join_error(format!("failed to lookup player identity: {error}")),
        };

        if !ensure_session_cached(&state, session_code).await.unwrap_or(false) {
            return bad_join_request("Workshop not found.");
        }

        let timestamp = Utc::now();
        let mut sessions = state.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_code) else {
            return bad_join_request("Workshop not found.");
        };
        let Some(player) = session.players.get_mut(&identity.player_id) else {
            return bad_join_request("Session identity is invalid or expired.");
        };
        player.is_connected = true;
        session.ensure_host_assigned(true);
        session.updated_at = timestamp;

        if let Err(error) = state.store.save_session(session) {
            return internal_join_error(format!("failed to save session: {error}"));
        }
        if let Err(error) = state.store.touch_player_identity(reconnect_token, &timestamp.to_rfc3339()) {
            return internal_join_error(format!("failed to touch player identity: {error}"));
        }
        if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
            id: random_prefixed_id("artifact"),
            session_id: session.id.to_string(),
            phase: session.phase,
            step: match session.phase {
                protocol::Phase::Lobby => 0,
                protocol::Phase::Phase1 => 1,
                protocol::Phase::Handover => 2,
                protocol::Phase::Phase2 | protocol::Phase::Voting => 3,
                protocol::Phase::End => 4,
            },
            kind: SessionArtifactKind::PlayerReconnected,
            player_id: Some(identity.player_id.clone()),
            created_at: timestamp.to_rfc3339(),
            payload: json!({ "sessionCode": session_code, "playerId": identity.player_id.clone() }),
        }) {
            return internal_join_error(format!("failed to append session artifact: {error}"));
        }

        let response = WorkshopJoinSuccess {
            ok: true,
            session_code: session.code.0.clone(),
            player_id: identity.player_id.clone(),
            reconnect_token: reconnect_token.to_string(),
            coordinator_type: CoordinatorType::Rust,
            state: to_client_game_state(session, &identity.player_id),
        };

        let response = (StatusCode::OK, Json(WorkshopJoinResult::Success(response)));
        drop(sessions);
        broadcast_session_state(&state, session_code, None).await;
        return response;
    }

    let normalized_name = payload.name.unwrap_or_default().trim().to_string();
    if normalized_name.is_empty() {
        return bad_join_request("Please enter a player name.");
    }

    if !ensure_session_cached(&state, session_code).await.unwrap_or(false) {
        return bad_join_request("Workshop not found.");
    }

    let mut sessions = state.sessions.lock().await;
    let Some(session) = sessions.get_mut(session_code) else {
        return bad_join_request("Workshop not found.");
    };
    if session.phase != protocol::Phase::Lobby {
        return bad_join_request("This workshop has already started. New players can only join in the lobby.");
    }
    let duplicate_name = session
        .players
        .values()
        .any(|player| player.name.eq_ignore_ascii_case(&normalized_name));
    if duplicate_name {
        return bad_join_request("That player name is already taken in this workshop.");
    }

    let timestamp = Utc::now();
    let player_id = random_prefixed_id("player");
    let reconnect_token = random_prefixed_id("reconnect");
    let player = SessionPlayer {
        id: player_id.clone(),
        name: normalized_name.clone(),
        pet_description: Some(format!("{}'s workshop dragon", normalized_name)),
        is_host: false,
        is_connected: true,
        is_ready: false,
        score: 0,
        current_dragon_id: None,
        achievements: Vec::new(),
        joined_at: timestamp,
    };
    session.add_player(player.clone());

    if let Err(error) = state.store.save_session(session) {
        return internal_join_error(format!("failed to save session: {error}"));
    }
    if let Err(error) = state.store.create_player_identity(&persistence::PlayerIdentity {
        session_id: session.id.to_string(),
        player_id: player_id.clone(),
        reconnect_token: reconnect_token.clone(),
        created_at: timestamp.to_rfc3339(),
        last_seen_at: timestamp.to_rfc3339(),
    }) {
        return internal_join_error(format!("failed to save player identity: {error}"));
    }
    if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
        id: random_prefixed_id("artifact"),
        session_id: session.id.to_string(),
        phase: protocol::Phase::Lobby,
        step: 0,
        kind: SessionArtifactKind::PlayerJoined,
        player_id: Some(player_id.clone()),
        created_at: timestamp.to_rfc3339(),
        payload: json!({ "sessionCode": session_code, "playerName": normalized_name }),
    }) {
        return internal_join_error(format!("failed to append session artifact: {error}"));
    }

    let response = WorkshopJoinSuccess {
        ok: true,
        session_code: session.code.0.clone(),
        player_id: player_id.clone(),
        reconnect_token,
        coordinator_type: CoordinatorType::Rust,
        state: to_client_game_state(session, &player_id),
    };

    let response = (StatusCode::OK, Json(WorkshopJoinResult::Success(response)));
    drop(sessions);
    broadcast_session_state(&state, session_code, None).await;
    response
}

async fn workshop_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<WorkshopCommandRequest>,
) -> (StatusCode, Json<WorkshopCommandResult>) {
    if let Some(response) = reject_disallowed_command_origin(&headers, &state.config.origin_policy) {
        return response;
    }

    let session_code = request.session_code.trim();
    let reconnect_token = request.reconnect_token.trim();
    if session_code.is_empty() || reconnect_token.is_empty() || validate_session_code(session_code).is_err() {
        return bad_command_request("Missing workshop credentials.");
    }

    let identity = match state.store.find_player_identity(session_code, reconnect_token) {
        Ok(Some(identity)) => identity,
        Ok(None) => return bad_command_request("Session identity is invalid or expired."),
        Err(error) => return internal_command_error(format!("failed to lookup identity: {error}")),
    };

    if !ensure_session_cached(&state, session_code).await.unwrap_or(false) {
        return bad_command_request("Workshop not found.");
    }

    let (response, should_broadcast) = {
        let mut sessions = state.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_code) else {
            return bad_command_request("Workshop not found.");
        };
        let mut should_broadcast = false;

        let response = match request.command {
        SessionCommand::StartPhase1 => {
            if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                return bad_command_request("Only the host can start the workshop.");
            }
            if session.phase != protocol::Phase::Lobby {
                return bad_command_request("Phase 1 can only start from the lobby.");
            }

            let assignments = session
                .players
                .keys()
                .cloned()
                .map(|player_id| Phase1Assignment {
                    dragon_id: format!("dragon_{player_id}"),
                    player_id,
                })
                .collect::<Vec<_>>();
            if let Err(error) = session.begin_phase1(&assignments) {
                return bad_command_request(&error.to_string());
            }
            if let Err(error) = state.store.save_session(session) {
                return internal_command_error(format!("failed to save session: {error}"));
            }
            if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
                id: random_prefixed_id("artifact"),
                session_id: session.id.to_string(),
                phase: session.phase,
                step: 1,
                kind: SessionArtifactKind::PhaseChanged,
                player_id: Some(identity.player_id.clone()),
                created_at: Utc::now().to_rfc3339(),
                payload: json!({ "toPhase": "phase1" }),
            }) {
                return internal_command_error(format!("failed to append session artifact: {error}"));
            }

            successful_workshop_command(&mut should_broadcast)
        }
        SessionCommand::SubmitObservation => {
            if session.phase != protocol::Phase::Phase1 {
                return bad_command_request("Observations can only be saved during Phase 1.");
            }
            let payload = match request.payload.clone() {
                Some(value) => serde_json::from_value::<DiscoveryObservationRequest>(value).ok(),
                None => None,
            };
            let Some(payload) = payload else {
                return bad_command_request("Observation payload is invalid.");
            };
            let text = payload.text.trim();
            if text.is_empty() {
                return bad_command_request("Observation text is required.");
            }
            let dragon_id = session
                .players
                .get(&identity.player_id)
                .and_then(|player| player.current_dragon_id.clone())
                .ok_or_else(|| bad_command_request("Player is not assigned to a dragon."));
            let Ok(dragon_id) = dragon_id else {
                return dragon_id.err().expect("dragon assignment error");
            };

            session.record_discovery_observation(&identity.player_id, text.to_string());
            if let Err(error) = state.store.save_session(session) {
                return internal_command_error(format!("failed to save session: {error}"));
            }
            if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
                id: random_prefixed_id("artifact"),
                session_id: session.id.to_string(),
                phase: session.phase,
                step: phase_step(session.phase),
                kind: SessionArtifactKind::DiscoveryObservationSaved,
                player_id: Some(identity.player_id.clone()),
                created_at: Utc::now().to_rfc3339(),
                payload: json!({ "dragonId": dragon_id, "text": text }),
            }) {
                return internal_command_error(format!("failed to append session artifact: {error}"));
            }

            successful_workshop_command(&mut should_broadcast)
        }
        SessionCommand::Action => {
            let payload = match request.payload.clone() {
                Some(value) => serde_json::from_value::<ActionPayload>(value).ok(),
                None => None,
            };
            let Some(payload) = payload else {
                return bad_command_request("Action payload is invalid.");
            };
            let Some(action) = parse_player_action(&payload) else {
                return bad_command_request("Action payload is invalid.");
            };
            let dragon_id = match session
                .players
                .get(&identity.player_id)
                .and_then(|player| player.current_dragon_id.clone())
            {
                Some(dragon_id) => dragon_id,
                None => return bad_command_request("Player is not assigned to a dragon."),
            };
            let action_type = payload.action_type.trim().to_ascii_lowercase();
            let action_value = payload.value.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(str::to_ascii_lowercase);
            let outcome = match session.apply_action(&identity.player_id, action) {
                Ok(outcome) => outcome,
                Err(error) => {
                    let message = match error {
                        DomainError::ActionNotAllowed => "Action is not allowed right now.".to_string(),
                        DomainError::DragonNotAssigned => "Player is not assigned to a dragon.".to_string(),
                        _ => error.to_string(),
                    };
                    return bad_command_request(&message);
                }
            };
            if let Err(error) = state.store.save_session(session) {
                return internal_command_error(format!("failed to save session: {error}"));
            }

            let mut artifact_payload = json!({
                "dragonId": dragon_id,
                "actionType": action_type,
                "actionValue": action_value,
            });
            if let Some(dragon) = session.dragons.get(&dragon_id) {
                if let Some(payload_map) = artifact_payload.as_object_mut() {
                    match outcome {
                        domain::ActionOutcome::Applied { .. } => {
                            payload_map.insert("hunger".to_string(), json!(dragon.hunger));
                            payload_map.insert("energy".to_string(), json!(dragon.energy));
                            payload_map.insert("happiness".to_string(), json!(dragon.happiness));
                        }
                        domain::ActionOutcome::Blocked { reason } => {
                            payload_map.insert(
                                "blockedReason".to_string(),
                                json!(match reason {
                                    domain::ActionBlockReason::AlreadyFull => "already_full",
                                    domain::ActionBlockReason::TooHungryToPlay => "too_hungry_to_play",
                                    domain::ActionBlockReason::TooTiredToPlay => "too_tired_to_play",
                                    domain::ActionBlockReason::TooAwakeToSleep => "too_awake_to_sleep",
                                }),
                            );
                        }
                    }
                }
            }

            if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
                id: random_prefixed_id("artifact"),
                session_id: session.id.to_string(),
                phase: session.phase,
                step: phase_step(session.phase),
                kind: SessionArtifactKind::ActionProcessed,
                player_id: Some(identity.player_id.clone()),
                created_at: Utc::now().to_rfc3339(),
                payload: artifact_payload,
            }) {
                return internal_command_error(format!("failed to append session artifact: {error}"));
            }

            successful_workshop_command(&mut should_broadcast)
        }
        SessionCommand::StartHandover => {
            if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                return bad_command_request("Only the host can trigger handover.");
            }
            if session.phase != protocol::Phase::Phase1 {
                return bad_command_request("Handover can only begin during Phase 1.");
            }
            if let Err(error) = session.transition_to(protocol::Phase::Handover) {
                return bad_command_request(&error.to_string());
            }
            if let Err(error) = state.store.save_session(session) {
                return internal_command_error(format!("failed to save session: {error}"));
            }
            if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
                id: random_prefixed_id("artifact"),
                session_id: session.id.to_string(),
                phase: session.phase,
                step: 2,
                kind: SessionArtifactKind::PhaseChanged,
                player_id: Some(identity.player_id.clone()),
                created_at: Utc::now().to_rfc3339(),
                payload: json!({ "toPhase": "handover" }),
            }) {
                return internal_command_error(format!("failed to append session artifact: {error}"));
            }

            successful_workshop_command(&mut should_broadcast)
        }
        SessionCommand::SubmitTags => {
            if session.phase != protocol::Phase::Handover {
                return bad_command_request("Handover notes can only be saved during handover.");
            }
            let tags = match request.payload.as_ref() {
                Some(serde_json::Value::Array(values)) => values
                    .iter()
                    .map(|value| value.as_str().map(str::trim).filter(|value| !value.is_empty()).map(str::to_string))
                    .collect::<Option<Vec<_>>>(),
                _ => None,
            };
            let Some(tags) = tags else {
                return bad_command_request("Handover notes must be sent as a list.");
            };

            session.save_handover_tags(&identity.player_id, tags);
            if let Err(error) = state.store.save_session(session) {
                return internal_command_error(format!("failed to save session: {error}"));
            }

            let saved_tags = session
                .players
                .get(&identity.player_id)
                .and_then(|player| player.current_dragon_id.clone())
                .and_then(|dragon_id| session.dragons.get(&dragon_id))
                .map(|dragon| dragon.handover_tags.clone())
                .unwrap_or_default();

            if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
                id: random_prefixed_id("artifact"),
                session_id: session.id.to_string(),
                phase: session.phase,
                step: 2,
                kind: SessionArtifactKind::HandoverSaved,
                player_id: Some(identity.player_id.clone()),
                created_at: Utc::now().to_rfc3339(),
                payload: json!({ "tagCount": saved_tags.len(), "tags": saved_tags }),
            }) {
                return internal_command_error(format!("failed to append session artifact: {error}"));
            }

            successful_workshop_command(&mut should_broadcast)
        }
        SessionCommand::StartPhase2 => {
            if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                return bad_command_request("Only the host can begin Phase 2.");
            }
            if session.phase != protocol::Phase::Handover {
                return bad_command_request("Phase 2 can only begin from handover.");
            }
            if let Err(error) = session.enter_phase2() {
                return match error {
                    DomainError::MissingHandoverTags { players } => {
                        bad_command_request(&format!("Still waiting on: {}.", players.join(", ")))
                    }
                    _ => bad_command_request(&error.to_string()),
                };
            }
            if let Err(error) = state.store.save_session(session) {
                return internal_command_error(format!("failed to save session: {error}"));
            }
            if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
                id: random_prefixed_id("artifact"),
                session_id: session.id.to_string(),
                phase: session.phase,
                step: 2,
                kind: SessionArtifactKind::PhaseChanged,
                player_id: Some(identity.player_id.clone()),
                created_at: Utc::now().to_rfc3339(),
                payload: json!({ "toPhase": "phase2" }),
            }) {
                return internal_command_error(format!("failed to append session artifact: {error}"));
            }

            successful_workshop_command(&mut should_broadcast)
        }
        SessionCommand::EndGame => {
            if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                return bad_command_request("Only the host can end the workshop.");
            }
            if session.phase != protocol::Phase::Phase2 {
                return bad_command_request("Voting can only begin from Phase 2.");
            }
            let immediate_finalize = match session.enter_voting() {
                Ok(immediate_finalize) => immediate_finalize,
                Err(error) => return bad_command_request(&error.to_string()),
            };
            if immediate_finalize {
                if let Err(error) = session.finalize_voting() {
                    return bad_command_request(&error.to_string());
                }
            }
            if let Err(error) = state.store.save_session(session) {
                return internal_command_error(format!("failed to save session: {error}"));
            }
            if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
                id: random_prefixed_id("artifact"),
                session_id: session.id.to_string(),
                phase: session.phase,
                step: 3,
                kind: SessionArtifactKind::PhaseChanged,
                player_id: Some(identity.player_id.clone()),
                created_at: Utc::now().to_rfc3339(),
                payload: json!({ "toPhase": if immediate_finalize { "end" } else { "voting" } }),
            }) {
                return internal_command_error(format!("failed to append session artifact: {error}"));
            }

            successful_workshop_command(&mut should_broadcast)
        }
        SessionCommand::SubmitVote => {
            if session.phase != protocol::Phase::Voting {
                return bad_command_request("Voting is not active right now.");
            }
            let payload = match request.payload.clone() {
                Some(value) => serde_json::from_value::<VotePayload>(value).ok(),
                None => None,
            };
            let Some(payload) = payload else {
                return bad_command_request("Vote payload is invalid.");
            };
            if let Err(error) = session.submit_vote(&identity.player_id, &payload.dragon_id) {
                let message = match error {
                    DomainError::VotingNotActive => "Voting is not active right now.".to_string(),
                    DomainError::IneligibleVoter => "Player is not eligible to vote.".to_string(),
                    DomainError::UnknownDragon => "Unknown dragon selected for vote.".to_string(),
                    DomainError::SelfVoteForbidden => "You cannot vote for your own dragon.".to_string(),
                    _ => error.to_string(),
                };
                return bad_command_request(&message);
            }
            if let Err(error) = state.store.save_session(session) {
                return internal_command_error(format!("failed to save session: {error}"));
            }
            if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
                id: random_prefixed_id("artifact"),
                session_id: session.id.to_string(),
                phase: session.phase,
                step: 3,
                kind: SessionArtifactKind::VoteSubmitted,
                player_id: Some(identity.player_id.clone()),
                created_at: Utc::now().to_rfc3339(),
                payload: json!({ "dragonId": payload.dragon_id }),
            }) {
                return internal_command_error(format!("failed to append session artifact: {error}"));
            }

            successful_workshop_command(&mut should_broadcast)
        }
        SessionCommand::RevealVotingResults => {
            if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                return bad_command_request("Only the host can reveal voting results.");
            }
            if session.phase != protocol::Phase::Voting {
                return bad_command_request("Results can only be revealed during voting.");
            }
            if let Some(voting) = session.voting.as_ref() {
                if voting.votes_by_player_id.len() < voting.eligible_player_ids.len() {
                    return bad_command_request("Wait until every eligible player has voted.");
                }
            }
            if let Err(error) = session.finalize_voting() {
                return bad_command_request(&error.to_string());
            }
            if let Err(error) = state.store.save_session(session) {
                return internal_command_error(format!("failed to save session: {error}"));
            }
            if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
                id: random_prefixed_id("artifact"),
                session_id: session.id.to_string(),
                phase: session.phase,
                step: 3,
                kind: SessionArtifactKind::VotingFinalized,
                player_id: Some(identity.player_id.clone()),
                created_at: Utc::now().to_rfc3339(),
                payload: json!({
                    "toPhase": "end",
                    "playerScores": session
                        .players
                        .iter()
                        .map(|(player_id, player)| (player_id.clone(), player.score))
                        .collect::<BTreeMap<_, _>>()
                }),
            }) {
                return internal_command_error(format!("failed to append session artifact: {error}"));
            }

            successful_workshop_command(&mut should_broadcast)
        }
        SessionCommand::ResetGame => {
            if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                return bad_command_request("Only the host can reset the workshop.");
            }
            if let Err(error) = session.reset_to_lobby() {
                return bad_command_request(&error.to_string());
            }
            if let Err(error) = state.store.save_session(session) {
                return internal_command_error(format!("failed to save session: {error}"));
            }
            if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
                id: random_prefixed_id("artifact"),
                session_id: session.id.to_string(),
                phase: session.phase,
                step: 0,
                kind: SessionArtifactKind::SessionReset,
                player_id: Some(identity.player_id.clone()),
                created_at: Utc::now().to_rfc3339(),
                payload: json!({ "toPhase": "lobby" }),
            }) {
                return internal_command_error(format!("failed to append session artifact: {error}"));
            }

            successful_workshop_command(&mut should_broadcast)
        }
        _ => bad_command_request("Unsupported workshop command."),
        };

        (response, should_broadcast)
    };

    if should_broadcast {
        broadcast_session_state(&state, session_code, None).await;
    }

    response
}

async fn workshop_judge_bundle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<WorkshopJudgeBundleRequest>,
) -> (StatusCode, Json<WorkshopJudgeBundleResult>) {
    if let Some(response) = reject_disallowed_judge_bundle_origin(&headers, &state.config.origin_policy) {
        return response;
    }

    let session_code = request.session_code.trim();
    let reconnect_token = request.reconnect_token.trim();
    if session_code.is_empty() || reconnect_token.is_empty() || validate_session_code(session_code).is_err() {
        return bad_judge_bundle_request("Missing workshop credentials.");
    }

    let session = {
        if !ensure_session_cached(&state, session_code).await.unwrap_or(false) {
            return bad_judge_bundle_request("Workshop not found.");
        }
        let sessions = state.sessions.lock().await;
        let Some(session) = sessions.get(session_code) else {
            return bad_judge_bundle_request("Workshop not found.");
        };
        session.clone()
    };

    let identity = match state.store.find_player_identity(session_code, reconnect_token) {
        Ok(Some(identity)) => identity,
        Ok(None) => return bad_judge_bundle_request("Session identity is invalid or expired."),
        Err(error) => return internal_judge_bundle_error(format!("failed to lookup identity: {error}")),
    };

    let artifacts = match state.store.list_session_artifacts(&session.id.to_string()) {
        Ok(artifacts) => artifacts,
        Err(error) => return internal_judge_bundle_error(format!("failed to list session artifacts: {error}")),
    };

    let bundle = build_judge_bundle(&session, &artifacts);

    if let Err(error) = state.store.append_session_artifact(&SessionArtifactRecord {
        id: random_prefixed_id("artifact"),
        session_id: session.id.to_string(),
        phase: session.phase,
        step: 4,
        kind: SessionArtifactKind::JudgeBundleGenerated,
        player_id: Some(identity.player_id.clone()),
        created_at: Utc::now().to_rfc3339(),
        payload: json!({
            "dragonCount": bundle.dragons.len(),
            "artifactCount": bundle.artifact_count,
        }),
    }) {
        return internal_judge_bundle_error(format!("failed to append session artifact: {error}"));
    }

    (
        StatusCode::OK,
        Json(WorkshopJudgeBundleResult::Success(WorkshopJudgeBundleSuccess {
            ok: true,
            bundle,
        })),
    )
}

 async fn ready(State(state): State<AppState>) -> Json<serde_json::Value> {
     let store_healthy = state.store.health_check().unwrap_or(false);

     Json(json!({
         "ok": store_healthy,
         "service": "app-server",
         "status": if store_healthy { "ready" } else { "degraded" },
         "checks": {
             "store": store_healthy
         }
     }))
 }

 async fn runtime_snapshot(State(state): State<AppState>) -> Json<RuntimeSnapshot> {
     let active_realtime_sessions = state
         .realtime
         .lock()
         .await
         .total_connection_count();
     let allowed_origins = state
         .config
         .origin_policy
         .allowed_origins
         .iter()
         .cloned()
         .collect::<Vec<_>>();

     Json(RuntimeSnapshot {
         bind_addr: state.config.bind_addr.to_string(),
         is_production: state.config.is_production,
         rust_session_code_prefix: state.config.rust_session_code_prefix.clone(),
         persistence_backend: state.config.persistence_backend.clone(),
         allow_any_origin: state.config.origin_policy.allow_any_origin,
         require_origin: state.config.origin_policy.require_origin,
         allowed_origins,
         active_realtime_sessions,
     })
 }

 fn internal_join_error(message: String) -> (StatusCode, Json<WorkshopJoinResult>) {
     (
         StatusCode::INTERNAL_SERVER_ERROR,
         Json(WorkshopJoinResult::Error(WorkshopError {
             ok: false,
             error: message,
         })),
     )
 }

 fn bad_judge_bundle_request(message: &str) -> (StatusCode, Json<WorkshopJudgeBundleResult>) {
     (
         StatusCode::BAD_REQUEST,
         Json(WorkshopJudgeBundleResult::Error(WorkshopError {
             ok: false,
             error: message.to_string(),
         })),
     )
 }

 fn internal_judge_bundle_error(message: String) -> (StatusCode, Json<WorkshopJudgeBundleResult>) {
     (
         StatusCode::INTERNAL_SERVER_ERROR,
         Json(WorkshopJudgeBundleResult::Error(WorkshopError {
             ok: false,
             error: message,
         })),
     )
 }

 fn bad_join_request(message: &str) -> (StatusCode, Json<WorkshopJoinResult>) {
     (
         StatusCode::BAD_REQUEST,
         Json(WorkshopJoinResult::Error(WorkshopError {
             ok: false,
             error: message.to_string(),
         })),
     )
 }

 fn bad_command_request(message: &str) -> (StatusCode, Json<WorkshopCommandResult>) {
     (
         StatusCode::BAD_REQUEST,
         Json(WorkshopCommandResult::Error(WorkshopError {
             ok: false,
             error: message.to_string(),
         })),
     )
 }

 fn internal_command_error(message: String) -> (StatusCode, Json<WorkshopCommandResult>) {
     (
         StatusCode::INTERNAL_SERVER_ERROR,
         Json(WorkshopCommandResult::Error(WorkshopError {
             ok: false,
             error: message,
         })),
     )
 }

 fn random_prefixed_id(prefix: &str) -> String {
     format!("{prefix}_{}", Uuid::new_v4().simple())
 }

 async fn allocate_session_code(state: &AppState) -> String {
     loop {
         let entropy = Uuid::new_v4().simple().to_string();
         let suffix = entropy
             .chars()
             .filter(|ch| ch.is_ascii_hexdigit())
             .take(5)
             .map(|ch| (((ch as u8) % 10) + b'0') as char)
             .collect::<String>();
         let candidate = format!("{}{}", state.config.rust_session_code_prefix, suffix);
         if !state.sessions.lock().await.contains_key(&candidate)
             && state.store.load_session_by_code(&candidate).ok().flatten().is_none()
         {
             return candidate;
         }
     }
 }

 fn to_client_game_state(session: &WorkshopSession, current_player_id: &str) -> ClientGameState {
     let players = session
         .players
         .iter()
         .map(|(player_id, player)| {
             (
                 player_id.clone(),
                 Player {
                     id: player.id.clone(),
                     name: player.name.clone(),
                     is_host: player.is_host,
                     score: player.score,
                     current_dragon_id: player.current_dragon_id.clone(),
                     achievements: player.achievements.clone(),
                     is_ready: player.is_ready,
                     is_connected: player.is_connected,
                     pet_description: player.pet_description.clone(),
                 },
             )
         })
         .collect();

     ClientGameState {
         session: SessionMeta {
             id: session.id.to_string(),
             code: session.code.0.clone(),
             created_at: session.created_at.to_rfc3339(),
             updated_at: session.updated_at.to_rfc3339(),
             host_player_id: session.host_player_id.clone(),
             settings: create_default_session_settings(),
         },
         phase: session.phase,
         time: session.time,
         players,
         dragons: BTreeMap::new(),
         current_player_id: Some(current_player_id.to_string()),
         voting: None,
     }
 }

 fn phase_step(phase: protocol::Phase) -> u8 {
    match phase {
        protocol::Phase::Lobby => 0,
        protocol::Phase::Phase1 => 1,
        protocol::Phase::Handover => 2,
        protocol::Phase::Phase2 => 2,
        protocol::Phase::Voting => 3,
        protocol::Phase::End => 4,
    }
}

fn parse_player_action(payload: &ActionPayload) -> Option<PlayerAction> {
    let action_type = payload.action_type.trim().to_ascii_lowercase();
    match action_type.as_str() {
        "sleep" => Some(PlayerAction::Sleep),
        "feed" => match payload.value.as_deref().map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("meat") => Some(PlayerAction::Feed(FoodType::Meat)),
            Some("fruit") => Some(PlayerAction::Feed(FoodType::Fruit)),
            Some("fish") => Some(PlayerAction::Feed(FoodType::Fish)),
            _ => None,
        },
        "play" => match payload.value.as_deref().map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("fetch") => Some(PlayerAction::Play(PlayType::Fetch)),
            Some("puzzle") => Some(PlayerAction::Play(PlayType::Puzzle)),
            Some("music") => Some(PlayerAction::Play(PlayType::Music)),
            _ => None,
        },
        _ => None,
    }
}

fn build_judge_action_traces(
     session: &WorkshopSession,
     artifacts: &[SessionArtifactRecord],
 ) -> BTreeMap<String, Vec<JudgeActionTrace>> {
     let mut traces_by_dragon_id = BTreeMap::new();

     for artifact in artifacts {
         if artifact.kind != SessionArtifactKind::ActionProcessed {
             continue;
         }

         let Some(dragon_id) = artifact.payload.get("dragonId").and_then(|value| value.as_str()) else {
             continue;
         };

         let player = artifact
             .player_id
             .as_ref()
             .and_then(|player_id| session.players.get(player_id));

         let resulting_stats = match (
             artifact.payload.get("hunger").and_then(|value| value.as_i64()),
             artifact.payload.get("energy").and_then(|value| value.as_i64()),
             artifact.payload.get("happiness").and_then(|value| value.as_i64()),
         ) {
             (Some(hunger), Some(energy), Some(happiness)) => Some(DragonStats {
                 hunger: hunger as i32,
                 energy: energy as i32,
                 happiness: happiness as i32,
             }),
             _ => None,
         };

         let trace = JudgeActionTrace {
             player_id: artifact.player_id.clone().unwrap_or_else(|| "unknown".to_string()),
             player_name: player
                 .map(|player| player.name.clone())
                 .unwrap_or_else(|| "Unknown".to_string()),
             phase: artifact.phase,
             action_type: artifact
                 .payload
                 .get("actionType")
                 .and_then(|value| value.as_str())
                 .unwrap_or("unknown")
                 .to_string(),
             action_value: artifact
                 .payload
                 .get("actionValue")
                 .and_then(|value| value.as_str())
                 .map(str::to_string),
             created_at: artifact.created_at.clone(),
             resulting_stats,
         };

         traces_by_dragon_id
             .entry(dragon_id.to_string())
             .or_insert_with(Vec::new)
             .push(trace);
     }

     traces_by_dragon_id
 }

 fn build_judge_bundle(session: &WorkshopSession, artifacts: &[SessionArtifactRecord]) -> JudgeBundle {
     let mut vote_counts = BTreeMap::new();
     if let Some(voting) = session.voting.as_ref() {
         for dragon_id in voting.votes_by_player_id.values() {
             *vote_counts.entry(dragon_id.clone()).or_insert(0) += 1;
         }
     }

     let phase2_actions = build_judge_action_traces(session, artifacts);

     JudgeBundle {
         session_id: session.id.to_string(),
         session_code: session.code.0.clone(),
         current_phase: session.phase,
         generated_at: Utc::now().to_rfc3339(),
         artifact_count: artifacts.len() as i32,
         players: session
             .players
             .values()
             .map(|player| JudgePlayerSummary {
                 player_id: player.id.clone(),
                 name: player.name.clone(),
                 score: player.score,
                 achievements: player.achievements.clone(),
             })
             .collect(),
         dragons: session
             .dragons
             .values()
             .map(|dragon| JudgeDragonBundle {
                 dragon_id: dragon.id.clone(),
                 dragon_name: dragon.name.clone(),
                 creator_player_id: dragon.original_owner_id.clone(),
                 creator_name: session
                     .players
                     .get(&dragon.original_owner_id)
                     .map(|player| player.name.clone())
                     .unwrap_or_else(|| "Unknown".to_string()),
                 current_owner_id: dragon.current_owner_id.clone(),
                 current_owner_name: session
                     .players
                     .get(&dragon.current_owner_id)
                     .map(|player| player.name.clone())
                     .unwrap_or_else(|| "Unknown".to_string()),
                 creative_vote_count: vote_counts.get(&dragon.id).copied().unwrap_or(0),
                 final_stats: DragonStats {
                     hunger: dragon.hunger,
                     energy: dragon.energy,
                     happiness: dragon.happiness,
                 },
                 handover_chain: JudgeHandoverChain {
                     creator_instructions: dragon.creator_instructions.clone(),
                     discovery_observations: dragon.discovery_observations.clone(),
                     handover_tags: dragon.handover_tags.clone(),
                 },
                 phase2_actions: phase2_actions.get(&dragon.id).cloned().unwrap_or_default(),
             })
             .collect(),
     }
 }

 fn reject_disallowed_origin(
     headers: &HeaderMap,
     policy: &OriginPolicy,
 ) -> Option<(StatusCode, Json<WorkshopJoinResult>)> {
     let origin = headers.get("origin").and_then(|value| value.to_str().ok());
     if security::is_origin_allowed(origin, policy) {
         None
     } else {
         Some((
             StatusCode::FORBIDDEN,
             Json(WorkshopJoinResult::Error(WorkshopError {
                 ok: false,
                 error: "Origin is not allowed.".to_string(),
             })),
         ))
     }
 }

 fn reject_disallowed_command_origin(
     headers: &HeaderMap,
     policy: &OriginPolicy,
 ) -> Option<(StatusCode, Json<WorkshopCommandResult>)> {
     let origin = headers.get("origin").and_then(|value| value.to_str().ok());
     if security::is_origin_allowed(origin, policy) {
         None
     } else {
         Some((
             StatusCode::FORBIDDEN,
             Json(WorkshopCommandResult::Error(WorkshopError {
                 ok: false,
                 error: "Origin is not allowed.".to_string(),
             })),
         ))
     }
 }

 fn reject_disallowed_judge_bundle_origin(
     headers: &HeaderMap,
     policy: &OriginPolicy,
 ) -> Option<(StatusCode, Json<WorkshopJudgeBundleResult>)> {
     let origin = headers.get("origin").and_then(|value| value.to_str().ok());
     if security::is_origin_allowed(origin, policy) {
         None
     } else {
         Some((
             StatusCode::FORBIDDEN,
             Json(WorkshopJudgeBundleResult::Error(WorkshopError {
                 ok: false,
                 error: "Origin is not allowed.".to_string(),
             })),
         ))
     }
 }

 async fn reject_rate_limited(
     limiter: &Arc<Mutex<FixedWindowRateLimiter>>,
     client_key: String,
 ) -> Option<(StatusCode, Json<WorkshopJoinResult>)> {
     let now_ms = Utc::now().timestamp_millis().max(0) as u64;
     let decision = limiter.lock().await.consume(&client_key, now_ms);
     if decision.allowed {
         None
     } else {
         Some((
             StatusCode::TOO_MANY_REQUESTS,
             Json(WorkshopJoinResult::Error(WorkshopError {
                 ok: false,
                 error: "Too many requests. Please slow down and try again.".to_string(),
             })),
         ))
     }
 }

 fn client_key_from_headers(headers: &HeaderMap) -> String {
     headers
         .get("x-forwarded-for")
         .and_then(|value| value.to_str().ok())
         .and_then(|value| value.split(',').next())
         .map(str::trim)
         .filter(|value| !value.is_empty())
         .unwrap_or("unknown")
         .to_string()
 }

fn successful_workshop_command(should_broadcast: &mut bool) -> (StatusCode, Json<WorkshopCommandResult>) {
    *should_broadcast = true;
    (
        StatusCode::OK,
        Json(WorkshopCommandResult::Success(WorkshopCommandSuccess { ok: true })),
    )
}

 #[cfg(test)]
 mod tests {
     use super::*;
     use axum::{
         body::{to_bytes, Body},
         http::{HeaderValue, Request, StatusCode},
     };
     use futures_util::{SinkExt, StreamExt};
     use tokio_tungstenite::{
         connect_async,
         tungstenite::{client::IntoClientRequest, Message as WsMessage},
     };
     use tower::util::ServiceExt;

     fn session_player(id: &str, name: &str, joined_at_seconds: i64) -> SessionPlayer {
         SessionPlayer {
             id: id.to_string(),
             name: name.to_string(),
             pet_description: Some(format!("{name}'s workshop dragon")),
             is_host: false,
             is_connected: true,
             is_ready: false,
             score: 0,
             current_dragon_id: None,
             achievements: Vec::new(),
             joined_at: chrono::DateTime::from_timestamp(joined_at_seconds, 0).expect("valid timestamp"),
         }
     }

     fn test_state() -> AppState {
         test_state_with_limits(20, 40)
     }

     fn test_state_with_limits(create_limit: u32, join_limit: u32) -> AppState {
         let config = Arc::new(AppConfig {
             bind_addr: SocketAddr::from(([127, 0, 0, 1], 4100)),
             is_production: false,
             rust_session_code_prefix: DEFAULT_RUST_SESSION_CODE_PREFIX.to_string(),
             origin_policy: create_origin_policy(OriginPolicyOptions {
                 allowed_origins: Some("http://localhost:5173"),
                 app_origin: None,
                 is_production: false,
             })
             .expect("create origin policy"),
             static_assets_dir: std::env::temp_dir().join("dragon-shift-test-static-missing"),
             database_url: None,
             persistence_backend: "memory".to_string(),
         });

         AppState {
            config,
            store: Arc::new(InMemorySessionStore::new()),
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            create_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(create_limit, 60_000))),
            join_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(join_limit, 60_000))),
            realtime: Arc::new(Mutex::new(SessionRegistry::new())),
            realtime_senders: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

     async fn spawn_test_server(app: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
         let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
             .await
             .expect("bind test listener");
         let addr = listener.local_addr().expect("listener addr");
         let handle = tokio::spawn(async move {
             axum::serve(listener, app).await.expect("serve test app");
         });
         (addr, handle)
     }

     fn ws_request(addr: SocketAddr) -> tokio_tungstenite::tungstenite::handshake::client::Request {
         let mut request = format!("ws://{addr}/api/workshops/ws")
             .into_client_request()
             .expect("ws client request");
         request
             .headers_mut()
             .insert("origin", HeaderValue::from_static("http://localhost:5173"));
         request
     }

     fn test_state_with_static_assets() -> AppState {
         let static_assets_dir = std::env::temp_dir().join(format!("dragon-shift-test-static-{}", Uuid::new_v4()));
         std::fs::create_dir_all(&static_assets_dir).expect("create static assets dir");
         std::fs::write(
             static_assets_dir.join("index.html"),
             "<!doctype html><html><body><div id=\"root\">dragon shift</div></body></html>",
         )
         .expect("write static index");

         let mut state = test_state();
         state.config = Arc::new(AppConfig {
             static_assets_dir,
             ..state.config.as_ref().clone()
         });
         state
     }

     #[tokio::test]
     async fn live_endpoint_returns_ok() {
         let app = build_app(test_state());

         let response = app
             .oneshot(Request::builder().uri("/api/live").body(Body::empty()).expect("build request"))
             .await
             .expect("call live endpoint");

         assert_eq!(response.status(), StatusCode::OK);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read live body");
         let json: serde_json::Value = serde_json::from_slice(&body).expect("parse live json");
         assert_eq!(json["status"], "live");
         assert_eq!(json["ok"], true);
     }

     #[tokio::test]
     async fn root_path_serves_static_index_when_bundle_exists() {
         let app = build_app(test_state_with_static_assets());

         let response = app
             .oneshot(Request::builder().uri("/").body(Body::empty()).expect("build request"))
             .await
             .expect("call root path");

         assert_eq!(response.status(), StatusCode::OK);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read root body");
         let html = String::from_utf8(body.to_vec()).expect("decode html");
         assert!(html.contains("dragon shift"));
     }

     #[tokio::test]
     async fn workshop_ws_attach_sends_current_state_for_valid_identity() {
         let state = test_state();
         let app = build_app(state.clone());

         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let (addr, server_handle) = spawn_test_server(app).await;
         let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
         let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
             session_code: create_success.session_code.clone(),
             player_id: create_success.player_id.clone(),
             reconnect_token: create_success.reconnect_token.clone(),
             coordinator_type: Some(CoordinatorType::Rust),
         });
         socket
             .send(WsMessage::Text(serde_json::to_string(&attach_message).expect("encode attach").into()))
             .await
             .expect("send attach");

         let message = socket.next().await.expect("state update frame").expect("state update message");
         let payload = match message {
             WsMessage::Text(payload) => payload,
             other => panic!("expected text frame, got {other:?}"),
         };
         let server_message: ServerWsMessage = serde_json::from_str(&payload).expect("parse server ws message");
         match server_message {
             ServerWsMessage::StateUpdate(client_state) => {
                 assert_eq!(client_state.session.code, create_success.session_code);
                 assert_eq!(client_state.current_player_id.as_deref(), Some(create_success.player_id.as_str()));
             }
             other => panic!("expected state update, got {other:?}"),
         }
         assert_eq!(state.realtime.lock().await.session_connection_count(&create_success.session_code), 1);

         let _ = socket.close(None).await;
         server_handle.abort();
     }

     #[tokio::test]
     async fn workshop_command_pushes_state_update_to_attached_websocket() {
         let state = test_state();
         let app = build_app(state.clone());

         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let (addr, server_handle) = spawn_test_server(app.clone()).await;
         let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
         let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
             session_code: create_success.session_code.clone(),
             player_id: create_success.player_id.clone(),
             reconnect_token: create_success.reconnect_token.clone(),
             coordinator_type: Some(CoordinatorType::Rust),
         });
         socket
             .send(WsMessage::Text(serde_json::to_string(&attach_message).expect("encode attach").into()))
             .await
             .expect("send attach");

         let _ = socket.next().await.expect("initial state update frame").expect("initial state update message");

         let command_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("origin", "http://localhost:5173")
                     .header("content-type", "application/json")
                     .body(Body::from(
                         serde_json::to_vec(&WorkshopCommandRequest {
                             session_code: create_success.session_code.clone(),
                             reconnect_token: create_success.reconnect_token.clone(),
                             coordinator_type: Some(CoordinatorType::Rust),
                             command: SessionCommand::StartPhase1,
                             payload: None,
                         })
                         .expect("encode command request"),
                     ))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");
         assert_eq!(command_response.status(), StatusCode::OK);

         let message = socket.next().await.expect("broadcast state update frame").expect("broadcast state update message");
         let payload = match message {
             WsMessage::Text(payload) => payload,
             other => panic!("expected text frame, got {other:?}"),
         };
         let server_message: ServerWsMessage = serde_json::from_str(&payload).expect("parse server ws message");
         match server_message {
             ServerWsMessage::StateUpdate(client_state) => {
                 assert_eq!(client_state.phase, protocol::Phase::Phase1);
                 assert_eq!(client_state.session.code, create_success.session_code);
             }
             other => panic!("expected state update, got {other:?}"),
         }

         let _ = socket.close(None).await;
         server_handle.abort();
     }

     #[tokio::test]
     async fn workshop_ws_close_marks_player_offline_and_reassigns_host() {
         let state = test_state();
         let app = build_app(state.clone());

         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, create_success.session_code)))
                     .expect("build join request"),
             )
             .await
             .expect("call join workshop");
         let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
         let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
         let join_success = match join_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
         };

         let (addr, server_handle) = spawn_test_server(app).await;
         let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
         let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
             session_code: create_success.session_code.clone(),
             player_id: create_success.player_id.clone(),
             reconnect_token: create_success.reconnect_token.clone(),
             coordinator_type: Some(CoordinatorType::Rust),
         });
         socket
             .send(WsMessage::Text(serde_json::to_string(&attach_message).expect("encode attach").into()))
             .await
             .expect("send attach");

         let _ = socket.next().await.expect("state update frame").expect("state update message");
         assert_eq!(state.realtime.lock().await.session_connection_count(&create_success.session_code), 1);

         let _ = socket.close(None).await;
         tokio::time::sleep(std::time::Duration::from_millis(50)).await;

         assert_eq!(state.realtime.lock().await.session_connection_count(&create_success.session_code), 0);

         let (session_id, host_player, guest_player) = {
             let sessions = state.sessions.lock().await;
             let session = sessions
                 .get(&create_success.session_code)
                 .expect("session exists after disconnect");
             (
                 session.id.to_string(),
                 session
                     .players
                     .get(&create_success.player_id)
                     .expect("host player exists")
                     .clone(),
                 session
                     .players
                     .get(&join_success.player_id)
                     .expect("guest player exists")
                     .clone(),
             )
         };

         assert!(!host_player.is_connected);
         assert!(!host_player.is_host);
         assert!(guest_player.is_host);
         assert!(guest_player.is_connected);

         let artifacts = state
             .store
             .list_session_artifacts(&session_id)
             .expect("list session artifacts");
         let disconnect_artifact = artifacts
             .iter()
             .rev()
             .find(|artifact| artifact.kind == SessionArtifactKind::PlayerLeft)
             .expect("player left artifact");
         assert_eq!(disconnect_artifact.player_id.as_deref(), Some(create_success.player_id.as_str()));
         assert_eq!(
             disconnect_artifact.payload.get("sessionCode").and_then(|value| value.as_str()),
             Some(create_success.session_code.as_str())
         );

         server_handle.abort();
     }

     #[tokio::test]
     async fn workshop_ws_rejects_invalid_identity() {
         let app = build_app(test_state());
         let (addr, server_handle) = spawn_test_server(app).await;
         let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
         let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
             session_code: "123456".to_string(),
             player_id: "player-1".to_string(),
             reconnect_token: "missing".to_string(),
             coordinator_type: Some(CoordinatorType::Rust),
         });
         socket
             .send(WsMessage::Text(serde_json::to_string(&attach_message).expect("encode attach").into()))
             .await
             .expect("send attach");

         let message = socket.next().await.expect("error frame").expect("error message");
         let payload = match message {
             WsMessage::Text(payload) => payload,
             other => panic!("expected text frame, got {other:?}"),
         };
         let server_message: ServerWsMessage = serde_json::from_str(&payload).expect("parse server ws message");
         match server_message {
             ServerWsMessage::Error { message } => {
                 assert_eq!(message, "Session identity is invalid or expired.");
             }
             other => panic!("expected error message, got {other:?}"),
         }

         let _ = socket.close(None).await;
         server_handle.abort();
     }

     #[tokio::test]
     async fn workshop_ws_replies_with_pong_for_ping_message() {
         let app = build_app(test_state());
         let (addr, server_handle) = spawn_test_server(app).await;
         let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
         socket
             .send(WsMessage::Text(serde_json::to_string(&ClientWsMessage::Ping).expect("encode ping").into()))
             .await
             .expect("send ping message");

         let message = socket.next().await.expect("pong frame").expect("pong message");
         let payload = match message {
             WsMessage::Text(payload) => payload,
             other => panic!("expected text frame, got {other:?}"),
         };
         let server_message: ServerWsMessage = serde_json::from_str(&payload).expect("parse server ws message");
         assert_eq!(server_message, ServerWsMessage::Pong);

         let _ = socket.close(None).await;
         server_handle.abort();
     }

     #[test]
     fn build_judge_action_traces_groups_action_artifacts_by_dragon() {
         let mut session = WorkshopSession::new(
             Uuid::new_v4(),
             SessionCode("123456".into()),
             chrono::DateTime::from_timestamp(1, 0).expect("valid timestamp"),
         );
         session.add_player(session_player("p1", "Alice", 10));

         let artifacts = vec![
             SessionArtifactRecord {
                 id: "artifact-1".into(),
                 session_id: session.id.to_string(),
                 phase: protocol::Phase::Phase2,
                 step: 2,
                 kind: SessionArtifactKind::ActionProcessed,
                 player_id: Some("p1".into()),
                 created_at: "2026-01-01T00:00:00Z".into(),
                 payload: serde_json::json!({
                     "dragonId": "dragon-a",
                     "actionType": "feed",
                     "actionValue": "meat",
                     "hunger": 90,
                     "energy": 100,
                     "happiness": 95
                 }),
             },
             SessionArtifactRecord {
                 id: "artifact-2".into(),
                 session_id: session.id.to_string(),
                 phase: protocol::Phase::Phase2,
                 step: 2,
                 kind: SessionArtifactKind::ActionProcessed,
                 player_id: Some("p1".into()),
                 created_at: "2026-01-01T00:00:01Z".into(),
                 payload: serde_json::json!({
                     "dragonId": "dragon-a",
                     "actionType": "play",
                     "actionValue": "fetch"
                 }),
             },
             SessionArtifactRecord {
                 id: "artifact-3".into(),
                 session_id: session.id.to_string(),
                 phase: protocol::Phase::Phase2,
                 step: 2,
                 kind: SessionArtifactKind::PhaseChanged,
                 player_id: Some("p1".into()),
                 created_at: "2026-01-01T00:00:02Z".into(),
                 payload: serde_json::json!({ "toPhase": "voting" }),
             },
         ];

         let traces = build_judge_action_traces(&session, &artifacts);

         let dragon_traces = traces.get("dragon-a").expect("dragon-a traces");
         assert_eq!(dragon_traces.len(), 2);
         assert_eq!(dragon_traces[0].player_name, "Alice");
         assert_eq!(dragon_traces[0].action_type, "feed");
         assert_eq!(dragon_traces[0].action_value.as_deref(), Some("meat"));
         assert_eq!(dragon_traces[0].resulting_stats, Some(DragonStats { hunger: 90, energy: 100, happiness: 95 }));
         assert_eq!(dragon_traces[1].action_type, "play");
         assert_eq!(dragon_traces[1].resulting_stats, None);
     }

     #[test]
     fn build_judge_action_traces_uses_unknown_fallbacks_for_missing_player_or_payload() {
         let session = WorkshopSession::new(
             Uuid::new_v4(),
             SessionCode("123456".into()),
             chrono::DateTime::from_timestamp(1, 0).expect("valid timestamp"),
         );
         let artifacts = vec![SessionArtifactRecord {
             id: "artifact-1".into(),
             session_id: session.id.to_string(),
             phase: protocol::Phase::Phase2,
             step: 2,
             kind: SessionArtifactKind::ActionProcessed,
             player_id: None,
             created_at: "2026-01-01T00:00:00Z".into(),
             payload: serde_json::json!({ "dragonId": "dragon-a" }),
         }];

         let traces = build_judge_action_traces(&session, &artifacts);

         let dragon_traces = traces.get("dragon-a").expect("dragon-a traces");
         assert_eq!(dragon_traces.len(), 1);
         assert_eq!(dragon_traces[0].player_id, "unknown");
         assert_eq!(dragon_traces[0].player_name, "Unknown");
         assert_eq!(dragon_traces[0].action_type, "unknown");
         assert_eq!(dragon_traces[0].action_value, None);
         assert_eq!(dragon_traces[0].resulting_stats, None);
     }

     #[tokio::test]
     async fn workshop_judge_bundle_rejects_invalid_credentials() {
         let state = test_state();
         let app = build_app(state.clone());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let session_code = match create_result {
             WorkshopJoinResult::Success(success) => success.session_code,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/judge-bundle")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"missing"}}"#, session_code)))
                     .expect("build request"),
             )
             .await
             .expect("call judge bundle endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read judge bundle body");
         let result: WorkshopJudgeBundleResult = serde_json::from_slice(&body).expect("parse judge bundle result");
         match result {
             WorkshopJudgeBundleResult::Error(error) => {
                 assert_eq!(error.error, "Session identity is invalid or expired.");
             }
             WorkshopJudgeBundleResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_judge_bundle_returns_bundle_for_completed_session() {
         let state = test_state();
         let app = build_app(state.clone());

         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };
         let session_code = create_success.session_code.clone();

         let join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code)))
                     .expect("build join request"),
             )
             .await
             .expect("call join workshop");
         let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
         let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
         let join_success = match join_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
         };

         for request_body in [
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#, session_code, join_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#, session_code, create_success.reconnect_token),
         ] {
             let response = app
                 .clone()
                 .oneshot(
                     Request::builder()
                         .method("POST")
                         .uri("/api/workshops/command")
                         .header("content-type", "application/json")
                         .body(Body::from(request_body))
                         .expect("build setup request"),
                 )
                 .await
                 .expect("call setup command");
             assert_eq!(response.status(), StatusCode::OK);
         }

         let sessions = state.sessions.lock().await;
         let session = sessions.get(&session_code).expect("session exists");
         let alice_dragon_id = session
             .players
             .get(&create_success.player_id)
             .and_then(|player| player.current_dragon_id.clone())
             .expect("alice dragon id");
         let bob_dragon_id = session
             .players
             .get(&join_success.player_id)
             .and_then(|player| player.current_dragon_id.clone())
             .expect("bob dragon id");
         let session_id = session.id.to_string();
         drop(sessions);

         for request_body in [
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#, session_code, create_success.reconnect_token, bob_dragon_id),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#, session_code, join_success.reconnect_token, alice_dragon_id),
         ] {
             let response = app
                 .clone()
                 .oneshot(
                     Request::builder()
                         .method("POST")
                         .uri("/api/workshops/command")
                         .header("content-type", "application/json")
                         .body(Body::from(request_body))
                         .expect("build vote request"),
                 )
                 .await
                 .expect("call submitVote command");
             assert_eq!(response.status(), StatusCode::OK);
         }

         let reveal_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build reveal results request"),
             )
             .await
             .expect("call revealVotingResults command");
         assert_eq!(reveal_response.status(), StatusCode::OK);

         state.store.append_session_artifact(&SessionArtifactRecord {
             id: "artifact-action-1".into(),
             session_id: session_id.clone(),
             phase: protocol::Phase::Phase2,
             step: 2,
             kind: SessionArtifactKind::ActionProcessed,
             player_id: Some(join_success.player_id.clone()),
             created_at: "2026-01-01T00:00:00Z".into(),
             payload: serde_json::json!({
                 "dragonId": alice_dragon_id,
                 "actionType": "feed",
                 "actionValue": "meat",
                 "hunger": 88,
                 "energy": 100,
                 "happiness": 97
             }),
         }).expect("append action artifact");

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/judge-bundle")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build judge bundle request"),
             )
             .await
             .expect("call judge bundle endpoint");

         assert_eq!(response.status(), StatusCode::OK);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read judge bundle body");
         let result: WorkshopJudgeBundleResult = serde_json::from_slice(&body).expect("parse judge bundle result");
         let success = match result {
             WorkshopJudgeBundleResult::Success(success) => success,
             WorkshopJudgeBundleResult::Error(error) => panic!("expected success, got error: {}", error.error),
         };

         assert!(success.ok);
         assert_eq!(success.bundle.session_code, session_code);
         assert_eq!(success.bundle.current_phase, protocol::Phase::End);
         assert_eq!(success.bundle.players.len(), 2);
         assert_eq!(success.bundle.dragons.len(), 2);
         let judged_dragon = success
             .bundle
             .dragons
             .iter()
             .find(|dragon| dragon.dragon_id == alice_dragon_id)
             .expect("judged dragon bundle");
         assert_eq!(judged_dragon.creative_vote_count, 1);
         assert_eq!(judged_dragon.handover_chain.discovery_observations, Vec::<String>::new());
         assert_eq!(judged_dragon.phase2_actions.len(), 1);
         assert_eq!(judged_dragon.phase2_actions[0].player_name, "Bob");
         assert_eq!(judged_dragon.phase2_actions[0].action_type, "feed");
     }

     #[tokio::test]
     async fn create_workshop_endpoint_returns_join_success() {
         let app = build_app(test_state());

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build request"),
             )
             .await
             .expect("call create workshop");

         assert_eq!(response.status(), StatusCode::OK);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read create body");
         let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
         match result {
             WorkshopJoinResult::Success(success) => {
                 assert!(success.ok);
                 assert_eq!(success.coordinator_type, CoordinatorType::Rust);
                 assert_eq!(success.state.current_player_id.as_deref(), Some(success.player_id.as_str()));
                 assert_eq!(success.state.players.len(), 1);
                 let host = success.state.players.get(&success.player_id).expect("host player in state");
                 assert_eq!(host.pet_description.as_deref(), Some("Alice's workshop dragon"));
             }
             WorkshopJoinResult::Error(error) => panic!("expected success, got error: {}", error.error),
         }
     }

     #[tokio::test]
     async fn create_workshop_endpoint_rejects_empty_host_name() {
         let app = build_app(test_state());

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"   "}"#))
                     .expect("build request"),
             )
             .await
             .expect("call create workshop");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read error body");
         let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
         match result {
             WorkshopJoinResult::Error(error) => assert_eq!(error.error, "Please enter a host name."),
             WorkshopJoinResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn create_workshop_endpoint_rejects_forbidden_origin() {
         let app = build_app(test_state());

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .header("origin", "https://evil.example.com")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build request"),
             )
             .await
             .expect("call create workshop");

         assert_eq!(response.status(), StatusCode::FORBIDDEN);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read error body");
         let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
         match result {
             WorkshopJoinResult::Error(error) => assert_eq!(error.error, "Origin is not allowed."),
             WorkshopJoinResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn create_workshop_endpoint_is_rate_limited_for_repeated_requests() {
         let app = build_app(test_state_with_limits(1, 40));

         let first = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .header("x-forwarded-for", "10.0.0.1")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build first request"),
             )
             .await
             .expect("call first create workshop");
         assert_eq!(first.status(), StatusCode::OK);

         let second = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .header("x-forwarded-for", "10.0.0.1")
                     .body(Body::from(r#"{"name":"Bob"}"#))
                     .expect("build second request"),
             )
             .await
             .expect("call second create workshop");

         assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
         let body = to_bytes(second.into_body(), usize::MAX).await.expect("read rate limited body");
         let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse rate limited result");
         match result {
             WorkshopJoinResult::Error(error) => {
                 assert_eq!(error.error, "Too many requests. Please slow down and try again.");
             }
             WorkshopJoinResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn join_workshop_endpoint_rejects_invalid_code() {
         let app = build_app(test_state());

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"sessionCode":"12ab56","name":"Bob"}"#))
                     .expect("build request"),
             )
             .await
             .expect("call join workshop");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read join body");
         let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
         match result {
             WorkshopJoinResult::Error(error) => assert_eq!(error.error, "Workshop codes must be 6 digits."),
             WorkshopJoinResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn join_workshop_endpoint_rejects_missing_session() {
         let app = build_app(test_state());

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"sessionCode":"123456","name":"Bob"}"#))
                     .expect("build request"),
             )
             .await
             .expect("call join workshop");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read join body");
         let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
         match result {
             WorkshopJoinResult::Error(error) => assert_eq!(error.error, "Workshop not found."),
             WorkshopJoinResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn join_workshop_endpoint_returns_join_success_for_lobby_session() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let session_code = match create_result {
             WorkshopJoinResult::Success(success) => success.session_code,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code)))
                     .expect("build request"),
             )
             .await
             .expect("call join workshop");

         assert_eq!(response.status(), StatusCode::OK);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read join body");
         let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
         match result {
             WorkshopJoinResult::Success(success) => {
                 assert!(success.ok);
                 assert_eq!(success.coordinator_type, CoordinatorType::Rust);
                 assert_eq!(success.state.players.len(), 2);
                 assert_eq!(success.state.current_player_id.as_deref(), Some(success.player_id.as_str()));
                 let joined = success.state.players.get(&success.player_id).expect("joined player in state");
                 assert_eq!(joined.pet_description.as_deref(), Some("Bob's workshop dragon"));
             }
             WorkshopJoinResult::Error(error) => panic!("expected success, got error: {}", error.error),
         }
     }

     #[tokio::test]
    async fn join_workshop_endpoint_reconnects_existing_player_without_name() {
        let state = test_state();
        let app = build_app(state.clone());
        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"Alice"}"#))
                    .expect("build create request"),
            )
            .await
            .expect("call create workshop");
        let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
        let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
        let create_success = match create_result {
            WorkshopJoinResult::Success(success) => success,
            WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
        };

        let start_phase1_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                        create_success.session_code, create_success.reconnect_token
                    )))
                    .expect("build start phase1 request"),
            )
            .await
            .expect("call start phase1 command");
        assert_eq!(start_phase1_response.status(), StatusCode::OK);

        {
            let mut sessions = state.sessions.lock().await;
            let session = sessions
                .get_mut(&create_success.session_code)
                .expect("session exists");
            let player = session
                .players
                .get_mut(&create_success.player_id)
                .expect("player exists");
            player.is_connected = false;
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/join")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                        create_success.session_code, create_success.reconnect_token
                    )))
                    .expect("build reconnect request"),
            )
            .await
            .expect("call reconnect join workshop");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.expect("read reconnect body");
        let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse reconnect result");
        match result {
            WorkshopJoinResult::Success(success) => {
                assert!(success.ok);
                assert_eq!(success.player_id, create_success.player_id);
                assert_eq!(success.reconnect_token, create_success.reconnect_token);
                assert_eq!(success.state.phase, protocol::Phase::Phase1);
                assert_eq!(success.state.current_player_id.as_deref(), Some(create_success.player_id.as_str()));
                let player = success
                    .state
                    .players
                    .get(&create_success.player_id)
                    .expect("reconnected player in state");
                assert!(player.is_connected);
                assert!(player.current_dragon_id.is_some());
            }
            WorkshopJoinResult::Error(error) => panic!("expected reconnect success, got error: {}", error.error),
        }
    }

    #[tokio::test]
    async fn join_workshop_endpoint_rejects_invalid_reconnect_token() {
        let app = build_app(test_state());
        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"Alice"}"#))
                    .expect("build create request"),
            )
            .await
            .expect("call create workshop");
        let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
        let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
        let session_code = match create_result {
            WorkshopJoinResult::Success(success) => success.session_code,
            WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
        };

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/join")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"sessionCode":"{}","reconnectToken":"missing"}}"#,
                        session_code
                    )))
                    .expect("build reconnect request"),
            )
            .await
            .expect("call reconnect join workshop");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.expect("read reconnect body");
        let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse reconnect result");
        match result {
            WorkshopJoinResult::Error(error) => {
                assert_eq!(error.error, "Session identity is invalid or expired.");
            }
            WorkshopJoinResult::Success(_) => panic!("expected reconnect error response"),
        }
    }

    #[tokio::test]
    async fn workshop_command_saves_observation_during_phase1() {
        let state = test_state();
        let app = build_app(state.clone());
        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"Alice"}"#))
                    .expect("build create request"),
            )
            .await
            .expect("call create workshop");
        let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
        let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
        let create_success = match create_result {
            WorkshopJoinResult::Success(success) => success,
            WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
        };

        let start_phase1_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                        create_success.session_code, create_success.reconnect_token
                    )))
                    .expect("build start phase1 request"),
            )
            .await
            .expect("call start phase1 command");
        assert_eq!(start_phase1_response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitObservation","payload":{{"text":"Calms down at dusk"}}}}"#,
                        create_success.session_code, create_success.reconnect_token
                    )))
                    .expect("build observation request"),
            )
            .await
            .expect("call observation command");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.expect("read observation body");
        let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse observation result");
        match result {
            WorkshopCommandResult::Success(success) => assert!(success.ok),
            WorkshopCommandResult::Error(error) => panic!("expected observation success, got error: {}", error.error),
        }

        let sessions = state.sessions.lock().await;
        let session = sessions
            .get(&create_success.session_code)
            .expect("session exists");
        let dragon_id = session
            .players
            .get(&create_success.player_id)
            .and_then(|player| player.current_dragon_id.as_ref())
            .expect("current dragon id");
        let dragon = session.dragons.get(dragon_id).expect("dragon exists");
        assert_eq!(dragon.discovery_observations, vec!["Calms down at dusk".to_string()]);
    }

    #[tokio::test]
    async fn workshop_command_records_action_artifact_during_phase2() {
        let state = test_state();
        let app = build_app(state.clone());
        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"Alice"}"#))
                    .expect("build create request"),
            )
            .await
            .expect("call create workshop");
        let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
        let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
        let create_success = match create_result {
            WorkshopJoinResult::Success(success) => success,
            WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
        };
        let join_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/join")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, create_success.session_code)))
                    .expect("build join request"),
            )
            .await
            .expect("call join workshop");
        let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
        let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
        let join_success = match join_result {
            WorkshopJoinResult::Success(success) => success,
            WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
        };

        for request_body in [
            format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, create_success.session_code, create_success.reconnect_token),
            format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, create_success.session_code, create_success.reconnect_token),
            format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["Rule 1","Rule 2","Rule 3"]}}"#, create_success.session_code, create_success.reconnect_token),
            format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["Rule A","Rule B","Rule C"]}}"#, create_success.session_code, join_success.reconnect_token),
            format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#, create_success.session_code, create_success.reconnect_token),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/workshops/command")
                        .header("content-type", "application/json")
                        .body(Body::from(request_body))
                        .expect("build setup request"),
                )
                .await
                .expect("call setup command");
            assert_eq!(response.status(), StatusCode::OK);
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"action","payload":{{"type":"sleep"}}}}"#,
                        create_success.session_code, create_success.reconnect_token
                    )))
                    .expect("build action request"),
            )
            .await
            .expect("call action command");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.expect("read action body");
        let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse action result");
        match result {
            WorkshopCommandResult::Success(success) => assert!(success.ok),
            WorkshopCommandResult::Error(error) => panic!("expected action success, got error: {}", error.error),
        }

        let artifacts = state
            .store
            .list_session_artifacts(&create_success.state.session.id)
            .expect("list artifacts");
        let action_artifact = artifacts
            .iter()
            .rev()
            .find(|artifact| artifact.kind == SessionArtifactKind::ActionProcessed)
            .expect("action artifact exists");
        assert_eq!(action_artifact.phase, protocol::Phase::Phase2);
        assert_eq!(action_artifact.payload.get("actionType").and_then(|value| value.as_str()), Some("sleep"));
        assert!(action_artifact.payload.get("dragonId").and_then(|value| value.as_str()).is_some());
    }

    #[tokio::test]
     async fn workshop_command_rejects_invalid_credentials() {
         let app = build_app(test_state());

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"sessionCode":"123456","reconnectToken":"missing","command":"startPhase1"}"#))
                     .expect("build request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Session identity is invalid or expired.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_rejects_non_host_start_phase1() {
         let state = test_state();
         let app = build_app(state.clone());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };
         let session_code = create_success.session_code.clone();
         let join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code)))
                     .expect("build join request"),
             )
             .await
             .expect("call join workshop");
         let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
         let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
         let join_success = match join_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
         };

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, session_code, join_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Only the host can start the workshop.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_starts_phase1_for_single_player_host() {
         let state = test_state();
         let app = build_app(state.clone());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::OK);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Success(success) => assert!(success.ok),
             WorkshopCommandResult::Error(error) => panic!("expected success, got error: {}", error.error),
         }

         let sessions = state.sessions.lock().await;
         let session = sessions.get(&create_success.session_code).expect("session exists");
         assert_eq!(session.phase, protocol::Phase::Phase1);
         assert_eq!(session.dragons.len(), 1);
     }

     #[tokio::test]
     async fn workshop_command_rejects_start_handover_outside_phase1() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Handover can only begin during Phase 1.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_rejects_non_host_start_handover() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };
         let session_code = create_success.session_code.clone();
         let join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code)))
                     .expect("build join request"),
             )
             .await
             .expect("call join workshop");
         let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
         let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
         let join_success = match join_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
         };

         let host_start_phase1_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call startPhase1 command");
         assert_eq!(host_start_phase1_response.status(), StatusCode::OK);

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, session_code, join_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Only the host can trigger handover.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_starts_handover_for_host_in_phase1() {
         let state = test_state();
         let app = build_app(state.clone());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let start_phase1_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build start phase1 request"),
             )
             .await
             .expect("call startPhase1 command");
         assert_eq!(start_phase1_response.status(), StatusCode::OK);

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build handover request"),
             )
             .await
             .expect("call handover command");

         assert_eq!(response.status(), StatusCode::OK);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Success(success) => assert!(success.ok),
             WorkshopCommandResult::Error(error) => panic!("expected success, got error: {}", error.error),
         }

         let sessions = state.sessions.lock().await;
         let session = sessions.get(&create_success.session_code).expect("session exists");
         assert_eq!(session.phase, protocol::Phase::Handover);
     }

     #[tokio::test]
     async fn workshop_command_rejects_submit_tags_outside_handover() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["a","b","c"]}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Handover notes can only be saved during handover.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_rejects_invalid_submit_tags_payload() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let start_phase1_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build start phase1 request"),
             )
             .await
             .expect("call startPhase1 command");
         assert_eq!(start_phase1_response.status(), StatusCode::OK);

         let start_handover_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build start handover request"),
             )
             .await
             .expect("call startHandover command");
         assert_eq!(start_handover_response.status(), StatusCode::OK);

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":{{"tags":["a"]}}}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Handover notes must be sent as a list.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_saves_submit_tags_in_handover_phase() {
         let state = test_state();
         let app = build_app(state.clone());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let start_phase1_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build start phase1 request"),
             )
             .await
             .expect("call startPhase1 command");
         assert_eq!(start_phase1_response.status(), StatusCode::OK);

         let start_handover_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build start handover request"),
             )
             .await
             .expect("call startHandover command");
         assert_eq!(start_handover_response.status(), StatusCode::OK);

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["first","second","third","fourth"]}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::OK);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Success(success) => assert!(success.ok),
             WorkshopCommandResult::Error(error) => panic!("expected success, got error: {}", error.error),
         }

         let sessions = state.sessions.lock().await;
         let session = sessions.get(&create_success.session_code).expect("session exists");
         let dragon_id = session
             .players
             .get(&create_success.player_id)
             .and_then(|player| player.current_dragon_id.clone())
             .expect("current dragon id");
         let dragon = session.dragons.get(&dragon_id).expect("dragon exists");
         assert_eq!(dragon.handover_tags, vec!["first", "second", "third"]);
     }

     #[tokio::test]
     async fn workshop_command_rejects_start_phase2_outside_handover() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Phase 2 can only begin from handover.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_rejects_non_host_start_phase2() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };
         let session_code = create_success.session_code.clone();

         let join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code)))
                     .expect("build join request"),
             )
             .await
             .expect("call join workshop");
         let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
         let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
         let join_success = match join_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
         };

         let start_phase1_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build start phase1 request"),
             )
             .await
             .expect("call startPhase1 command");
         assert_eq!(start_phase1_response.status(), StatusCode::OK);

         let start_handover_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build start handover request"),
             )
             .await
             .expect("call startHandover command");
         assert_eq!(start_handover_response.status(), StatusCode::OK);

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#, session_code, join_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Only the host can begin Phase 2.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_rejects_start_phase2_when_tags_are_missing() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let start_phase1_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build start phase1 request"),
             )
             .await
             .expect("call startPhase1 command");
         assert_eq!(start_phase1_response.status(), StatusCode::OK);

         let start_handover_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build start handover request"),
             )
             .await
             .expect("call startHandover command");
         assert_eq!(start_handover_response.status(), StatusCode::OK);

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Still waiting on: Alice.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_starts_phase2_when_handover_is_complete() {
         let state = test_state();
         let app = build_app(state.clone());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let start_phase1_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build start phase1 request"),
             )
             .await
             .expect("call startPhase1 command");
         assert_eq!(start_phase1_response.status(), StatusCode::OK);

         let start_handover_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build start handover request"),
             )
             .await
             .expect("call startHandover command");
         assert_eq!(start_handover_response.status(), StatusCode::OK);

         let submit_tags_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["first","second","third"]}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build submit tags request"),
             )
             .await
             .expect("call submitTags command");
         assert_eq!(submit_tags_response.status(), StatusCode::OK);

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::OK);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Success(success) => assert!(success.ok),
             WorkshopCommandResult::Error(error) => panic!("expected success, got error: {}", error.error),
         }

         let sessions = state.sessions.lock().await;
         let session = sessions.get(&create_success.session_code).expect("session exists");
         assert_eq!(session.phase, protocol::Phase::Phase2);
     }

     #[tokio::test]
     async fn workshop_command_rejects_end_game_outside_phase2() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Voting can only begin from Phase 2.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_rejects_non_host_end_game() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };
         let session_code = create_success.session_code.clone();

         let join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code)))
                     .expect("build join request"),
             )
             .await
             .expect("call join workshop");
         let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
         let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
         let join_success = match join_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
         };

         let start_phase1_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build start phase1 request"),
             )
             .await
             .expect("call startPhase1 command");
         assert_eq!(start_phase1_response.status(), StatusCode::OK);

         let start_handover_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build start handover request"),
             )
             .await
             .expect("call startHandover command");
         assert_eq!(start_handover_response.status(), StatusCode::OK);

         let submit_tags_host_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#, session_code, create_success.reconnect_token)))
                     .expect("build submit tags request"),
             )
             .await
             .expect("call host submitTags command");
         assert_eq!(submit_tags_host_response.status(), StatusCode::OK);

         let submit_tags_join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#, session_code, join_success.reconnect_token)))
                     .expect("build submit tags request"),
             )
             .await
             .expect("call join submitTags command");
         assert_eq!(submit_tags_join_response.status(), StatusCode::OK);

         let start_phase2_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build start phase2 request"),
             )
             .await
             .expect("call startPhase2 command");
         assert_eq!(start_phase2_response.status(), StatusCode::OK);

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#, session_code, join_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Only the host can end the workshop.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_starts_voting_when_host_ends_multiplayer_phase2() {
         let state = test_state();
         let app = build_app(state.clone());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };
         let session_code = create_success.session_code.clone();

         let join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code)))
                     .expect("build join request"),
             )
             .await
             .expect("call join workshop");
         let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
         let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
         let join_success = match join_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
         };

         for request_body in [
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#, session_code, join_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#, session_code, create_success.reconnect_token),
         ] {
             let response = app
                 .clone()
                 .oneshot(
                     Request::builder()
                         .method("POST")
                         .uri("/api/workshops/command")
                         .header("content-type", "application/json")
                         .body(Body::from(request_body))
                         .expect("build command request"),
                 )
                 .await
                 .expect("call setup command");
             assert_eq!(response.status(), StatusCode::OK);
         }

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call endGame command");

         assert_eq!(response.status(), StatusCode::OK);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Success(success) => assert!(success.ok),
             WorkshopCommandResult::Error(error) => panic!("expected success, got error: {}", error.error),
         }

         let sessions = state.sessions.lock().await;
         let session = sessions.get(&session_code).expect("session exists");
         assert_eq!(session.phase, protocol::Phase::Voting);
         assert!(session.voting.is_some());
     }

     #[tokio::test]
     async fn workshop_command_rejects_submit_vote_outside_voting() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"dragon-a"}}}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Voting is not active right now.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_rejects_invalid_submit_vote_payload() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragon":"dragon-a"}}}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Voting is not active right now.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_rejects_self_vote_in_voting() {
         let state = test_state();
         let app = build_app(state.clone());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };
         let session_code = create_success.session_code.clone();

         let join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code)))
                     .expect("build join request"),
             )
             .await
             .expect("call join workshop");
         let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
         let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
         let join_success = match join_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
         };

         for request_body in [
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#, session_code, join_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#, session_code, create_success.reconnect_token),
         ] {
             let response = app
                 .clone()
                 .oneshot(
                     Request::builder()
                         .method("POST")
                         .uri("/api/workshops/command")
                         .header("content-type", "application/json")
                         .body(Body::from(request_body))
                         .expect("build setup request"),
                 )
                 .await
                 .expect("call setup command");
             assert_eq!(response.status(), StatusCode::OK);
         }

         let sessions = state.sessions.lock().await;
         let session = sessions.get(&session_code).expect("session exists");
         let bob_dragon_id = session
             .players
             .get(&join_success.player_id)
             .and_then(|player| player.current_dragon_id.clone())
             .expect("bob dragon id");
         drop(sessions);

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#, session_code, join_success.reconnect_token, bob_dragon_id)))
                     .expect("build command request"),
             )
             .await
             .expect("call submitVote command");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "You cannot vote for your own dragon.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_accepts_valid_vote_in_voting() {
         let state = test_state();
         let app = build_app(state.clone());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };
         let session_code = create_success.session_code.clone();

         let join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code)))
                     .expect("build join request"),
             )
             .await
             .expect("call join workshop");
         let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
         let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
         let join_success = match join_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
         };

         for request_body in [
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#, session_code, join_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#, session_code, create_success.reconnect_token),
         ] {
             let response = app
                 .clone()
                 .oneshot(
                     Request::builder()
                         .method("POST")
                         .uri("/api/workshops/command")
                         .header("content-type", "application/json")
                         .body(Body::from(request_body))
                         .expect("build setup request"),
                 )
                 .await
                 .expect("call setup command");
             assert_eq!(response.status(), StatusCode::OK);
         }

         let sessions = state.sessions.lock().await;
         let session = sessions.get(&session_code).expect("session exists");
         let bob_dragon_id = session
             .players
             .get(&join_success.player_id)
             .and_then(|player| player.current_dragon_id.clone())
             .expect("bob dragon id");
         drop(sessions);

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#, session_code, create_success.reconnect_token, bob_dragon_id)))
                     .expect("build command request"),
             )
             .await
             .expect("call submitVote command");

         assert_eq!(response.status(), StatusCode::OK);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Success(success) => assert!(success.ok),
             WorkshopCommandResult::Error(error) => panic!("expected success, got error: {}", error.error),
         }
     }

     #[tokio::test]
     async fn workshop_command_rejects_reveal_results_outside_voting() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Results can only be revealed during voting.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_rejects_non_host_reveal_results() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };
         let session_code = create_success.session_code.clone();

         let join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code)))
                     .expect("build join request"),
             )
             .await
             .expect("call join workshop");
         let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
         let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
         let join_success = match join_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
         };

         for request_body in [
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#, session_code, join_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#, session_code, create_success.reconnect_token),
         ] {
             let response = app
                 .clone()
                 .oneshot(
                     Request::builder()
                         .method("POST")
                         .uri("/api/workshops/command")
                         .header("content-type", "application/json")
                         .body(Body::from(request_body))
                         .expect("build setup request"),
                 )
                 .await
                 .expect("call setup command");
             assert_eq!(response.status(), StatusCode::OK);
         }

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, session_code, join_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Only the host can reveal voting results.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_rejects_reveal_results_while_votes_are_pending() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };
         let session_code = create_success.session_code.clone();

         let join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code)))
                     .expect("build join request"),
             )
             .await
             .expect("call join workshop");
         let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
         let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
         let join_success = match join_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
         };

         for request_body in [
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#, session_code, join_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#, session_code, create_success.reconnect_token),
         ] {
             let response = app
                 .clone()
                 .oneshot(
                     Request::builder()
                         .method("POST")
                         .uri("/api/workshops/command")
                         .header("content-type", "application/json")
                         .body(Body::from(request_body))
                         .expect("build setup request"),
                 )
                 .await
                 .expect("call setup command");
             assert_eq!(response.status(), StatusCode::OK);
         }

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Wait until every eligible player has voted.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_reveals_voting_results_after_all_votes() {
         let state = test_state();
         let app = build_app(state.clone());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };
         let session_code = create_success.session_code.clone();

         let join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code)))
                     .expect("build join request"),
             )
             .await
             .expect("call join workshop");
         let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
         let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
         let join_success = match join_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
         };

         for request_body in [
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#, session_code, join_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#, session_code, create_success.reconnect_token),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#, session_code, create_success.reconnect_token),
         ] {
             let response = app
                 .clone()
                 .oneshot(
                     Request::builder()
                         .method("POST")
                         .uri("/api/workshops/command")
                         .header("content-type", "application/json")
                         .body(Body::from(request_body))
                         .expect("build setup request"),
                 )
                 .await
                 .expect("call setup command");
             assert_eq!(response.status(), StatusCode::OK);
         }

         let sessions = state.sessions.lock().await;
         let session = sessions.get(&session_code).expect("session exists");
         let alice_dragon_id = session
             .players
             .get(&create_success.player_id)
             .and_then(|player| player.current_dragon_id.clone())
             .expect("alice dragon id");
         let bob_dragon_id = session
             .players
             .get(&join_success.player_id)
             .and_then(|player| player.current_dragon_id.clone())
             .expect("bob dragon id");
         drop(sessions);

         for request_body in [
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#, session_code, create_success.reconnect_token, bob_dragon_id),
             format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#, session_code, join_success.reconnect_token, alice_dragon_id),
         ] {
             let response = app
                 .clone()
                 .oneshot(
                     Request::builder()
                         .method("POST")
                         .uri("/api/workshops/command")
                         .header("content-type", "application/json")
                         .body(Body::from(request_body))
                         .expect("build vote request"),
                 )
                 .await
                 .expect("call submitVote command");
             assert_eq!(response.status(), StatusCode::OK);
         }

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call revealVotingResults command");

         assert_eq!(response.status(), StatusCode::OK);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Success(success) => assert!(success.ok),
             WorkshopCommandResult::Error(error) => panic!("expected success, got error: {}", error.error),
         }

         let sessions = state.sessions.lock().await;
         let session = sessions.get(&session_code).expect("session exists");
         assert_eq!(session.phase, protocol::Phase::End);
         assert!(session.players.values().all(|player| player.score >= 0));
     }

     #[tokio::test]
     async fn workshop_command_rejects_non_host_reset_game() {
         let app = build_app(test_state());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let session_code = match create_result {
             WorkshopJoinResult::Success(success) => success.session_code,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };
         let join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/join")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","name":"Bob"}}"#, session_code)))
                     .expect("build join request"),
             )
             .await
             .expect("call join workshop");
         let join_body = to_bytes(join_response.into_body(), usize::MAX).await.expect("read join body");
         let join_result: WorkshopJoinResult = serde_json::from_slice(&join_body).expect("parse join result");
         let join_success = match join_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected join success, got error: {}", error.error),
         };

         let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"resetGame"}}"#, session_code, join_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
         let body = to_bytes(response.into_body(), usize::MAX).await.expect("read command body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse command result");
         match result {
             WorkshopCommandResult::Error(error) => {
                 assert_eq!(error.error, "Only the host can reset the workshop.");
             }
             WorkshopCommandResult::Success(_) => panic!("expected error response"),
         }
     }

     #[tokio::test]
     async fn workshop_command_reset_game_returns_session_to_lobby() {
         let state = test_state();
         let app = build_app(state.clone());
         let create_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"name":"Alice"}"#))
                     .expect("build create request"),
             )
             .await
             .expect("call create workshop");
         let create_body = to_bytes(create_response.into_body(), usize::MAX).await.expect("read create body");
         let create_result: WorkshopJoinResult = serde_json::from_slice(&create_body).expect("parse create result");
         let create_success = match create_result {
             WorkshopJoinResult::Success(success) => success,
             WorkshopJoinResult::Error(error) => panic!("expected create success, got error: {}", error.error),
         };

         let start_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build start request"),
             )
             .await
             .expect("call startPhase1 command");
         assert_eq!(start_response.status(), StatusCode::OK);

         let reset_response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"resetGame"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build reset request"),
             )
             .await
             .expect("call reset command");

         assert_eq!(reset_response.status(), StatusCode::OK);
         let body = to_bytes(reset_response.into_body(), usize::MAX).await.expect("read reset body");
         let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse reset result");
         match result {
             WorkshopCommandResult::Success(success) => assert!(success.ok),
             WorkshopCommandResult::Error(error) => panic!("expected success, got error: {}", error.error),
         }

         let sessions = state.sessions.lock().await;
         let session = sessions.get(&create_success.session_code).expect("session exists");
         assert_eq!(session.phase, protocol::Phase::Lobby);
         assert!(session.dragons.is_empty());
         assert!(session.players.values().all(|player| player.current_dragon_id.is_none()));
     }
 }
