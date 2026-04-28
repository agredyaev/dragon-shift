use axum::{
    Json,
    body::Body,
    extract::{ConnectInfo, FromRequestParts, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::{Key, SignedCookieJar};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use domain::{
    DomainError, MAX_CHARACTERS_PER_ACCOUNT, Phase1Assignment, SessionCode, SessionPlayer,
    WorkshopSession,
};
use persistence::{CharacterRecord, timeout_companion_defaults};
use protocol::{
    ActionPayload, CharacterCatalogRequest, CharacterCatalogResult, CharacterCatalogSuccess,
    CharacterProfile, CharacterSpritePreviewRequest, CharacterSpritePreviewResponse,
    CharacterSpriteSheetRequest, CharacterSpriteSheetResult, CharacterSpriteSheetSuccess,
    CoordinatorType, CreateCharacterRequest, CreateWorkshopRequest, DiscoveryObservationRequest,
    EligibleCharactersResponse, JoinWorkshopRequest, JudgeBundle, JudgeDragonBundle,
    ListOpenWorkshopsResponse, LlmDragonEvaluation, LlmImageRequest, LlmImageResult,
    LlmImageSuccess, LlmJudgeEvaluation, LlmJudgeRequest, LlmJudgeResult, LlmJudgeSuccess,
    MyCharactersResponse, OpenWorkshopCursor, OpenWorkshopSummary,
    SPRITE_ATELIER_ACCEPTED_NOTICE_MESSAGE, SPRITE_ATELIER_DRAWING_NOTICE_MESSAGE,
    SPRITE_ATELIER_FALLBACK_NOTICE_MESSAGE, SPRITE_ATELIER_NOTICE_TITLE,
    SPRITE_ATELIER_QUEUED_NOTICE_MESSAGE, SelectCharacterRequest, SessionArtifactKind,
    SessionArtifactRecord, SessionCommand, SessionNoticeCode, SpriteSheetRequest,
    SpriteSheetResult, SpriteSheetSuccess, UpdateCharacterRequest, VotePayload,
    WorkshopCommandRequest, WorkshopCommandResult, WorkshopCommandSuccess, WorkshopCreateResult,
    WorkshopCreateSuccess, WorkshopError, WorkshopJoinResult, WorkshopJoinSuccess,
    WorkshopJudgeBundleRequest, WorkshopJudgeBundleResult, WorkshopJudgeBundleSuccess,
};
use security::{FixedWindowRateLimiter, OriginPolicy};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    net::SocketAddr,
    sync::Arc,
};
use tokio::sync::{Mutex, OwnedSemaphorePermit, TryAcquireError};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::{AccountSession, SESSION_COOKIE_NAME};
use crate::cache::{SessionWriteLease, ensure_session_cached, reload_cached_session};
use crate::helpers::{
    build_judge_bundle, character_sprite_reference_set, parse_player_action, phase_step,
    random_prefixed_id, sprite_set_uses_references, to_client_game_state,
};
use crate::ws::{
    broadcast_session_state, close_local_workshop_connections, send_player_notice_with_code,
};

enum ImageJobAdmissionError {
    TimedOut,
    QueueUnavailable,
}

async fn wait_for_image_job_turn(
    state: &AppState,
) -> Result<OwnedSemaphorePermit, ImageJobAdmissionError> {
    match tokio::time::timeout(
        state.config.sprite_queue_timeout,
        state.image_job_queue.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) => Err(ImageJobAdmissionError::QueueUnavailable),
        Err(_) => Err(ImageJobAdmissionError::TimedOut),
    }
}

/// Outcome returned by [`acquire_image_job_permit`] when admission to the
/// image-job queue fails. Callers map each variant to their own
/// response/fallback contract (workshop notices or HTTP error body).
enum ImageQueueAdmissionOutcome {
    TimedOut,
    Unavailable,
}

const SPRITE_PREVIEW_BUSY_ERROR: &str = "Sprite API is busy. Please try again in a few minutes.";

fn character_profile_with_sprite_references(record: &CharacterRecord) -> CharacterProfile {
    CharacterProfile {
        id: record.id.clone(),
        name: record.name.clone(),
        description: record.description.clone(),
        sprites: character_sprite_reference_set(&record.id),
        remaining_sprite_regenerations: record.remaining_sprite_regenerations,
        creator_account_id: record.owner_account_id.clone(),
        creator_name: None,
    }
}

fn sprite_base64_for_emotion<'a>(
    sprites: &'a protocol::SpriteSet,
    emotion: &str,
) -> Option<&'a str> {
    match emotion {
        "neutral" => Some(&sprites.neutral),
        "happy" => Some(&sprites.happy),
        "angry" => Some(&sprites.angry),
        "sleepy" => Some(&sprites.sleepy),
        _ => None,
    }
}

fn sprite_preview_busy_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": SPRITE_PREVIEW_BUSY_ERROR })),
    )
        .into_response()
}

/// Consolidate the `try_acquire_owned → NoPermits→wait_for_image_job_turn
/// → Closed` admission ladder used by every image-job producer.
///
/// `on_queued` is invoked exactly once, only when the first non-blocking
/// attempt fails with `NoPermits` (i.e., the caller will actually have to
/// wait). The workshop sprite-sheet path uses it to send a "queued"
/// session notice; paths without a session pass a no-op future.
async fn acquire_image_job_permit<F, Fut>(
    state: &AppState,
    on_queued: F,
) -> Result<OwnedSemaphorePermit, ImageQueueAdmissionOutcome>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    match state.image_job_queue.clone().try_acquire_owned() {
        Ok(permit) => Ok(permit),
        Err(TryAcquireError::NoPermits) => {
            on_queued().await;
            match wait_for_image_job_turn(state).await {
                Ok(permit) => Ok(permit),
                Err(ImageJobAdmissionError::TimedOut) => Err(ImageQueueAdmissionOutcome::TimedOut),
                Err(ImageJobAdmissionError::QueueUnavailable) => {
                    Err(ImageQueueAdmissionOutcome::Unavailable)
                }
            }
        }
        Err(TryAcquireError::Closed) => Err(ImageQueueAdmissionOutcome::Unavailable),
    }
}

async fn sprite_sheet_fallback_with_notice(
    state: &AppState,
    session_code: &str,
    player_id: &str,
    level: protocol::NoticeLevel,
) -> Result<(protocol::SpriteSet, bool), String> {
    send_player_notice_with_code(
        state,
        session_code,
        player_id,
        level,
        SPRITE_ATELIER_NOTICE_TITLE,
        SPRITE_ATELIER_FALLBACK_NOTICE_MESSAGE,
        Some(SessionNoticeCode::SpriteAtelierFallback),
    )
    .await;

    Ok(((*state.fallback_companion_sprites).clone(), true))
}

async fn generate_sprite_sheet_with_queue(
    state: AppState,
    session_code: &str,
    player_id: &str,
    description: &str,
) -> Result<(protocol::SpriteSet, bool), String> {
    send_player_notice_with_code(
        &state,
        session_code,
        player_id,
        protocol::NoticeLevel::Info,
        SPRITE_ATELIER_NOTICE_TITLE,
        SPRITE_ATELIER_ACCEPTED_NOTICE_MESSAGE,
        Some(SessionNoticeCode::SpriteAtelierAccepted),
    )
    .await;

    let _queue_lease = match acquire_image_job_permit(&state, || async {
        send_player_notice_with_code(
            &state,
            session_code,
            player_id,
            protocol::NoticeLevel::Info,
            SPRITE_ATELIER_NOTICE_TITLE,
            SPRITE_ATELIER_QUEUED_NOTICE_MESSAGE,
            Some(SessionNoticeCode::SpriteAtelierQueued),
        )
        .await;
    })
    .await
    {
        Ok(permit) => permit,
        Err(ImageQueueAdmissionOutcome::TimedOut) => {
            return sprite_sheet_fallback_with_notice(
                &state,
                session_code,
                player_id,
                protocol::NoticeLevel::Warning,
            )
            .await;
        }
        Err(ImageQueueAdmissionOutcome::Unavailable) => {
            return Err("image generation queue is unavailable".to_string());
        }
    };

    send_player_notice_with_code(
        &state,
        session_code,
        player_id,
        protocol::NoticeLevel::Info,
        SPRITE_ATELIER_NOTICE_TITLE,
        SPRITE_ATELIER_DRAWING_NOTICE_MESSAGE,
        Some(SessionNoticeCode::SpriteAtelierDrawing),
    )
    .await;

    let lease = state.llm_client.acquire_image_generation_lease();

    match state
        .llm_client
        .generate_sprite_sheet_with_lease(&lease, description)
        .await
    {
        Ok(sprites) => Ok((sprites, false)),
        Err(error) => {
            tracing::warn!(
                session_code = %session_code,
                player_id = %player_id,
                %error,
                "sprite sheet generation failed, using fallback companion"
            );
            sprite_sheet_fallback_with_notice(
                &state,
                session_code,
                player_id,
                protocol::NoticeLevel::Warning,
            )
            .await
        }
    }
}

fn clamp_score(score: i32) -> i32 {
    score.clamp(0, 100)
}

/// Resolve the character to use when entering a workshop.
///
/// Rules (locked decision #6, #9):
/// - If `character_id` is supplied, load that specific character (must be
///   owned by the account OR be a starter-pool character).
/// - If the account owns zero characters, **lease** random existing sprites.
///   "Lease" means we copy a `CharacterProfile` into
///   `SessionPlayer.selected_character` but do NOT flip `owner_account_id`.
/// - If the account owns characters but passed no `character_id`, return
///   `Err` asking them to pick one.
async fn resolve_character_for_session(
    state: &AppState,
    account_id: &str,
    requested_character_id: Option<&str>,
    excluded_character_ids: &BTreeSet<String>,
) -> Result<Option<CharacterProfile>, String> {
    // Explicit selection — must be owned by the requesting account or be a
    // starter-pool character (owner_account_id IS NULL).
    if let Some(character_id) = requested_character_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        let record = state
            .store
            .load_character(character_id)
            .await
            .map_err(|e| format!("failed to load character: {e}"))?;
        return match record {
            Some(r) => {
                let is_owned_by_requester = r.owner_account_id.as_deref() == Some(account_id);
                let is_starter = r.owner_account_id.is_none();
                if !is_owned_by_requester && !is_starter {
                    return Err("you do not own this character".to_string());
                }
                // plan2.md item 3: an explicit starter selection must also
                // respect session-level uniqueness. Otherwise a client could
                // observe another seated player's starter id (broadcast in
                // GameState) and intentionally duplicate it by POSTing
                // /api/workshops/join with that id, bypassing the auto-lease
                // exclusion. Owned-by-requester characters cannot collide by
                // construction, so only gate the starter branch.
                if is_starter && excluded_character_ids.contains(character_id) {
                    return Err("that starter is already taken in this workshop".to_string());
                }
                Ok(Some(character_profile_with_sprite_references(&r)))
            }
            None => Ok(None),
        };
    }

    // No explicit selection. Check owned count.
    let owned_count = state
        .store
        .count_characters_by_owner(account_id)
        .await
        .map_err(|e| format!("failed to count owned characters: {e}"))?;

    if owned_count > 0 {
        // Has characters but didn't pick one — error.
        return Err("please select a character".to_string());
    }

    // Zero characters — lease random existing sprites so the player can start
    // immediately without generating a fresh sprite sheet.
    pick_random_starter_profile(state, excluded_character_ids).await
}

/// Pick random reusable sprites from persisted characters. Prefer generated
/// non-placeholder sprites; fall back to legacy starter rows when no generated
/// sprites have been saved yet.
async fn pick_random_starter_profile(
    state: &AppState,
    excluded_character_ids: &BTreeSet<String>,
) -> Result<Option<CharacterProfile>, String> {
    let characters = state
        .store
        .list_characters()
        .await
        .map_err(|error| format!("failed to list characters: {error}"))?;
    let fallback_sprites = timeout_companion_defaults().sprites;
    let reusable: Vec<_> = characters
        .iter()
        .filter(|r| r.sprites != fallback_sprites && !excluded_character_ids.contains(&r.id))
        .collect();
    if !reusable.is_empty() {
        let index = rand::random_range(0..reusable.len());
        let Some(record) = reusable.get(index) else {
            return Ok(None);
        };
        let creator_name = match record.owner_account_id.as_deref() {
            Some(account_id) => state
                .store
                .find_account_by_id(account_id)
                .await
                .map_err(|error| format!("failed to load character creator: {error}"))?
                .map(|account| account.name),
            None => None,
        };
        return Ok(Some(CharacterProfile {
            id: random_prefixed_id("starter"),
            name: record.name.clone(),
            description: record.description.clone(),
            sprites: character_sprite_reference_set(&record.id),
            remaining_sprite_regenerations: 0,
            creator_account_id: record.owner_account_id.clone(),
            creator_name,
        }));
    }

    let starters: Vec<_> = characters
        .iter()
        .filter(|r| r.owner_account_id.is_none() && !excluded_character_ids.contains(&r.id))
        .collect();
    if starters.is_empty() {
        return Ok(None);
    }
    let index = rand::random_range(0..starters.len());
    Ok(starters.get(index).map(|record| CharacterProfile {
        id: record.id.clone(),
        name: record.name.clone(),
        description: record.description.clone(),
        sprites: character_sprite_reference_set(&record.id),
        remaining_sprite_regenerations: record.remaining_sprite_regenerations,
        creator_account_id: record.owner_account_id.clone(),
        creator_name: None,
    }))
}

/// Extract the authenticated account from request headers without going
/// through the `AccountSession` axum extractor. Used by `join_workshop`
/// whose reconnect branch must work without a cookie.
async fn extract_account_from_headers(
    state: &AppState,
    headers: &HeaderMap,
) -> Option<domain::Account> {
    use crate::auth::account_from_record;
    let jar = SignedCookieJar::<Key>::from_headers(headers, state.config.cookie_key.clone());
    let cookie = jar.get(SESSION_COOKIE_NAME)?;
    let account_id = cookie.value().to_string();
    if account_id.is_empty() {
        return None;
    }
    match state.store.find_account_by_id(&account_id).await {
        Ok(Some(record)) => Some(account_from_record(&record)),
        _ => None,
    }
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

#[allow(dead_code)]
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

    let combined_notes = dragon
        .handover_chain
        .discovery_observations
        .iter()
        .map(String::as_str)
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
            + (combined_notes.contains(food_keyword(dragon.actual_favorite_food)) as i32)
            + (combined_notes.contains(play_keyword(dragon.actual_favorite_play)) as i32);

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
    let observation_feedback = format!(
        "Phase 1 handover matched {mechanic_clue_hits}/3 hidden habits with {observation_count} observation(s) and {tags_count} handover tag(s). Character design text was ignored for care scoring."
    );
    let handover_relevance = if mechanic_clue_hits >= 2 {
        "the received handover was relevant"
    } else if mechanic_clue_hits == 1 {
        "the received handover was only partly relevant"
    } else {
        "the received handover was not relevant to the hidden habits"
    };
    let care_feedback = if total_actions == 0 {
        format!(
            "Phase 2 had no recorded care actions; {handover_relevance}, and final average stats were {stat_average}/100.",
        )
    } else if mistake_count > 0 || dragon.cooldown_violations > 0 {
        format!(
            "Phase 2 followed {correct_actions}/{total_actions} actions correctly; {handover_relevance}, with {mistake_count} wrong food/play actions, {} cooldown violations, and {stat_average}/100 final average stats.",
            dragon.cooldown_violations.max(0),
        )
    } else {
        format!(
            "Phase 2 followed {correct_actions}/{total_actions} actions correctly; {handover_relevance}, and final average stats were {stat_average}/100.",
        )
    };
    let feedback = format!("{observation_feedback} {care_feedback}");

    LlmDragonEvaluation {
        dragon_id: dragon.dragon_id.clone(),
        dragon_name: dragon.dragon_name.clone(),
        observation_score,
        care_score,
        creativity_score,
        observation_feedback,
        care_feedback,
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

pub(crate) async fn run_judge_for_session(
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
        let score_tuples: Vec<(String, i32, i32, String, String, String)> = evaluation
            .dragon_evaluations
            .iter()
            .map(|d| {
                (
                    d.dragon_id.clone(),
                    d.observation_score,
                    d.care_score,
                    d.feedback.clone(),
                    d.observation_feedback.clone(),
                    d.care_feedback.clone(),
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

pub(crate) async fn get_character_sprite(
    State(state): State<AppState>,
    Path((character_id, emotion)): Path<(String, String)>,
) -> Response {
    let record = match state.store.load_character(&character_id).await {
        Ok(Some(record)) => record,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(%error, character_id = %character_id, "load_character failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let Some(base64) = sprite_base64_for_emotion(&record.sprites, emotion.as_str()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let bytes = match base64::engine::general_purpose::STANDARD.decode(base64) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, character_id = %character_id, emotion = %emotion, "sprite base64 decode failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let mut response = Body::from(bytes).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    response
}

pub(crate) async fn create_workshop(
    State(state): State<AppState>,
    session: AccountSession,
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
    // Resolve the effective session config at the HTTP boundary: callers may
    // omit `config` to accept the server-side default (see
    // `WorkshopCreateConfig::default`). The domain always stores a concrete
    // config.
    let session_config = payload.config.clone().unwrap_or_default();
    // Derive name from authenticated account (locked decision #9).
    let normalized_name = session.account.name.clone();
    let timestamp = Utc::now();
    // Creator is the first player in a brand-new session; no other players
    // exist yet, so the starter-exclusion set is trivially empty.
    let selected_character = match resolve_character_for_session(
        &state,
        &session.account.id,
        payload.character_id.as_deref(),
        &BTreeSet::new(),
    )
    .await
    {
        Ok(character) => character,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(WorkshopJoinResult::Error(WorkshopError {
                    ok: false,
                    error,
                })),
            );
        }
    };
    let session_code = allocate_session_code(&state).await;
    let player_id = random_prefixed_id("player");
    let reconnect_token = random_prefixed_id("reconnect");
    let mut workshop = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.clone()),
        timestamp,
        session_config,
    );
    workshop.owner_account_id = Some(session.account.id.clone());
    let host_player = SessionPlayer {
        id: player_id.clone(),
        name: normalized_name.clone(),
        account_id: Some(session.account.id.clone()),
        character_id: selected_character
            .as_ref()
            .map(|character| character.id.clone()),
        selected_character: selected_character.clone(),
        is_host: true,
        is_connected: true,
        is_ready: selected_character.is_some(),
        score: 0,
        current_dragon_id: None,
        achievements: Vec::new(),
        joined_at: timestamp,
    };
    workshop.add_player(host_player.clone());

    let identity = persistence::PlayerIdentity {
        session_id: workshop.id.to_string(),
        player_id: player_id.clone(),
        reconnect_token: reconnect_token.clone(),
        created_at: timestamp.to_rfc3339(),
        last_seen_at: timestamp.to_rfc3339(),
    };
    let artifact = SessionArtifactRecord {
        id: random_prefixed_id("artifact"),
        session_id: workshop.id.to_string(),
        phase: protocol::Phase::Lobby,
        step: 0,
        kind: SessionArtifactKind::SessionCreated,
        player_id: Some(player_id.clone()),
        created_at: timestamp.to_rfc3339(),
        payload: json!({
            "sessionCode": session_code,
            "hostName": normalized_name,
            "accountId": session.account.id,
            "characterId": selected_character.as_ref().map(|character| character.id.clone()),
            "phase0Minutes": workshop.config.phase0_minutes,
            "phase1Minutes": workshop.config.phase1_minutes,
            "phase2Minutes": workshop.config.phase2_minutes,
            "imageModelConfigured": state.config.llm_pool.is_image_configured(),
            "judgeModelConfigured": state.config.llm_pool.is_judge_configured(),
        }),
    };

    if let Err(error) = state
        .store
        .save_session_with_identity_and_artifact(&workshop, &identity, &artifact)
        .await
    {
        return internal_join_error(format!("failed to persist workshop creation: {error}"));
    }

    let response = WorkshopJoinSuccess {
        ok: true,
        session_code: workshop.code.0.clone(),
        player_id: player_id.clone(),
        reconnect_token,
        coordinator_type: CoordinatorType::Rust,
        state: to_client_game_state(&workshop, &player_id),
    };

    state
        .sessions
        .lock()
        .await
        .insert(workshop.code.0.clone(), workshop);

    (
        StatusCode::CREATED,
        Json(WorkshopJoinResult::Success(response)),
    )
}

pub(crate) async fn create_workshop_lobby(
    State(state): State<AppState>,
    session: AccountSession,
    connect_info: MaybeConnectInfo,
    headers: HeaderMap,
    Json(payload): Json<CreateWorkshopRequest>,
) -> (StatusCode, Json<WorkshopCreateResult>) {
    if let Some((status, payload)) = reject_disallowed_origin(&headers, &state.config.origin_policy)
    {
        return (status, map_join_error_to_create(payload));
    }
    let client_key = client_key(&state, connect_info, &headers);
    if let Some((status, payload)) = reject_rate_limited(&state.create_limiter, &client_key).await {
        return (status, map_join_error_to_create(payload));
    }

    let session_config = payload.config.unwrap_or_default();
    let normalized_name = session.account.name.clone();
    let timestamp = Utc::now();
    let session_code = allocate_session_code(&state).await;
    let mut workshop = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.clone()),
        timestamp,
        session_config,
    );
    workshop.reserve_host(session.account.id.clone(), normalized_name.clone());

    let artifact = SessionArtifactRecord {
        id: random_prefixed_id("artifact"),
        session_id: workshop.id.to_string(),
        phase: protocol::Phase::Lobby,
        step: 0,
        kind: SessionArtifactKind::SessionCreated,
        player_id: None,
        created_at: timestamp.to_rfc3339(),
        payload: json!({
            "sessionCode": session_code,
            "hostName": normalized_name,
            "accountId": session.account.id,
            "createdWithoutJoin": true,
            "phase0Minutes": workshop.config.phase0_minutes,
            "phase1Minutes": workshop.config.phase1_minutes,
            "phase2Minutes": workshop.config.phase2_minutes,
            "imageModelConfigured": state.config.llm_pool.is_image_configured(),
            "judgeModelConfigured": state.config.llm_pool.is_judge_configured(),
        }),
    };

    if let Err(error) = state
        .store
        .save_session_with_artifact(&workshop, &artifact)
        .await
    {
        return internal_create_error(format!("failed to persist workshop creation: {error}"));
    }

    state
        .sessions
        .lock()
        .await
        .insert(workshop.code.0.clone(), workshop.clone());

    (
        StatusCode::CREATED,
        Json(WorkshopCreateResult::Success(WorkshopCreateSuccess {
            ok: true,
            session_code: workshop.code.0,
            host_name: normalized_name,
        })),
    )
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

    // New join — requires an authenticated account.
    // We cannot add `AccountSession` to the handler signature because the
    // reconnect branch above must work without a cookie (locked decision #1).
    // Extract the account manually from the signed cookie jar.
    let account = match extract_account_from_headers(&state, &headers).await {
        Some(a) => a,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(WorkshopJoinResult::Error(WorkshopError {
                    ok: false,
                    error: "Authentication required to join a workshop.".to_string(),
                })),
            );
        }
    };
    let normalized_name = account.name.clone();

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

    let restore_candidate = {
        let mut sessions = state.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_code) else {
            return bad_join_request("Workshop not found.");
        };
        if let Some(existing_player_id) = session.players.values().find_map(|player| {
            (player.account_id.as_deref() == Some(&account.id)).then(|| player.id.clone())
        }) {
            let fallback_sprites = timeout_companion_defaults().sprites;
            let needs_sprite_upgrade = session.phase == protocol::Phase::Lobby
                && session
                    .players
                    .get(&existing_player_id)
                    .and_then(|player| player.selected_character.as_ref())
                    .is_some_and(|character| character.sprites == fallback_sprites);
            let excluded_starter_ids = session
                .players
                .values()
                .filter(|player| player.account_id.as_deref() != Some(&account.id))
                .filter_map(|player| {
                    player
                        .selected_character
                        .as_ref()
                        .map(|character| character.id.clone())
                })
                .collect();
            Some((
                existing_player_id,
                needs_sprite_upgrade,
                excluded_starter_ids,
            ))
        } else {
            if session.phase != protocol::Phase::Lobby {
                return conflict_join_request(
                    "This workshop has already started. New players can only join in the lobby.",
                );
            }
            None
        }
    };

    if let Some((player_id, needs_sprite_upgrade, excluded_starter_ids)) = restore_candidate {
        let upgraded_character = if needs_sprite_upgrade {
            match pick_random_starter_profile(&state, &excluded_starter_ids).await {
                Ok(character) => character,
                Err(error) => return internal_join_error(error),
            }
        } else {
            None
        };
        let (session_before, session_clone, reconnect_token, timestamp) = {
            let mut sessions = state.sessions.lock().await;
            let Some(session) = sessions.get_mut(session_code) else {
                return bad_join_request("Workshop not found.");
            };
            let session_before = session.clone();
            let timestamp = Utc::now();
            let reconnect_token = random_prefixed_id("reconnect");
            let Some(player) = session.players.get_mut(&player_id) else {
                return bad_join_request("Session identity is invalid or expired.");
            };
            player.is_connected = true;
            if let Some(character) = upgraded_character {
                player.character_id = Some(character.id.clone());
                player.selected_character = Some(character);
                player.is_ready = true;
            }
            session.ensure_host_assigned(true);
            session.updated_at = timestamp;
            (session_before, session.clone(), reconnect_token, timestamp)
        };

        let identity = persistence::PlayerIdentity {
            session_id: session_clone.id.to_string(),
            player_id: player_id.clone(),
            reconnect_token: reconnect_token.clone(),
            created_at: timestamp.to_rfc3339(),
            last_seen_at: timestamp.to_rfc3339(),
        };
        let reconnect_artifact = SessionArtifactRecord {
            id: random_prefixed_id("artifact"),
            session_id: session_clone.id.to_string(),
            phase: session_clone.phase,
            step: phase_step(session_clone.phase),
            kind: SessionArtifactKind::PlayerReconnected,
            player_id: Some(player_id.clone()),
            created_at: timestamp.to_rfc3339(),
            payload: json!({ "sessionCode": session_code, "playerId": player_id.clone(), "source": "account" }),
        };
        if let Err(error) = write_lease.ensure_active() {
            let mut sessions = state.sessions.lock().await;
            sessions.insert(session_code.to_string(), session_before);
            return internal_join_error(format!(
                "lost session lease before account restore persist: {error}"
            ));
        }
        if let Err(error) = state
            .store
            .save_session_with_identity_and_artifact(&session_clone, &identity, &reconnect_artifact)
            .await
        {
            let mut sessions = state.sessions.lock().await;
            sessions.insert(session_code.to_string(), session_before);
            return internal_join_error(format!("failed to persist account restore: {error}"));
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
        return response;
    }

    let excluded_starter_ids: BTreeSet<String> = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(session_code)
            .map(|session| {
                session
                    .players
                    .values()
                    .filter(|player| player.account_id.as_deref() != Some(&account.id))
                    .filter_map(|player| {
                        player
                            .selected_character
                            .as_ref()
                            .map(|character| character.id.clone())
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    let selected_character = match resolve_character_for_session(
        &state,
        &account.id,
        payload.character_id.as_deref(),
        &excluded_starter_ids,
    )
    .await
    {
        Ok(character) => character,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(WorkshopJoinResult::Error(WorkshopError {
                    ok: false,
                    error,
                })),
            );
        }
    };

    // Starter-lease path (no explicit selection) that yielded no character
    // means every starter is already leased to another player in this session
    // (or none are seeded). Surface a join error so the UI can prompt the
    // player to create their own character instead of silently seating them
    // with an unset pet (plan2.md item 3).
    let requested_character_id = payload
        .character_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if selected_character.is_none() && requested_character_id.is_none() {
        return conflict_join_request("no starter available");
    }

    let (session_before, session_clone, player_id, reconnect_token) = {
        let mut sessions = state.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_code) else {
            return bad_join_request("Workshop not found.");
        };
        if session.phase != protocol::Phase::Lobby {
            return conflict_join_request(
                "This workshop has already started. New players can only join in the lobby.",
            );
        }
        let duplicate_name = session
            .players
            .values()
            .any(|player| player.name.eq_ignore_ascii_case(&normalized_name));
        if duplicate_name {
            return conflict_join_request("That player name is already taken in this workshop.");
        }

        let session_before = session.clone();
        let timestamp = Utc::now();
        let player_id = random_prefixed_id("player");
        let reconnect_token = random_prefixed_id("reconnect");
        let is_reserved_host = session.reserved_host_account_id() == Some(account.id.as_str());
        let host_pending_owner_join = session.host_player_id.is_none()
            && session.reserved_host_account_id().is_none()
            && session.owner_account_id() == Some(account.id.as_str());
        let player = SessionPlayer {
            id: player_id.clone(),
            name: normalized_name.clone(),
            account_id: Some(account.id.clone()),
            character_id: selected_character
                .as_ref()
                .map(|character| character.id.clone()),
            selected_character: selected_character.clone(),
            is_host: is_reserved_host || host_pending_owner_join,
            is_connected: true,
            is_ready: selected_character.is_some(),
            score: 0,
            current_dragon_id: None,
            achievements: Vec::new(),
            joined_at: timestamp,
        };
        session.add_player(player.clone());
        if is_reserved_host {
            session.assign_reserved_host_to_player(&player_id);
        } else if host_pending_owner_join {
            session.ensure_host_assigned(false);
        }
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
        payload: json!({
            "sessionCode": session_code,
            "playerName": normalized_name,
            "characterId": selected_character.as_ref().map(|character| character.id.clone()),
        }),
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
            SessionCommand::StartPhase1 => {
                if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
                    return bad_command_request("Only the host can start the workshop.");
                }
                // Session 4 / refactor: Phase0 is no longer a reachable state.
                // Phase1 now starts directly from Lobby.
                if session.phase != protocol::Phase::Lobby {
                    return conflict_command_request("Phase 1 can only start from the lobby.");
                }

                // Session 4 / refactor: the previous auto-assign-character fallback
                // (which called `pick_random_character_profile`) is removed. Players
                // are now required to have selected a character at join time (either
                // one they own or a leased starter). `begin_phase1` is strict and
                // returns `MissingSelectedCharacter` if this invariant is violated;
                // the error is surfaced to the host as a 400.

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
                    return conflict_command_request(&error.to_string());
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
            SessionCommand::SelectCharacter => {
                let payload = match request.payload.clone() {
                    Some(value) => serde_json::from_value::<SelectCharacterRequest>(value).ok(),
                    None => None,
                };
                let Some(payload) = payload else {
                    return bad_command_request("Character selection payload is invalid.");
                };

                let character_id = payload.character_id.trim();
                if character_id.starts_with("starter_")
                    && session.players.values().any(|player| {
                        player.id != identity.player_id
                            && player.character_id.as_deref() == Some(character_id)
                    })
                {
                    return bad_command_request("That starter is already taken in this workshop.");
                }
                let record = match state.store.load_character(character_id).await {
                    Ok(Some(r)) => r,
                    Ok(None) => return bad_command_request("Selected character was not found."),
                    Err(error) => {
                        return internal_command_error(format!(
                            "failed to load character: {error}"
                        ));
                    }
                };

                // Ownership check: must be owned by the requesting player's
                // account or be a starter-pool character (owner_account_id IS NULL).
                let player_account_id = session
                    .players
                    .get(&identity.player_id)
                    .and_then(|p| p.account_id.as_deref());
                let is_owned = player_account_id.is_some()
                    && record.owner_account_id.as_deref() == player_account_id;
                let is_starter = record.owner_account_id.is_none();
                if !is_owned && !is_starter {
                    return bad_command_request("You do not own this character.");
                }
                if is_starter
                    && session.players.values().any(|player| {
                        player.id != identity.player_id
                            && player.character_id.as_deref() == Some(character_id)
                    })
                {
                    return bad_command_request("That starter is already taken in this workshop.");
                }
                let character = character_profile_with_sprite_references(&record);

                if let Err(error) =
                    session.assign_player_character(&identity.player_id, character.clone())
                {
                    return conflict_command_request(&error.to_string());
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
                    payload: json!({
                        "characterId": character.id,
                        "remainingSpriteRegenerations": character.remaining_sprite_regenerations,
                    }),
                });

                successful_workshop_command(&mut should_broadcast)
            }
            SessionCommand::SubmitObservation => {
                if session.phase != protocol::Phase::Phase1 {
                    return conflict_command_request(
                        "Observations can only be saved during Phase 1.",
                    );
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
                    .ok_or_else(|| conflict_command_request("Player is not assigned to a dragon."));
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
                    None => return conflict_command_request("Player is not assigned to a dragon."),
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
                        return conflict_command_request(&message);
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
                    return conflict_command_request("Handover can only begin during Phase 1.");
                }
                if let Err(error) = session.transition_to(protocol::Phase::Handover) {
                    return conflict_command_request(&error.to_string());
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
                    return conflict_command_request(
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

                if let Err(error) = session.save_handover_tags(&identity.player_id, tags) {
                    return match error {
                        DomainError::InvalidHandoverTagCount { expected, got } => {
                            bad_command_request(&format!(
                                "Exactly {expected} handover notes are required (got {got})."
                            ))
                        }
                        _ => conflict_command_request(&error.to_string()),
                    };
                }
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
                    return conflict_command_request("Phase 2 can only begin from handover.");
                }
                let phase2_result = if session.remaining_phase_seconds(Utc::now()) == Some(0) {
                    session.enter_phase2_after_deadline()
                } else {
                    session.enter_phase2()
                };
                if let Err(error) = phase2_result {
                    return match error {
                        DomainError::MissingHandoverTags { players } => conflict_command_request(
                            &format!("Still waiting on: {}.", players.join(", ")),
                        ),
                        _ => conflict_command_request(&error.to_string()),
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
                if !matches!(
                    session.phase,
                    protocol::Phase::Phase2 | protocol::Phase::Judge
                ) {
                    return conflict_command_request("Design voting can only begin from Phase 2.");
                }
                session.award_phase_end_achievements();
                if let Err(error) = session.enter_voting() {
                    return conflict_command_request(&error.to_string());
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
                    return conflict_command_request("Design voting can only begin from Phase 2.");
                }
                if let Err(error) = session.enter_voting() {
                    return conflict_command_request(&error.to_string());
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
                    return conflict_command_request("Voting is not active right now.");
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
                        DomainError::VotingClosed => "Voting is already closed.".to_string(),
                        _ => error.to_string(),
                    };
                    return conflict_command_request(&message);
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
                    return conflict_command_request("Results can only be revealed during voting.");
                }
                if let Err(error) = session.reveal_voting_results() {
                    let message = match error {
                        DomainError::VotingRevealNotReady => {
                            "Voting can only be finished after at least one eligible vote is submitted."
                                .to_string()
                        }
                        _ => error.to_string(),
                    };
                    return conflict_command_request(&message);
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
                    return conflict_command_request("Session can only be ended during voting.");
                }
                if let Err(error) = session.finalize_voting() {
                    let message = match error {
                        DomainError::VotingResultsNotRevealed => {
                            "Session can only be ended after voting results are revealed."
                                .to_string()
                        }
                        _ => error.to_string(),
                    };
                    return conflict_command_request(&message);
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
                if let Err(error) = session.reset_to_lobby(&identity.player_id) {
                    return conflict_command_request(&error.to_string());
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

    let (_, _write_guard, write_lease) =
        match SessionWriteLease::acquire(&state, session_code).await {
            Ok(guard) => guard,
            Err(error) => {
                return internal_judge_bundle_error(format!(
                    "failed to acquire session lease: {error}"
                ));
            }
        };
    if let Err(error) = write_lease.ensure_active() {
        return internal_judge_bundle_error(format!(
            "lost session lease before workshop archive load: {error}"
        ));
    }
    match reload_cached_session(&state, session_code).await {
        Ok(true) => {}
        Ok(false) => return bad_judge_bundle_request("Workshop not found."),
        Err(error) => {
            return internal_judge_bundle_error(format!("failed to reload session: {error}"));
        }
    }
    if let Err(error) = write_lease.ensure_active() {
        return internal_judge_bundle_error(format!(
            "lost session lease before workshop archive validation: {error}"
        ));
    }

    let session = {
        let sessions = state.sessions.lock().await;
        let Some(session) = sessions.get(session_code) else {
            return bad_judge_bundle_request("Workshop not found.");
        };
        session.clone()
    };

    if session.phase != protocol::Phase::End {
        return conflict_judge_bundle_request(
            "Workshop archive can only be built after the game ends.",
        );
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

    let archive_already_built = artifacts
        .iter()
        .any(|artifact| artifact.kind == SessionArtifactKind::JudgeBundleGenerated);
    if !archive_already_built
        && session.host_player_id.as_deref() != Some(identity.player_id.as_str())
    {
        return bad_judge_bundle_request("Only the host can build the workshop archive.");
    }

    let bundle = build_judge_bundle(&session, &artifacts);

    if !archive_already_built {
        if let Err(error) = state
            .store
            .append_session_artifact(&SessionArtifactRecord {
                id: random_prefixed_id("artifact"),
                session_id: session.id.to_string(),
                phase: session.phase,
                step: phase_step(session.phase),
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
            return internal_judge_bundle_error(format!(
                "failed to append session artifact: {error}"
            ));
        }
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

fn internal_create_error(message: String) -> (StatusCode, Json<WorkshopCreateResult>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(WorkshopCreateResult::Error(WorkshopError {
            ok: false,
            error: message,
        })),
    )
}

fn map_join_error_to_create(payload: Json<WorkshopJoinResult>) -> Json<WorkshopCreateResult> {
    let Json(payload) = payload;
    Json(match payload {
        WorkshopJoinResult::Error(error) => WorkshopCreateResult::Error(error),
        WorkshopJoinResult::Success(_) => WorkshopCreateResult::Error(WorkshopError {
            ok: false,
            error: "unexpected join success payload".to_string(),
        }),
    })
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

fn conflict_judge_bundle_request(message: &str) -> (StatusCode, Json<WorkshopJudgeBundleResult>) {
    (
        StatusCode::CONFLICT,
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

fn conflict_join_request(message: &str) -> (StatusCode, Json<WorkshopJoinResult>) {
    (
        StatusCode::CONFLICT,
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

fn conflict_command_request(message: &str) -> (StatusCode, Json<WorkshopCommandResult>) {
    tracing::warn!(error = %message, "workshop_command returning 409 Conflict");
    (
        StatusCode::CONFLICT,
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

    let session = match ensure_session_cached(&state, session_code).await {
        Ok(true) => {
            let sessions = state.sessions.lock().await;
            sessions.get(session_code).cloned()
        }
        Ok(false) => None,
        Err(error) => {
            return internal_llm_judge_error(format!("failed to load session: {error}"));
        }
    };
    let Some(session) = session else {
        return bad_llm_judge_request("Workshop not found.");
    };
    if session.host_player_id.as_deref() != Some(identity.player_id.as_str()) {
        return bad_llm_judge_request("Only the host can run judge scoring.");
    }
    if session.phase != protocol::Phase::End {
        return conflict_llm_judge_request("Judge scoring is only available after the game ends.");
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

    let session = match ensure_session_cached(&state, session_code).await {
        Ok(true) => {
            let sessions = state.sessions.lock().await;
            sessions.get(session_code).cloned()
        }
        Ok(false) => None,
        Err(error) => {
            return internal_llm_image_error(format!("failed to load session: {error}"));
        }
    };
    let Some(session) = session else {
        return bad_llm_image_request("Workshop not found.");
    };
    if session.phase != protocol::Phase::Phase0 {
        return conflict_llm_image_request(
            "Image generation is only available during character creation.",
        );
    }

    let _queue_lease = match acquire_image_job_permit(&state, || async {}).await {
        Ok(permit) => permit,
        Err(ImageQueueAdmissionOutcome::TimedOut) => {
            return internal_llm_image_error(
                "image request timed out while waiting for generation capacity".to_string(),
            );
        }
        Err(ImageQueueAdmissionOutcome::Unavailable) => {
            return internal_llm_image_error("image generation queue is unavailable".to_string());
        }
    };

    let session_after_wait = match ensure_session_cached(&state, session_code).await {
        Ok(true) => {
            let sessions = state.sessions.lock().await;
            sessions.get(session_code).cloned()
        }
        Ok(false) => None,
        Err(error) => {
            return internal_llm_image_error(format!("failed to reload session: {error}"));
        }
    };
    let Some(session_after_wait) = session_after_wait else {
        return bad_llm_image_request("Workshop not found.");
    };
    if session_after_wait.phase != protocol::Phase::Phase0 {
        return conflict_llm_image_request(
            "Image generation is only available during character creation.",
        );
    };
    let image_lease = state.llm_client.acquire_image_generation_lease();

    let (image_base64, mime_type) = match state
        .llm_client
        .generate_image_with_lease(&image_lease, prompt)
        .await
    {
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
    let fallback_companion_sprites = (*state.fallback_companion_sprites).clone();
    match generate_or_update_character_sprite_sheet(
        state,
        connect_info,
        headers,
        CharacterSpriteSheetRequest {
            session_code: request.session_code,
            reconnect_token: request.reconnect_token,
            description: request.description,
            character_id: None,
        },
    )
    .await
    {
        (status, Json(CharacterSpriteSheetResult::Success(success))) => {
            let fallback_used = success.sprites == fallback_companion_sprites;
            (
                status,
                Json(SpriteSheetResult::Success(SpriteSheetSuccess {
                    ok: success.ok,
                    sprites: success.sprites,
                    fallback_used,
                })),
            )
        }
        (status, Json(CharacterSpriteSheetResult::Error(error))) => {
            (status, Json(SpriteSheetResult::Error(error)))
        }
    }
}

pub(crate) async fn list_character_catalog(
    State(state): State<AppState>,
    connect_info: MaybeConnectInfo,
    headers: HeaderMap,
    Json(_request): Json<CharacterCatalogRequest>,
) -> (StatusCode, Json<CharacterCatalogResult>) {
    if let Some(response) =
        reject_disallowed_character_catalog_origin(&headers, &state.config.origin_policy)
    {
        return response;
    }
    let client_key = client_key(&state, connect_info, &headers);
    if is_rate_limited(&state.command_limiter, &client_key).await {
        return too_many_character_catalog_requests();
    }

    let mut characters = match state.store.list_characters().await {
        Ok(characters) => characters,
        Err(error) => {
            return internal_character_catalog_error(format!("failed to list characters: {error}"));
        }
    };
    characters.sort_by(|left, right| left.id.cmp(&right.id));
    let profiles = characters
        .iter()
        .filter(|r| r.owner_account_id.is_none())
        .map(CharacterRecord::profile)
        .collect::<Vec<_>>();

    (
        StatusCode::OK,
        Json(CharacterCatalogResult::Success(CharacterCatalogSuccess {
            ok: true,
            characters: profiles,
        })),
    )
}

pub(crate) async fn generate_character_sprite_sheet(
    State(state): State<AppState>,
    connect_info: MaybeConnectInfo,
    headers: HeaderMap,
    Json(request): Json<CharacterSpriteSheetRequest>,
) -> (StatusCode, Json<CharacterSpriteSheetResult>) {
    generate_or_update_character_sprite_sheet(state, connect_info, headers, request).await
}

async fn generate_or_update_character_sprite_sheet(
    state: AppState,
    connect_info: MaybeConnectInfo,
    headers: HeaderMap,
    request: CharacterSpriteSheetRequest,
) -> (StatusCode, Json<CharacterSpriteSheetResult>) {
    if let Some(response) =
        reject_disallowed_character_sprite_sheet_origin(&headers, &state.config.origin_policy)
    {
        return response;
    }
    let client_key = client_key(&state, connect_info, &headers);
    if is_rate_limited(&state.command_limiter, &client_key).await {
        return too_many_character_sprite_sheet_requests();
    }

    let session_code = request.session_code.trim();
    let reconnect_token = request.reconnect_token.trim();
    if session_code.is_empty()
        || reconnect_token.is_empty()
        || security::validate_session_code(session_code).is_err()
    {
        return bad_character_sprite_sheet_request("Missing workshop credentials.");
    }

    let identity = match authorize_reconnect_identity(&state, session_code, reconnect_token).await {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            return bad_character_sprite_sheet_request("Session identity is invalid or expired.");
        }
        Err(error) => {
            return internal_character_sprite_sheet_error(format!(
                "failed to lookup identity: {error}"
            ));
        }
    };

    if let Err(error) = refresh_reconnect_identity(&state, reconnect_token, Utc::now()).await {
        return internal_character_sprite_sheet_error(format!(
            "failed to touch player identity: {error}"
        ));
    }

    match reload_cached_session(&state, session_code).await {
        Ok(true) => {}
        Ok(false) => return bad_character_sprite_sheet_request("Workshop not found."),
        Err(error) => {
            return internal_character_sprite_sheet_error(format!(
                "failed to load session: {error}"
            ));
        }
    }

    let current_character = {
        let sessions = state.sessions.lock().await;
        let Some(session) = sessions.get(session_code) else {
            return bad_character_sprite_sheet_request("Workshop not found.");
        };
        let Some(player) = session.players.get(&identity.player_id) else {
            return bad_character_sprite_sheet_request("Session identity is invalid or expired.");
        };
        player.selected_character.clone()
    };

    if let Some(requested_character_id) = request
        .character_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let Some(selected_character) = current_character.as_ref() else {
            return conflict_character_sprite_sheet_request(
                "Select a character before redrawing it.",
            );
        };
        if selected_character.id != requested_character_id {
            return bad_character_sprite_sheet_request(
                "You can only redraw the character currently selected for this player.",
            );
        }
    }

    if current_character.as_ref().is_some_and(|character| {
        character.remaining_sprite_regenerations == 0
            && !sprite_set_uses_references(&character.sprites)
    }) {
        return conflict_character_sprite_sheet_request(
            "Your dragon has already used its one redraw.",
        );
    }

    let existing_character_record = match current_character.as_ref() {
        Some(character) => match state.store.load_character(&character.id).await {
            Ok(record) => record,
            Err(error) => {
                return internal_character_sprite_sheet_error(format!(
                    "failed to load character: {error}"
                ));
            }
        },
        None => None,
    };

    let existing_character_created_at = existing_character_record
        .as_ref()
        .map(|record| record.created_at.clone());
    let existing_owner_account_id = existing_character_record
        .as_ref()
        .and_then(|record| record.owner_account_id.clone());

    let description = request.description.trim();
    if description.is_empty() {
        return bad_character_sprite_sheet_request("Dragon description is required.");
    }

    let (sprites, fallback_used) = match generate_sprite_sheet_with_queue(
        state.clone(),
        session_code,
        &identity.player_id,
        description,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return internal_character_sprite_sheet_error(error),
    };

    let fallback_used = fallback_used;

    let (session_before, session_to_persist, artifact_to_append, next_character_record) = {
        let mut sessions = state.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_code) else {
            return bad_character_sprite_sheet_request("Workshop not found.");
        };
        let session_before = session.clone();
        let Some(player) = session.players.get(&identity.player_id) else {
            return bad_character_sprite_sheet_request("Session identity is invalid or expired.");
        };
        let existing_character = player.selected_character.clone();
        let next_remaining_sprite_regenerations = existing_character
            .as_ref()
            .map(|character| character.remaining_sprite_regenerations.saturating_sub(1))
            .unwrap_or(1);
        let character_id = if existing_character
            .as_ref()
            .is_some_and(|character| sprite_set_uses_references(&character.sprites))
        {
            random_prefixed_id("character")
        } else {
            existing_character
                .as_ref()
                .map(|character| character.id.clone())
                .unwrap_or_else(|| random_prefixed_id("character"))
        };
        let timestamp = Utc::now().to_rfc3339();
        let character_record = CharacterRecord {
            id: character_id.clone(),
            name: existing_character
                .as_ref()
                .and_then(|character| character.name.clone()),
            description: description.to_string(),
            sprites: sprites.clone(),
            remaining_sprite_regenerations: next_remaining_sprite_regenerations,
            created_at: existing_character_created_at
                .clone()
                .unwrap_or_else(|| timestamp.clone()),
            updated_at: timestamp,
            owner_account_id: existing_owner_account_id,
        };
        let mut character_profile = character_record.profile();
        character_profile.sprites = sprites.clone();
        if let Err(error) = session.assign_player_character(&identity.player_id, character_profile)
        {
            return conflict_character_sprite_sheet_request(&error.to_string());
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
                "characterId": character_record.id,
                "fallbackUsed": fallback_used,
                "remainingSpriteRegenerations": character_record.remaining_sprite_regenerations,
            }),
        };
        (session_before, session.clone(), artifact, character_record)
    };

    if let Err(error) = state.store.save_character(&next_character_record).await {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_code.to_string(), session_before.clone());
        return internal_character_sprite_sheet_error(format!(
            "failed to persist character: {error}"
        ));
    }

    if let Err(error) = state
        .store
        .save_session_with_artifact(&session_to_persist, &artifact_to_append)
        .await
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_code.to_string(), session_before);
        return internal_character_sprite_sheet_error(format!(
            "failed to persist character selection: {error}"
        ));
    }
    broadcast_session_state(&state, session_code, None).await;

    (
        StatusCode::OK,
        Json(CharacterSpriteSheetResult::Success(
            CharacterSpriteSheetSuccess {
                ok: true,
                character_id: next_character_record.id,
                sprites,
                remaining_sprite_regenerations: next_character_record
                    .remaining_sprite_regenerations,
            },
        )),
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

fn conflict_llm_judge_request(message: &str) -> (StatusCode, Json<LlmJudgeResult>) {
    (
        StatusCode::CONFLICT,
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

fn conflict_llm_image_request(message: &str) -> (StatusCode, Json<LlmImageResult>) {
    (
        StatusCode::CONFLICT,
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

fn too_many_character_catalog_requests() -> (StatusCode, Json<CharacterCatalogResult>) {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(CharacterCatalogResult::Error(WorkshopError {
            ok: false,
            error: "Too many requests. Please slow down and try again.".to_string(),
        })),
    )
}

fn too_many_character_sprite_sheet_requests() -> (StatusCode, Json<CharacterSpriteSheetResult>) {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(CharacterSpriteSheetResult::Error(WorkshopError {
            ok: false,
            error: "Too many requests. Please slow down and try again.".to_string(),
        })),
    )
}

fn reject_disallowed_character_catalog_origin(
    headers: &HeaderMap,
    policy: &OriginPolicy,
) -> Option<(StatusCode, Json<CharacterCatalogResult>)> {
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    if security::is_origin_allowed(origin, policy) {
        None
    } else {
        Some((
            StatusCode::FORBIDDEN,
            Json(CharacterCatalogResult::Error(WorkshopError {
                ok: false,
                error: "Origin is not allowed.".to_string(),
            })),
        ))
    }
}

fn reject_disallowed_character_sprite_sheet_origin(
    headers: &HeaderMap,
    policy: &OriginPolicy,
) -> Option<(StatusCode, Json<CharacterSpriteSheetResult>)> {
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    if security::is_origin_allowed(origin, policy) {
        None
    } else {
        Some((
            StatusCode::FORBIDDEN,
            Json(CharacterSpriteSheetResult::Error(WorkshopError {
                ok: false,
                error: "Origin is not allowed.".to_string(),
            })),
        ))
    }
}

fn internal_character_catalog_error(message: String) -> (StatusCode, Json<CharacterCatalogResult>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(CharacterCatalogResult::Error(WorkshopError {
            ok: false,
            error: message,
        })),
    )
}

fn bad_character_sprite_sheet_request(
    message: &str,
) -> (StatusCode, Json<CharacterSpriteSheetResult>) {
    (
        StatusCode::BAD_REQUEST,
        Json(CharacterSpriteSheetResult::Error(WorkshopError {
            ok: false,
            error: message.to_string(),
        })),
    )
}

fn conflict_character_sprite_sheet_request(
    message: &str,
) -> (StatusCode, Json<CharacterSpriteSheetResult>) {
    (
        StatusCode::CONFLICT,
        Json(CharacterSpriteSheetResult::Error(WorkshopError {
            ok: false,
            error: message.to_string(),
        })),
    )
}

fn internal_character_sprite_sheet_error(
    message: String,
) -> (StatusCode, Json<CharacterSpriteSheetResult>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(CharacterSpriteSheetResult::Error(WorkshopError {
            ok: false,
            error: message,
        })),
    )
}

// ---------------------------------------------------------------------------
// Account-scoped character CRUD (C2-b)
// ---------------------------------------------------------------------------

/// `POST /api/characters` — create a new owned character.
///
/// Requires `AccountSession`. Enforces the per-account character limit
/// and a 20/hr/account rate limit.
pub(crate) async fn create_character(
    State(state): State<AppState>,
    session: AccountSession,
    Json(request): Json<CreateCharacterRequest>,
) -> Response {
    // Rate limit: 20/hr per account.
    if is_rate_limited(&state.character_create_limiter, &session.account.id).await {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "Too many requests. Please slow down and try again." })),
        )
            .into_response();
    }

    let description = request.description.trim().to_string();
    if description.is_empty() || description.len() > 512 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "description must be 1-512 characters" })),
        )
            .into_response();
    }

    let now = Utc::now().to_rfc3339();
    let record = CharacterRecord {
        id: random_prefixed_id("character"),
        name: None,
        description,
        sprites: request.sprites,
        remaining_sprite_regenerations: 1,
        created_at: now.clone(),
        updated_at: now,
        owner_account_id: Some(session.account.id.clone()),
    };

    // Atomic cap enforcement: count + insert happen under a single per-owner
    // lock in the persistence layer to prevent concurrent creates from
    // racing past `MAX_CHARACTERS_PER_ACCOUNT`.
    match state
        .store
        .save_character_enforcing_cap(&record, MAX_CHARACTERS_PER_ACCOUNT as u32)
        .await
    {
        Ok(()) => (StatusCode::CREATED, Json(record.profile())).into_response(),
        Err(persistence::PersistenceError::CharacterLimitReached { max }) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("character limit reached ({max} per account)") })),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "save_character_enforcing_cap failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal error" })),
            )
                .into_response()
        }
    }
}

/// `POST /api/characters/preview-sprites` — generate a 4-frame sprite sheet
/// from a description without persisting anything.
///
/// Account-scoped companion to the workshop-scoped
/// `POST /api/characters/sprite-sheet`. Requires `AccountSession`; does
/// NOT create a `CharacterRecord`, does NOT mutate any session, does NOT
/// broadcast. The frontend holds the returned `SpriteSet` in memory and
/// submits it via `POST /api/characters` on confirm.
///
/// Rate limit: shares the per-account `character_create_limiter` quota
/// (20/hr) so preview + save together stay bounded. See `app.rs`.
pub(crate) async fn generate_character_sprite_preview(
    State(state): State<AppState>,
    session: AccountSession,
    Json(request): Json<CharacterSpritePreviewRequest>,
) -> Response {
    if is_rate_limited(&state.character_create_limiter, &session.account.id).await {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "Too many requests. Please slow down and try again." })),
        )
            .into_response();
    }

    let description = request.description.trim();
    if description.is_empty() || description.len() > 512 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "description must be 1-512 characters" })),
        )
            .into_response();
    }

    // Reuse the same image-job queue semaphore used by the workshop-scoped
    // sprite-sheet producer so preview requests do not starve it. We do not
    // send session notices (no session exists); on timeout or LLM failure,
    // return a retryable busy error instead of showing a fake companion.
    let _queue_permit = match acquire_image_job_permit(&state, || async {}).await {
        Ok(permit) => permit,
        Err(ImageQueueAdmissionOutcome::TimedOut) => {
            return sprite_preview_busy_response();
        }
        Err(ImageQueueAdmissionOutcome::Unavailable) => {
            tracing::warn!(
                account_id = %session.account.id,
                "character sprite preview queue unavailable"
            );
            return sprite_preview_busy_response();
        }
    };

    let lease = state.llm_client.acquire_image_generation_lease();
    let sprites = match state
        .llm_client
        .generate_sprite_sheet_with_lease(&lease, description)
        .await
    {
        Ok(sprites) => sprites,
        Err(error) => {
            tracing::warn!(
                account_id = %session.account.id,
                %error,
                "character sprite preview generation failed"
            );
            return sprite_preview_busy_response();
        }
    };

    (
        StatusCode::OK,
        Json(CharacterSpritePreviewResponse { sprites }),
    )
        .into_response()
}

/// `GET /api/characters/mine` — list the authenticated account's characters.
pub(crate) async fn list_my_characters(
    State(state): State<AppState>,
    session: AccountSession,
) -> Response {
    let records = match state
        .store
        .list_characters_by_owner(&session.account.id)
        .await
    {
        Ok(r) => r,
        Err(error) => {
            tracing::error!(%error, "list_characters_by_owner failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal error" })),
            )
                .into_response();
        }
    };
    let characters: Vec<CharacterProfile> = records.iter().map(|r| r.profile()).collect();
    (
        StatusCode::OK,
        Json(MyCharactersResponse {
            characters,
            limit: MAX_CHARACTERS_PER_ACCOUNT as u8,
        }),
    )
        .into_response()
}

/// `DELETE /api/characters/:id` — delete an owned character.
pub(crate) async fn delete_character(
    State(state): State<AppState>,
    session: AccountSession,
    Path(character_id): Path<String>,
) -> Response {
    match state
        .store
        .delete_character_by_owner(&character_id, &session.account.id)
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "character not found or not owned by you" })),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "delete_character_by_owner failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal error" })),
            )
                .into_response()
        }
    }
}

/// `PATCH /api/characters/:id` — rename an owned character.
pub(crate) async fn update_character(
    State(state): State<AppState>,
    session: AccountSession,
    Path(character_id): Path<String>,
    Json(request): Json<UpdateCharacterRequest>,
) -> Response {
    let name = request.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "name must be 1-64 characters" })),
        )
            .into_response();
    }

    let updated = match state
        .store
        .update_character_name_by_owner(&character_id, &session.account.id, &name)
        .await
    {
        Ok(updated) => updated,
        Err(error) => {
            tracing::error!(%error, "update_character_name_by_owner failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal error" })),
            )
                .into_response();
        }
    };

    if !updated {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "character not found or not owned by you" })),
        )
            .into_response();
    }

    match state.store.load_character(&character_id).await {
        Ok(Some(record))
            if record.owner_account_id.as_deref() == Some(session.account.id.as_str()) =>
        {
            (StatusCode::OK, Json(record.profile())).into_response()
        }
        Ok(Some(_)) | Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "character not found or not owned by you" })),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "load_character after rename failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal error" })),
            )
                .into_response()
        }
    }
}

/// `DELETE /api/workshops/:code` — delete an owned lobby workshop.
pub(crate) async fn delete_workshop(
    State(state): State<AppState>,
    headers: HeaderMap,
    session: AccountSession,
    Path(session_code): Path<String>,
) -> Response {
    if let Some((status, payload)) = reject_disallowed_origin(&headers, &state.config.origin_policy)
    {
        let error = match payload.0 {
            WorkshopJoinResult::Success(_) => "Origin is not allowed.".to_string(),
            WorkshopJoinResult::Error(error) => error.error,
        };
        return (status, Json(json!({ "error": error }))).into_response();
    }

    let (_, _write_guard, write_lease) =
        match SessionWriteLease::acquire(&state, &session_code).await {
            Ok(guard) => guard,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("failed to acquire session lease: {error}") })),
                )
                    .into_response();
            }
        };
    if let Err(error) = write_lease.ensure_active() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("lost session lease before delete: {error}") })),
        )
            .into_response();
    }

    match reload_cached_session(&state, &session_code).await {
        Ok(_) => {}
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({ "error": format!("failed to reload session before delete: {error}") }),
                ),
            )
                .into_response();
        }
    }
    if let Err(error) = write_lease.ensure_active() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("lost session lease before delete persist: {error}") })),
        )
            .into_response();
    }

    match state
        .store
        .delete_lobby_workshop_by_owner(&session_code, &session.account.id)
        .await
    {
        Ok(true) => {
            state.sessions.lock().await.remove(&session_code);
            match state
                .store
                .delete_realtime_connections_for_session(&session_code)
                .await
            {
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, %session_code, "failed to delete realtime connections after workshop delete");
                }
            }
            close_local_workshop_connections(&state, &session_code, Some("Workshop not found.")).await;
            let notification = persistence::SessionUpdateNotification::workshop_deleted(&session_code);
            if let Err(error) = state.store.publish_session_notification(&notification).await {
                tracing::warn!(%error, %session_code, "failed to publish delete notification");
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Workshop not found, already started, not empty, or not owned by your account." })),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, %session_code, "delete_lobby_workshop_by_owner failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal error" })),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Open workshops list (C2-b)
// ---------------------------------------------------------------------------

/// Query params for `GET /api/workshops/open`. All four fields are optional
/// on the wire, but validation requires:
///   - at most one of `{after_*}` or `{before_*}` groups may be supplied;
///   - if any field from a group is present, both fields must be.
/// These invariants give a clean keyset cursor shape without a separate
/// opaque-cursor-string encoding.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct OpenWorkshopsQuery {
    pub after_created_at: Option<String>,
    pub after_session_code: Option<String>,
    pub before_created_at: Option<String>,
    pub before_session_code: Option<String>,
}

/// `GET /api/workshops/open` — list open lobbies and archived workshops.
/// Supports bidirectional keyset pagination via query params:
///   `?after_created_at=<rfc3339>&after_session_code=<code>` (Next) XOR
///   `?before_created_at=<rfc3339>&before_session_code=<code>` (Prev).
pub(crate) async fn list_open_workshops(
    State(state): State<AppState>,
    session: AccountSession,
    Query(query): Query<OpenWorkshopsQuery>,
) -> Response {
    // `serde_urlencoded` turns `?after_created_at=&after_session_code=` into
    // `Some("")`/`Some("")` — not `None`/`None`. Treat empty-string params as
    // absent so fully-empty pairs fall through to `First` instead of being
    // fed into cursor logic (which would return HTTP 200 with an empty list,
    // indistinguishable from a legitimate empty result). Unpaired halves
    // (one present + one empty/absent) still get rejected with 400 below.
    let after_ts = query.after_created_at.filter(|s| !s.is_empty());
    let after_code = query.after_session_code.filter(|s| !s.is_empty());
    let before_ts = query.before_created_at.filter(|s| !s.is_empty());
    let before_code = query.before_session_code.filter(|s| !s.is_empty());

    // Validate: at most one side, and each side's pair must be complete.
    let after_provided = after_ts.is_some() || after_code.is_some();
    let before_provided = before_ts.is_some() || before_code.is_some();
    if after_provided && before_provided {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "only one of after_* or before_* may be supplied" })),
        )
            .into_response();
    }
    let paging = if after_provided {
        match (after_ts, after_code) {
            (Some(ts), Some(code)) => persistence::OpenWorkshopsPaging::After(OpenWorkshopCursor {
                created_at: ts,
                session_code: code,
            }),
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "after_created_at and after_session_code must both be supplied" })),
                )
                    .into_response();
            }
        }
    } else if before_provided {
        match (before_ts, before_code) {
            (Some(ts), Some(code)) => {
                persistence::OpenWorkshopsPaging::Before(OpenWorkshopCursor {
                    created_at: ts,
                    session_code: code,
                })
            }
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "before_created_at and before_session_code must both be supplied" })),
                )
                    .into_response();
            }
        }
    } else {
        persistence::OpenWorkshopsPaging::First
    };

    let account_id = session.account.id.as_str();
    let page = match state
        .store
        .list_open_workshops(paging.clone(), Some(account_id.to_string()))
        .await
    {
        Ok(p) => p,
        Err(error) => {
            tracing::error!(%error, "list_open_workshops failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal error" })),
            )
                .into_response();
        }
    };

    // Compute next/prev cursors from the page's bookends + has_more flags.
    // Semantics are symmetric so the UI only needs to disable buttons when
    // the respective cursor is None.
    let first_row = page.rows.first();
    let last_row = page.rows.last();
    let first_cursor = first_row.map(|r| OpenWorkshopCursor {
        created_at: r.created_at.clone(),
        session_code: r.session_code.clone(),
    });
    let last_cursor = last_row.map(|r| OpenWorkshopCursor {
        created_at: r.created_at.clone(),
        session_code: r.session_code.clone(),
    });
    let (next_cursor, prev_cursor) = match &paging {
        persistence::OpenWorkshopsPaging::First => {
            let next = if page.has_more_after {
                last_cursor
            } else {
                None
            };
            (next, None)
        }
        persistence::OpenWorkshopsPaging::After(_) => {
            let next = if page.has_more_after {
                last_cursor
            } else {
                None
            };
            // We navigated forward, so a prev page exists unconditionally —
            // it's the page we came from. Anchor it to the first row so the
            // next "Prev" query lands on rows strictly newer than it.
            (next, first_cursor)
        }
        persistence::OpenWorkshopsPaging::Before(_) => {
            // User clicked Prev, so a Next page (the one we came from) exists.
            let next = last_cursor;
            let prev = if page.has_more_before {
                first_cursor
            } else {
                None
            };
            (next, prev)
        }
    };

    let workshops: Vec<OpenWorkshopSummary> = page
        .rows
        .into_iter()
        .map(|r| OpenWorkshopSummary {
            session_code: r.session_code,
            host_name: r.host_name,
            player_count: r.player_count,
            created_at: r.created_at,
            archived: r.archived,
            can_delete: !r.archived
                && !r.resumable
                && r.player_count == 0
                && r.owner_account_id.as_deref() == Some(account_id),
            can_resume: r.resumable,
        })
        .collect();
    (
        StatusCode::OK,
        Json(ListOpenWorkshopsResponse {
            workshops,
            next_cursor,
            prev_cursor,
        }),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Eligible characters for a workshop (C2-b)
// ---------------------------------------------------------------------------

/// `GET /api/workshops/:code/eligible-characters` — list the account's
/// characters eligible to join this workshop. MVP: returns all owned chars.
pub(crate) async fn eligible_characters(
    State(state): State<AppState>,
    session: AccountSession,
    Path(_code): Path<String>,
) -> Response {
    let records = match state
        .store
        .list_characters_by_owner(&session.account.id)
        .await
    {
        Ok(r) => r,
        Err(error) => {
            tracing::error!(%error, "list_characters_by_owner failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal error" })),
            )
                .into_response();
        }
    };
    let characters: Vec<CharacterProfile> = records.iter().map(|r| r.profile()).collect();
    (
        StatusCode::OK,
        Json(EligibleCharactersResponse { characters }),
    )
        .into_response()
}
