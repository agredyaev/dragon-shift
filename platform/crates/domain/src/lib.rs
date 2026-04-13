use chrono::{DateTime, Utc};
use protocol::{
    ActiveTime, DragonAction, DragonEmotion, FoodType, Phase, PlayType, WorkshopCreateConfig,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionCode(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VotingState {
    pub eligible_player_ids: Vec<String>,
    pub votes_by_player_id: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase1Assignment {
    pub player_id: String,
    pub dragon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase2TransitionResult {
    pub auto_filled_players: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Counters for wrong/correct actions and penalty tracking.
    pub wrong_food_count: i32,
    pub wrong_play_count: i32,
    pub cooldown_violations: i32,
    pub total_actions: i32,
    pub correct_actions: i32,
    /// Accumulated penalty stacks — each wrong action adds 1, decays by 1 every 6 ticks.
    /// Increases happiness decay while > 0.
    pub penalty_stacks: i32,
    pub penalty_decay_timer: i32,
    /// Highest penalty_stacks ever reached (for chaos_gremlin achievement).
    #[serde(default)]
    pub peak_penalty_stacks: i32,
    /// Whether the player has ever fed the correct food (for speed_learner achievement).
    pub found_correct_food: bool,
    /// Whether the player has ever played the correct game (for speed_learner achievement).
    pub found_correct_play: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerAction {
    Feed(FoodType),
    Play(PlayType),
    Sleep,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionOutcome {
    Applied {
        awarded_achievement: Option<&'static str>,
        was_correct: bool,
    },
    Blocked {
        reason: ActionBlockReason,
    },
    CooldownViolation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionBlockReason {
    AlreadyFull,
    TooHungryToPlay,
    TooTiredToPlay,
    TooAwakeToSleep,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

pub struct SessionSummary {
    pub id: Uuid,
    pub code: SessionCode,
    pub phase: Phase,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkshopSession {
    pub id: Uuid,
    pub code: SessionCode,
    pub phase: Phase,
    pub time: i32,
    pub config: WorkshopCreateConfig,
    pub phase_started_at: DateTime<Utc>,
    pub warned_for_current_phase: bool,
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
    pub fn new(
        id: Uuid,
        code: SessionCode,
        created_at: DateTime<Utc>,
        config: WorkshopCreateConfig,
    ) -> Self {
        Self {
            id,
            code,
            phase: Phase::Lobby,
            time: 8,
            config,
            phase_started_at: created_at,
            warned_for_current_phase: false,
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
        self.phase_started_at = Utc::now();
        self.warned_for_current_phase = false;
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
                        name: random_dragon_name(),
                        original_owner_id: assignment.player_id.clone(),
                        current_owner_id: assignment.player_id.clone(),
                        creator_instructions,
                        active_time: random_active_time(),
                        day_food: random_food_type(),
                        night_food: random_food_type_excluding(None),
                        day_play: random_play_type(),
                        night_play: random_play_type_excluding(None),
                        sleep_rate: rand::rng().random_range(1..=3),
                        hunger: 50,
                        energy: 50,
                        happiness: 50,
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
                        wrong_food_count: 0,
                        wrong_play_count: 0,
                        cooldown_violations: 0,
                        total_actions: 0,
                        correct_actions: 0,
                        penalty_stacks: 0,
                        penalty_decay_timer: 0,
                        peak_penalty_stacks: 0,
                        found_correct_food: false,
                        found_correct_play: false,
                    },
                );
            }
        }

        self.touch();
        Ok(())
    }

    pub fn save_handover_tags(&mut self, player_id: &str, tags: Vec<String>) {
        let Some(dragon_id) = self
            .players
            .get(player_id)
            .and_then(|player| player.current_dragon_id.clone())
        else {
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
                shifted_dragon_ids.extend(
                    dragon_ids
                        .iter()
                        .take(dragon_ids.len().saturating_sub(1))
                        .cloned(),
                );
            }

            for (index, player_id) in player_ids.iter().enumerate() {
                if let Some(dragon_id) = shifted_dragon_ids.get(index) {
                    if let Some(player) = self.players.get_mut(player_id) {
                        player.current_dragon_id = Some(dragon_id.clone());
                    }
                    if let Some(dragon) = self.dragons.get_mut(dragon_id) {
                        dragon.current_owner_id = player_id.clone();
                        dragon.hunger = 50;
                        dragon.energy = 50;
                        dragon.happiness = 50;
                        dragon.food_tries = 0;
                        dragon.play_tries = 0;
                        dragon.action_cooldown = 0;
                        dragon.sleep_shield_ticks = 0;
                        dragon.phase2_ticks = 0;
                        dragon.phase2_lowest_happiness = 100;
                        dragon.wrong_food_count = 0;
                        dragon.wrong_play_count = 0;
                        dragon.cooldown_violations = 0;
                        dragon.total_actions = 0;
                        dragon.correct_actions = 0;
                        dragon.penalty_stacks = 0;
                        dragon.penalty_decay_timer = 0;
                        dragon.peak_penalty_stacks = 0;
                        dragon.found_correct_food = false;
                        dragon.found_correct_play = false;
                        dragon.last_action = DragonAction::Idle;
                        dragon.last_emotion = DragonEmotion::Neutral;
                        dragon.speech = Some("Where am I? Who are you?".to_string());
                        dragon.speech_timer = 5;
                    }
                }
            }
        } else if let Some(player_id) = player_ids.first()
            && let Some(dragon_id) = self
                .players
                .get(player_id)
                .and_then(|player| player.current_dragon_id.clone())
            && let Some(dragon) = self.dragons.get_mut(&dragon_id)
        {
            dragon.hunger = 50;
            dragon.energy = 50;
            dragon.happiness = 50;
            dragon.food_tries = 0;
            dragon.play_tries = 0;
            dragon.action_cooldown = 0;
            dragon.sleep_shield_ticks = 0;
            dragon.phase2_ticks = 0;
            dragon.phase2_lowest_happiness = 100;
            dragon.wrong_food_count = 0;
            dragon.wrong_play_count = 0;
            dragon.cooldown_violations = 0;
            dragon.total_actions = 0;
            dragon.correct_actions = 0;
            dragon.penalty_stacks = 0;
            dragon.penalty_decay_timer = 0;
            dragon.peak_penalty_stacks = 0;
            dragon.found_correct_food = false;
            dragon.found_correct_play = false;
            dragon.last_action = DragonAction::Idle;
            dragon.last_emotion = DragonEmotion::Neutral;
            dragon.speech = Some(
                "New shift, same dragon. Time to document and support your own handoff."
                    .to_string(),
            );
            dragon.speech_timer = 5;
        }

        self.touch();
        Ok(Phase2TransitionResult {
            auto_filled_players,
        })
    }

    pub fn apply_action(
        &mut self,
        player_id: &str,
        action: PlayerAction,
    ) -> Result<ActionOutcome, DomainError> {
        if self.phase != Phase::Phase1 && self.phase != Phase::Phase2 {
            return Err(DomainError::ActionNotAllowed);
        }

        let current_is_day = is_daytime(self.time);
        let dragon_id = self
            .players
            .get(player_id)
            .and_then(|player| player.current_dragon_id.clone())
            .ok_or(DomainError::DragonNotAssigned)?;

        let dragon = self
            .dragons
            .get_mut(&dragon_id)
            .ok_or(DomainError::DragonNotAssigned)?;

        if dragon.action_cooldown > 0 {
            dragon.cooldown_violations += 1;
            self.touch();
            return Ok(ActionOutcome::CooldownViolation);
        }

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
                    dragon.total_actions += 1;
                    let favorite_food = if current_is_day {
                        dragon.day_food
                    } else {
                        dragon.night_food
                    };
                    let mut awarded = None;
                    let was_correct;
                    if food == favorite_food {
                        was_correct = true;
                        dragon.correct_actions += 1;
                        dragon.found_correct_food = true;
                        if dragon.food_tries == 1 {
                            awarded = Some("master_chef");
                        }
                        // speed_learner: found both correct food & play within 3 actions
                        if awarded.is_none()
                            && dragon.found_correct_play
                            && dragon.total_actions <= 3
                        {
                            awarded = Some("speed_learner");
                        }
                        dragon.hunger = (dragon.hunger + 40).min(100);
                        dragon.happiness = (dragon.happiness + 15).min(100);
                        dragon.last_emotion = DragonEmotion::Happy;
                        dragon.speech = Some(
                            format!("Yummy! I love {:?}!", food)
                                .to_lowercase()
                                .replace("feed(", ""),
                        );
                        // Correct action reduces penalty stacks
                        dragon.penalty_stacks = (dragon.penalty_stacks - 1).max(0);
                    } else {
                        was_correct = false;
                        dragon.wrong_food_count += 1;
                        // Escalating penalty: repeated wrong food hurts more
                        let penalty = 20 + (dragon.wrong_food_count - 1).min(3) * 5;
                        dragon.hunger = (dragon.hunger + 5).min(100);
                        dragon.happiness = (dragon.happiness - penalty).max(0);
                        dragon.last_emotion = DragonEmotion::Angry;
                        dragon.speech = Some("Eww... I don't want that right now.".to_string());
                        dragon.penalty_stacks += 1;
                        if dragon.penalty_stacks > dragon.peak_penalty_stacks {
                            dragon.peak_penalty_stacks = dragon.penalty_stacks;
                        }
                    }
                    dragon.speech_timer = 4;
                    ActionOutcome::Applied {
                        awarded_achievement: awarded,
                        was_correct,
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
                    dragon.total_actions += 1;
                    let favorite_play = if current_is_day {
                        dragon.day_play
                    } else {
                        dragon.night_play
                    };
                    let mut awarded = None;
                    let was_correct;
                    if play == favorite_play {
                        was_correct = true;
                        dragon.correct_actions += 1;
                        dragon.found_correct_play = true;
                        if dragon.play_tries == 1 {
                            awarded = Some("playful_spirit");
                        }
                        // speed_learner: found both correct food & play within 3 actions
                        if awarded.is_none()
                            && dragon.found_correct_food
                            && dragon.total_actions <= 3
                        {
                            awarded = Some("speed_learner");
                        }
                        dragon.energy = (dragon.energy - 20).max(0);
                        dragon.happiness = (dragon.happiness + 30).min(100);
                        dragon.last_emotion = DragonEmotion::Happy;
                        dragon.speech = Some("Yay! Favorite game!".to_string());
                        dragon.penalty_stacks = (dragon.penalty_stacks - 1).max(0);
                    } else {
                        was_correct = false;
                        dragon.wrong_play_count += 1;
                        let penalty = 20 + (dragon.wrong_play_count - 1).min(3) * 5;
                        dragon.energy = (dragon.energy - 15).max(0);
                        dragon.happiness = (dragon.happiness - penalty).max(0);
                        dragon.last_emotion = DragonEmotion::Angry;
                        dragon.speech = Some("I don't want to play that...".to_string());
                        dragon.penalty_stacks += 1;
                        if dragon.penalty_stacks > dragon.peak_penalty_stacks {
                            dragon.peak_penalty_stacks = dragon.penalty_stacks;
                        }
                    }
                    dragon.speech_timer = 4;
                    ActionOutcome::Applied {
                        awarded_achievement: awarded,
                        was_correct,
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
                    dragon.total_actions += 1;
                    dragon.energy = (dragon.energy + 50).min(100);
                    let is_correct_time = (dragon.active_time == ActiveTime::Day
                        && !current_is_day)
                        || (dragon.active_time == ActiveTime::Night && current_is_day);
                    let was_correct = is_correct_time;
                    if is_correct_time {
                        dragon.correct_actions += 1;
                        dragon.happiness = (dragon.happiness + 10).min(100);
                        dragon.penalty_stacks = (dragon.penalty_stacks - 1).max(0);
                    }
                    dragon.sleep_shield_ticks = 1;
                    dragon.last_emotion = DragonEmotion::Sleepy;
                    dragon.speech = Some("Zzz... Good night...".to_string());
                    dragon.speech_timer = 5;
                    ActionOutcome::Applied {
                        awarded_achievement: None,
                        was_correct,
                    }
                }
            }
        };

        self.touch();
        Ok(outcome)
    }

    /// Returns a list of (player_id, achievement_name) awarded during this tick.
    pub fn advance_tick(&mut self) -> Vec<(String, &'static str)> {
        if self.phase != Phase::Phase1 && self.phase != Phase::Phase2 {
            return Vec::new();
        }

        let mut awarded = Vec::new();

        self.time = (self.time + 1) % 24;
        let current_is_day = is_daytime(self.time);
        let previous_is_day = is_daytime((self.time + 23) % 24);
        let decay_multiplier = if self.phase == Phase::Phase2 { 3 } else { 1 };

        for dragon in self.dragons.values_mut() {
            let Some(owner) = self.players.get(&dragon.current_owner_id) else {
                continue;
            };
            if !owner.is_connected {
                continue;
            }

            let mut tick_achievements: Vec<&'static str> = Vec::new();

            let wrong_time = (dragon.active_time == ActiveTime::Day && !current_is_day)
                || (dragon.active_time == ActiveTime::Night && current_is_day);
            let time_penalty = if wrong_time { 2 } else { 1 };

            if current_is_day != previous_is_day {
                dragon.food_tries = 0;
                dragon.play_tries = 0;
            }

            dragon.hunger = (dragon.hunger - decay_multiplier).max(0);
            dragon.energy =
                (dragon.energy - (dragon.sleep_rate * time_penalty * decay_multiplier)).max(0);

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
            // Penalty stacks add extra happiness decay
            happiness_decay += dragon.penalty_stacks.min(4);
            dragon.happiness = (dragon.happiness - happiness_decay * decay_multiplier).max(0);

            // "rock_bottom" — happiness reached 0 at any point
            if dragon.happiness == 0 {
                tick_achievements.push("rock_bottom");
            }

            // Decay penalty stacks over time (1 stack removed every 6 ticks)
            if dragon.penalty_stacks > 0 {
                dragon.penalty_decay_timer += 1;
                if dragon.penalty_decay_timer >= 6 {
                    dragon.penalty_stacks -= 1;
                    dragon.penalty_decay_timer = 0;
                }
            } else {
                dragon.penalty_decay_timer = 0;
            }

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
                // "steady_hand" — happiness >= 60 for 20+ consecutive ticks in Phase 2
                if dragon.happiness >= 60 && dragon.phase2_ticks >= 20
                    && dragon.phase2_lowest_happiness >= 60
                {
                    tick_achievements.push("steady_hand");
                }
            }

            if !tick_achievements.is_empty() {
                awarded.push((dragon.current_owner_id.clone(), tick_achievements));
            }
        }

        // Deduplicate: only award if player doesn't already have it
        let mut result = Vec::new();
        for (player_id, achievements) in &awarded {
            if let Some(player) = self.players.get_mut(player_id) {
                for &ach in achievements {
                    if !player.achievements.iter().any(|a| a == ach) {
                        player.achievements.push(ach.to_string());
                        result.push((player_id.clone(), ach));
                    }
                }
            }
        }

        self.touch();
        result
    }

    /// Check and award end-of-phase achievements.
    /// Call before entering voting to finalize Phase 2 achievements.
    /// Returns (player_id, achievement_name) pairs.
    pub fn award_phase_end_achievements(&mut self) -> Vec<(String, &'static str)> {
        let mut result = Vec::new();

        for dragon in self.dragons.values() {
            let owner_id = &dragon.current_owner_id;
            let creator_id = &dragon.original_owner_id;

            // "no_mistakes" — 0 wrong actions and >= 5 total actions (Phase 2 sitter)
            if dragon.wrong_food_count == 0
                && dragon.wrong_play_count == 0
                && dragon.total_actions >= 5
            {
                if let Some(player) = self.players.get(owner_id) {
                    if !player.achievements.iter().any(|a| a == "no_mistakes") {
                        result.push((owner_id.clone(), "no_mistakes"));
                    }
                }
            }

            // "zen_master" — 0 penalty stacks at end of Phase 2, >= 8 total actions
            if dragon.penalty_stacks == 0 && dragon.total_actions >= 8 {
                if let Some(player) = self.players.get(owner_id) {
                    if !player.achievements.iter().any(|a| a == "zen_master") {
                        result.push((owner_id.clone(), "zen_master"));
                    }
                }
            }

            // "button_masher" — 5+ cooldown violations (spammer award)
            if dragon.cooldown_violations >= 5 {
                if let Some(player) = self.players.get(owner_id) {
                    if !player.achievements.iter().any(|a| a == "button_masher") {
                        result.push((owner_id.clone(), "button_masher"));
                    }
                }
            }

            // "helicopter_parent" — 20+ total actions (over-attentive caretaker)
            if dragon.total_actions >= 20 {
                if let Some(player) = self.players.get(owner_id) {
                    if !player.achievements.iter().any(|a| a == "helicopter_parent") {
                        result.push((owner_id.clone(), "helicopter_parent"));
                    }
                }
            }

            // "comeback_kid" — lowest happiness <= 15 but ended >= 70 (epic recovery)
            if dragon.phase2_lowest_happiness <= 15 && dragon.happiness >= 70 {
                if let Some(player) = self.players.get(owner_id) {
                    if !player.achievements.iter().any(|a| a == "comeback_kid") {
                        result.push((owner_id.clone(), "comeback_kid"));
                    }
                }
            }

            // "chaos_gremlin" — peak penalty stacks reached 4+ (maximum chaos)
            if dragon.peak_penalty_stacks >= 4 {
                if let Some(player) = self.players.get(owner_id) {
                    if !player.achievements.iter().any(|a| a == "chaos_gremlin") {
                        result.push((owner_id.clone(), "chaos_gremlin"));
                    }
                }
            }

            // "perfectionist" — >= 80% correct action ratio with >= 10 total actions
            if dragon.total_actions >= 10
                && dragon.correct_actions * 100 >= dragon.total_actions * 80
            {
                if let Some(player) = self.players.get(owner_id) {
                    if !player.achievements.iter().any(|a| a == "perfectionist") {
                        result.push((owner_id.clone(), "perfectionist"));
                    }
                }
            }

            // "speed_learner" — found correct food AND play within first 3 total actions
            // (Phase 1 only — check creator)
            if dragon.original_owner_id == dragon.current_owner_id {
                // Phase 1 still going, skip
                continue;
            }
            // This was a Phase 1 thing — we check if creator's observations count is >= 2
            // and food_tries/play_tries were low. But we already lost Phase 1 counters at
            // phase2 reset. So speed_learner must be awarded inline during apply_action.
            // Skip here.
            let _ = creator_id;
        }

        // Apply awards
        for (player_id, ach) in &result {
            if let Some(player) = self.players.get_mut(player_id) {
                if !player.achievements.iter().any(|a| a == *ach) {
                    player.achievements.push(ach.to_string());
                }
            }
        }

        self.touch();
        result
    }

    pub fn phase_duration_minutes(&self, phase: Phase) -> u32 {
        match phase {
            Phase::Lobby => self.config.phase0_minutes,
            Phase::Phase1 => self.config.phase1_minutes,
            Phase::Handover | Phase::Phase2 => self.config.phase2_minutes,
            Phase::Voting | Phase::End => 0,
        }
    }

    pub fn phase_warning_threshold_seconds(&self) -> i32 {
        30
    }

    pub fn elapsed_phase_seconds(&self, now: DateTime<Utc>) -> i32 {
        (now - self.phase_started_at).num_seconds().max(0) as i32
    }

    pub fn remaining_phase_seconds(&self, now: DateTime<Utc>) -> Option<i32> {
        let duration_seconds = self.phase_duration_minutes(self.phase) as i32 * 60;
        if duration_seconds <= 0 {
            return None;
        }
        Some((duration_seconds - self.elapsed_phase_seconds(now)).max(0))
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

        if !voting
            .eligible_player_ids
            .iter()
            .any(|eligible| eligible == player_id)
        {
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

        // Scores start at 0; the LLM judge will fill them via apply_judge_scores.
        for player in self.players.values_mut() {
            player.score = 0;
        }

        self.touch();
        Ok(())
    }

    /// Apply judge evaluation scores to player scores.
    ///
    /// For each dragon evaluation:
    /// - `observation_score` is awarded to the dragon's **original owner** (Phase 1 sitter).
    /// - `care_score` is awarded to the dragon's **current owner** (Phase 2 sitter).
    ///
    /// Each player creates exactly one dragon and cares for exactly one other dragon,
    /// so the final score = observation_score + care_score.
    pub fn apply_judge_scores(
        &mut self,
        evaluations: &[(String, i32, i32)], // (dragon_id, observation_score, care_score)
    ) {
        // Reset scores first
        for player in self.players.values_mut() {
            player.score = 0;
        }

        for (dragon_id, observation_score, care_score) in evaluations {
            let Some(dragon) = self.dragons.get(dragon_id) else {
                continue;
            };
            let creator_id = dragon.original_owner_id.clone();
            let caretaker_id = dragon.current_owner_id.clone();

            if let Some(creator) = self.players.get_mut(&creator_id) {
                creator.score += observation_score;
            }
            if let Some(caretaker) = self.players.get_mut(&caretaker_id) {
                caretaker.score += care_score;
            }
        }

        self.touch();
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
        if let Some(current_host_id) = self.host_player_id.clone()
            && let Some(current_host) = self.players.get(&current_host_id)
            && (!prefer_connected || current_host.is_connected)
        {
            self.reconcile_host_flags(Some(current_host_id.clone()));
            return Some(current_host_id.clone());
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
    (6..18).contains(&hour)
}

fn default_pet_description(player_name: &str) -> String {
    format!("{player_name}'s workshop dragon")
}

fn random_dragon_name() -> String {
    const PREFIXES: &[&str] = &[
        "Ember", "Frost", "Shadow", "Storm", "Blaze", "Thorn", "Ivy", "Coral",
        "Ash", "Dusk", "Dawn", "Mist", "Flint", "Sage", "Onyx", "Pearl",
        "Rune", "Gale", "Cobalt", "Crimson", "Jade", "Amber", "Slate", "Breeze",
        "Cinder", "Spark", "Glimmer", "Dew", "Fern", "Vex",
    ];
    const SUFFIXES: &[&str] = &[
        "wing", "claw", "scale", "fang", "tail", "heart", "eye", "flame",
        "frost", "spark", "shade", "storm", "thorn", "bloom", "drift",
    ];
    let mut rng = rand::rng();
    let prefix = PREFIXES[rng.random_range(0..PREFIXES.len())];
    let suffix = SUFFIXES[rng.random_range(0..SUFFIXES.len())];
    format!("{prefix}{suffix}")
}

fn random_active_time() -> ActiveTime {
    if rand::rng().random_bool(0.5) {
        ActiveTime::Day
    } else {
        ActiveTime::Night
    }
}

fn random_food_type() -> FoodType {
    match rand::rng().random_range(0..3u32) {
        0 => FoodType::Meat,
        1 => FoodType::Fruit,
        _ => FoodType::Fish,
    }
}

fn random_food_type_excluding(_: Option<FoodType>) -> FoodType {
    random_food_type()
}

fn random_play_type() -> PlayType {
    match rand::rng().random_range(0..3u32) {
        0 => PlayType::Fetch,
        1 => PlayType::Puzzle,
        _ => PlayType::Music,
    }
}

fn random_play_type_excluding(_: Option<PlayType>) -> PlayType {
    random_play_type()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> WorkshopCreateConfig {
        WorkshopCreateConfig {
            phase0_minutes: 5,
            phase1_minutes: 10,
            phase2_minutes: 10,
        }
    }

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
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );

        let result = session.transition_to(Phase::Phase1);

        assert!(result.is_ok());
        assert_eq!(session.phase, Phase::Phase1);
    }

    #[test]
    fn rejects_invalid_lobby_to_end_transition() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );

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
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));

        assert_eq!(session.host_player_id.as_deref(), Some("p1"));
        assert!(session.players.get("p1").expect("player p1").is_host);
    }

    #[test]
    fn ensure_host_assigned_prefers_connected_player_when_requested() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
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
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );

        let host = session.ensure_host_assigned(true);

        assert_eq!(host, None);
        assert_eq!(session.host_player_id, None);
    }

    #[test]
    fn begin_phase1_assigns_dragons_and_resets_player_progress() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
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
        assert_eq!(
            session
                .players
                .get("p1")
                .and_then(|p| p.current_dragon_id.as_deref()),
            Some("dragon-a")
        );
        assert_eq!(
            session
                .players
                .get("p2")
                .and_then(|p| p.current_dragon_id.as_deref()),
            Some("dragon-b")
        );
        assert_eq!(session.players.get("p1").map(|p| p.score), Some(0));
        assert!(
            session
                .players
                .get("p1")
                .expect("player p1")
                .achievements
                .is_empty()
        );
        let dragon_a = session.dragons.get("dragon-a").expect("dragon a");
        assert!(!dragon_a.name.contains("player-p1"), "Dragon name should not contain player name");
        assert!(!dragon_a.name.is_empty(), "Dragon name should not be empty");
        assert_eq!(dragon_a.creator_instructions, "Curious cave dragon");
        assert!(dragon_a.discovery_observations.is_empty());
        let dragon_b = session.dragons.get("dragon-b").expect("dragon b");
        assert_eq!(dragon_b.creator_instructions, "player-p2's workshop dragon");
    }

    #[test]
    fn record_discovery_observation_keeps_last_six_entries() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
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
        assert_eq!(
            dragon.discovery_observations.first().map(String::as_str),
            Some("note-2")
        );
        assert_eq!(
            dragon.discovery_observations.last().map(String::as_str),
            Some("note-7")
        );
    }

    #[test]
    fn enter_voting_with_single_assigned_player_immediately_finalizes() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session
            .begin_phase1(&[Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "dragon-a".into(),
            }])
            .expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.transition_to(Phase::Phase2).expect("to phase2");

        let immediate_finalize = session.enter_voting().expect("enter voting");

        assert!(immediate_finalize);
        assert_eq!(session.phase, Phase::Voting);
        assert_eq!(
            session.voting.as_ref().map(|v| v.eligible_player_ids.len()),
            Some(0)
        );
    }

    #[test]
    fn reset_to_lobby_clears_runtime_player_state() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        let mut p1 = player("p1", true, 10);
        p1.is_ready = true;
        session.add_player(p1);
        session
            .begin_phase1(&[Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "dragon-a".into(),
            }])
            .expect("start phase1");
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
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", false, 10));
        session
            .begin_phase1(&[Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "dragon-a".into(),
            }])
            .expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");

        let result = session.enter_phase2().expect("enter phase2");

        assert_eq!(result.auto_filled_players, vec!["player-p1".to_string()]);
        let dragon = session.dragons.get("dragon-a").expect("dragon-a");
        assert_eq!(dragon.handover_tags.len(), 3);
        assert_eq!(session.phase, Phase::Phase2);
    }

    #[test]
    fn enter_phase2_rejects_connected_players_with_missing_tags() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session
            .begin_phase1(&[Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "dragon-a".into(),
            }])
            .expect("start phase1");
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
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session.add_player(player("p2", true, 20));
        session
            .begin_phase1(&[
                Phase1Assignment {
                    player_id: "p1".into(),
                    dragon_id: "dragon-a".into(),
                },
                Phase1Assignment {
                    player_id: "p2".into(),
                    dragon_id: "dragon-b".into(),
                },
            ])
            .expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]);
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]);

        let result = session.enter_phase2().expect("enter phase2");

        assert!(result.auto_filled_players.is_empty());
        assert_eq!(
            session
                .players
                .get("p1")
                .and_then(|p| p.current_dragon_id.as_deref()),
            Some("dragon-b")
        );
        assert_eq!(
            session
                .players
                .get("p2")
                .and_then(|p| p.current_dragon_id.as_deref()),
            Some("dragon-a")
        );
        assert_eq!(
            session
                .dragons
                .get("dragon-a")
                .map(|d| d.current_owner_id.as_str()),
            Some("p2")
        );
        assert_eq!(
            session
                .dragons
                .get("dragon-b")
                .map(|d| d.current_owner_id.as_str()),
            Some("p1")
        );
    }

    #[test]
    fn enter_phase2_single_player_keeps_same_dragon_and_updates_speech() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session
            .begin_phase1(&[Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "dragon-a".into(),
            }])
            .expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]);

        let result = session.enter_phase2().expect("enter phase2");

        assert!(result.auto_filled_players.is_empty());
        assert_eq!(
            session
                .players
                .get("p1")
                .and_then(|p| p.current_dragon_id.as_deref()),
            Some("dragon-a")
        );
        let dragon = session.dragons.get("dragon-a").expect("dragon-a");
        assert_eq!(dragon.current_owner_id, "p1");
        assert_eq!(
            dragon.speech.as_deref(),
            Some("New shift, same dragon. Time to document and support your own handoff.")
        );
    }

    #[test]
    fn submit_vote_rejects_ineligible_player() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session.add_player(player("p2", true, 20));
        session
            .begin_phase1(&[
                Phase1Assignment {
                    player_id: "p1".into(),
                    dragon_id: "dragon-a".into(),
                },
                Phase1Assignment {
                    player_id: "p2".into(),
                    dragon_id: "dragon-b".into(),
                },
            ])
            .expect("start phase1");
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
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session.add_player(player("p2", true, 20));
        session
            .begin_phase1(&[
                Phase1Assignment {
                    player_id: "p1".into(),
                    dragon_id: "dragon-a".into(),
                },
                Phase1Assignment {
                    player_id: "p2".into(),
                    dragon_id: "dragon-b".into(),
                },
            ])
            .expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]);
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]);
        session.enter_phase2().expect("enter phase2");
        session.enter_voting().expect("enter voting");

        let eligible_player = session
            .voting
            .as_ref()
            .and_then(|v| v.eligible_player_ids.first())
            .cloned()
            .expect("eligible player");
        let result = session.submit_vote(&eligible_player, "missing-dragon");

        assert_eq!(result, Err(DomainError::UnknownDragon));
    }

    #[test]
    fn submit_vote_rejects_vote_for_current_dragon() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session.add_player(player("p2", true, 20));
        session
            .begin_phase1(&[
                Phase1Assignment {
                    player_id: "p1".into(),
                    dragon_id: "dragon-a".into(),
                },
                Phase1Assignment {
                    player_id: "p2".into(),
                    dragon_id: "dragon-b".into(),
                },
            ])
            .expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]);
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]);
        session.enter_phase2().expect("enter phase2");
        session.enter_voting().expect("enter voting");

        let eligible_player = session
            .voting
            .as_ref()
            .and_then(|v| v.eligible_player_ids.first())
            .cloned()
            .expect("eligible player");
        let own_dragon = session
            .players
            .get(&eligible_player)
            .and_then(|p| p.current_dragon_id.clone())
            .expect("current dragon");
        let result = session.submit_vote(&eligible_player, &own_dragon);

        assert_eq!(result, Err(DomainError::SelfVoteForbidden));
    }

    #[test]
    fn finalize_voting_sets_end_phase_and_zeroes_scores() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session.add_player(player("p2", true, 20));
        session
            .begin_phase1(&[
                Phase1Assignment {
                    player_id: "p1".into(),
                    dragon_id: "dragon-a".into(),
                },
                Phase1Assignment {
                    player_id: "p2".into(),
                    dragon_id: "dragon-b".into(),
                },
            ])
            .expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]);
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]);
        session.enter_phase2().expect("enter phase2");
        session.enter_voting().expect("enter voting");

        let result = session.finalize_voting();

        assert!(result.is_ok());
        assert_eq!(session.phase, Phase::End);
        // Scores start at 0; the LLM judge fills them via apply_judge_scores.
        assert_eq!(session.players.get("p1").map(|p| p.score), Some(0));
        assert_eq!(session.players.get("p2").map(|p| p.score), Some(0));
    }

    #[test]
    fn apply_judge_scores_distributes_observation_and_care_scores() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session.add_player(player("p2", true, 20));
        session
            .begin_phase1(&[
                Phase1Assignment {
                    player_id: "p1".into(),
                    dragon_id: "dragon-a".into(),
                },
                Phase1Assignment {
                    player_id: "p2".into(),
                    dragon_id: "dragon-b".into(),
                },
            ])
            .expect("start phase1");
        session.transition_to(Phase::Handover).expect("to handover");
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]);
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]);
        session.enter_phase2().expect("enter phase2");
        session.enter_voting().expect("enter voting");
        session.finalize_voting().expect("finalize");

        // dragon-a: created by p1 (observation_score=70), now owned by p2 (care_score=80)
        // dragon-b: created by p2 (observation_score=60), now owned by p1 (care_score=90)
        let dragon_a_owner = session.dragons.get("dragon-a").map(|d| d.current_owner_id.clone());
        let dragon_b_owner = session.dragons.get("dragon-b").map(|d| d.current_owner_id.clone());

        // After shuffle: p1 created dragon-a, p2 created dragon-b
        // Phase 2 ownership is swapped (rotate-by-one)
        assert_eq!(
            session.dragons.get("dragon-a").map(|d| d.original_owner_id.clone()),
            Some("p1".to_string())
        );
        assert_eq!(
            session.dragons.get("dragon-b").map(|d| d.original_owner_id.clone()),
            Some("p2".to_string())
        );

        // The current owners should differ from original owners
        assert_ne!(dragon_a_owner.as_deref(), Some("p1"));
        assert_ne!(dragon_b_owner.as_deref(), Some("p2"));

        session.apply_judge_scores(&[
            ("dragon-a".into(), 70, 80),  // obs→p1(+70), care→current_owner(+80)
            ("dragon-b".into(), 60, 90),  // obs→p2(+60), care→current_owner(+90)
        ]);

        // p1 = observation_score for dragon-a (70) + care_score for dragon-b (90) = 160
        // p2 = observation_score for dragon-b (60) + care_score for dragon-a (80) = 140
        assert_eq!(session.players.get("p1").map(|p| p.score), Some(70 + 90));
        assert_eq!(session.players.get("p2").map(|p| p.score), Some(60 + 80));
    }

    #[test]
    fn feed_action_blocks_when_dragon_is_already_full() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session
            .begin_phase1(&[Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "dragon-a".into(),
            }])
            .expect("start phase1");
        // Set hunger high enough to trigger AlreadyFull block (>= 95)
        let dragon = session.dragons.get_mut("dragon-a").expect("dragon-a");
        dragon.hunger = 100;

        let outcome = session
            .apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .expect("apply action");

        assert_eq!(
            outcome,
            ActionOutcome::Blocked {
                reason: ActionBlockReason::AlreadyFull,
            }
        );
    }

    #[test]
    fn play_action_blocks_when_dragon_is_too_hungry() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session
            .begin_phase1(&[Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "dragon-a".into(),
            }])
            .expect("start phase1");
        let dragon = session.dragons.get_mut("dragon-a").expect("dragon-a");
        dragon.hunger = 10;

        let outcome = session
            .apply_action("p1", PlayerAction::Play(PlayType::Fetch))
            .expect("apply action");

        assert_eq!(
            outcome,
            ActionOutcome::Blocked {
                reason: ActionBlockReason::TooHungryToPlay,
            }
        );
    }

    #[test]
    fn sleep_action_blocks_when_dragon_is_too_awake() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session
            .begin_phase1(&[Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "dragon-a".into(),
            }])
            .expect("start phase1");
        // Set energy high enough to trigger TooAwakeToSleep block (>= 90)
        let dragon = session.dragons.get_mut("dragon-a").expect("dragon-a");
        dragon.energy = 100;

        let outcome = session
            .apply_action("p1", PlayerAction::Sleep)
            .expect("apply action");

        assert_eq!(
            outcome,
            ActionOutcome::Blocked {
                reason: ActionBlockReason::TooAwakeToSleep,
            }
        );
    }

    #[test]
    fn phase2_tick_uses_stronger_decay_multiplier() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session
            .begin_phase1(&[Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "dragon-a".into(),
            }])
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
        // decay_multiplier = 3 in Phase 2
        // hunger: 100 - 3 = 97
        // energy: 100 - (sleep_rate=1 * time_penalty=1 * 3) = 97
        // happiness: 100 - (base_decay=1 * 3) = 97
        assert_eq!(dragon.hunger, 97);
        assert_eq!(dragon.energy, 97);
        assert_eq!(dragon.happiness, 97);
    }
}
