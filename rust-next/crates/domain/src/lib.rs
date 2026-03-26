use chrono::{DateTime, Utc};
use protocol::{ActiveTime, DragonAction, DragonEmotion, FoodType, Phase, PlayType};
use std::collections::BTreeMap;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionCode(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VotingState {
    pub eligible_player_ids: Vec<String>,
    pub votes_by_player_id: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phase1Assignment {
    pub player_id: String,
    pub dragon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phase2TransitionResult {
    pub auto_filled_players: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDragon {
    pub id: String,
    pub name: String,
    pub original_owner_id: String,
    pub current_owner_id: String,
    pub creator_instructions: String,
    pub active_time: ActiveTime,
    pub day_food: FoodType,
    pub night_food: FoodType,
    pub day_play: PlayType,
    pub night_play: PlayType,
    pub sleep_rate: i32,
    pub hunger: i32,
    pub energy: i32,
    pub happiness: i32,
    pub discovery_observations: Vec<String>,
    pub handover_tags: Vec<String>,
    pub last_action: DragonAction,
    pub last_emotion: DragonEmotion,
    pub speech: Option<String>,
    pub speech_timer: i32,
    pub action_cooldown: i32,
    pub sleep_shield_ticks: i32,
    pub food_tries: i32,
    pub play_tries: i32,
    pub high_happiness_ticks: i32,
    pub phase2_ticks: i32,
    pub phase2_lowest_happiness: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerAction {
    Feed(FoodType),
    Play(PlayType),
    Sleep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionOutcome {
    Applied { awarded_achievement: Option<&'static str> },
    Blocked { reason: ActionBlockReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionBlockReason {
    AlreadyFull,
    TooHungryToPlay,
    TooTiredToPlay,
    TooAwakeToSleep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPlayer {
    pub id: String,
    pub name: String,
    pub pet_description: Option<String>,
    pub is_host: bool,
    pub is_connected: bool,
    pub is_ready: bool,
    pub score: i32,
    pub current_dragon_id: Option<String>,
    pub achievements: Vec<String>,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: Uuid,
    pub code: SessionCode,
    pub phase: Phase,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkshopSession {
    pub id: Uuid,
    pub code: SessionCode,
    pub phase: Phase,
    pub time: i32,
    pub host_player_id: Option<String>,
    pub players: BTreeMap<String, SessionPlayer>,
    pub dragons: BTreeMap<String, SessionDragon>,
    pub voting: Option<VotingState>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("invalid session transition from {from:?} to {to:?}")]
    InvalidSessionTransition { from: Phase, to: Phase },
    #[error("phase2 transition blocked; missing handover tags for: {players:?}")]
    MissingHandoverTags { players: Vec<String> },
    #[error("voting is not active right now")]
    VotingNotActive,
    #[error("player is not eligible to vote")]
    IneligibleVoter,
    #[error("dragon is not available for voting")]
    UnknownDragon,
    #[error("cannot vote for current dragon")]
    SelfVoteForbidden,
    #[error("action is not allowed in current context")]
    ActionNotAllowed,
    #[error("player is not assigned to a dragon")]
    DragonNotAssigned,
}

impl WorkshopSession {
    pub fn new(id: Uuid, code: SessionCode, created_at: DateTime<Utc>) -> Self {
        Self {
            id,
            code,
            phase: Phase::Lobby,
            time: 8,
            host_player_id: None,
            players: BTreeMap::new(),
            dragons: BTreeMap::new(),
            voting: None,
            created_at,
            updated_at: created_at,
        }
    }

    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id,
            code: self.code.clone(),
            phase: self.phase,
            updated_at: self.updated_at,
        }
    }

    pub fn add_player(&mut self, player: SessionPlayer) {
        let player_id = player.id.clone();
        if self.host_player_id.is_none() {
            self.host_player_id = Some(player_id.clone());
        }
        self.players.insert(player_id, player);
        self.ensure_host_assigned(false);
        self.touch();
    }

    pub fn transition_to(&mut self, next: Phase) -> Result<(), DomainError> {
        if !can_transition(self.phase, next) {
            return Err(DomainError::InvalidSessionTransition {
                from: self.phase,
                to: next,
            });
        }

        self.phase = next;
        self.touch();
        Ok(())
    }

    pub fn begin_phase1(&mut self, assignments: &[Phase1Assignment]) -> Result<(), DomainError> {
        self.transition_to(Phase::Phase1)?;
        self.time = 8;
        self.voting = None;
        self.dragons.clear();

        for player in self.players.values_mut() {
            player.score = 0;
            player.achievements.clear();
            player.current_dragon_id = None;
        }

        for assignment in assignments {
            if let Some(player) = self.players.get_mut(&assignment.player_id) {
                let creator_instructions = player
                    .pet_description
                    .clone()
                    .unwrap_or_else(|| default_pet_description(&player.name));
                player.current_dragon_id = Some(assignment.dragon_id.clone());
                self.dragons.insert(
                    assignment.dragon_id.clone(),
                    SessionDragon {
                        id: assignment.dragon_id.clone(),
                        name: default_dragon_name(&player.name),
                        original_owner_id: assignment.player_id.clone(),
                        current_owner_id: assignment.player_id.clone(),
                        creator_instructions,
                        active_time: ActiveTime::Day,
                        day_food: FoodType::Meat,
                        night_food: FoodType::Fruit,
                        day_play: PlayType::Fetch,
                        night_play: PlayType::Music,
                        sleep_rate: 1,
                        hunger: 100,
                        energy: 100,
                        happiness: 100,
                        discovery_observations: Vec::new(),
                        handover_tags: Vec::new(),
                        last_action: DragonAction::Idle,
                        last_emotion: DragonEmotion::Neutral,
                        speech: Some("Hello! I'm new here!".to_string()),
                        speech_timer: 5,
                        action_cooldown: 0,
                        sleep_shield_ticks: 0,
                        food_tries: 0,
                        play_tries: 0,
                        high_happiness_ticks: 0,
                        phase2_ticks: 0,
                        phase2_lowest_happiness: 100,
                    },
                );
            }
        }

        self.touch();
        Ok(())
    }

    pub fn save_handover_tags(&mut self, player_id: &str, tags: Vec<String>) {
        let Some(dragon_id) = self.players.get(player_id).and_then(|player| player.current_dragon_id.clone()) else {
            return;
        };
        if let Some(dragon) = self.dragons.get_mut(&dragon_id) {
            dragon.handover_tags = tags.into_iter().take(3).collect();
        }
        self.touch();
    }

    pub fn enter_phase2(&mut self) -> Result<Phase2TransitionResult, DomainError> {
        self.transition_to(Phase::Phase2)?;

        let mut auto_filled_players = Vec::new();
        for player in self.players.values() {
            let Some(dragon_id) = player.current_dragon_id.clone() else {
                continue;
            };
            let Some(dragon) = self.dragons.get_mut(&dragon_id) else {
                continue;
            };
            if !player.is_connected && dragon.handover_tags.len() < 3 {
                dragon.handover_tags = fallback_handover_tags();
                auto_filled_players.push(player.name.clone());
            }
        }

        let missing_players = self
            .players
            .values()
            .filter_map(|player| {
                let dragon_id = player.current_dragon_id.as_ref()?;
                let dragon = self.dragons.get(dragon_id)?;
                if player.is_connected && dragon.handover_tags.len() < 3 {
                    Some(player.name.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        if !missing_players.is_empty() {
            return Err(DomainError::MissingHandoverTags {
                players: missing_players,
            });
        }

        let player_ids = self.players.keys().cloned().collect::<Vec<_>>();
        let dragon_ids = self.dragons.keys().cloned().collect::<Vec<_>>();

        if player_ids.len() > 1 {
            let mut shifted_dragon_ids = Vec::new();
            if let Some(last) = dragon_ids.last() {
                shifted_dragon_ids.push(last.clone());
                shifted_dragon_ids.extend(dragon_ids.iter().take(dragon_ids.len().saturating_sub(1)).cloned());
            }

            for (index, player_id) in player_ids.iter().enumerate() {
                if let Some(dragon_id) = shifted_dragon_ids.get(index) {
                    if let Some(player) = self.players.get_mut(player_id) {
                        player.current_dragon_id = Some(dragon_id.clone());
                    }
                    if let Some(dragon) = self.dragons.get_mut(dragon_id) {
                        dragon.current_owner_id = player_id.clone();
                        dragon.hunger = 100;
                        dragon.energy = 100;
                        dragon.happiness = 100;
                        dragon.food_tries = 0;
                        dragon.play_tries = 0;
                        dragon.action_cooldown = 0;
                        dragon.sleep_shield_ticks = 0;
                        dragon.phase2_ticks = 0;
                        dragon.phase2_lowest_happiness = 100;
                        dragon.last_action = DragonAction::Idle;
                        dragon.last_emotion = DragonEmotion::Neutral;
                        dragon.speech = Some("Where am I? Who are you?".to_string());
                        dragon.speech_timer = 5;
                    }
                }
            }
        } else if let Some(player_id) = player_ids.first() {
            if let Some(dragon_id) = self.players.get(player_id).and_then(|player| player.current_dragon_id.clone()) {
                if let Some(dragon) = self.dragons.get_mut(&dragon_id) {
                    dragon.hunger = 100;
                    dragon.energy = 100;
                    dragon.happiness = 100;
                    dragon.food_tries = 0;
                    dragon.play_tries = 0;
                    dragon.action_cooldown = 0;
                    dragon.sleep_shield_ticks = 0;
                    dragon.phase2_ticks = 0;
                    dragon.phase2_lowest_happiness = 100;
                    dragon.last_action = DragonAction::Idle;
                    dragon.last_emotion = DragonEmotion::Neutral;
                    dragon.speech = Some(
                        "New shift, same dragon. Time to document and support your own handoff.".to_string(),
                    );
                    dragon.speech_timer = 5;
                }
            }
        }

        self.touch();
        Ok(Phase2TransitionResult { auto_filled_players })
    }

    pub fn apply_action(&mut self, player_id: &str, action: PlayerAction) -> Result<ActionOutcome, DomainError> {
        if self.phase != Phase::Phase1 && self.phase != Phase::Phase2 {
            return Err(DomainError::ActionNotAllowed);
        }

        let current_is_day = is_daytime(self.time);
        let dragon_id = self
            .players
            .get(player_id)
            .and_then(|player| player.current_dragon_id.clone())
            .ok_or(DomainError::DragonNotAssigned)?;

        let dragon = self.dragons.get_mut(&dragon_id).ok_or(DomainError::DragonNotAssigned)?;

        let outcome = match action {
            PlayerAction::Feed(food) => {
                if dragon.hunger >= 95 {
                    dragon.speech = Some("I'm full! *burp*".to_string());
                    dragon.speech_timer = 4;
                    dragon.last_emotion = DragonEmotion::Neutral;
                    ActionOutcome::Blocked {
                        reason: ActionBlockReason::AlreadyFull,
                    }
                } else {
                    dragon.last_action = DragonAction::Feed;
                    dragon.action_cooldown = 3;
                    dragon.sleep_shield_ticks = 0;
                    dragon.food_tries += 1;
                    let favorite_food = if current_is_day { dragon.day_food } else { dragon.night_food };
                    let mut awarded = None;
                    if food == favorite_food {
                        if dragon.food_tries == 1 {
                            awarded = Some("master_chef");
                        }
                        dragon.hunger = (dragon.hunger + 40).min(100);
                        dragon.happiness = (dragon.happiness + 15).min(100);
                        dragon.last_emotion = DragonEmotion::Happy;
                        dragon.speech = Some(format!("Yummy! I love {:?}!", food).to_lowercase().replace("feed(", ""));
                    } else {
                        dragon.hunger = (dragon.hunger + 5).min(100);
                        dragon.happiness = (dragon.happiness - 20).max(0);
                        dragon.last_emotion = DragonEmotion::Angry;
                        dragon.speech = Some("Eww... I don't want that right now.".to_string());
                    }
                    dragon.speech_timer = 4;
                    ActionOutcome::Applied {
                        awarded_achievement: awarded,
                    }
                }
            }
            PlayerAction::Play(play) => {
                if dragon.hunger < 20 {
                    dragon.speech = Some("I'm too hungry to play!".to_string());
                    dragon.speech_timer = 4;
                    dragon.last_emotion = DragonEmotion::Angry;
                    ActionOutcome::Blocked {
                        reason: ActionBlockReason::TooHungryToPlay,
                    }
                } else if dragon.energy < 20 {
                    dragon.speech = Some("I'm too tired to play...".to_string());
                    dragon.speech_timer = 4;
                    dragon.last_emotion = DragonEmotion::Sleepy;
                    ActionOutcome::Blocked {
                        reason: ActionBlockReason::TooTiredToPlay,
                    }
                } else {
                    dragon.last_action = DragonAction::Play;
                    dragon.action_cooldown = 3;
                    dragon.sleep_shield_ticks = 0;
                    dragon.play_tries += 1;
                    let favorite_play = if current_is_day { dragon.day_play } else { dragon.night_play };
                    let mut awarded = None;
                    if play == favorite_play {
                        if dragon.play_tries == 1 {
                            awarded = Some("playful_spirit");
                        }
                        dragon.energy = (dragon.energy - 20).max(0);
                        dragon.happiness = (dragon.happiness + 30).min(100);
                        dragon.last_emotion = DragonEmotion::Happy;
                        dragon.speech = Some("Yay! Favorite game!".to_string());
                    } else {
                        dragon.energy = (dragon.energy - 15).max(0);
                        dragon.happiness = (dragon.happiness - 20).max(0);
                        dragon.last_emotion = DragonEmotion::Angry;
                        dragon.speech = Some("I don't want to play that...".to_string());
                    }
                    dragon.speech_timer = 4;
                    ActionOutcome::Applied {
                        awarded_achievement: awarded,
                    }
                }
            }
            PlayerAction::Sleep => {
                if dragon.energy >= 90 {
                    dragon.speech = Some("I'm not tired!".to_string());
                    dragon.speech_timer = 4;
                    dragon.last_emotion = DragonEmotion::Angry;
                    ActionOutcome::Blocked {
                        reason: ActionBlockReason::TooAwakeToSleep,
                    }
                } else {
                    dragon.last_action = DragonAction::Sleep;
                    dragon.action_cooldown = 3;
                    dragon.energy = (dragon.energy + 50).min(100);
                    let is_correct_time =
                        (dragon.active_time == ActiveTime::Day && !current_is_day)
                            || (dragon.active_time == ActiveTime::Night && current_is_day);
                    if is_correct_time {
                        dragon.happiness = (dragon.happiness + 10).min(100);
                    }
                    dragon.sleep_shield_ticks = 1;
                    dragon.last_emotion = DragonEmotion::Sleepy;
                    dragon.speech = Some("Zzz... Good night...".to_string());
                    dragon.speech_timer = 5;
                    ActionOutcome::Applied {
                        awarded_achievement: None,
                    }
                }
            }
        };

        self.touch();
        Ok(outcome)
    }

    pub fn advance_tick(&mut self) {
        if self.phase != Phase::Phase1 && self.phase != Phase::Phase2 {
            return;
        }

        self.time = (self.time + 1) % 24;
        let current_is_day = is_daytime(self.time);
        let previous_is_day = is_daytime((self.time + 23) % 24);
        let decay_multiplier = if self.phase == Phase::Phase2 { 2 } else { 1 };

        for dragon in self.dragons.values_mut() {
            let Some(owner) = self.players.get(&dragon.current_owner_id) else {
                continue;
            };
            if !owner.is_connected {
                continue;
            }

            let wrong_time =
                (dragon.active_time == ActiveTime::Day && !current_is_day)
                    || (dragon.active_time == ActiveTime::Night && current_is_day);
            let time_penalty = if wrong_time { 2 } else { 1 };

            if current_is_day != previous_is_day {
                dragon.food_tries = 0;
                dragon.play_tries = 0;
            }

            dragon.hunger = (dragon.hunger - decay_multiplier).max(0);
            dragon.energy = (dragon.energy - (dragon.sleep_rate * time_penalty * decay_multiplier)).max(0);

            let mut happiness_decay = 1;
            if dragon.hunger < 30 {
                happiness_decay += 1;
            }
            if dragon.energy < 30 {
                happiness_decay += 1;
            }
            if wrong_time && dragon.sleep_shield_ticks == 0 {
                happiness_decay += 1;
            }
            dragon.happiness = (dragon.happiness - happiness_decay * decay_multiplier).max(0);

            if dragon.speech_timer > 0 {
                dragon.speech_timer -= 1;
                if dragon.speech_timer == 0 {
                    dragon.speech = None;
                }
            }

            if dragon.action_cooldown > 0 {
                dragon.action_cooldown -= 1;
            }

            if dragon.sleep_shield_ticks > 0 {
                dragon.sleep_shield_ticks -= 1;
                if dragon.sleep_shield_ticks == 0 && dragon.last_action == DragonAction::Sleep {
                    dragon.last_action = DragonAction::Idle;
                    if dragon.speech.is_none() {
                        dragon.last_emotion = DragonEmotion::Neutral;
                    }
                }
            }

            if self.phase == Phase::Phase1 {
                if dragon.happiness >= 90 {
                    dragon.high_happiness_ticks += 1;
                } else {
                    dragon.high_happiness_ticks = 0;
                }
            } else {
                dragon.phase2_ticks += 1;
                if dragon.happiness < dragon.phase2_lowest_happiness {
                    dragon.phase2_lowest_happiness = dragon.happiness;
                }
            }
        }

        self.touch();
    }

    pub fn enter_voting(&mut self) -> Result<bool, DomainError> {
        self.transition_to(Phase::Voting)?;

        let eligible_player_ids = self
            .players
            .values()
            .filter(|player| player.current_dragon_id.is_some())
            .map(|player| player.id.clone())
            .collect::<Vec<_>>();

        let normalized_eligible = if eligible_player_ids.len() > 1 {
            eligible_player_ids
        } else {
            Vec::new()
        };

        let immediate_finalize = normalized_eligible.is_empty();
        self.voting = Some(VotingState {
            eligible_player_ids: normalized_eligible,
            votes_by_player_id: BTreeMap::new(),
        });

        self.touch();
        Ok(immediate_finalize)
    }

    pub fn submit_vote(&mut self, player_id: &str, dragon_id: &str) -> Result<(), DomainError> {
        let voting = self.voting.as_mut().ok_or(DomainError::VotingNotActive)?;

        if !voting.eligible_player_ids.iter().any(|eligible| eligible == player_id) {
            return Err(DomainError::IneligibleVoter);
        }

        if !self.dragons.contains_key(dragon_id) {
            return Err(DomainError::UnknownDragon);
        }

        if self
            .players
            .get(player_id)
            .and_then(|player| player.current_dragon_id.as_deref())
            == Some(dragon_id)
        {
            return Err(DomainError::SelfVoteForbidden);
        }

        voting
            .votes_by_player_id
            .insert(player_id.to_string(), dragon_id.to_string());
        self.touch();
        Ok(())
    }

    pub fn finalize_voting(&mut self) -> Result<(), DomainError> {
        self.transition_to(Phase::End)?;

        for player in self.players.values_mut() {
            let Some(dragon_id) = player.current_dragon_id.as_ref() else {
                continue;
            };
            let Some(dragon) = self.dragons.get(dragon_id) else {
                continue;
            };

            player.score = dragon.happiness + dragon.hunger + dragon.energy + (player.achievements.len() as i32 * 50);
        }

        self.touch();
        Ok(())
    }

    pub fn reset_to_lobby(&mut self) -> Result<(), DomainError> {
        self.phase = Phase::Lobby;
        self.time = 8;
        self.voting = None;
        self.dragons.clear();

        for player in self.players.values_mut() {
            player.score = 0;
            player.current_dragon_id = None;
            player.achievements.clear();
            player.is_ready = false;
        }

        self.touch();
        Ok(())
    }

    pub fn ensure_host_assigned(&mut self, prefer_connected: bool) -> Option<String> {
        if let Some(current_host_id) = self.host_player_id.clone() {
            if let Some(current_host) = self.players.get(&current_host_id) {
                if !prefer_connected || current_host.is_connected {
                    self.reconcile_host_flags(Some(current_host_id.clone()));
                    return Some(current_host_id.clone());
                }
            }
        }

        let next_host = self
            .players
            .values()
            .filter(|player| !prefer_connected || player.is_connected)
            .min_by_key(|player| player.joined_at)
            .map(|player| player.id.clone());

        self.host_player_id = next_host.clone();
        self.reconcile_host_flags(next_host.clone());
        next_host
    }

    fn reconcile_host_flags(&mut self, host_id: Option<String>) {
        for player in self.players.values_mut() {
            player.is_host = host_id.as_ref().is_some_and(|id| id == &player.id);
        }
    }

    fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    pub fn record_discovery_observation(&mut self, player_id: &str, text: impl Into<String>) {
        let Some(dragon_id) = self
            .players
            .get(player_id)
            .and_then(|player| player.current_dragon_id.clone())
        else {
            return;
        };
        if let Some(dragon) = self.dragons.get_mut(&dragon_id) {
            dragon.discovery_observations.push(text.into());
            dragon.discovery_observations = dragon
                .discovery_observations
                .iter()
                .rev()
                .take(6)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
        }
        self.touch();
    }
}

pub fn can_transition(current: Phase, next: Phase) -> bool {
    matches!(
        (current, next),
        (Phase::Lobby, Phase::Phase1)
            | (Phase::Phase1, Phase::Handover)
            | (Phase::Handover, Phase::Phase2)
            | (Phase::Phase2, Phase::Voting)
            | (Phase::Voting, Phase::End)
            | (Phase::End, Phase::Lobby)
    )
}

fn fallback_handover_tags() -> Vec<String> {
    vec![
        "Auto handover: teammate went offline before finishing notes.".to_string(),
        "Start with safe observations and watch how needs change over time.".to_string(),
        "Test food and play again after day/night changes.".to_string(),
    ]
}

fn is_daytime(hour: i32) -> bool {
    hour >= 6 && hour < 18
}

fn default_pet_description(player_name: &str) -> String {
    format!("{player_name}'s workshop dragon")
}

fn default_dragon_name(player_name: &str) -> String {
    format!("{player_name}'s dragon")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).expect("valid timestamp")
    }

    fn player(id: &str, connected: bool, joined_at_seconds: i64) -> SessionPlayer {
        SessionPlayer {
            id: id.to_string(),
            name: format!("player-{id}"),
            pet_description: None,
            is_host: false,
            is_connected: connected,
            is_ready: false,
            score: 0,
            current_dragon_id: None,
            achievements: Vec::new(),
            joined_at: ts(joined_at_seconds),
        }
    }

    #[test]
    fn allows_valid_lobby_to_phase1_transition() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));

        let result = session.transition_to(Phase::Phase1);

        assert!(result.is_ok());
        assert_eq!(session.phase, Phase::Phase1);
    }

    #[test]
    fn rejects_invalid_lobby_to_end_transition() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));

        let result = session.transition_to(Phase::End);

        assert_eq!(
            result,
            Err(DomainError::InvalidSessionTransition {
                from: Phase::Lobby,
                to: Phase::End,
            })
        );
        assert_eq!(session.phase, Phase::Lobby);
    }

    #[test]
    fn first_player_becomes_host_automatically() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));

        assert_eq!(session.host_player_id.as_deref(), Some("p1"));
        assert!(session.players.get("p1").expect("player p1").is_host);
    }

    #[test]
    fn ensure_host_assigned_prefers_connected_player_when_requested() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", false, 10));
        session.add_player(player("p2", true, 20));
        session.host_player_id = Some("p1".to_string());

        let host = session.ensure_host_assigned(true);

        assert_eq!(host.as_deref(), Some("p2"));
        assert!(!session.players.get("p1").expect("player p1").is_host);
        assert!(session.players.get("p2").expect("player p2").is_host);
    }

    #[test]
    fn ensure_host_assigned_returns_none_when_session_has_no_players() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));

        let host = session.ensure_host_assigned(true);

        assert_eq!(host, None);
        assert_eq!(session.host_player_id, None);
    }

    #[test]
    fn begin_phase1_assigns_dragons_and_resets_player_progress() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        let mut p1 = player("p1", true, 10);
        p1.pet_description = Some("Curious cave dragon".into());
        p1.score = 90;
        p1.achievements = vec!["master_chef".into()];
        p1.current_dragon_id = Some("old-dragon".into());
        let mut p2 = player("p2", true, 20);
        p2.score = 50;
        p2.achievements = vec!["playful_spirit".into()];
        session.add_player(p1);
        session.add_player(p2);

        let result = session.begin_phase1(&[
            Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "dragon-a".into(),
            },
            Phase1Assignment {
                player_id: "p2".into(),
                dragon_id: "dragon-b".into(),
            },
        ]);

        assert!(result.is_ok());
        assert_eq!(session.phase, Phase::Phase1);
        assert_eq!(session.time, 8);
        assert_eq!(session.players.get("p1").and_then(|p| p.current_dragon_id.as_deref()), Some("dragon-a"));
        assert_eq!(session.players.get("p2").and_then(|p| p.current_dragon_id.as_deref()), Some("dragon-b"));
        assert_eq!(session.players.get("p1").map(|p| p.score), Some(0));
        assert!(session.players.get("p1").expect("player p1").achievements.is_empty());
        let dragon_a = session.dragons.get("dragon-a").expect("dragon a");
        assert_eq!(dragon_a.name, "player-p1's dragon");
        assert_eq!(dragon_a.creator_instructions, "Curious cave dragon");
        assert!(dragon_a.discovery_observations.is_empty());
        let dragon_b = session.dragons.get("dragon-b").expect("dragon b");
        assert_eq!(dragon_b.creator_instructions, "player-p2's workshop dragon");
    }

    #[test]
    fn record_discovery_observation_keeps_last_six_entries() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));
        session
            .begin_phase1(&[Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "dragon-a".into(),
            }])
            .expect("start phase1");

        for index in 1..=7 {
            session.record_discovery_observation("p1", format!("note-{index}"));
        }

        let dragon = session.dragons.get("dragon-a").expect("dragon-a");
        assert_eq!(dragon.discovery_observations.len(), 6);
        assert_eq!(dragon.discovery_observations.first().map(String::as_str), Some("note-2"));
        assert_eq!(dragon.discovery_observations.last().map(String::as_str), Some("note-7"));
    }

    #[test]
    fn enter_voting_with_single_assigned_player_immediately_finalizes() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));
        session.begin_phase1(&[Phase1Assignment {
            player_id: "p1".into(),
            dragon_id: "dragon-a".into(),
        }]).expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.transition_to(Phase::Phase2).expect("to phase2");

        let immediate_finalize = session.enter_voting().expect("enter voting");

        assert!(immediate_finalize);
        assert_eq!(session.phase, Phase::Voting);
        assert_eq!(session.voting.as_ref().map(|v| v.eligible_player_ids.len()), Some(0));
    }

    #[test]
    fn reset_to_lobby_clears_runtime_player_state() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        let mut p1 = player("p1", true, 10);
        p1.is_ready = true;
        session.add_player(p1);
        session.begin_phase1(&[Phase1Assignment {
            player_id: "p1".into(),
            dragon_id: "dragon-a".into(),
        }]).expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.transition_to(Phase::Phase2).expect("to phase2");
        session.enter_voting().expect("enter voting");
        session.transition_to(Phase::End).expect("to end");
        {
            let player = session.players.get_mut("p1").expect("player p1");
            player.score = 77;
            player.achievements = vec!["smooth_transition".into()];
        }

        let result = session.reset_to_lobby();

        assert!(result.is_ok());
        assert_eq!(session.phase, Phase::Lobby);
        assert_eq!(session.time, 8);
        assert!(session.voting.is_none());
        let player = session.players.get("p1").expect("player p1");
        assert_eq!(player.score, 0);
        assert!(player.current_dragon_id.is_none());
        assert!(player.achievements.is_empty());
        assert!(!player.is_ready);
    }

    #[test]
    fn enter_phase2_autofills_offline_players_with_missing_notes() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", false, 10));
        session.begin_phase1(&[Phase1Assignment {
            player_id: "p1".into(),
            dragon_id: "dragon-a".into(),
        }]).expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");

        let result = session.enter_phase2().expect("enter phase2");

        assert_eq!(result.auto_filled_players, vec!["player-p1".to_string()]);
        let dragon = session.dragons.get("dragon-a").expect("dragon-a");
        assert_eq!(dragon.handover_tags.len(), 3);
        assert_eq!(session.phase, Phase::Phase2);
    }

    #[test]
    fn enter_phase2_rejects_connected_players_with_missing_tags() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));
        session.begin_phase1(&[Phase1Assignment {
            player_id: "p1".into(),
            dragon_id: "dragon-a".into(),
        }]).expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");

        let result = session.enter_phase2();

        assert_eq!(
            result,
            Err(DomainError::MissingHandoverTags {
                players: vec!["player-p1".to_string()],
            })
        );
    }

    #[test]
    fn enter_phase2_reassigns_dragons_in_multiplayer_session() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));
        session.add_player(player("p2", true, 20));
        session.begin_phase1(&[
            Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "dragon-a".into(),
            },
            Phase1Assignment {
                player_id: "p2".into(),
                dragon_id: "dragon-b".into(),
            },
        ]).expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]);
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]);

        let result = session.enter_phase2().expect("enter phase2");

        assert!(result.auto_filled_players.is_empty());
        assert_eq!(session.players.get("p1").and_then(|p| p.current_dragon_id.as_deref()), Some("dragon-b"));
        assert_eq!(session.players.get("p2").and_then(|p| p.current_dragon_id.as_deref()), Some("dragon-a"));
        assert_eq!(session.dragons.get("dragon-a").map(|d| d.current_owner_id.as_str()), Some("p2"));
        assert_eq!(session.dragons.get("dragon-b").map(|d| d.current_owner_id.as_str()), Some("p1"));
    }

    #[test]
    fn enter_phase2_single_player_keeps_same_dragon_and_updates_speech() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));
        session.begin_phase1(&[Phase1Assignment {
            player_id: "p1".into(),
            dragon_id: "dragon-a".into(),
        }]).expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]);

        let result = session.enter_phase2().expect("enter phase2");

        assert!(result.auto_filled_players.is_empty());
        assert_eq!(session.players.get("p1").and_then(|p| p.current_dragon_id.as_deref()), Some("dragon-a"));
        let dragon = session.dragons.get("dragon-a").expect("dragon-a");
        assert_eq!(dragon.current_owner_id, "p1");
        assert_eq!(dragon.speech.as_deref(), Some("New shift, same dragon. Time to document and support your own handoff."));
    }

    #[test]
    fn submit_vote_rejects_ineligible_player() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));
        session.add_player(player("p2", true, 20));
        session.begin_phase1(&[
            Phase1Assignment { player_id: "p1".into(), dragon_id: "dragon-a".into() },
            Phase1Assignment { player_id: "p2".into(), dragon_id: "dragon-b".into() },
        ]).expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]);
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]);
        session.enter_phase2().expect("enter phase2");
        session.enter_voting().expect("enter voting");

        let result = session.submit_vote("ghost", "dragon-a");

        assert_eq!(result, Err(DomainError::IneligibleVoter));
    }

    #[test]
    fn submit_vote_rejects_unknown_dragon() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));
        session.add_player(player("p2", true, 20));
        session.begin_phase1(&[
            Phase1Assignment { player_id: "p1".into(), dragon_id: "dragon-a".into() },
            Phase1Assignment { player_id: "p2".into(), dragon_id: "dragon-b".into() },
        ]).expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]);
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]);
        session.enter_phase2().expect("enter phase2");
        session.enter_voting().expect("enter voting");

        let eligible_player = session.voting.as_ref().and_then(|v| v.eligible_player_ids.first()).cloned().expect("eligible player");
        let result = session.submit_vote(&eligible_player, "missing-dragon");

        assert_eq!(result, Err(DomainError::UnknownDragon));
    }

    #[test]
    fn submit_vote_rejects_vote_for_current_dragon() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));
        session.add_player(player("p2", true, 20));
        session.begin_phase1(&[
            Phase1Assignment { player_id: "p1".into(), dragon_id: "dragon-a".into() },
            Phase1Assignment { player_id: "p2".into(), dragon_id: "dragon-b".into() },
        ]).expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]);
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]);
        session.enter_phase2().expect("enter phase2");
        session.enter_voting().expect("enter voting");

        let eligible_player = session.voting.as_ref().and_then(|v| v.eligible_player_ids.first()).cloned().expect("eligible player");
        let own_dragon = session.players.get(&eligible_player).and_then(|p| p.current_dragon_id.clone()).expect("current dragon");
        let result = session.submit_vote(&eligible_player, &own_dragon);

        assert_eq!(result, Err(DomainError::SelfVoteForbidden));
    }

    #[test]
    fn finalize_voting_sets_end_phase_and_computes_scores() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));
        session.add_player(player("p2", true, 20));
        session.begin_phase1(&[
            Phase1Assignment { player_id: "p1".into(), dragon_id: "dragon-a".into() },
            Phase1Assignment { player_id: "p2".into(), dragon_id: "dragon-b".into() },
        ]).expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]);
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]);
        session.enter_phase2().expect("enter phase2");
        session.enter_voting().expect("enter voting");
        {
            let dragon_id = session.players.get("p1").and_then(|p| p.current_dragon_id.clone()).expect("p1 dragon");
            let dragon = session.dragons.get_mut(&dragon_id).expect("dragon stats");
            dragon.happiness = 80;
            dragon.hunger = 70;
            dragon.energy = 60;
        }
        {
            let player = session.players.get_mut("p1").expect("player p1");
            player.achievements = vec!["smooth_transition".into(), "master_chef".into()];
        }

        let result = session.finalize_voting();

        assert!(result.is_ok());
        assert_eq!(session.phase, Phase::End);
        assert_eq!(session.players.get("p1").map(|p| p.score), Some(80 + 70 + 60 + 100));
    }

    #[test]
    fn feed_action_blocks_when_dragon_is_already_full() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));
        session.begin_phase1(&[Phase1Assignment { player_id: "p1".into(), dragon_id: "dragon-a".into() }])
            .expect("start phase1");

        let outcome = session.apply_action("p1", PlayerAction::Feed(FoodType::Meat)).expect("apply action");

        assert_eq!(
            outcome,
            ActionOutcome::Blocked {
                reason: ActionBlockReason::AlreadyFull,
            }
        );
    }

    #[test]
    fn play_action_blocks_when_dragon_is_too_hungry() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));
        session.begin_phase1(&[Phase1Assignment { player_id: "p1".into(), dragon_id: "dragon-a".into() }])
            .expect("start phase1");
        let dragon = session.dragons.get_mut("dragon-a").expect("dragon-a");
        dragon.hunger = 10;

        let outcome = session.apply_action("p1", PlayerAction::Play(PlayType::Fetch)).expect("apply action");

        assert_eq!(
            outcome,
            ActionOutcome::Blocked {
                reason: ActionBlockReason::TooHungryToPlay,
            }
        );
    }

    #[test]
    fn sleep_action_blocks_when_dragon_is_too_awake() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));
        session.begin_phase1(&[Phase1Assignment { player_id: "p1".into(), dragon_id: "dragon-a".into() }])
            .expect("start phase1");

        let outcome = session.apply_action("p1", PlayerAction::Sleep).expect("apply action");

        assert_eq!(
            outcome,
            ActionOutcome::Blocked {
                reason: ActionBlockReason::TooAwakeToSleep,
            }
        );
    }

    #[test]
    fn phase2_tick_uses_stronger_decay_multiplier() {
        let mut session = WorkshopSession::new(Uuid::new_v4(), SessionCode("123456".into()), ts(1));
        session.add_player(player("p1", true, 10));
        session.begin_phase1(&[Phase1Assignment { player_id: "p1".into(), dragon_id: "dragon-a".into() }])
            .expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]);
        session.enter_phase2().expect("enter phase2");
        let dragon = session.dragons.get_mut("dragon-a").expect("dragon-a");
        dragon.hunger = 100;
        dragon.energy = 100;
        dragon.happiness = 100;
        dragon.sleep_rate = 1;
        dragon.active_time = ActiveTime::Day;
        session.time = 8;

        session.advance_tick();

        let dragon = session.dragons.get("dragon-a").expect("dragon-a");
        assert_eq!(dragon.hunger, 98);
        assert_eq!(dragon.energy, 98);
        assert_eq!(dragon.happiness, 98);
    }
}
