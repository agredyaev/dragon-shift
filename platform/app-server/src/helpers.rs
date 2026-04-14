use chrono::Utc;
use domain::{PlayerAction, SessionDragon, WorkshopSession};
use protocol::{
    ActionPayload, ActiveTime, ClientDragon, ClientGameState, ClientVotingState, DragonStats,
    DragonVisuals, FoodType, JudgeActionTrace, JudgeBundle, JudgeDragonBundle, JudgeHandoverChain,
    JudgePlayerSummary, PlayType, Player, SessionArtifactKind, SessionArtifactRecord, SessionMeta,
    VoteResult, create_session_settings,
};
use std::collections::BTreeMap;
use uuid::Uuid;

pub(crate) fn phase_label(phase: protocol::Phase) -> &'static str {
    match phase {
        protocol::Phase::Lobby => "Phase 0",
        protocol::Phase::Phase0 => "Create",
        protocol::Phase::Phase1 => "Phase 1",
        protocol::Phase::Handover => "Handover",
        protocol::Phase::Phase2 => "Phase 2",
        protocol::Phase::Judge => "Judge",
        protocol::Phase::Voting => "Voting",
        protocol::Phase::End => "Results",
    }
}

pub(crate) fn random_prefixed_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

fn client_dragon_visuals(dragon: &SessionDragon) -> DragonVisuals {
    const PALETTES: [(&str, &str, &str); 4] = [
        ("#88ccff", "#4466aa", "#ffee88"),
        ("#ffaa88", "#cc6644", "#fff0aa"),
        ("#b8f28f", "#4b8f4a", "#f5ffb8"),
        ("#d4b4ff", "#7b5ac7", "#ffd9a8"),
    ];

    let seed = dragon.id.bytes().fold(0_u32, |acc, byte| {
        acc.wrapping_mul(33).wrapping_add(byte as u32)
    });
    let (color_p, color_s, color_a) = PALETTES[(seed as usize) % PALETTES.len()];

    DragonVisuals {
        base: (seed % PALETTES.len() as u32) as i32,
        color_p: color_p.to_string(),
        color_s: color_s.to_string(),
        color_a: color_a.to_string(),
    }
}

fn condition_hint(dragon: &SessionDragon, time: i32) -> String {
    let is_day = (6..=17).contains(&time);

    // Mood / happiness hint
    let mood = if dragon.happiness >= 80 {
        "Your dragon looks cheerful and content."
    } else if dragon.happiness >= 50 {
        "Your dragon seems fairly relaxed."
    } else if dragon.happiness >= 25 {
        "Your dragon is grumpy and restless."
    } else {
        "Your dragon is visibly unhappy — something isn't right."
    };

    // Hunger hint
    let belly = if dragon.hunger >= 80 {
        "Its belly is full."
    } else if dragon.hunger >= 50 {
        "It could probably eat something soon."
    } else if dragon.hunger >= 25 {
        "Its stomach growls audibly."
    } else {
        "It looks famished!"
    };

    // Energy hint
    let energy = if dragon.energy >= 80 {
        "It's brimming with energy."
    } else if dragon.energy >= 50 {
        "It seems moderately alert."
    } else if dragon.energy >= 25 {
        "Its eyes are drooping."
    } else {
        "It can barely keep its eyes open."
    };

    // Subtle time-of-day reactivity hint (does NOT reveal the preference directly)
    let time_hint = match (is_day, dragon.active_time) {
        (true, ActiveTime::Day) | (false, ActiveTime::Night) => {
            "It seems especially lively right now."
        }
        _ => "It seems a bit sluggish at this hour.",
    };

    format!("{mood} {belly} {energy} {time_hint}")
}

fn client_voting_state(
    session: &WorkshopSession,
    current_player_id: &str,
) -> Option<ClientVotingState> {
    let voting = session.voting.as_ref()?;
    let vote_counts = voting.votes_by_player_id.values().fold(
        BTreeMap::<String, i32>::new(),
        |mut counts, dragon_id| {
            *counts.entry(dragon_id.clone()).or_insert(0) += 1;
            counts
        },
    );
    let results = if session.phase == protocol::Phase::End && !voting.eligible_player_ids.is_empty()
    {
        Some(
            session
                .dragons
                .keys()
                .map(|dragon_id| VoteResult {
                    dragon_id: dragon_id.clone(),
                    votes: vote_counts.get(dragon_id).copied().unwrap_or(0),
                })
                .collect(),
        )
    } else {
        None
    };

    Some(ClientVotingState {
        eligible_count: voting.eligible_player_ids.len() as i32,
        submitted_count: voting.votes_by_player_id.len() as i32,
        current_player_vote_dragon_id: voting.votes_by_player_id.get(current_player_id).cloned(),
        results,
    })
}

pub(crate) fn to_client_game_state(
    session: &WorkshopSession,
    current_player_id: &str,
) -> ClientGameState {
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
                    custom_sprites: player.custom_sprites.clone(),
                },
            )
        })
        .collect();

    let dragons = session
        .dragons
        .iter()
        .map(|(dragon_id, dragon)| {
            let hide_owner_identity = session.phase == protocol::Phase::Voting;
            (
                dragon_id.clone(),
                ClientDragon {
                    id: dragon.id.clone(),
                    name: dragon.name.clone(),
                    visuals: client_dragon_visuals(dragon),
                    original_owner_id: if hide_owner_identity {
                        None
                    } else {
                        Some(dragon.original_owner_id.clone())
                    },
                    current_owner_id: if hide_owner_identity {
                        None
                    } else {
                        Some(dragon.current_owner_id.clone())
                    },
                    stats: DragonStats {
                        hunger: dragon.hunger,
                        energy: dragon.energy,
                        happiness: dragon.happiness,
                    },
                    condition_hint: Some(condition_hint(dragon, session.time)),
                    discovery_observations: dragon.discovery_observations.clone(),
                    handover_tags: dragon.handover_tags.clone(),
                    last_action: dragon.last_action,
                    last_emotion: dragon.last_emotion,
                    speech: dragon.speech.clone(),
                    speech_timer: dragon.speech_timer,
                    action_cooldown: dragon.action_cooldown,
                    custom_sprites: session
                        .players
                        .get(&dragon.original_owner_id)
                        .and_then(|player| player.custom_sprites.clone()),
                    judge_observation_score: dragon.judge_observation_score,
                    judge_care_score: dragon.judge_care_score,
                    judge_feedback: dragon.judge_feedback.clone(),
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
            phase_started_at: session.phase_started_at.to_rfc3339(),
            host_player_id: session.host_player_id.clone(),
            settings: create_session_settings(&session.config),
        },
        phase: session.phase,
        time: session.time,
        players,
        dragons,
        current_player_id: Some(current_player_id.to_string()),
        voting: client_voting_state(session, current_player_id),
    }
}

pub(crate) fn phase_step(phase: protocol::Phase) -> u8 {
    match phase {
        protocol::Phase::Lobby => 0,
        protocol::Phase::Phase0 => 1,
        protocol::Phase::Phase1 => 2,
        protocol::Phase::Handover => 3,
        protocol::Phase::Phase2 => 4,
        protocol::Phase::Judge => 5,
        protocol::Phase::Voting => 6,
        protocol::Phase::End => 7,
    }
}

pub(crate) fn parse_player_action(payload: &ActionPayload) -> Option<PlayerAction> {
    let action_type = payload.action_type.trim().to_ascii_lowercase();
    match action_type.as_str() {
        "sleep" => Some(PlayerAction::Sleep),
        "feed" => match payload
            .value
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("meat") => Some(PlayerAction::Feed(FoodType::Meat)),
            Some("fruit") => Some(PlayerAction::Feed(FoodType::Fruit)),
            Some("fish") => Some(PlayerAction::Feed(FoodType::Fish)),
            _ => None,
        },
        "play" => match payload
            .value
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("fetch") => Some(PlayerAction::Play(PlayType::Fetch)),
            Some("puzzle") => Some(PlayerAction::Play(PlayType::Puzzle)),
            Some("music") => Some(PlayerAction::Play(PlayType::Music)),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn build_judge_action_traces(
    session: &WorkshopSession,
    artifacts: &[SessionArtifactRecord],
) -> BTreeMap<String, Vec<JudgeActionTrace>> {
    let mut traces_by_dragon_id = BTreeMap::new();

    for artifact in artifacts {
        if artifact.kind != SessionArtifactKind::ActionProcessed {
            continue;
        }

        let Some(dragon_id) = artifact
            .payload
            .get("dragonId")
            .and_then(|value| value.as_str())
        else {
            continue;
        };

        let player = artifact
            .player_id
            .as_ref()
            .and_then(|player_id| session.players.get(player_id));

        let resulting_stats = match (
            artifact
                .payload
                .get("hunger")
                .and_then(|value| value.as_i64()),
            artifact
                .payload
                .get("energy")
                .and_then(|value| value.as_i64()),
            artifact
                .payload
                .get("happiness")
                .and_then(|value| value.as_i64()),
        ) {
            (Some(hunger), Some(energy), Some(happiness)) => Some(DragonStats {
                hunger: hunger as i32,
                energy: energy as i32,
                happiness: happiness as i32,
            }),
            _ => None,
        };

        let trace = JudgeActionTrace {
            player_id: artifact
                .player_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
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
            was_correct: artifact
                .payload
                .get("wasCorrect")
                .and_then(|value| value.as_bool()),
            block_reason: artifact
                .payload
                .get("blockedReason")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        };

        traces_by_dragon_id
            .entry(dragon_id.to_string())
            .or_insert_with(Vec::new)
            .push(trace);
    }

    traces_by_dragon_id
}

pub(crate) fn build_judge_bundle(
    session: &WorkshopSession,
    artifacts: &[SessionArtifactRecord],
) -> JudgeBundle {
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
                actual_active_time: dragon.active_time,
                actual_day_food: dragon.day_food,
                actual_night_food: dragon.night_food,
                actual_day_play: dragon.day_play,
                actual_night_play: dragon.night_play,
                actual_sleep_rate: dragon.sleep_rate,
                handover_chain: JudgeHandoverChain {
                    creator_instructions: dragon.creator_instructions.clone(),
                    discovery_observations: dragon.discovery_observations.clone(),
                    handover_tags: dragon.handover_tags.clone(),
                },
                phase2_actions: phase2_actions.get(&dragon.id).cloned().unwrap_or_default(),
                total_actions: dragon.total_actions,
                correct_actions: dragon.correct_actions,
                wrong_food_count: dragon.wrong_food_count,
                wrong_play_count: dragon.wrong_play_count,
                cooldown_violations: dragon.cooldown_violations,
                penalty_stacks_at_end: dragon.penalty_stacks,
                phase2_lowest_happiness: dragon.phase2_lowest_happiness,
            })
            .collect(),
    }
}

pub(crate) fn session_config_from_request(
    payload: &protocol::CreateWorkshopRequest,
) -> protocol::WorkshopCreateConfig {
    protocol::WorkshopCreateConfig {
        phase0_minutes: payload.config.phase0_minutes,
        phase1_minutes: payload.config.phase1_minutes,
        phase2_minutes: payload.config.phase2_minutes,
    }
}
