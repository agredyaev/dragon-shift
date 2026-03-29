use chrono::{DateTime, Utc};
use protocol::{
    ClientDragon, ClientGameState, DragonAction, DragonEmotion, JudgeBundle, Phase, Player,
    SessionCommand,
};

use crate::state::{ConnectionStatus, NoticeTone, ShellScreen};

// ---------------------------------------------------------------------------
// View-model row structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LobbyPlayerRow {
    pub name: String,
    pub role_label: &'static str,
    pub readiness_label: &'static str,
    pub connectivity_label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VotingOptionRow {
    pub dragon_id: String,
    pub dragon_name: String,
    pub is_selected: bool,
    pub is_current_players_dragon: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndVoteResultRow {
    pub placement_label: String,
    pub dragon_name: String,
    pub creator_name: String,
    pub votes_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndPlayerScoreRow {
    pub placement_label: String,
    pub player_name: String,
    pub score_label: String,
    pub achievements_label: String,
    pub is_winner: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeBundlePlayerRow {
    pub player_name: String,
    pub score_label: String,
    pub achievements_label: String,
    pub is_top_score: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeBundleDragonRow {
    pub dragon_name: String,
    pub creator_name: String,
    pub caretaker_name: String,
    pub votes_label: String,
    pub actions_label: String,
    pub handover_label: String,
}

// ---------------------------------------------------------------------------
// Pure view-model functions
// ---------------------------------------------------------------------------

pub fn screen_title(screen: &ShellScreen) -> &'static str {
    match screen {
        ShellScreen::Home => "Raise a dragon, hand it off, and jump back into your workshop",
        ShellScreen::Session => "Your Dragon Shift session is live",
    }
}

pub fn connection_status_label(status: &ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::Offline => "Offline",
        ConnectionStatus::Connecting => "Connecting",
        ConnectionStatus::Connected => "Connected",
    }
}

pub fn connection_status_class(status: &ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::Offline => "status-offline",
        ConnectionStatus::Connecting => "status-connecting",
        ConnectionStatus::Connected => "status-connected",
    }
}

pub fn notice_class(tone: NoticeTone) -> &'static str {
    match tone {
        NoticeTone::Info => "notice-info",
        NoticeTone::Success => "notice-success",
        NoticeTone::Warning => "notice-warning",
        NoticeTone::Error => "notice-error",
    }
}

pub fn pending_command_label(command: SessionCommand) -> &'static str {
    match command {
        SessionCommand::StartPhase1 => "Starting Phase 1…",
        SessionCommand::StartHandover => "Starting handover…",
        SessionCommand::SubmitTags => "Saving handover tags…",
        SessionCommand::StartPhase2 => "Starting Phase 2…",
        SessionCommand::EndGame => "Ending workshop…",
        SessionCommand::RevealVotingResults => "Revealing results…",
        SessionCommand::ResetGame => "Resetting workshop…",
        _ => "Sending command…",
    }
}

pub fn parse_tags_input(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn active_player_name(state: &ClientGameState) -> Option<String> {
    let player_id = state.current_player_id.as_ref()?;
    state
        .players
        .get(player_id)
        .map(|player| player.name.clone())
}

pub fn phase_screen_title(phase: Phase) -> &'static str {
    match phase {
        Phase::Lobby => "Workshop lobby",
        Phase::Phase1 => "Discovery round",
        Phase::Handover => "Handover",
        Phase::Phase2 => "Care round",
        Phase::Voting => "Voting",
        Phase::End => "Workshop results",
    }
}

pub fn phase_screen_body(phase: Phase) -> &'static str {
    match phase {
        Phase::Lobby => {
            "Review the roster, make sure everyone is here, and start when the workshop is ready."
        }
        Phase::Phase1 => {
            "Observe your dragon, capture what stands out, and get ready for the handover."
        }
        Phase::Handover => {
            "Write the handover notes your teammate will need for the next care round."
        }
        Phase::Phase2 => {
            "Use the handover notes to guide care actions and keep the dragon thriving."
        }
        Phase::Voting => {
            "Cast a creative vote, track submission progress, and wait for the host to reveal the standings."
        }
        Phase::End => {
            "Review creative awards and final standings, then let the host reset when the workshop is complete."
        }
    }
}

pub fn phase_duration_seconds(state: &ClientGameState) -> Option<i32> {
    state
        .session
        .settings
        .phases
        .get(&state.phase)
        .map(|settings| settings.duration_seconds)
        .filter(|seconds| *seconds > 0)
}

pub fn phase_remaining_seconds(state: &ClientGameState, now: DateTime<Utc>) -> Option<i32> {
    let duration_seconds = phase_duration_seconds(state)?;
    let phase_started_at = DateTime::parse_from_rfc3339(&state.session.phase_started_at)
        .ok()?
        .with_timezone(&Utc);
    let elapsed = (now - phase_started_at).num_seconds().max(0) as i32;
    Some((duration_seconds - elapsed).max(0))
}

pub fn format_remaining_duration(total_seconds: i32) -> String {
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}")
}

pub fn current_player(state: &ClientGameState) -> Option<&Player> {
    let player_id = state.current_player_id.as_ref()?;
    state.players.get(player_id)
}

pub fn current_dragon(state: &ClientGameState) -> Option<&ClientDragon> {
    let player = current_player(state)?;
    let dragon_id = player.current_dragon_id.as_ref()?;
    state.dragons.get(dragon_id)
}

pub fn dragon_action_label(action: DragonAction) -> &'static str {
    match action {
        DragonAction::Feed => "Feed",
        DragonAction::Play => "Play",
        DragonAction::Sleep => "Sleep",
        DragonAction::Idle => "Idle",
    }
}

pub fn dragon_emotion_label(emotion: DragonEmotion) -> &'static str {
    match emotion {
        DragonEmotion::Happy => "Happy",
        DragonEmotion::Angry => "Angry",
        DragonEmotion::Sleepy => "Sleepy",
        DragonEmotion::Neutral => "Neutral",
    }
}

pub fn player_name_by_id(state: &ClientGameState, player_id: Option<&str>) -> String {
    player_id
        .and_then(|player_id| state.players.get(player_id))
        .map(|player| player.name.clone())
        .unwrap_or_else(|| "Unknown".to_string())
}

// ---------------------------------------------------------------------------
// Lobby helpers
// ---------------------------------------------------------------------------

pub fn lobby_player_rows(state: &ClientGameState) -> Vec<LobbyPlayerRow> {
    let mut players = state.players.values().collect::<Vec<_>>();
    players.sort_by(|left, right| {
        right
            .is_host
            .cmp(&left.is_host)
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
            .then_with(|| left.id.cmp(&right.id))
    });

    players
        .into_iter()
        .map(|player| LobbyPlayerRow {
            name: player.name.clone(),
            role_label: if player.is_host { "Host" } else { "Player" },
            readiness_label: if player.is_ready {
                "Ready"
            } else {
                "Setting up"
            },
            connectivity_label: if player.is_connected {
                "Online"
            } else {
                "Offline"
            },
        })
        .collect()
}

pub fn lobby_ready_summary(state: &ClientGameState) -> String {
    let ready_count = state.players.values().filter(|p| p.is_ready).count();
    format!("{ready_count} / {} ready", state.players.len())
}

pub fn lobby_status_copy(state: &ClientGameState) -> String {
    let total_players = state.players.len();
    let offline_players = state.players.values().filter(|p| !p.is_connected).count();

    if total_players == 0 {
        "No players have joined the workshop yet.".to_string()
    } else if total_players == 1 {
        "Single-player workshops can start as soon as the host is ready.".to_string()
    } else if offline_players == 0 {
        "All players are online. The host can start once the lobby is ready.".to_string()
    } else {
        format!(
            "{offline_players} player(s) are currently offline and may need to reconnect before start."
        )
    }
}

// ---------------------------------------------------------------------------
// Phase 1 helpers
// ---------------------------------------------------------------------------

pub fn phase1_focus_title(state: &ClientGameState) -> String {
    current_dragon(state)
        .map(|dragon| format!("Meet {}", dragon.name))
        .unwrap_or_else(|| "Awaiting assigned dragon".to_string())
}

pub fn phase1_focus_body(state: &ClientGameState) -> String {
    let Some(dragon) = current_dragon(state) else {
        return "Phase 1 will unlock once the session assigns you a dragon to observe.".to_string();
    };

    let speech = dragon
        .speech
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("No direct speech hint yet.");
    let condition = dragon
        .condition_hint
        .as_deref()
        .filter(|h| !h.trim().is_empty())
        .unwrap_or("Watch for timing changes between food, play, and sleep.");

    format!("{speech} {condition}")
}

pub fn phase1_observation_summary(state: &ClientGameState) -> String {
    let Some(dragon) = current_dragon(state) else {
        return "No discovery notes saved yet.".to_string();
    };

    let count = dragon.discovery_observations.len();
    if count == 0 {
        "No discovery notes saved yet.".to_string()
    } else {
        format!("{count} discovery note(s) captured for handover.")
    }
}

// ---------------------------------------------------------------------------
// Handover helpers
// ---------------------------------------------------------------------------

pub fn handover_focus_title(state: &ClientGameState) -> String {
    current_dragon(state)
        .map(|dragon| format!("Handover for {}", dragon.name))
        .unwrap_or_else(|| "Awaiting dragon handover".to_string())
}

pub fn handover_saved_tags(state: &ClientGameState) -> Vec<String> {
    current_dragon(state)
        .map(|dragon| dragon.handover_tags.clone())
        .unwrap_or_default()
}

pub fn handover_saved_summary(state: &ClientGameState) -> String {
    let saved_count = handover_saved_tags(state).len();
    format!("{saved_count} / 3 handover rules saved")
}

pub fn handover_status_copy(state: &ClientGameState) -> String {
    let saved_count = handover_saved_tags(state).len();
    match saved_count {
        0 => "Write three concrete care rules so the next player can continue without re-discovering everything.".to_string(),
        1 | 2 => format!(
            "Add {} more rule(s) to complete the handover bundle.",
            3 - saved_count
        ),
        _ => "Handover bundle is complete. Host can move the workshop into Phase 2 once everyone finishes.".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Phase 2 helpers
// ---------------------------------------------------------------------------

pub fn phase2_focus_title(state: &ClientGameState) -> String {
    current_dragon(state)
        .map(|dragon| format!("Phase 2 care for {}", dragon.name))
        .unwrap_or_else(|| "Awaiting Phase 2 dragon".to_string())
}

pub fn phase2_creator_label(state: &ClientGameState) -> String {
    let Some(dragon) = current_dragon(state) else {
        return "Creator: Unknown".to_string();
    };

    format!(
        "Creator: {}",
        player_name_by_id(state, dragon.original_owner_id.as_deref())
    )
}

pub fn phase2_handover_summary(state: &ClientGameState) -> String {
    let Some(dragon) = current_dragon(state) else {
        return "No handover notes yet.".to_string();
    };

    if dragon.handover_tags.is_empty() {
        "No handover notes yet.".to_string()
    } else {
        format!(
            "{} handover note(s) available from the previous caretaker.",
            dragon.handover_tags.len()
        )
    }
}

pub fn phase2_care_copy(state: &ClientGameState) -> String {
    let Some(dragon) = current_dragon(state) else {
        return "Phase 2 will begin once a dragon is assigned.".to_string();
    };

    let condition = dragon
        .condition_hint
        .as_deref()
        .filter(|h| !h.trim().is_empty())
        .unwrap_or("Expect faster decay in Phase 2 and react before the bars collapse.");

    format!("{condition} Phase 2 decay is stronger, so adjust faster than in discovery.")
}

// ---------------------------------------------------------------------------
// Voting helpers
// ---------------------------------------------------------------------------

pub fn voting_progress_label(state: &ClientGameState) -> String {
    let Some(voting) = state.voting.as_ref() else {
        return "0 / 0 votes submitted".to_string();
    };

    format!(
        "{} / {} votes submitted",
        voting.submitted_count, voting.eligible_count
    )
}

pub fn voting_status_copy(state: &ClientGameState) -> String {
    let Some(voting) = state.voting.as_ref() else {
        return "Voting has not started yet.".to_string();
    };

    if voting.current_player_vote_dragon_id.is_some() {
        if voting.submitted_count >= voting.eligible_count {
            "Vote submitted. Host can reveal the results now.".to_string()
        } else {
            "Vote submitted. Waiting for the remaining players before reveal.".to_string()
        }
    } else {
        "Choose the most creative dragon that is not currently assigned to you.".to_string()
    }
}

pub fn voting_reveal_ready(state: &ClientGameState) -> bool {
    state
        .voting
        .as_ref()
        .map(|voting| voting.eligible_count > 0 && voting.submitted_count >= voting.eligible_count)
        .unwrap_or(false)
}

pub fn voting_option_rows(state: &ClientGameState) -> Vec<VotingOptionRow> {
    let current_player_dragon_id =
        current_player(state).and_then(|player| player.current_dragon_id.as_deref());
    let current_vote_dragon_id = state
        .voting
        .as_ref()
        .and_then(|voting| voting.current_player_vote_dragon_id.as_deref());
    let mut dragons = state.dragons.values().collect::<Vec<_>>();
    dragons.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });

    dragons
        .into_iter()
        .map(|dragon| VotingOptionRow {
            dragon_id: dragon.id.clone(),
            dragon_name: dragon.name.clone(),
            is_selected: current_vote_dragon_id == Some(dragon.id.as_str()),
            is_current_players_dragon: current_player_dragon_id == Some(dragon.id.as_str()),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// End-game helpers
// ---------------------------------------------------------------------------

pub fn end_vote_result_rows(state: &ClientGameState) -> Vec<EndVoteResultRow> {
    let Some(results) = state
        .voting
        .as_ref()
        .and_then(|voting| voting.results.as_ref())
    else {
        return Vec::new();
    };
    let mut results = results.iter().collect::<Vec<_>>();
    results.sort_by(|left, right| {
        right
            .votes
            .cmp(&left.votes)
            .then_with(|| {
                let left_name = state
                    .dragons
                    .get(&left.dragon_id)
                    .map(|d| d.name.to_ascii_lowercase())
                    .unwrap_or_else(|| left.dragon_id.to_ascii_lowercase());
                let right_name = state
                    .dragons
                    .get(&right.dragon_id)
                    .map(|d| d.name.to_ascii_lowercase())
                    .unwrap_or_else(|| right.dragon_id.to_ascii_lowercase());
                left_name.cmp(&right_name)
            })
            .then_with(|| left.dragon_id.cmp(&right.dragon_id))
    });

    results
        .into_iter()
        .enumerate()
        .map(|(index, result)| {
            let dragon = state.dragons.get(&result.dragon_id);
            EndVoteResultRow {
                placement_label: format!("#{} Creative pick", index + 1),
                dragon_name: dragon
                    .map(|d| d.name.clone())
                    .unwrap_or_else(|| "Unknown dragon".to_string()),
                creator_name: player_name_by_id(
                    state,
                    dragon.and_then(|d| d.original_owner_id.as_deref()),
                ),
                votes_label: if result.votes == 1 {
                    "1 vote".to_string()
                } else {
                    format!("{} votes", result.votes)
                },
            }
        })
        .collect()
}

pub fn end_results_status_copy(state: &ClientGameState) -> String {
    let rows = end_vote_result_rows(state);
    let Some(top_result) = rows.first() else {
        return "Results will appear once the host reveals the creative vote.".to_string();
    };

    format!(
        "Creative awards locked in. {} leads the reveal and the final standings are ready.",
        top_result.dragon_name
    )
}

pub fn end_player_score_rows(state: &ClientGameState) -> Vec<EndPlayerScoreRow> {
    let mut players = state.players.values().collect::<Vec<_>>();
    players.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.achievements.len().cmp(&left.achievements.len()))
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
            .then_with(|| left.id.cmp(&right.id))
    });

    players
        .into_iter()
        .enumerate()
        .map(|(index, player)| EndPlayerScoreRow {
            placement_label: format!("#{}", index + 1),
            player_name: player.name.clone(),
            score_label: format!("{} pts", player.score),
            achievements_label: if player.achievements.is_empty() {
                "No achievements yet".to_string()
            } else {
                format!("{} achievement(s)", player.achievements.len())
            },
            is_winner: index == 0,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Judge bundle helpers
// ---------------------------------------------------------------------------

pub fn judge_bundle_summary(bundle: &JudgeBundle) -> String {
    format!(
        "Artifacts: {} - Dragons: {} - Generated: {}",
        bundle.artifact_count,
        bundle.dragons.len(),
        bundle.generated_at
    )
}

pub fn judge_bundle_player_rows(bundle: &JudgeBundle) -> Vec<JudgeBundlePlayerRow> {
    let mut players = bundle.players.iter().collect::<Vec<_>>();
    players.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.achievements.len().cmp(&left.achievements.len()))
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
            .then_with(|| left.player_id.cmp(&right.player_id))
    });

    players
        .into_iter()
        .enumerate()
        .map(|(index, player)| JudgeBundlePlayerRow {
            player_name: player.name.clone(),
            score_label: format!("{} pts", player.score),
            achievements_label: if player.achievements.is_empty() {
                "No achievements yet".to_string()
            } else {
                format!("{} achievement(s)", player.achievements.len())
            },
            is_top_score: index == 0,
        })
        .collect()
}

pub fn judge_bundle_dragon_rows(bundle: &JudgeBundle) -> Vec<JudgeBundleDragonRow> {
    let mut dragons = bundle.dragons.iter().collect::<Vec<_>>();
    dragons.sort_by(|left, right| {
        right
            .creative_vote_count
            .cmp(&left.creative_vote_count)
            .then_with(|| {
                left.dragon_name
                    .to_ascii_lowercase()
                    .cmp(&right.dragon_name.to_ascii_lowercase())
            })
            .then_with(|| left.dragon_id.cmp(&right.dragon_id))
    });

    dragons
        .into_iter()
        .map(|dragon| JudgeBundleDragonRow {
            dragon_name: dragon.dragon_name.clone(),
            creator_name: dragon.creator_name.clone(),
            caretaker_name: dragon.current_owner_name.clone(),
            votes_label: format!("{} creative vote(s)", dragon.creative_vote_count),
            actions_label: format!("{} phase 2 action(s) captured", dragon.phase2_actions.len()),
            handover_label: if dragon.handover_chain.handover_tags.is_empty() {
                "No handover tags captured".to_string()
            } else {
                format!(
                    "{} handover tag(s) captured",
                    dragon.handover_chain.handover_tags.len()
                )
            },
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use super::*;
    use protocol::{
        ClientDragon, ClientGameState, CoordinatorType, DragonAction, DragonEmotion, Phase, Player,
        SessionMeta, WorkshopJoinSuccess, create_default_session_settings,
    };
    use std::collections::BTreeMap;

    pub fn mock_join_success() -> WorkshopJoinSuccess {
        let mut players = BTreeMap::new();
        players.insert(
            "player-1".to_string(),
            Player {
                id: "player-1".to_string(),
                name: "Alice".to_string(),
                is_host: true,
                score: 0,
                current_dragon_id: None,
                achievements: Vec::new(),
                is_ready: false,
                is_connected: true,
                pet_description: Some("Alice's workshop dragon".to_string()),
            },
        );

        WorkshopJoinSuccess {
            ok: true,
            session_code: "123456".to_string(),
            player_id: "player-1".to_string(),
            reconnect_token: "reconnect-1".to_string(),
            coordinator_type: CoordinatorType::Rust,
            state: ClientGameState {
                session: SessionMeta {
                    id: "session-1".to_string(),
                    code: "123456".to_string(),
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                    updated_at: "2026-01-01T00:00:00Z".to_string(),
                    phase_started_at: "2026-01-01T00:00:00Z".to_string(),
                    host_player_id: Some("player-1".to_string()),
                    settings: create_default_session_settings(),
                },
                phase: Phase::Lobby,
                time: 8,
                players,
                dragons: BTreeMap::new(),
                current_player_id: Some("player-1".to_string()),
                voting: None,
            },
        }
    }

    fn mock_phase1_state() -> ClientGameState {
        let mut state = mock_join_success().state;
        state.phase = Phase::Phase1;
        state
            .players
            .get_mut("player-1")
            .expect("player-1")
            .current_dragon_id = Some("dragon-1".to_string());
        state.dragons.insert(
            "dragon-1".to_string(),
            ClientDragon {
                id: "dragon-1".to_string(),
                name: "Comet".to_string(),
                visuals: protocol::DragonVisuals {
                    base: 1,
                    color_p: "#88ccff".to_string(),
                    color_s: "#4466aa".to_string(),
                    color_a: "#ffee88".to_string(),
                },
                original_owner_id: Some("player-1".to_string()),
                current_owner_id: Some("player-1".to_string()),
                stats: protocol::DragonStats {
                    hunger: 72,
                    energy: 55,
                    happiness: 81,
                },
                condition_hint: Some("Gets restless after long idle stretches.".to_string()),
                discovery_observations: vec!["Loves food at dusk".to_string()],
                handover_tags: Vec::new(),
                last_action: DragonAction::Feed,
                last_emotion: DragonEmotion::Happy,
                speech: Some("The snack worked.".to_string()),
                speech_timer: 2,
                action_cooldown: 0,
                custom_sprites: None,
            },
        );
        state
    }

    fn mock_handover_state() -> ClientGameState {
        let mut state = mock_phase1_state();
        state.phase = Phase::Handover;
        state
            .dragons
            .get_mut("dragon-1")
            .expect("dragon-1")
            .handover_tags = vec![
            "Feed at dusk".to_string(),
            "Avoid long idle gaps".to_string(),
        ];
        state
    }

    fn mock_phase2_state() -> ClientGameState {
        let mut state = mock_handover_state();
        state.phase = Phase::Phase2;
        state
    }

    fn mock_voting_state() -> ClientGameState {
        let mut state = mock_phase2_state();
        state.phase = Phase::Voting;
        state.players.insert(
            "player-2".to_string(),
            Player {
                id: "player-2".to_string(),
                name: "Bob".to_string(),
                is_host: false,
                score: 0,
                current_dragon_id: Some("dragon-2".to_string()),
                achievements: Vec::new(),
                is_ready: true,
                is_connected: true,
                pet_description: Some("Bob's workshop dragon".to_string()),
            },
        );
        state.dragons.insert(
            "dragon-2".to_string(),
            ClientDragon {
                id: "dragon-2".to_string(),
                name: "Nova".to_string(),
                visuals: protocol::DragonVisuals {
                    base: 2,
                    color_p: "#ffaa88".to_string(),
                    color_s: "#cc6644".to_string(),
                    color_a: "#fff0aa".to_string(),
                },
                original_owner_id: Some("player-2".to_string()),
                current_owner_id: Some("player-2".to_string()),
                stats: protocol::DragonStats {
                    hunger: 61,
                    energy: 63,
                    happiness: 77,
                },
                condition_hint: Some("Responds well to music at night.".to_string()),
                discovery_observations: vec!["Settles quickly after music".to_string()],
                handover_tags: vec!["Start with music".to_string()],
                last_action: DragonAction::Play,
                last_emotion: DragonEmotion::Neutral,
                speech: Some("A calmer rhythm helps.".to_string()),
                speech_timer: 1,
                action_cooldown: 0,
                custom_sprites: None,
            },
        );
        state.voting = Some(protocol::ClientVotingState {
            eligible_count: 2,
            submitted_count: 1,
            current_player_vote_dragon_id: Some("dragon-2".to_string()),
            results: None,
        });
        state
    }

    fn mock_end_state() -> ClientGameState {
        let mut state = mock_voting_state();
        state.phase = Phase::End;
        state.players.get_mut("player-1").expect("player-1").score = 12;
        state
            .players
            .get_mut("player-1")
            .expect("player-1")
            .achievements = vec!["careful_observer".to_string()];
        state.players.get_mut("player-2").expect("player-2").score = 18;
        state
            .players
            .get_mut("player-2")
            .expect("player-2")
            .achievements = vec!["creative_pick".to_string(), "steady_hands".to_string()];
        state.voting = Some(protocol::ClientVotingState {
            eligible_count: 2,
            submitted_count: 2,
            current_player_vote_dragon_id: Some("dragon-2".to_string()),
            results: Some(vec![
                protocol::VoteResult {
                    dragon_id: "dragon-2".to_string(),
                    votes: 2,
                },
                protocol::VoteResult {
                    dragon_id: "dragon-1".to_string(),
                    votes: 1,
                },
            ]),
        });
        state
    }

    pub fn mock_judge_bundle() -> JudgeBundle {
        JudgeBundle {
            session_id: "session-1".to_string(),
            session_code: "123456".to_string(),
            current_phase: Phase::End,
            generated_at: "2026-01-01T12:00:00Z".to_string(),
            artifact_count: 6,
            players: vec![
                protocol::JudgePlayerSummary {
                    player_id: "player-1".to_string(),
                    name: "Alice".to_string(),
                    score: 12,
                    achievements: vec!["careful_observer".to_string()],
                },
                protocol::JudgePlayerSummary {
                    player_id: "player-2".to_string(),
                    name: "Bob".to_string(),
                    score: 18,
                    achievements: vec!["creative_pick".to_string(), "steady_hands".to_string()],
                },
            ],
            dragons: vec![
                protocol::JudgeDragonBundle {
                    dragon_id: "dragon-2".to_string(),
                    dragon_name: "Nova".to_string(),
                    creator_player_id: "player-2".to_string(),
                    creator_name: "Bob".to_string(),
                    current_owner_id: "player-2".to_string(),
                    current_owner_name: "Bob".to_string(),
                    creative_vote_count: 2,
                    final_stats: protocol::DragonStats {
                        hunger: 61,
                        energy: 63,
                        happiness: 77,
                    },
                    handover_chain: protocol::JudgeHandoverChain {
                        creator_instructions: "Start with music".to_string(),
                        discovery_observations: vec!["Settles quickly after music".to_string()],
                        handover_tags: vec!["Start with music".to_string()],
                    },
                    phase2_actions: vec![protocol::JudgeActionTrace {
                        player_id: "player-2".to_string(),
                        player_name: "Bob".to_string(),
                        phase: Phase::Phase2,
                        action_type: "play".to_string(),
                        action_value: None,
                        created_at: "2026-01-01T10:00:00Z".to_string(),
                        resulting_stats: None,
                    }],
                },
                protocol::JudgeDragonBundle {
                    dragon_id: "dragon-1".to_string(),
                    dragon_name: "Comet".to_string(),
                    creator_player_id: "player-1".to_string(),
                    creator_name: "Alice".to_string(),
                    current_owner_id: "player-1".to_string(),
                    current_owner_name: "Alice".to_string(),
                    creative_vote_count: 1,
                    final_stats: protocol::DragonStats {
                        hunger: 72,
                        energy: 55,
                        happiness: 81,
                    },
                    handover_chain: protocol::JudgeHandoverChain {
                        creator_instructions: "Feed at dusk".to_string(),
                        discovery_observations: vec!["Loves food at dusk".to_string()],
                        handover_tags: vec![
                            "Feed at dusk".to_string(),
                            "Avoid long idle gaps".to_string(),
                        ],
                    },
                    phase2_actions: vec![],
                },
            ],
        }
    }

    #[test]
    fn shell_labels_match_bootstrap_state() {
        assert_eq!(
            screen_title(&ShellScreen::Home),
            "Raise a dragon, hand it off, and jump back into your workshop"
        );
        assert_eq!(
            connection_status_label(&ConnectionStatus::Offline),
            "Offline"
        );
        assert_eq!(
            connection_status_class(&ConnectionStatus::Offline),
            "status-offline"
        );
    }

    #[test]
    fn connection_status_variants_have_distinct_labels_and_classes() {
        assert_eq!(
            connection_status_label(&ConnectionStatus::Connecting),
            "Connecting"
        );
        assert_eq!(
            connection_status_class(&ConnectionStatus::Connecting),
            "status-connecting"
        );
        assert_eq!(
            connection_status_label(&ConnectionStatus::Connected),
            "Connected"
        );
        assert_eq!(
            connection_status_class(&ConnectionStatus::Connected),
            "status-connected"
        );
    }

    #[test]
    fn parse_tags_input_trims_and_filters_empty_segments() {
        let tags = parse_tags_input(" one, two ,, three , ");
        assert_eq!(tags, vec!["one", "two", "three"]);
    }

    #[test]
    fn phase_screen_copy_matches_lobby_and_voting_states() {
        assert_eq!(phase_screen_title(Phase::Lobby), "Workshop lobby");
        assert_eq!(phase_screen_title(Phase::Voting), "Voting");
        assert_eq!(
            phase_screen_body(Phase::Lobby),
            "Review the roster, make sure everyone is here, and start when the workshop is ready."
        );
    }

    #[test]
    fn lobby_player_rows_prioritize_host_and_map_labels() {
        let mut state = mock_join_success().state;
        state.players.insert(
            "player-2".to_string(),
            Player {
                id: "player-2".to_string(),
                name: "Bob".to_string(),
                is_host: false,
                score: 0,
                current_dragon_id: None,
                achievements: Vec::new(),
                is_ready: true,
                is_connected: false,
                pet_description: Some("Bob's workshop dragon".to_string()),
            },
        );

        let rows = lobby_player_rows(&state);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Alice");
        assert_eq!(rows[0].role_label, "Host");
        assert_eq!(rows[0].readiness_label, "Setting up");
        assert_eq!(rows[1].name, "Bob");
        assert_eq!(rows[1].connectivity_label, "Offline");
        assert_eq!(rows[1].readiness_label, "Ready");
    }

    #[test]
    fn lobby_status_copy_handles_empty_and_single_player_states() {
        let mut empty_state = mock_join_success().state;
        empty_state.players.clear();
        assert_eq!(
            lobby_status_copy(&empty_state),
            "No players have joined the workshop yet."
        );

        let single_player_state = mock_join_success().state;
        assert_eq!(
            lobby_status_copy(&single_player_state),
            "Single-player workshops can start as soon as the host is ready."
        );
        assert_eq!(lobby_ready_summary(&single_player_state), "0 / 1 ready");
    }

    #[test]
    fn phase1_focus_helpers_use_current_dragon_context() {
        let state = mock_phase1_state();

        assert_eq!(phase1_focus_title(&state), "Meet Comet");
        assert_eq!(
            phase1_observation_summary(&state),
            "1 discovery note(s) captured for handover."
        );
        assert_eq!(
            dragon_emotion_label(current_dragon(&state).expect("dragon").last_emotion),
            "Happy"
        );
        assert_eq!(
            dragon_action_label(current_dragon(&state).expect("dragon").last_action),
            "Feed"
        );
        assert!(phase1_focus_body(&state).contains("The snack worked."));
    }

    #[test]
    fn phase1_focus_helpers_fall_back_when_player_has_no_dragon() {
        let state = mock_join_success().state;

        assert_eq!(phase1_focus_title(&state), "Awaiting assigned dragon");
        assert_eq!(
            phase1_observation_summary(&state),
            "No discovery notes saved yet."
        );
    }

    #[test]
    fn handover_helpers_report_saved_rules_and_remaining_work() {
        let state = mock_handover_state();

        assert_eq!(handover_focus_title(&state), "Handover for Comet");
        assert_eq!(handover_saved_summary(&state), "2 / 3 handover rules saved");
        assert_eq!(handover_saved_tags(&state).len(), 2);
        assert_eq!(
            handover_status_copy(&state),
            "Add 1 more rule(s) to complete the handover bundle."
        );
    }

    #[test]
    fn handover_helpers_handle_empty_bundle() {
        let mut state = mock_phase1_state();
        state.phase = Phase::Handover;

        assert_eq!(handover_saved_summary(&state), "0 / 3 handover rules saved");
        assert!(handover_status_copy(&state).contains("Write three concrete care rules"));
    }

    #[test]
    fn phase2_helpers_expose_creator_and_handover_context() {
        let state = mock_phase2_state();

        assert_eq!(phase2_focus_title(&state), "Phase 2 care for Comet");
        assert_eq!(phase2_creator_label(&state), "Creator: Alice");
        assert_eq!(
            phase2_handover_summary(&state),
            "2 handover note(s) available from the previous caretaker."
        );
        assert!(phase2_care_copy(&state).contains("Phase 2 decay is stronger"));
    }

    #[test]
    fn phase2_helpers_fall_back_without_handover_notes() {
        let mut state = mock_phase1_state();
        state.phase = Phase::Phase2;

        assert_eq!(phase2_handover_summary(&state), "No handover notes yet.");
        assert_eq!(phase2_creator_label(&state), "Creator: Alice");
    }

    #[test]
    fn voting_helpers_expose_progress_selection_and_self_vote_block() {
        let state = mock_voting_state();
        let rows = voting_option_rows(&state);

        assert_eq!(voting_progress_label(&state), "1 / 2 votes submitted");
        assert_eq!(
            voting_status_copy(&state),
            "Vote submitted. Waiting for the remaining players before reveal."
        );
        assert!(!voting_reveal_ready(&state));
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .any(|row| row.dragon_name == "Comet" && row.is_current_players_dragon)
        );
        assert!(
            rows.iter()
                .any(|row| row.dragon_name == "Nova" && row.is_selected)
        );
    }

    #[test]
    fn voting_helpers_mark_reveal_ready_when_all_votes_are_in() {
        let mut state = mock_voting_state();
        state.voting = Some(protocol::ClientVotingState {
            eligible_count: 2,
            submitted_count: 2,
            current_player_vote_dragon_id: Some("dragon-2".to_string()),
            results: None,
        });

        assert!(voting_reveal_ready(&state));
        assert_eq!(
            voting_status_copy(&state),
            "Vote submitted. Host can reveal the results now."
        );
    }

    #[test]
    fn end_helpers_rank_creative_results_and_final_scores() {
        let state = mock_end_state();
        let vote_rows = end_vote_result_rows(&state);
        let score_rows = end_player_score_rows(&state);

        assert_eq!(
            end_results_status_copy(&state),
            "Creative awards locked in. Nova leads the reveal and the final standings are ready."
        );
        assert_eq!(vote_rows.len(), 2);
        assert_eq!(vote_rows[0].dragon_name, "Nova");
        assert_eq!(vote_rows[0].creator_name, "Bob");
        assert_eq!(vote_rows[0].votes_label, "2 votes");
        assert_eq!(score_rows[0].player_name, "Bob");
        assert_eq!(score_rows[0].score_label, "18 pts");
        assert!(score_rows[0].is_winner);
    }

    #[test]
    fn end_helpers_fall_back_before_results_are_revealed() {
        let mut state = mock_voting_state();
        state.phase = Phase::End;
        state.players.get_mut("player-1").expect("player-1").score = 4;
        state.players.get_mut("player-2").expect("player-2").score = 3;

        assert_eq!(
            end_results_status_copy(&state),
            "Results will appear once the host reveals the creative vote."
        );
        assert!(end_vote_result_rows(&state).is_empty());
        assert_eq!(end_player_score_rows(&state)[0].player_name, "Alice");
    }

    #[test]
    fn judge_bundle_helpers_summarize_players_and_dragons() {
        let bundle = mock_judge_bundle();
        let players = judge_bundle_player_rows(&bundle);
        let dragons = judge_bundle_dragon_rows(&bundle);

        assert_eq!(
            judge_bundle_summary(&bundle),
            "Artifacts: 6 - Dragons: 2 - Generated: 2026-01-01T12:00:00Z"
        );
        assert_eq!(players[0].player_name, "Bob");
        assert_eq!(players[0].score_label, "18 pts");
        assert!(players[0].is_top_score);
        assert_eq!(dragons[0].dragon_name, "Nova");
        assert_eq!(dragons[0].votes_label, "2 creative vote(s)");
        assert_eq!(dragons[0].actions_label, "1 phase 2 action(s) captured");
    }

    // -----------------------------------------------------------------------
    // Computation budget tests
    //
    // These ensure render-path helper functions stay fast even with large
    // workshops. Each test scales mock data to 50 players / 50 dragons and
    // asserts the computation completes within a generous time budget.
    // If a test fails, a render-path regression was introduced.
    // -----------------------------------------------------------------------

    const BUDGET_PLAYERS: usize = 50;
    const BUDGET_BUDGET_MS: u128 = 50; // 50 ms — generous headroom over typical <1 ms

    fn scaled_end_state() -> ClientGameState {
        let mut state = mock_end_state();
        state.phase = Phase::End;
        for i in 3..=BUDGET_PLAYERS {
            let pid = format!("player-{i}");
            let did = format!("dragon-{i}");
            state.players.insert(
                pid.clone(),
                Player {
                    id: pid.clone(),
                    name: format!("Player{i}"),
                    is_host: false,
                    score: (i * 3) as i32,
                    current_dragon_id: Some(did.clone()),
                    achievements: vec![format!("badge_{i}")],
                    is_ready: i % 2 == 0,
                    is_connected: i % 3 != 0,
                    pet_description: Some(format!("Dragon description {i}")),
                },
            );
            state.dragons.insert(
                did.clone(),
                ClientDragon {
                    id: did.clone(),
                    name: format!("Dragon{i}"),
                    visuals: protocol::DragonVisuals {
                        base: (i % 4) as i32,
                        color_p: "#aabbcc".to_string(),
                        color_s: "#112233".to_string(),
                        color_a: "#ddeeff".to_string(),
                    },
                    original_owner_id: Some(pid.clone()),
                    current_owner_id: Some(pid.clone()),
                    stats: protocol::DragonStats {
                        hunger: 50 + (i % 30) as i32,
                        energy: 40 + (i % 40) as i32,
                        happiness: 60 + (i % 20) as i32,
                    },
                    condition_hint: Some(format!("Condition hint {i}")),
                    discovery_observations: vec![format!("Observation {i}")],
                    handover_tags: vec![format!("Tag {i}a"), format!("Tag {i}b")],
                    last_action: DragonAction::Feed,
                    last_emotion: DragonEmotion::Happy,
                    speech: Some(format!("Speech {i}")),
                    speech_timer: 1,
                    action_cooldown: 0,
                    custom_sprites: None,
                },
            );
        }
        // Set up voting results for all dragons
        let mut results = Vec::new();
        for i in 1..=BUDGET_PLAYERS {
            results.push(protocol::VoteResult {
                dragon_id: format!("dragon-{i}"),
                votes: (BUDGET_PLAYERS - i) as i32,
            });
        }
        state.voting = Some(protocol::ClientVotingState {
            eligible_count: BUDGET_PLAYERS as i32,
            submitted_count: BUDGET_PLAYERS as i32,
            current_player_vote_dragon_id: Some("dragon-2".to_string()),
            results: Some(results),
        });
        state
    }

    fn scaled_judge_bundle() -> JudgeBundle {
        let mut bundle = mock_judge_bundle();
        for i in 3..=BUDGET_PLAYERS {
            bundle.players.push(protocol::JudgePlayerSummary {
                player_id: format!("player-{i}"),
                name: format!("Player{i}"),
                score: (i * 3) as i32,
                achievements: vec![format!("badge_{i}")],
            });
            bundle.dragons.push(protocol::JudgeDragonBundle {
                dragon_id: format!("dragon-{i}"),
                dragon_name: format!("Dragon{i}"),
                creator_player_id: format!("player-{i}"),
                creator_name: format!("Player{i}"),
                current_owner_id: format!("player-{i}"),
                current_owner_name: format!("Player{i}"),
                creative_vote_count: (BUDGET_PLAYERS - i) as i32,
                final_stats: protocol::DragonStats {
                    hunger: 50,
                    energy: 50,
                    happiness: 50,
                },
                handover_chain: protocol::JudgeHandoverChain {
                    creator_instructions: format!("Instructions {i}"),
                    discovery_observations: vec![format!("Obs {i}")],
                    handover_tags: vec![format!("Tag {i}")],
                },
                phase2_actions: vec![protocol::JudgeActionTrace {
                    player_id: format!("player-{i}"),
                    player_name: format!("Player{i}"),
                    phase: Phase::Phase2,
                    action_type: "feed".to_string(),
                    action_value: None,
                    created_at: "2026-01-01T10:00:00Z".to_string(),
                    resulting_stats: None,
                }],
            });
        }
        bundle
    }

    #[test]
    fn budget_lobby_player_rows_50_players() {
        let mut state = scaled_end_state();
        state.phase = Phase::Lobby;
        let start = std::time::Instant::now();
        let rows = lobby_player_rows(&state);
        let elapsed = start.elapsed().as_millis();
        assert_eq!(rows.len(), BUDGET_PLAYERS);
        assert!(
            elapsed < BUDGET_BUDGET_MS,
            "lobby_player_rows took {elapsed}ms (budget: {BUDGET_BUDGET_MS}ms)"
        );
    }

    #[test]
    fn budget_voting_option_rows_50_dragons() {
        let state = scaled_end_state();
        let start = std::time::Instant::now();
        let rows = voting_option_rows(&state);
        let elapsed = start.elapsed().as_millis();
        assert_eq!(rows.len(), BUDGET_PLAYERS);
        assert!(
            elapsed < BUDGET_BUDGET_MS,
            "voting_option_rows took {elapsed}ms (budget: {BUDGET_BUDGET_MS}ms)"
        );
    }

    #[test]
    fn budget_end_vote_result_rows_50_results() {
        let state = scaled_end_state();
        let start = std::time::Instant::now();
        let rows = end_vote_result_rows(&state);
        let elapsed = start.elapsed().as_millis();
        assert_eq!(rows.len(), BUDGET_PLAYERS);
        assert!(
            elapsed < BUDGET_BUDGET_MS,
            "end_vote_result_rows took {elapsed}ms (budget: {BUDGET_BUDGET_MS}ms)"
        );
    }

    #[test]
    fn budget_end_player_score_rows_50_players() {
        let state = scaled_end_state();
        let start = std::time::Instant::now();
        let rows = end_player_score_rows(&state);
        let elapsed = start.elapsed().as_millis();
        assert_eq!(rows.len(), BUDGET_PLAYERS);
        assert!(
            elapsed < BUDGET_BUDGET_MS,
            "end_player_score_rows took {elapsed}ms (budget: {BUDGET_BUDGET_MS}ms)"
        );
    }

    #[test]
    fn budget_judge_bundle_rows_50_entries() {
        let bundle = scaled_judge_bundle();
        let start = std::time::Instant::now();
        let player_rows = judge_bundle_player_rows(&bundle);
        let dragon_rows = judge_bundle_dragon_rows(&bundle);
        let elapsed = start.elapsed().as_millis();
        assert_eq!(player_rows.len(), BUDGET_PLAYERS);
        assert_eq!(dragon_rows.len(), BUDGET_PLAYERS);
        assert!(
            elapsed < BUDGET_BUDGET_MS,
            "judge_bundle_*_rows took {elapsed}ms (budget: {BUDGET_BUDGET_MS}ms)"
        );
    }
}
