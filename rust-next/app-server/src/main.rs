use axum::{
    extract::State,
    http::HeaderMap,
    http::StatusCode,
    routing::post,
    routing::get,
    Json, Router,
};
use chrono::Utc;
use domain::{DomainError, Phase1Assignment, SessionCode, SessionPlayer, WorkshopSession};
use persistence::{InMemorySessionStore, SessionStore};
use protocol::{
    create_default_session_settings, ClientGameState, CoordinatorType, CreateWorkshopRequest,
    DragonStats, JudgeActionTrace, JoinWorkshopRequest, Player, SessionArtifactKind, SessionArtifactRecord, SessionMeta,
    SessionCommand, VotePayload, WorkshopCommandRequest, WorkshopCommandResult, WorkshopCommandSuccess,
    WorkshopError, WorkshopJoinResult, WorkshopJoinSuccess,
};
use realtime::SessionRegistry;
use security::{
    create_origin_policy, validate_session_code, FixedWindowRateLimiter, OriginPolicy, OriginPolicyOptions,
    DEFAULT_RUST_SESSION_CODE_PREFIX,
};
use serde::Serialize;
use serde_json::json;
use std::{
    collections::BTreeMap, env, net::SocketAddr, str::FromStr, sync::Arc,
};
use tokio::sync::Mutex;
use tower_http::trace::TraceLayer;
use tracing::info;
use uuid::Uuid;

#[derive(Debug, Clone)]
struct AppConfig {
    bind_addr: SocketAddr,
    is_production: bool,
    rust_session_code_prefix: String,
    origin_policy: OriginPolicy,
}

#[derive(Clone)]
struct AppState {
    config: Arc<AppConfig>,
    store: Arc<InMemorySessionStore>,
    sessions: Arc<Mutex<BTreeMap<String, WorkshopSession>>>,
    create_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    join_limiter: Arc<Mutex<FixedWindowRateLimiter>>,
    realtime: Arc<Mutex<SessionRegistry>>,
}

#[derive(Debug, Serialize)]
struct RuntimeSnapshot {
    bind_addr: String,
    is_production: bool,
    rust_session_code_prefix: String,
    allow_any_origin: bool,
    require_origin: bool,
    allowed_origins: Vec<String>,
    active_realtime_sessions: usize,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Arc::new(load_config().expect("load app config"));
    let store = Arc::new(InMemorySessionStore::new());
    store.init().expect("init session store");

    let state = AppState {
        config: config.clone(),
        store,
        sessions: Arc::new(Mutex::new(BTreeMap::new())),
        create_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(20, 60_000))),
        join_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(40, 60_000))),
        realtime: Arc::new(Mutex::new(SessionRegistry::new())),
    };

    let app = build_app(state);

    info!(bind_addr = %config.bind_addr, "starting rust-next app-server");

    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .expect("bind listener");
    axum::serve(listener, app).await.expect("serve app");
}

fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/api/workshops", post(create_workshop))
        .route("/api/workshops/join", post(join_workshop))
        .route("/api/workshops/command", post(workshop_command))
        .route("/api/live", get(live))
        .route("/api/ready", get(ready))
        .route("/api/runtime", get(runtime_snapshot))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
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

    Ok(AppConfig {
        bind_addr,
        is_production,
        rust_session_code_prefix,
        origin_policy,
    })
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

    if let Err(error) = state.store.save_session(&session.summary()) {
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

    let normalized_name = payload.name.unwrap_or_default().trim().to_string();
    if normalized_name.is_empty() {
        return bad_join_request("Please enter a player name.");
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

    if let Err(error) = state.store.save_session(&session.summary()) {
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

    (StatusCode::OK, Json(WorkshopJoinResult::Success(response)))
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

    let mut sessions = state.sessions.lock().await;
    let Some(session) = sessions.get_mut(session_code) else {
        return bad_command_request("Workshop not found.");
    };

    match request.command {
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
            if let Err(error) = state.store.save_session(&session.summary()) {
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

            (
                StatusCode::OK,
                Json(WorkshopCommandResult::Success(WorkshopCommandSuccess { ok: true })),
            )
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
            if let Err(error) = state.store.save_session(&session.summary()) {
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

            (
                StatusCode::OK,
                Json(WorkshopCommandResult::Success(WorkshopCommandSuccess { ok: true })),
            )
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
            if let Err(error) = state.store.save_session(&session.summary()) {
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

            (
                StatusCode::OK,
                Json(WorkshopCommandResult::Success(WorkshopCommandSuccess { ok: true })),
            )
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
            if let Err(error) = state.store.save_session(&session.summary()) {
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

            (
                StatusCode::OK,
                Json(WorkshopCommandResult::Success(WorkshopCommandSuccess { ok: true })),
            )
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
            if let Err(error) = state.store.save_session(&session.summary()) {
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

            (
                StatusCode::OK,
                Json(WorkshopCommandResult::Success(WorkshopCommandSuccess { ok: true })),
            )
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
            if let Err(error) = state.store.save_session(&session.summary()) {
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

            (
                StatusCode::OK,
                Json(WorkshopCommandResult::Success(WorkshopCommandSuccess { ok: true })),
            )
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
            if let Err(error) = state.store.save_session(&session.summary()) {
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

            (
                StatusCode::OK,
                Json(WorkshopCommandResult::Success(WorkshopCommandSuccess { ok: true })),
            )
        }
        SessionCommand::ResetGame => {
            if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                return bad_command_request("Only the host can reset the workshop.");
            }
            if let Err(error) = session.reset_to_lobby() {
                return bad_command_request(&error.to_string());
            }
            if let Err(error) = state.store.save_session(&session.summary()) {
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

            (
                StatusCode::OK,
                Json(WorkshopCommandResult::Success(WorkshopCommandSuccess { ok: true })),
            )
        }
        _ => bad_command_request("Unsupported workshop command."),
    }
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
         if !state.sessions.lock().await.contains_key(&candidate) {
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

 #[cfg(test)]
 mod tests {
     use super::*;
     use axum::{
         body::{to_bytes, Body},
         http::{Request, StatusCode},
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
         });

         AppState {
             config,
             store: Arc::new(InMemorySessionStore::new()),
             sessions: Arc::new(Mutex::new(BTreeMap::new())),
             create_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(create_limit, 60_000))),
             join_limiter: Arc::new(Mutex::new(FixedWindowRateLimiter::new(join_limit, 60_000))),
             realtime: Arc::new(Mutex::new(SessionRegistry::new())),
         }
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
