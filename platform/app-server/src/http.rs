use axum::{
    Json,
    extract::{ConnectInfo, FromRequestParts, State},
    http::{HeaderMap, StatusCode, request::Parts},
};
use chrono::{DateTime, Utc};
use domain::{DomainError, Phase1Assignment, SessionCode, SessionPlayer, WorkshopSession};
use protocol::{
    ActionPayload, CoordinatorType, CreateWorkshopRequest, DiscoveryObservationRequest,
    JoinWorkshopRequest, JudgeBundle, JudgeDragonBundle, LlmDragonEvaluation, LlmImageRequest,
    LlmImageResult, LlmImageSuccess, LlmJudgeEvaluation, LlmJudgeRequest, LlmJudgeResult,
    LlmJudgeSuccess, SessionArtifactKind, SessionArtifactRecord, SessionCommand,
    SpriteSheetRequest, SpriteSheetResult, SpriteSheetSuccess, UpdatePlayerPetRequest, VotePayload,
    WorkshopCommandRequest, WorkshopCommandResult, WorkshopCommandSuccess, WorkshopError,
    WorkshopJoinResult, WorkshopJoinSuccess, WorkshopJudgeBundleRequest, WorkshopJudgeBundleResult,
    WorkshopJudgeBundleSuccess,
};
use security::{FixedWindowRateLimiter, OriginPolicy};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    net::SocketAddr,
    sync::Arc,
};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::app::AppState;
use crate::cache::{SessionWriteLease, ensure_session_cached, reload_cached_session};
use crate::helpers::{
    build_judge_bundle, parse_player_action, phase_step, random_prefixed_id,
    session_config_from_request, to_client_game_state,
};
use crate::ws::broadcast_session_state;

fn clamp_score(score: i32) -> i32 {
    score.clamp(0, 100)
}

fn active_time_keyword(active_time: protocol::ActiveTime) -> &'static str {
    match active_time {
        protocol::ActiveTime::Day => "day",
        protocol::ActiveTime::Night => "night",
    }
}

fn food_keyword(food: protocol::FoodType) -> &'static str {
    match food {
        protocol::FoodType::Meat => "meat",
        protocol::FoodType::Fruit => "fruit",
        protocol::FoodType::Fish => "fish",
    }
}

fn play_keyword(play: protocol::PlayType) -> &'static str {
    match play {
        protocol::PlayType::Fetch => "fetch",
        protocol::PlayType::Puzzle => "puzzle",
        protocol::PlayType::Music => "music",
    }
}

fn unique_keyword_hits(text: &str, keywords: &[&str]) -> i32 {
    let mut seen = BTreeSet::new();
    let mut hits = 0;

    for keyword in keywords {
        if seen.insert(*keyword) && text.contains(keyword) {
            hits += 1;
        }
    }

    hits
}

fn deterministic_local_judge_dragon_evaluation(dragon: &JudgeDragonBundle) -> LlmDragonEvaluation {
    let observation_count = dragon
        .handover_chain
        .discovery_observations
        .iter()
        .filter(|note| !note.trim().is_empty())
        .count() as i32;
    let tags_count = dragon
        .handover_chain
        .handover_tags
        .iter()
        .filter(|tag| !tag.trim().is_empty())
        .count() as i32;

    let combined_notes = std::iter::once(dragon.handover_chain.creator_instructions.as_str())
        .chain(
            dragon
                .handover_chain
                .discovery_observations
                .iter()
                .map(String::as_str),
        )
        .chain(
            dragon
                .handover_chain
                .handover_tags
                .iter()
                .map(String::as_str),
        )
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();

    let mechanic_clue_hits =
        (combined_notes.contains(active_time_keyword(dragon.actual_active_time)) as i32)
            + unique_keyword_hits(
                &combined_notes,
                &[
                    food_keyword(dragon.actual_day_food),
                    food_keyword(dragon.actual_night_food),
                ],
            )
            + unique_keyword_hits(
                &combined_notes,
                &[
                    play_keyword(dragon.actual_day_play),
                    play_keyword(dragon.actual_night_play),
                ],
            );

    let completeness_score = match observation_count + tags_count {
        0 => 0,
        1 => 5,
        2 => 10,
        3 => 14,
        4 => 17,
        _ => 20,
    };
    let clarity_score = if observation_count >= 2 && tags_count >= 2 {
        15
    } else if observation_count + tags_count >= 3 {
        10
    } else if observation_count + tags_count >= 1 {
        5
    } else {
        0
    };
    let observation_score = clamp_score(
        (mechanic_clue_hits * 8).min(40)
            + ((tags_count * 8).min(24) + if tags_count > 0 { 1 } else { 0 })
            + completeness_score
            + clarity_score,
    );

    let total_actions = dragon.total_actions.max(0);
    let correct_actions = dragon.correct_actions.clamp(0, total_actions);
    let final_hunger = dragon.final_stats.hunger.clamp(0, 100);
    let final_energy = dragon.final_stats.energy.clamp(0, 100);
    let final_happiness = dragon.final_stats.happiness.clamp(0, 100);
    let stat_average = (final_hunger + final_energy + final_happiness) / 3;
    let recovery_delta = final_happiness - dragon.phase2_lowest_happiness.clamp(0, 100);
    let recovery_score = if recovery_delta >= 25 && final_happiness >= 70 {
        5
    } else if recovery_delta >= 15 && final_happiness >= 55 {
        4
    } else if recovery_delta >= 10 && final_happiness >= 45 {
        3
    } else if recovery_delta > 0 {
        2
    } else if final_happiness >= dragon.phase2_lowest_happiness {
        1
    } else {
        0
    };
    let care_score = clamp_score(
        if total_actions == 0 {
            0
        } else {
            correct_actions * 50 / total_actions
        } + stat_average / 5
            + (15
                - dragon.wrong_food_count.max(0) * 4
                - dragon.wrong_play_count.max(0) * 4
                - dragon.penalty_stacks_at_end.max(0) * 2)
                .clamp(0, 15)
            + (10 - dragon.cooldown_violations.max(0) * 2).clamp(0, 10)
            + recovery_score,
    );

    let word_count = combined_notes.split_whitespace().count() as i32;
    let creativity_score =
        clamp_score(20 + (word_count / 2).min(45) + observation_count * 5 + tags_count * 5);

    let mistake_count = dragon.wrong_food_count.max(0) + dragon.wrong_play_count.max(0);
    let feedback = if total_actions == 0 {
        format!(
            "Discovery covered {mechanic_clue_hits}/5 core clues with {tags_count} handover tags. Care had no recorded Phase 2 actions and finished at {stat_average}/100 average stats.",
        )
    } else if mistake_count > 0 || dragon.cooldown_violations > 0 {
        format!(
            "Discovery covered {mechanic_clue_hits}/5 core clues with {tags_count} handover tags. Care finished at {stat_average}/100 average stats with {correct_actions}/{total_actions} correct actions, {mistake_count} wrong actions, and {} cooldown violations.",
            dragon.cooldown_violations.max(0),
        )
    } else {
        format!(
            "Discovery covered {mechanic_clue_hits}/5 core clues with {tags_count} handover tags. Care finished at {stat_average}/100 average stats with {correct_actions}/{total_actions} correct actions.",
        )
    };

    LlmDragonEvaluation {
        dragon_id: dragon.dragon_id.clone(),
        dragon_name: dragon.dragon_name.clone(),
        observation_score,
        care_score,
        creativity_score,
        feedback,
    }
}

fn deterministic_local_judge_evaluation(bundle: &JudgeBundle) -> LlmJudgeEvaluation {
    let dragon_evaluations = bundle
        .dragons
        .iter()
        .map(deterministic_local_judge_dragon_evaluation)
        .collect::<Vec<_>>();
    let dragon_count = dragon_evaluations.len() as i32;
    let (avg_observation, avg_care) = if dragon_count > 0 {
        let total_observation = dragon_evaluations
            .iter()
            .map(|evaluation| evaluation.observation_score)
            .sum::<i32>();
        let total_care = dragon_evaluations
            .iter()
            .map(|evaluation| evaluation.care_score)
            .sum::<i32>();
        (total_observation / dragon_count, total_care / dragon_count)
    } else {
        (0, 0)
    };

    LlmJudgeEvaluation {
        summary: format!(
            "Local judge fallback scored {dragon_count} dragons because no judge model is configured. Average observation was {avg_observation}/100 and average care was {avg_care}/100.",
        ),
        dragon_evaluations,
    }
}

async fn run_judge_for_session(
    state: &AppState,
    session_code: &str,
    actor_player_id: &str,
) -> Result<LlmJudgeEvaluation, String> {
    let session = {
        match ensure_session_cached(state, session_code).await {
            Ok(true) => {}
            Ok(false) => return Err("Workshop not found.".to_string()),
            Err(error) => return Err(format!("failed to load session: {error}")),
        }
        let sessions = state.sessions.lock().await;
        let Some(session) = sessions.get(session_code) else {
            return Err("Workshop not found.".to_string());
        };
        session.clone()
    };

    let artifacts = state
        .store
        .list_session_artifacts(&session.id.to_string())
        .await
        .map_err(|error| format!("failed to list session artifacts: {error}"))?;

    let bundle = build_judge_bundle(&session, &artifacts);
    let evaluation = if state.config.llm_pool.is_judge_configured() {
        match state.llm_client.judge(&bundle).await {
            Ok(evaluation) => evaluation,
            Err(error) => {
                tracing::warn!(
                    %session_code,
                    %error,
                    "LLM judge failed, using deterministic local fallback"
                );
                deterministic_local_judge_evaluation(&bundle)
            }
        }
    } else {
        tracing::info!(%session_code, "using deterministic local judge fallback");
        deterministic_local_judge_evaluation(&bundle)
    };

    let (_, _write_guard, write_lease) = SessionWriteLease::acquire(state, session_code)
        .await
        .map_err(|error| format!("failed to acquire session lease: {error}"))?;
    write_lease
        .ensure_active()
        .map_err(|error| format!("lost session lease before judge mutation: {error}"))?;

    if !reload_cached_session(state, session_code)
        .await
        .map_err(|error| format!("failed to reload session before judge persist: {error}"))?
    {
        return Err("Workshop not found.".to_string());
    }
    write_lease
        .ensure_active()
        .map_err(|error| format!("lost session lease before judge scoring: {error}"))?;

    let (session_before, session_snapshot, artifact) = {
        let mut sessions = state.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_code) else {
            return Err("Workshop not found.".to_string());
        };
        let session_before = session.clone();
        if session.phase == protocol::Phase::Phase2 {
            session.award_phase_end_achievements();
        }
        let score_tuples: Vec<(String, i32, i32, String)> = evaluation
            .dragon_evaluations
            .iter()
            .map(|d| {
                (
                    d.dragon_id.clone(),
                    d.observation_score,
                    d.care_score,
                    d.feedback.clone(),
                )
            })
            .collect();
        session.apply_judge_scores(&score_tuples);
        if session.phase == protocol::Phase::Phase2 {
            session
                .enter_judge()
                .map_err(|error| format!("failed to enter judge phase: {error}"))?;
        }
        let session_snapshot = session.clone();
        let artifact = SessionArtifactRecord {
            id: random_prefixed_id("artifact"),
            session_id: session.id.to_string(),
            phase: session.phase,
            step: phase_step(session.phase),
            kind: SessionArtifactKind::JudgeBundleGenerated,
            player_id: Some(actor_player_id.to_string()),
            created_at: Utc::now().to_rfc3339(),
            payload: json!({
                "dragonCount": bundle.dragons.len(),
                "artifactCount": bundle.artifact_count,
                "llmSummary": evaluation.summary,
                "dragonEvaluations": evaluation.dragon_evaluations,
            }),
        };
        (session_before, session_snapshot, artifact)
    };

    write_lease
        .ensure_active()
        .map_err(|error| format!("lost session lease before judge persist: {error}"))?;
    if let Err(error) = state
        .store
        .save_session_with_artifact(&session_snapshot, &artifact)
        .await
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_code.to_string(), session_before);
        return Err(format!("failed to persist judge scores: {error}"));
    }

    broadcast_session_state(state, session_code, None).await;
    Ok(evaluation)
}

pub(crate) fn reconnect_identity_is_valid(
    identity: &persistence::PlayerIdentityMatch,
    ttl: std::time::Duration,
    now: DateTime<Utc>,
) -> bool {
    let Ok(last_seen_at) = DateTime::parse_from_rfc3339(&identity.last_seen_at) else {
        return false;
    };
    let Ok(ttl) = chrono::Duration::from_std(ttl) else {
        return false;
    };
    now.signed_duration_since(last_seen_at.with_timezone(&Utc)) <= ttl
}

pub(crate) async fn authorize_reconnect_identity(
    state: &AppState,
    session_code: &str,
    reconnect_token: &str,
) -> Result<Option<persistence::PlayerIdentityMatch>, persistence::PersistenceError> {
    let identity = match state
        .store
        .find_player_identity(session_code, reconnect_token)
        .await?
    {
        Some(identity) => identity,
        None => return Ok(None),
    };

    if reconnect_identity_is_valid(&identity, state.config.reconnect_token_ttl, Utc::now()) {
        Ok(Some(identity))
    } else {
        let _ = state.store.revoke_player_identity(reconnect_token).await;
        Ok(None)
    }
}

pub(crate) async fn refresh_reconnect_identity(
    state: &AppState,
    reconnect_token: &str,
    timestamp: DateTime<Utc>,
) -> Result<(), persistence::PersistenceError> {
    state
        .store
        .touch_player_identity(reconnect_token, &timestamp.to_rfc3339())
        .await
}

#[derive(Clone, Copy)]
pub(crate) struct MaybeConnectInfo(pub(crate) Option<SocketAddr>);

impl<S> FromRequestParts<S> for MaybeConnectInfo
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|connect_info| connect_info.0),
        ))
    }
}

pub(crate) async fn live() -> Json<serde_json::Value> {
    Json(json!({ "ok": true, "service": "app-server", "status": "live" }))
}

pub(crate) async fn create_workshop(
    State(state): State<AppState>,
    connect_info: MaybeConnectInfo,
    headers: HeaderMap,
    Json(payload): Json<CreateWorkshopRequest>,
) -> (StatusCode, Json<WorkshopJoinResult>) {
    if let Some(response) = reject_disallowed_origin(&headers, &state.config.origin_policy) {
        return response;
    }
    let client_key = client_key(&state, connect_info, &headers);
    if let Some(response) = reject_rate_limited(&state.create_limiter, &client_key).await {
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
    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.clone()),
        timestamp,
        session_config_from_request(&payload),
    );
    let host_player = SessionPlayer {
        id: player_id.clone(),
        name: normalized_name.to_string(),
        pet_description: None,
        custom_sprites: None,
        is_host: true,
        is_connected: true,
        is_ready: false,
        score: 0,
        current_dragon_id: None,
        achievements: Vec::new(),
        joined_at: timestamp,
    };
    session.add_player(host_player.clone());

    let identity = persistence::PlayerIdentity {
        session_id: session.id.to_string(),
        player_id: player_id.clone(),
        reconnect_token: reconnect_token.clone(),
        created_at: timestamp.to_rfc3339(),
        last_seen_at: timestamp.to_rfc3339(),
    };
    let artifact = SessionArtifactRecord {
        id: random_prefixed_id("artifact"),
        session_id: session.id.to_string(),
        phase: protocol::Phase::Lobby,
        step: 0,
        kind: SessionArtifactKind::SessionCreated,
        player_id: Some(player_id.clone()),
        created_at: timestamp.to_rfc3339(),
        payload: json!({
            "sessionCode": session_code,
            "hostName": normalized_name,
            "phase0Minutes": session.config.phase0_minutes,
            "phase1Minutes": session.config.phase1_minutes,
            "phase2Minutes": session.config.phase2_minutes,
            "imageModelConfigured": state.config.llm_pool.is_image_configured(),
            "judgeModelConfigured": state.config.llm_pool.is_judge_configured(),
        }),
    };

    if let Err(error) = state
        .store
        .save_session_with_identity_and_artifact(&session, &identity, &artifact)
        .await
    {
        return internal_join_error(format!("failed to persist workshop creation: {error}"));
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

pub(crate) async fn join_workshop(
    State(state): State<AppState>,
    connect_info: MaybeConnectInfo,
    headers: HeaderMap,
    Json(payload): Json<JoinWorkshopRequest>,
) -> (StatusCode, Json<WorkshopJoinResult>) {
    if let Some(response) = reject_disallowed_origin(&headers, &state.config.origin_policy) {
        return response;
    }
    let client_key = client_key(&state, connect_info, &headers);
    if let Some(response) = reject_rate_limited(&state.join_limiter, &client_key).await {
        return response;
    }
    let session_code = payload.session_code.trim();
    if session_code.is_empty() {
        return bad_join_request("Enter a workshop code.");
    }
    if security::validate_session_code(session_code).is_err() {
        return bad_join_request("Workshop codes must be 6 digits.");
    }

    if let Some(reconnect_token) = payload
        .reconnect_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let identity = match authorize_reconnect_identity(&state, session_code, reconnect_token)
            .await
        {
            Ok(Some(identity)) => identity,
            Ok(None) => return bad_join_request("Session identity is invalid or expired."),
            Err(error) => {
                return internal_join_error(format!("failed to lookup player identity: {error}"));
            }
        };

        let (_, _write_guard, write_lease) = match SessionWriteLease::acquire(&state, session_code)
            .await
        {
            Ok(guard) => guard,
            Err(error) => {
                return internal_join_error(format!("failed to acquire session lease: {error}"));
            }
        };
        if let Err(error) = write_lease.ensure_active() {
            return internal_join_error(format!(
                "lost session lease before reconnect load: {error}"
            ));
        }

        match reload_cached_session(&state, session_code).await {
            Ok(true) => {}
            Ok(false) => return bad_join_request("Workshop not found."),
            Err(error) => return internal_join_error(format!("failed to load session: {error}")),
        }
        if let Err(error) = write_lease.ensure_active() {
            return internal_join_error(format!(
                "lost session lease before reconnect mutation: {error}"
            ));
        }

        let timestamp = Utc::now();
        let (session_before, session_clone) = {
            let mut sessions = state.sessions.lock().await;
            let Some(session) = sessions.get_mut(session_code) else {
                return bad_join_request("Workshop not found.");
            };
            let session_before = session.clone();
            let Some(player) = session.players.get_mut(&identity.player_id) else {
                return bad_join_request("Session identity is invalid or expired.");
            };
            player.is_connected = true;
            session.ensure_host_assigned(true);
            session.updated_at = timestamp;
            (session_before, session.clone())
        };

        let next_reconnect_token = random_prefixed_id("reconnect");
        let next_identity = persistence::PlayerIdentity {
            session_id: identity.session_id.clone(),
            player_id: identity.player_id.clone(),
            reconnect_token: next_reconnect_token.clone(),
            created_at: timestamp.to_rfc3339(),
            last_seen_at: timestamp.to_rfc3339(),
        };
        let reconnect_artifact = SessionArtifactRecord {
            id: random_prefixed_id("artifact"),
            session_id: session_clone.id.to_string(),
            phase: session_clone.phase,
            step: phase_step(session_clone.phase),
            kind: SessionArtifactKind::PlayerReconnected,
            player_id: Some(identity.player_id.clone()),
            created_at: timestamp.to_rfc3339(),
            payload: json!({ "sessionCode": session_code, "playerId": identity.player_id.clone() }),
        };

        if let Err(error) = write_lease.ensure_active() {
            let mut sessions = state.sessions.lock().await;
            sessions.insert(session_code.to_string(), session_before);
            return internal_join_error(format!(
                "lost session lease before reconnect persist: {error}"
            ));
        }

        if let Err(error) = state
            .store
            .replace_player_identity_and_save_session_with_artifact(
                reconnect_token,
                &next_identity,
                &session_clone,
                &reconnect_artifact,
            )
            .await
        {
            let mut sessions = state.sessions.lock().await;
            sessions.insert(session_code.to_string(), session_before);
            return internal_join_error(format!("failed to persist reconnect: {error}"));
        }

        let response = WorkshopJoinSuccess {
            ok: true,
            session_code: session_clone.code.0.clone(),
            player_id: identity.player_id.clone(),
            reconnect_token: next_reconnect_token,
            coordinator_type: CoordinatorType::Rust,
            state: to_client_game_state(&session_clone, &identity.player_id),
        };

        let response = (StatusCode::OK, Json(WorkshopJoinResult::Success(response)));
        broadcast_session_state(&state, session_code, None).await;
        return response;
    }

    let normalized_name = payload.name.unwrap_or_default().trim().to_string();
    if normalized_name.is_empty() {
        return bad_join_request("Please enter a player name.");
    }

    let (_, _write_guard, write_lease) =
        match SessionWriteLease::acquire(&state, session_code).await {
            Ok(guard) => guard,
            Err(error) => {
                return internal_join_error(format!("failed to acquire session lease: {error}"));
            }
        };
    if let Err(error) = write_lease.ensure_active() {
        return internal_join_error(format!("lost session lease before join load: {error}"));
    }

    match reload_cached_session(&state, session_code).await {
        Ok(true) => {}
        Ok(false) => return bad_join_request("Workshop not found."),
        Err(error) => return internal_join_error(format!("failed to load session: {error}")),
    }
    if let Err(error) = write_lease.ensure_active() {
        return internal_join_error(format!("lost session lease before join mutation: {error}"));
    }

    let (session_before, session_clone, player_id, reconnect_token) = {
        let mut sessions = state.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_code) else {
            return bad_join_request("Workshop not found.");
        };
        if session.phase != protocol::Phase::Lobby {
            return bad_join_request(
                "This workshop has already started. New players can only join in the lobby.",
            );
        }
        let duplicate_name = session
            .players
            .values()
            .any(|player| player.name.eq_ignore_ascii_case(&normalized_name));
        if duplicate_name {
            return bad_join_request("That player name is already taken in this workshop.");
        }

        let session_before = session.clone();
        let timestamp = Utc::now();
        let player_id = random_prefixed_id("player");
        let reconnect_token = random_prefixed_id("reconnect");
        let player = SessionPlayer {
            id: player_id.clone(),
            name: normalized_name.clone(),
            pet_description: None,
            custom_sprites: None,
            is_host: false,
            is_connected: true,
            is_ready: false,
            score: 0,
            current_dragon_id: None,
            achievements: Vec::new(),
            joined_at: timestamp,
        };
        session.add_player(player.clone());
        (session_before, session.clone(), player_id, reconnect_token)
    };

    let timestamp = Utc::now();
    let identity = persistence::PlayerIdentity {
        session_id: session_clone.id.to_string(),
        player_id: player_id.clone(),
        reconnect_token: reconnect_token.clone(),
        created_at: timestamp.to_rfc3339(),
        last_seen_at: timestamp.to_rfc3339(),
    };
    let join_artifact = SessionArtifactRecord {
        id: random_prefixed_id("artifact"),
        session_id: session_clone.id.to_string(),
        phase: protocol::Phase::Lobby,
        step: 0,
        kind: SessionArtifactKind::PlayerJoined,
        player_id: Some(player_id.clone()),
        created_at: timestamp.to_rfc3339(),
        payload: json!({ "sessionCode": session_code, "playerName": normalized_name }),
    };
    if let Err(error) = write_lease.ensure_active() {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_code.to_string(), session_before);
        return internal_join_error(format!("lost session lease before join persist: {error}"));
    }
    if let Err(error) = state
        .store
        .save_session_with_identity_and_artifact(&session_clone, &identity, &join_artifact)
        .await
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_code.to_string(), session_before);
        return internal_join_error(format!("failed to persist join: {error}"));
    }

    let response = WorkshopJoinSuccess {
        ok: true,
        session_code: session_clone.code.0.clone(),
        player_id: player_id.clone(),
        reconnect_token,
        coordinator_type: CoordinatorType::Rust,
        state: to_client_game_state(&session_clone, &player_id),
    };

    let response = (StatusCode::OK, Json(WorkshopJoinResult::Success(response)));
    broadcast_session_state(&state, session_code, None).await;
    response
}

pub(crate) async fn workshop_command(
    State(state): State<AppState>,
    connect_info: MaybeConnectInfo,
    headers: HeaderMap,
    Json(request): Json<WorkshopCommandRequest>,
) -> (StatusCode, Json<WorkshopCommandResult>) {
    let command_name = format!("{:?}", request.command);
    tracing::info!(
        session_code = %request.session_code,
        command = %command_name,
        "workshop_command request received"
    );

    if let Some(response) = reject_disallowed_command_origin(&headers, &state.config.origin_policy)
    {
        tracing::warn!(
            session_code = %request.session_code,
            command = %command_name,
            "workshop_command rejected: origin not allowed"
        );
        return response;
    }
    let client_key = client_key(&state, connect_info, &headers);
    if is_rate_limited(&state.command_limiter, &client_key).await {
        tracing::warn!(
            session_code = %request.session_code,
            command = %command_name,
            client_key = %client_key,
            "workshop_command rejected: rate limited"
        );
        return too_many_command_requests();
    }

    let session_code = request.session_code.trim();
    let reconnect_token = request.reconnect_token.trim();
    if session_code.is_empty()
        || reconnect_token.is_empty()
        || security::validate_session_code(session_code).is_err()
    {
        tracing::warn!(
            session_code = %session_code,
            command = %command_name,
            session_code_empty = session_code.is_empty(),
            reconnect_token_empty = reconnect_token.is_empty(),
            "workshop_command rejected: missing workshop credentials"
        );
        return bad_command_request("Missing workshop credentials.");
    }

    let identity = match authorize_reconnect_identity(&state, session_code, reconnect_token).await {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            tracing::warn!(
                session_code = %session_code,
                command = %command_name,
                "workshop_command rejected: session identity invalid or expired"
            );
            return bad_command_request("Session identity is invalid or expired.");
        }
        Err(error) => {
            tracing::error!(
                session_code = %session_code,
                command = %command_name,
                %error,
                "workshop_command error: failed to lookup identity"
            );
            return internal_command_error(format!("failed to lookup identity: {error}"));
        }
    };

    if let Err(error) = refresh_reconnect_identity(&state, reconnect_token, Utc::now()).await {
        return internal_command_error(format!("failed to touch player identity: {error}"));
    }

    let (_, _write_guard, write_lease) =
        match SessionWriteLease::acquire(&state, session_code).await {
            Ok(guard) => guard,
            Err(error) => {
                return internal_command_error(format!("failed to acquire session lease: {error}"));
            }
        };
    if let Err(error) = write_lease.ensure_active() {
        return internal_command_error(format!("lost session lease before command load: {error}"));
    }

    match reload_cached_session(&state, session_code).await {
        Ok(true) => {}
        Ok(false) => return bad_command_request("Workshop not found."),
        Err(error) => return internal_command_error(format!("failed to load session: {error}")),
    }
    if let Err(error) = write_lease.ensure_active() {
        return internal_command_error(format!(
            "lost session lease before command mutation: {error}"
        ));
    }

    let (response, should_broadcast, session_before, session_to_persist, artifact_to_append) = {
        let mut sessions = state.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_code) else {
            return bad_command_request("Workshop not found.");
        };
        let mut should_broadcast = false;
        let session_before = session.clone();
        let mut session_to_persist = None;
        let mut artifact_to_append = None;

        let response = match request.command {
            SessionCommand::StartPhase0 => {
                if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                    return bad_command_request("Only the host can open character creation.");
                }
                if session.phase != protocol::Phase::Lobby {
                    return bad_command_request(
                        "Character creation can only begin from the lobby.",
                    );
                }
                if let Err(error) = session.transition_to(protocol::Phase::Phase0) {
                    return bad_command_request(&error.to_string());
                }
                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: phase_step(session.phase),
                    kind: SessionArtifactKind::PhaseChanged,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: json!({ "toPhase": "phase0" }),
                });

                successful_workshop_command(&mut should_broadcast)
            }
            SessionCommand::StartPhase1 => {
                if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                    return bad_command_request("Only the host can start the workshop.");
                }
                if session.phase != protocol::Phase::Phase0 {
                    return bad_command_request("Phase 1 can only start after character creation.");
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
                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: phase_step(session.phase),
                    kind: SessionArtifactKind::PhaseChanged,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: json!({ "toPhase": "phase1" }),
                });

                successful_workshop_command(&mut should_broadcast)
            }
            SessionCommand::SubmitObservation => {
                if session.phase != protocol::Phase::Phase1 {
                    return bad_command_request("Observations can only be saved during Phase 1.");
                }
                let payload = match request.payload.clone() {
                    Some(value) => {
                        serde_json::from_value::<DiscoveryObservationRequest>(value).ok()
                    }
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
                    return dragon_id.expect_err("dragon assignment error");
                };

                session.record_discovery_observation(&identity.player_id, text.to_string());
                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: phase_step(session.phase),
                    kind: SessionArtifactKind::DiscoveryObservationSaved,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: json!({ "dragonId": dragon_id, "text": text }),
                });

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
                let action_value = payload
                    .value
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_ascii_lowercase);
                let outcome = match session.apply_action(&identity.player_id, action) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        let message = match error {
                            DomainError::ActionNotAllowed => {
                                "Action is not allowed right now.".to_string()
                            }
                            DomainError::DragonNotAssigned => {
                                "Player is not assigned to a dragon.".to_string()
                            }
                            _ => error.to_string(),
                        };
                        return bad_command_request(&message);
                    }
                };
                let mut artifact_payload = json!({
                    "dragonId": dragon_id,
                    "actionType": action_type,
                    "actionValue": action_value,
                });
                // Persist achievement before borrowing dragon immutably
                if let domain::ActionOutcome::Applied {
                    awarded_achievement: Some(achievement),
                    ..
                } = &outcome
                {
                    if let Some(player) = session.players.get_mut(&identity.player_id) {
                        player.achievements.push(achievement.to_string());
                    }
                }
                if let Some(dragon) = session.dragons.get(&dragon_id)
                    && let Some(payload_map) = artifact_payload.as_object_mut()
                {
                    match &outcome {
                        domain::ActionOutcome::Applied { was_correct, .. } => {
                            payload_map.insert("hunger".to_string(), json!(dragon.hunger));
                            payload_map.insert("energy".to_string(), json!(dragon.energy));
                            payload_map.insert("happiness".to_string(), json!(dragon.happiness));
                            payload_map.insert("wasCorrect".to_string(), json!(was_correct));
                        }
                        domain::ActionOutcome::Blocked { reason } => {
                            let reason_str = match reason {
                                domain::ActionBlockReason::AlreadyFull => "already_full",
                                domain::ActionBlockReason::TooHungryToPlay => "too_hungry_to_play",
                                domain::ActionBlockReason::TooTiredToPlay => "too_tired_to_play",
                                domain::ActionBlockReason::TooAwakeToSleep => "too_awake_to_sleep",
                            };
                            payload_map.insert("blockedReason".to_string(), json!(reason_str));
                        }
                        domain::ActionOutcome::CooldownViolation => {
                            payload_map
                                .insert("blockedReason".to_string(), json!("cooldown_violation"));
                        }
                    }
                }

                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: phase_step(session.phase),
                    kind: SessionArtifactKind::ActionProcessed,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: artifact_payload,
                });

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
                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: phase_step(session.phase),
                    kind: SessionArtifactKind::PhaseChanged,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: json!({ "toPhase": "handover" }),
                });

                successful_workshop_command(&mut should_broadcast)
            }
            SessionCommand::SubmitTags => {
                if session.phase != protocol::Phase::Handover {
                    return bad_command_request(
                        "Handover notes can only be saved during handover.",
                    );
                }
                let tags = match request.payload.as_ref() {
                    Some(serde_json::Value::Array(values)) => values
                        .iter()
                        .map(|value| {
                            value
                                .as_str()
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .map(str::to_string)
                        })
                        .collect::<Option<Vec<_>>>(),
                    _ => None,
                };
                let Some(tags) = tags else {
                    return bad_command_request("Handover notes must be sent as a list.");
                };

                session.save_handover_tags(&identity.player_id, tags);
                let saved_tags = session
                    .players
                    .get(&identity.player_id)
                    .and_then(|player| player.current_dragon_id.clone())
                    .and_then(|dragon_id| session.dragons.get(&dragon_id))
                    .map(|dragon| dragon.handover_tags.clone())
                    .unwrap_or_default();

                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: phase_step(session.phase),
                    kind: SessionArtifactKind::HandoverSaved,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: json!({ "tagCount": saved_tags.len(), "tags": saved_tags }),
                });

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
                        DomainError::MissingHandoverTags { players } => bad_command_request(
                            &format!("Still waiting on: {}.", players.join(", ")),
                        ),
                        _ => bad_command_request(&error.to_string()),
                    };
                }
                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: phase_step(session.phase),
                    kind: SessionArtifactKind::PhaseChanged,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: json!({ "toPhase": "phase2" }),
                });

                successful_workshop_command(&mut should_broadcast)
            }
            SessionCommand::EndGame => {
                if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                    return bad_command_request("Only the host can end the workshop.");
                }
                if !matches!(session.phase, protocol::Phase::Phase2 | protocol::Phase::Judge) {
                    return bad_command_request("Design voting can only begin from Phase 2.");
                }
                session.award_phase_end_achievements();
                if let Err(error) = session.enter_voting() {
                    return bad_command_request(&error.to_string());
                }
                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: phase_step(session.phase),
                    kind: SessionArtifactKind::ActionProcessed,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: json!({
                        "command": "endGame",
                        "judgeQueued": true,
                        "toPhase": "voting"
                    }),
                });

                successful_workshop_command(&mut should_broadcast)
            }
            SessionCommand::StartVoting => {
                if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                    return bad_command_request("Only the host can open the design vote.");
                }
                if session.phase != protocol::Phase::Phase2 {
                    return bad_command_request("Design voting can only begin from Phase 2.");
                }
                if let Err(error) = session.enter_voting() {
                    return bad_command_request(&error.to_string());
                }
                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: phase_step(session.phase),
                    kind: SessionArtifactKind::PhaseChanged,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: json!({ "toPhase": "voting" }),
                });

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
                        DomainError::VotingNotActive => {
                            "Voting is not active right now.".to_string()
                        }
                        DomainError::IneligibleVoter => {
                            "Player is not eligible to vote.".to_string()
                        }
                        DomainError::UnknownDragon => {
                            "Unknown dragon selected for vote.".to_string()
                        }
                        DomainError::SelfVoteForbidden => {
                            "You cannot vote for your own dragon.".to_string()
                        }
                        _ => error.to_string(),
                    };
                    return bad_command_request(&message);
                }
                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: phase_step(session.phase),
                    kind: SessionArtifactKind::VoteSubmitted,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: json!({ "dragonId": payload.dragon_id }),
                });

                successful_workshop_command(&mut should_broadcast)
            }
            SessionCommand::RevealVotingResults => {
                if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                    return bad_command_request("Only the host can reveal voting results.");
                }
                if session.phase != protocol::Phase::Voting {
                    return bad_command_request("Results can only be revealed during voting.");
                }
                if let Err(error) = session.reveal_voting_results() {
                    return bad_command_request(&error.to_string());
                }
                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: phase_step(session.phase),
                    kind: SessionArtifactKind::VotingFinalized,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: json!({
                        "resultsRevealed": true,
                        "playerScores": session
                            .players
                            .iter()
                            .map(|(player_id, player)| (player_id.clone(), player.score))
                            .collect::<BTreeMap<_, _>>()
                    }),
                });

                successful_workshop_command(&mut should_broadcast)
            }
            SessionCommand::EndSession => {
                if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                    return bad_command_request("Only the host can end the session.");
                }
                if session.phase != protocol::Phase::Voting {
                    return bad_command_request("Session can only be ended during voting.");
                }
                if let Err(error) = session.finalize_voting() {
                    return bad_command_request(&error.to_string());
                }
                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: phase_step(session.phase),
                    kind: SessionArtifactKind::VotingFinalized,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: json!({
                        "toPhase": "end",
                        "endedEarly": true,
                        "playerScores": session
                            .players
                            .iter()
                            .map(|(player_id, player)| (player_id.clone(), player.score))
                            .collect::<BTreeMap<_, _>>()
                    }),
                });

                successful_workshop_command(&mut should_broadcast)
            }
            SessionCommand::ResetGame => {
                if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                    return bad_command_request("Only the host can reset the workshop.");
                }
                if let Err(error) = session.reset_to_lobby() {
                    return bad_command_request(&error.to_string());
                }
                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: 0,
                    kind: SessionArtifactKind::SessionReset,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: json!({ "toPhase": "lobby" }),
                });

                successful_workshop_command(&mut should_broadcast)
            }
            SessionCommand::UpdatePlayerPet => {
                if session.phase != protocol::Phase::Phase0 {
                    return bad_command_request(
                        "Pet profile can only be updated during character creation.",
                    );
                }
                let payload = match request.payload.clone() {
                    Some(value) => serde_json::from_value::<UpdatePlayerPetRequest>(value).ok(),
                    None => None,
                };
                let Some(payload) = payload else {
                    return bad_command_request("Pet profile payload is invalid.");
                };
                let description = payload.description.trim().to_string();
                if description.is_empty() {
                    return bad_command_request("Pet description is required.");
                }
                let has_sprites = payload.sprites.is_some();
                if let Err(error) =
                    session.update_player_pet(&identity.player_id, description, payload.sprites)
                {
                    return bad_command_request(&error.to_string());
                }
                session_to_persist = Some(session.clone());
                artifact_to_append = Some(SessionArtifactRecord {
                    id: random_prefixed_id("artifact"),
                    session_id: session.id.to_string(),
                    phase: session.phase,
                    step: phase_step(session.phase),
                    kind: SessionArtifactKind::PetProfileUpdated,
                    player_id: Some(identity.player_id.clone()),
                    created_at: Utc::now().to_rfc3339(),
                    payload: json!({ "hasSprites": has_sprites }),
                });

                successful_workshop_command(&mut should_broadcast)
            }
            _ => bad_command_request("Unsupported workshop command."),
        };

        (
            response,
            should_broadcast,
            session_before,
            session_to_persist,
            artifact_to_append,
        )
    };

    if session_to_persist.is_some() && artifact_to_append.is_none() {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_code.to_string(), session_before);
        return internal_command_error(
            "session command mutated state without an artifact".to_string(),
        );
    }

    if session_to_persist.is_none() && artifact_to_append.is_some() {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_code.to_string(), session_before);
        return internal_command_error(
            "session command emitted an artifact without session state".to_string(),
        );
    }

    match (session_to_persist.as_ref(), artifact_to_append.as_ref()) {
        (Some(session), Some(artifact)) => {
            if let Err(error) = write_lease.ensure_active() {
                let mut sessions = state.sessions.lock().await;
                sessions.insert(session_code.to_string(), session_before);
                return internal_command_error(format!(
                    "lost session lease before command persist: {error}"
                ));
            }
            if let Err(error) = state
                .store
                .save_session_with_artifact(session, artifact)
                .await
            {
                let mut sessions = state.sessions.lock().await;
                sessions.insert(session_code.to_string(), session_before);
                return internal_command_error(format!(
                    "failed to persist session command: {error}"
                ));
            }
        }
        (None, None) => {}
        _ => unreachable!("checked command persistence invariants above"),
    }

    let should_run_judge = request.command == SessionCommand::EndGame;

    drop(_write_guard);
    drop(write_lease);

    if should_broadcast {
        broadcast_session_state(&state, session_code, None).await;
    }

    if should_run_judge {
        let background_state = state.clone();
        let background_session_code = session_code.to_string();
        let background_player_id = identity.player_id.clone();
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
                    "background judge run failed"
                );
            }
        });
    }

    tracing::info!(
        session_code = %session_code,
        command = %command_name,
        player_id = %identity.player_id,
        broadcast = should_broadcast,
        "workshop_command completed successfully"
    );

    response
}

pub(crate) async fn workshop_judge_bundle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<WorkshopJudgeBundleRequest>,
) -> (StatusCode, Json<WorkshopJudgeBundleResult>) {
    if let Some(response) =
        reject_disallowed_judge_bundle_origin(&headers, &state.config.origin_policy)
    {
        return response;
    }

    let session_code = request.session_code.trim();
    let reconnect_token = request.reconnect_token.trim();
    if session_code.is_empty()
        || reconnect_token.is_empty()
        || security::validate_session_code(session_code).is_err()
    {
        return bad_judge_bundle_request("Missing workshop credentials.");
    }

    let session = {
        match ensure_session_cached(&state, session_code).await {
            Ok(true) => {}
            Ok(false) => return bad_judge_bundle_request("Workshop not found."),
            Err(error) => {
                return internal_judge_bundle_error(format!("failed to load session: {error}"));
            }
        }
        let sessions = state.sessions.lock().await;
        let Some(session) = sessions.get(session_code) else {
            return bad_judge_bundle_request("Workshop not found.");
        };
        session.clone()
    };

    let identity = match authorize_reconnect_identity(&state, session_code, reconnect_token).await {
        Ok(Some(identity)) => identity,
        Ok(None) => return bad_judge_bundle_request("Session identity is invalid or expired."),
        Err(error) => {
            return internal_judge_bundle_error(format!("failed to lookup identity: {error}"));
        }
    };

    if let Err(error) = refresh_reconnect_identity(&state, reconnect_token, Utc::now()).await {
        return internal_judge_bundle_error(format!("failed to touch player identity: {error}"));
    }

    let artifacts = match state
        .store
        .list_session_artifacts(&session.id.to_string())
        .await
    {
        Ok(artifacts) => artifacts,
        Err(error) => {
            return internal_judge_bundle_error(format!(
                "failed to list session artifacts: {error}"
            ));
        }
    };

    let bundle = build_judge_bundle(&session, &artifacts);

    if let Err(error) = state
        .store
        .append_session_artifact(&SessionArtifactRecord {
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
        })
        .await
    {
        return internal_judge_bundle_error(format!("failed to append session artifact: {error}"));
    }

    (
        StatusCode::OK,
        Json(WorkshopJudgeBundleResult::Success(
            WorkshopJudgeBundleSuccess { ok: true, bundle },
        )),
    )
}

pub(crate) async fn ready(State(state): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    let store_healthy = state.store.health_check().await.unwrap_or(false);
    let status = if store_healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        Json(json!({
            "ok": store_healthy,
            "service": "app-server",
            "status": if store_healthy { "ready" } else { "degraded" },
            "checks": {
                "store": store_healthy
            }
        })),
    )
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
    tracing::warn!(error = %message, "workshop_command returning 400 Bad Request");
    (
        StatusCode::BAD_REQUEST,
        Json(WorkshopCommandResult::Error(WorkshopError {
            ok: false,
            error: message.to_string(),
        })),
    )
}

fn internal_command_error(message: String) -> (StatusCode, Json<WorkshopCommandResult>) {
    tracing::error!(error = %message, "workshop_command returning 500 Internal Server Error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(WorkshopCommandResult::Error(WorkshopError {
            ok: false,
            error: message,
        })),
    )
}

pub(crate) async fn allocate_session_code(state: &AppState) -> String {
    loop {
        let entropy = Uuid::new_v4().simple().to_string();
        let suffix = entropy
            .chars()
            .filter(|ch| ch.is_ascii_hexdigit())
            .take(5)
            .map(|ch| (((ch as u8) % 10) + b'0') as char)
            .collect::<String>();
        let candidate = format!("{}{}", state.config.rust_session_code_prefix, suffix);
        let is_cached = {
            let sessions = state.sessions.lock().await;
            sessions.contains_key(&candidate)
        };
        let is_persisted = state
            .store
            .load_session_by_code(&candidate)
            .await
            .map(|session| session.is_some())
            .unwrap_or(true);
        if !is_cached && !is_persisted {
            return candidate;
        }
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
    client_key: &str,
) -> Option<(StatusCode, Json<WorkshopJoinResult>)> {
    let decision = consume_rate_limit(limiter, client_key).await;
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

pub(crate) async fn is_rate_limited(
    limiter: &Arc<Mutex<FixedWindowRateLimiter>>,
    client_key: &str,
) -> bool {
    !consume_rate_limit(limiter, client_key).await.allowed
}

async fn consume_rate_limit(
    limiter: &Arc<Mutex<FixedWindowRateLimiter>>,
    client_key: &str,
) -> security::RateLimitDecision {
    let now_ms = Utc::now().timestamp_millis().max(0) as u64;
    limiter.lock().await.consume(client_key, now_ms)
}

pub(crate) fn client_key(
    state: &AppState,
    connect_info: MaybeConnectInfo,
    headers: &HeaderMap,
) -> String {
    if state.config.trust_forwarded_for
        && let Some(forwarded_for) = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    {
        return forwarded_for.to_string();
    }

    connect_info
        .0
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn successful_workshop_command(
    should_broadcast: &mut bool,
) -> (StatusCode, Json<WorkshopCommandResult>) {
    *should_broadcast = true;
    (
        StatusCode::OK,
        Json(WorkshopCommandResult::Success(WorkshopCommandSuccess {
            ok: true,
        })),
    )
}

fn too_many_command_requests() -> (StatusCode, Json<WorkshopCommandResult>) {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(WorkshopCommandResult::Error(WorkshopError {
            ok: false,
            error: "Too many requests. Please slow down and try again.".to_string(),
        })),
    )
}

// ---------------------------------------------------------------------------
// LLM endpoints
// ---------------------------------------------------------------------------

pub(crate) async fn llm_judge(
    State(state): State<AppState>,
    connect_info: MaybeConnectInfo,
    headers: HeaderMap,
    Json(request): Json<LlmJudgeRequest>,
) -> (StatusCode, Json<LlmJudgeResult>) {
    if let Some(response) = reject_disallowed_llm_origin(&headers, &state.config.origin_policy) {
        return response;
    }
    let client_key = client_key(&state, connect_info, &headers);
    if is_rate_limited(&state.command_limiter, &client_key).await {
        return too_many_llm_judge_requests();
    }

    let session_code = request.session_code.trim();
    let reconnect_token = request.reconnect_token.trim();
    if session_code.is_empty()
        || reconnect_token.is_empty()
        || security::validate_session_code(session_code).is_err()
    {
        return bad_llm_judge_request("Missing workshop credentials.");
    }

    let identity = match authorize_reconnect_identity(&state, session_code, reconnect_token).await {
        Ok(Some(identity)) => identity,
        Ok(None) => return bad_llm_judge_request("Session identity is invalid or expired."),
        Err(error) => {
            return internal_llm_judge_error(format!("failed to lookup identity: {error}"));
        }
    };

    if let Err(error) = refresh_reconnect_identity(&state, reconnect_token, Utc::now()).await {
        return internal_llm_judge_error(format!("failed to touch player identity: {error}"));
    }

    let evaluation = match run_judge_for_session(&state, session_code, &identity.player_id).await {
        Ok(evaluation) => evaluation,
        Err(error) => return internal_llm_judge_error(error),
    };

    (
        StatusCode::OK,
        Json(LlmJudgeResult::Success(LlmJudgeSuccess {
            ok: true,
            evaluation,
        })),
    )
}

pub(crate) async fn llm_generate_image(
    State(state): State<AppState>,
    connect_info: MaybeConnectInfo,
    headers: HeaderMap,
    Json(request): Json<LlmImageRequest>,
) -> (StatusCode, Json<LlmImageResult>) {
    if let Some(response) =
        reject_disallowed_llm_image_origin(&headers, &state.config.origin_policy)
    {
        return response;
    }
    let client_key = client_key(&state, connect_info, &headers);
    if is_rate_limited(&state.command_limiter, &client_key).await {
        return too_many_llm_image_requests();
    }

    let session_code = request.session_code.trim();
    let reconnect_token = request.reconnect_token.trim();
    if session_code.is_empty()
        || reconnect_token.is_empty()
        || security::validate_session_code(session_code).is_err()
    {
        return bad_llm_image_request("Missing workshop credentials.");
    }

    let _identity = match authorize_reconnect_identity(&state, session_code, reconnect_token).await
    {
        Ok(Some(identity)) => identity,
        Ok(None) => return bad_llm_image_request("Session identity is invalid or expired."),
        Err(error) => {
            return internal_llm_image_error(format!("failed to lookup identity: {error}"));
        }
    };

    if let Err(error) = refresh_reconnect_identity(&state, reconnect_token, Utc::now()).await {
        return internal_llm_image_error(format!("failed to touch player identity: {error}"));
    }

    let prompt = request.prompt.trim();
    if prompt.is_empty() {
        return bad_llm_image_request("Image prompt is required.");
    }

    let (image_base64, mime_type) = match state.llm_client.generate_image(prompt).await {
        Ok(result) => result,
        Err(error) => {
            return internal_llm_image_error(format!("image generation failed: {error}"));
        }
    };

    (
        StatusCode::OK,
        Json(LlmImageResult::Success(LlmImageSuccess {
            ok: true,
            image_base64,
            mime_type,
        })),
    )
}

pub(crate) async fn generate_sprite_sheet(
    State(state): State<AppState>,
    connect_info: MaybeConnectInfo,
    headers: HeaderMap,
    Json(request): Json<SpriteSheetRequest>,
) -> (StatusCode, Json<SpriteSheetResult>) {
    if let Some(response) =
        reject_disallowed_sprite_sheet_origin(&headers, &state.config.origin_policy)
    {
        return response;
    }
    let client_key = client_key(&state, connect_info, &headers);
    if is_rate_limited(&state.command_limiter, &client_key).await {
        return too_many_sprite_sheet_requests();
    }

    let session_code = request.session_code.trim();
    let reconnect_token = request.reconnect_token.trim();
    if session_code.is_empty()
        || reconnect_token.is_empty()
        || security::validate_session_code(session_code).is_err()
    {
        return bad_sprite_sheet_request("Missing workshop credentials.");
    }

    let identity = match authorize_reconnect_identity(&state, session_code, reconnect_token).await {
        Ok(Some(identity)) => identity,
        Ok(None) => return bad_sprite_sheet_request("Session identity is invalid or expired."),
        Err(error) => {
            return internal_sprite_sheet_error(format!("failed to lookup identity: {error}"));
        }
    };

    if let Err(error) = refresh_reconnect_identity(&state, reconnect_token, Utc::now()).await {
        return internal_sprite_sheet_error(format!("failed to touch player identity: {error}"));
    }

    let (_, _write_guard, write_lease) =
        match SessionWriteLease::acquire(&state, session_code).await {
            Ok(guard) => guard,
            Err(error) => {
                return internal_sprite_sheet_error(format!(
                    "failed to acquire session lease: {error}"
                ));
            }
        };
    if let Err(error) = write_lease.ensure_active() {
        return internal_sprite_sheet_error(format!(
            "lost session lease before sprite generation load: {error}"
        ));
    }

    match reload_cached_session(&state, session_code).await {
        Ok(true) => {}
        Ok(false) => return bad_sprite_sheet_request("Workshop not found."),
        Err(error) => {
            return internal_sprite_sheet_error(format!("failed to load session: {error}"));
        }
    }

    if let Err(error) = write_lease.ensure_active() {
        return internal_sprite_sheet_error(format!(
            "lost session lease before sprite generation validation: {error}"
        ));
    }

    {
        let sessions = state.sessions.lock().await;
        let Some(session) = sessions.get(session_code) else {
            return bad_sprite_sheet_request("Workshop not found.");
        };
        if session.players.get(&identity.player_id).is_none() {
            return bad_sprite_sheet_request("Session identity is invalid or expired.");
        }
        if session.phase != protocol::Phase::Phase0 {
            return bad_sprite_sheet_request(
                "Dragon sprites can only be generated during character creation.",
            );
        }
    }

    let description = request.description.trim();
    if description.is_empty() {
        return bad_sprite_sheet_request("Dragon description is required.");
    }

    let sprites = match state.llm_client.generate_sprite_sheet(description).await {
        Ok(result) => result,
        Err(error) => {
            return internal_sprite_sheet_error(format!("sprite sheet generation failed: {error}"));
        }
    };

    let (session_before, session_to_persist, artifact_to_append) = {
        let mut sessions = state.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_code) else {
            return bad_sprite_sheet_request("Workshop not found.");
        };
        let session_before = session.clone();
        if session.players.get(&identity.player_id).is_none() {
            return bad_sprite_sheet_request("Session identity is invalid or expired.");
        }
        if session.phase != protocol::Phase::Phase0 {
            return bad_sprite_sheet_request(
                "Dragon sprites can only be generated during character creation.",
            );
        }
        if let Err(error) = session.update_player_sprite_draft(
            &identity.player_id,
            description.to_string(),
            sprites.clone(),
        ) {
            return bad_sprite_sheet_request(&error.to_string());
        }
        let artifact = SessionArtifactRecord {
            id: random_prefixed_id("artifact"),
            session_id: session.id.to_string(),
            phase: session.phase,
            step: phase_step(session.phase),
            kind: SessionArtifactKind::PetProfileUpdated,
            player_id: Some(identity.player_id.clone()),
            created_at: Utc::now().to_rfc3339(),
            payload: json!({
                "hasSprites": true,
                "draftOnly": true,
            }),
        };
        (session_before, session.clone(), artifact)
    };

    if let Err(error) = write_lease.ensure_active() {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_code.to_string(), session_before);
        return internal_sprite_sheet_error(format!(
            "lost session lease before sprite draft persist: {error}"
        ));
    }

    if let Err(error) = state
        .store
        .save_session_with_artifact(&session_to_persist, &artifact_to_append)
        .await
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_code.to_string(), session_before);
        return internal_sprite_sheet_error(format!("failed to persist sprite draft: {error}"));
    }

    drop(_write_guard);
    drop(write_lease);
    broadcast_session_state(&state, session_code, None).await;

    (
        StatusCode::OK,
        Json(SpriteSheetResult::Success(SpriteSheetSuccess {
            ok: true,
            sprites,
        })),
    )
}

fn reject_disallowed_llm_origin(
    headers: &HeaderMap,
    policy: &OriginPolicy,
) -> Option<(StatusCode, Json<LlmJudgeResult>)> {
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    if security::is_origin_allowed(origin, policy) {
        None
    } else {
        Some((
            StatusCode::FORBIDDEN,
            Json(LlmJudgeResult::Error(WorkshopError {
                ok: false,
                error: "Origin is not allowed.".to_string(),
            })),
        ))
    }
}

fn reject_disallowed_llm_image_origin(
    headers: &HeaderMap,
    policy: &OriginPolicy,
) -> Option<(StatusCode, Json<LlmImageResult>)> {
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    if security::is_origin_allowed(origin, policy) {
        None
    } else {
        Some((
            StatusCode::FORBIDDEN,
            Json(LlmImageResult::Error(WorkshopError {
                ok: false,
                error: "Origin is not allowed.".to_string(),
            })),
        ))
    }
}

fn bad_llm_judge_request(message: &str) -> (StatusCode, Json<LlmJudgeResult>) {
    (
        StatusCode::BAD_REQUEST,
        Json(LlmJudgeResult::Error(WorkshopError {
            ok: false,
            error: message.to_string(),
        })),
    )
}

fn internal_llm_judge_error(message: String) -> (StatusCode, Json<LlmJudgeResult>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(LlmJudgeResult::Error(WorkshopError {
            ok: false,
            error: message,
        })),
    )
}

fn bad_llm_image_request(message: &str) -> (StatusCode, Json<LlmImageResult>) {
    (
        StatusCode::BAD_REQUEST,
        Json(LlmImageResult::Error(WorkshopError {
            ok: false,
            error: message.to_string(),
        })),
    )
}

fn internal_llm_image_error(message: String) -> (StatusCode, Json<LlmImageResult>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(LlmImageResult::Error(WorkshopError {
            ok: false,
            error: message,
        })),
    )
}

fn too_many_llm_judge_requests() -> (StatusCode, Json<LlmJudgeResult>) {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(LlmJudgeResult::Error(WorkshopError {
            ok: false,
            error: "Too many requests. Please slow down and try again.".to_string(),
        })),
    )
}

fn too_many_llm_image_requests() -> (StatusCode, Json<LlmImageResult>) {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(LlmImageResult::Error(WorkshopError {
            ok: false,
            error: "Too many requests. Please slow down and try again.".to_string(),
        })),
    )
}

fn reject_disallowed_sprite_sheet_origin(
    headers: &HeaderMap,
    policy: &OriginPolicy,
) -> Option<(StatusCode, Json<SpriteSheetResult>)> {
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    if security::is_origin_allowed(origin, policy) {
        None
    } else {
        Some((
            StatusCode::FORBIDDEN,
            Json(SpriteSheetResult::Error(WorkshopError {
                ok: false,
                error: "Origin is not allowed.".to_string(),
            })),
        ))
    }
}

fn bad_sprite_sheet_request(message: &str) -> (StatusCode, Json<SpriteSheetResult>) {
    (
        StatusCode::BAD_REQUEST,
        Json(SpriteSheetResult::Error(WorkshopError {
            ok: false,
            error: message.to_string(),
        })),
    )
}

fn internal_sprite_sheet_error(message: String) -> (StatusCode, Json<SpriteSheetResult>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(SpriteSheetResult::Error(WorkshopError {
            ok: false,
            error: message,
        })),
    )
}

fn too_many_sprite_sheet_requests() -> (StatusCode, Json<SpriteSheetResult>) {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(SpriteSheetResult::Error(WorkshopError {
            ok: false,
            error: "Too many requests. Please slow down and try again.".to_string(),
        })),
    )
}
