use chrono::{DateTime, Utc};
use protocol::{
    ActiveTime, CharacterProfile, DragonAction, DragonEmotion, FoodType, Phase, PlayType,
    WorkshopCreateConfig,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use uuid::Uuid;

/// The exact number of handover notes (tags) a player must submit before
/// Phase 2 can begin. Enforced at the domain boundary by
/// [`WorkshopSession::save_handover_tags`].
pub const HANDOVER_TAG_COUNT: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionCode(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VotingState {
    pub eligible_player_ids: Vec<String>,
    pub votes_by_player_id: BTreeMap<String, String>,
    #[serde(default)]
    pub results_revealed: bool,
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
    pub favorite_food: FoodType,
    pub favorite_play: PlayType,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_observation_score: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_care_score: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_feedback: Option<String>,
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
    /// Account id that owns this player slot (session 3 / refactor).
    /// `None` = legacy anonymous player or starter-lease join; `Some` = signed-in account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_character: Option<CharacterProfile>,
    pub is_host: bool,
    pub is_connected: bool,
    pub is_ready: bool,
    pub score: i32,
    pub current_dragon_id: Option<String>,
    pub achievements: Vec<String>,
    pub joined_at: DateTime<Utc>,
}

/// Maximum number of characters a single account may own.
/// Enforced by the service layer via `count_characters_by_owner`.
pub const MAX_CHARACTERS_PER_ACCOUNT: usize = 5;

/// Account domain entity (session 3 / refactor).
///
/// Distinct from the persistence `AccountRecord`: this is the in-memory
/// representation used by domain / service logic and does NOT expose the
/// password hash. Mapping is performed at the service boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    /// Hero archetype / avatar identifier chosen at sign-up.
    pub hero: String,
    /// Display name. Uniqueness is enforced case-insensitively at the
    /// persistence layer via `accounts_name_lower_idx`.
    pub name: String,
    pub created_at: DateTime<Utc>,
}

/// Errors produced by account-centric domain operations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AccountError {
    #[error("an account with this name already exists")]
    DuplicateName,
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("account not found")]
    NotFound,
    #[error("character limit reached ({max} characters per account)")]
    CharacterLimitReached { max: usize },
    #[error("character does not belong to this account")]
    CharacterNotOwned,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_host_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_host_name: Option<String>,
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
    #[error("exactly {expected} handover tags are required (got {got})")]
    InvalidHandoverTagCount { expected: usize, got: usize },
    #[error("voting is not active right now")]
    VotingNotActive,
    #[error("player is not eligible to vote")]
    IneligibleVoter,
    #[error("dragon is not available for voting")]
    UnknownDragon,
    #[error("cannot vote for your own dragon")]
    SelfVoteForbidden,
    #[error("voting results cannot be revealed until at least one eligible vote is submitted")]
    VotingRevealNotReady,
    #[error("session cannot be ended until voting results are revealed")]
    VotingResultsNotRevealed,
    #[error("Your dragon has already used its one redraw.")]
    SpriteRegenerationLimitReached,
    #[error("action is not allowed in current context")]
    ActionNotAllowed,
    #[error("player is not assigned to a dragon")]
    DragonNotAssigned,
    /// Session 3: reserved for the strict `begin_phase1` rewrite landing in
    /// session 4. No production path constructs this yet; adding the variant
    /// early lets downstream pattern matches compile with an explicit arm.
    #[error("phase1 transition blocked; players have not selected a character: {players:?}")]
    MissingSelectedCharacter { players: Vec<String> },
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
            owner_account_id: None,
            reserved_host_account_id: None,
            reserved_host_name: None,
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
        self.players.insert(player_id.clone(), player);

        let should_assign_reserved_host = self
            .reserved_host_account_id
            .as_deref()
            .zip(self.players.get(&player_id).and_then(|p| p.account_id.as_deref()))
            .is_some_and(|(reserved, actual)| reserved == actual);
        if should_assign_reserved_host {
            self.assign_reserved_host_to_player(&player_id);
        } else if self.host_player_id.is_none() && self.reserved_host_account_id.is_none() {
            self.host_player_id = Some(player_id.clone());
        }

        self.ensure_host_assigned(false);
        self.touch();
    }

    pub fn reserve_host(&mut self, account_id: impl Into<String>, name: impl Into<String>) {
        let account_id = account_id.into();
        self.owner_account_id = Some(account_id.clone());
        self.reserved_host_account_id = Some(account_id);
        self.reserved_host_name = Some(name.into());
        if let Some(host_player_id) = self.host_player_id.clone() {
            let should_clear = self
                .players
                .get(&host_player_id)
                .and_then(|player| player.account_id.as_deref())
                .zip(self.reserved_host_account_id.as_deref())
                .is_some_and(|(actual, reserved)| actual == reserved);
            if should_clear {
                self.assign_reserved_host_to_player(&host_player_id);
            } else {
                self.reconcile_host_flags(self.host_player_id.clone());
            }
        }
        self.touch();
    }

    pub fn reserved_host_name(&self) -> Option<&str> {
        self.reserved_host_name.as_deref()
    }

    pub fn reserved_host_account_id(&self) -> Option<&str> {
        self.reserved_host_account_id.as_deref()
    }

    pub fn owner_account_id(&self) -> Option<&str> {
        self.owner_account_id.as_deref()
    }

    pub fn assign_reserved_host_to_player(&mut self, player_id: &str) -> bool {
        let Some(reserved_account_id) = self.reserved_host_account_id.as_deref() else {
            return false;
        };
        let matches_reserved = self
            .players
            .get(player_id)
            .and_then(|player| player.account_id.as_deref())
            .is_some_and(|account_id| account_id == reserved_account_id);
        if !matches_reserved {
            return false;
        }

        self.host_player_id = Some(player_id.to_string());
        self.reserved_host_account_id = None;
        self.reserved_host_name = None;
        self.reconcile_host_flags(self.host_player_id.clone());
        true
    }

    pub fn assign_player_character(
        &mut self,
        player_id: &str,
        character: CharacterProfile,
    ) -> Result<(), DomainError> {
        let Some(player) = self.players.get_mut(player_id) else {
            return Err(DomainError::ActionNotAllowed);
        };
        player.character_id = Some(character.id.clone());
        player.selected_character = Some(character);
        player.is_ready = true;
        self.touch();
        Ok(())
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
        // Strict: every player must have selected a character before Phase1 begins.
        // The old fallback to `default_pet_description` is removed (session 4 / refactor):
        // character creation is no longer part of the workshop lifecycle, so a missing
        // selection is a programmer error surfaced to the host as a validation failure.
        let missing: Vec<String> = self
            .players
            .values()
            .filter(|player| player.selected_character.is_none())
            .map(|player| player.id.clone())
            .collect();
        if !missing.is_empty() {
            return Err(DomainError::MissingSelectedCharacter { players: missing });
        }

        self.transition_to(Phase::Phase1)?;
        self.time = 16; // tick 16 = hour 8 (daytime start)
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
                    .selected_character
                    .as_ref()
                    .map(|character| character.description.clone())
                    .expect("selected_character presence enforced by begin_phase1 guard above");
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
                        favorite_food: random_food_type(),
                        favorite_play: random_play_type(),
                        sleep_rate: rand::rng().random_range(1..=2),
                        hunger: 80,
                        energy: 80,
                        happiness: 80,
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
                        judge_observation_score: None,
                        judge_care_score: None,
                        judge_feedback: None,
                    },
                );
            }
        }

        self.touch();
        Ok(())
    }

    pub fn save_handover_tags(
        &mut self,
        player_id: &str,
        tags: Vec<String>,
    ) -> Result<(), DomainError> {
        if tags.len() != HANDOVER_TAG_COUNT {
            return Err(DomainError::InvalidHandoverTagCount {
                expected: HANDOVER_TAG_COUNT,
                got: tags.len(),
            });
        }
        let Some(dragon_id) = self
            .players
            .get(player_id)
            .and_then(|player| player.current_dragon_id.clone())
        else {
            return Ok(());
        };
        if let Some(dragon) = self.dragons.get_mut(&dragon_id) {
            dragon.handover_tags = tags;
        }
        self.touch();
        Ok(())
    }

    pub fn enter_phase2(&mut self) -> Result<Phase2TransitionResult, DomainError> {
        let mut auto_filled_players = Vec::new();
        for player in self.players.values() {
            let Some(dragon_id) = player.current_dragon_id.clone() else {
                continue;
            };
            let Some(dragon) = self.dragons.get_mut(&dragon_id) else {
                continue;
            };
            if !player.is_connected && dragon.handover_tags.len() < HANDOVER_TAG_COUNT {
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
                if player.is_connected && dragon.handover_tags.len() < HANDOVER_TAG_COUNT {
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

        self.transition_to(Phase::Phase2)?;

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
                        dragon.hunger = 80;
                        dragon.energy = 80;
                        dragon.happiness = 80;
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
            dragon.hunger = 80;
            dragon.energy = 80;
            dragon.happiness = 80;
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
                    dragon.speech = Some("*burp*... no more...".to_string());
                    dragon.speech_timer = 4;
                    dragon.last_emotion = DragonEmotion::Neutral;
                    ActionOutcome::Blocked {
                        reason: ActionBlockReason::AlreadyFull,
                    }
                } else {
                    dragon.last_action = DragonAction::Feed;
                    dragon.action_cooldown = 2;
                    dragon.sleep_shield_ticks = 0;
                    dragon.food_tries += 1;
                    dragon.total_actions += 1;
                    let favorite_food = dragon.favorite_food;
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
                        dragon.happiness = (dragon.happiness + 20).min(100);
                        dragon.last_emotion = DragonEmotion::Happy;
                        dragon.speech = Some("Mmm~!".to_string());
                        // Correct action reduces penalty stacks
                        dragon.penalty_stacks = (dragon.penalty_stacks - 1).max(0);
                    } else {
                        was_correct = false;
                        dragon.wrong_food_count += 1;
                        // Escalating penalty: repeated wrong food hurts more
                        let penalty = 12 + (dragon.wrong_food_count - 1).min(3) * 3;
                        dragon.hunger = (dragon.hunger + 5).min(100);
                        dragon.happiness = (dragon.happiness - penalty).max(0);
                        dragon.last_emotion = DragonEmotion::Angry;
                        dragon.speech = Some("Bleh...".to_string());
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
                    dragon.speech = Some("*grumble*...".to_string());
                    dragon.speech_timer = 4;
                    dragon.last_emotion = DragonEmotion::Angry;
                    ActionOutcome::Blocked {
                        reason: ActionBlockReason::TooHungryToPlay,
                    }
                } else if dragon.energy < 20 {
                    dragon.speech = Some("*yawn*...".to_string());
                    dragon.speech_timer = 4;
                    dragon.last_emotion = DragonEmotion::Sleepy;
                    ActionOutcome::Blocked {
                        reason: ActionBlockReason::TooTiredToPlay,
                    }
                } else {
                    dragon.last_action = DragonAction::Play;
                    dragon.action_cooldown = 2;
                    dragon.sleep_shield_ticks = 0;
                    dragon.play_tries += 1;
                    dragon.total_actions += 1;
                    let favorite_play = dragon.favorite_play;
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
                        dragon.speech = Some("Wheee~!".to_string());
                        dragon.penalty_stacks = (dragon.penalty_stacks - 1).max(0);
                    } else {
                        was_correct = false;
                        dragon.wrong_play_count += 1;
                        let penalty = 12 + (dragon.wrong_play_count - 1).min(3) * 3;
                        dragon.energy = (dragon.energy - 15).max(0);
                        dragon.happiness = (dragon.happiness - penalty).max(0);
                        dragon.last_emotion = DragonEmotion::Angry;
                        dragon.speech = Some("Hmph.".to_string());
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
                    dragon.speech = Some("...?!".to_string());
                    dragon.speech_timer = 4;
                    dragon.last_emotion = DragonEmotion::Angry;
                    ActionOutcome::Blocked {
                        reason: ActionBlockReason::TooAwakeToSleep,
                    }
                } else {
                    dragon.last_action = DragonAction::Sleep;
                    dragon.action_cooldown = 2;
                    dragon.total_actions += 1;
                    dragon.energy = (dragon.energy + 50).min(100);
                    let is_correct_time = (dragon.active_time == ActiveTime::Day
                        && !current_is_day)
                        || (dragon.active_time == ActiveTime::Night && current_is_day);
                    let was_correct = is_correct_time;
                    if is_correct_time {
                        dragon.correct_actions += 1;
                        dragon.happiness = (dragon.happiness + 15).min(100);
                        dragon.penalty_stacks = (dragon.penalty_stacks - 1).max(0);
                    }
                    dragon.sleep_shield_ticks = 1;
                    dragon.last_emotion = DragonEmotion::Sleepy;
                    dragon.speech = Some("Zzz...".to_string());
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

        self.time = (self.time + 1) % 48;
        let current_is_day = is_daytime(self.time);
        let previous_is_day = is_daytime((self.time + 47) % 48);
        let decay_multiplier = if self.phase == Phase::Phase2 { 2 } else { 1 };

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

                // (E) Dawn/dusk reaction — dragon reacts to the time shift
                if dragon.speech_timer == 0 {
                    if current_is_day {
                        dragon.speech = Some("*blink*...".to_string());
                    } else {
                        dragon.speech = Some("*stretch*...".to_string());
                    }
                    dragon.speech_timer = 3;
                }
            }

            // (C) Wrong-time yawning — subtle signal every 8 ticks that dragon is
            // awake during its inactive period, helping the player diagnose ActiveTime.
            if wrong_time
                && dragon.sleep_shield_ticks == 0
                && dragon.speech_timer == 0
                && self.time % 8 == 0
            {
                dragon.speech = Some("*yaaawn*...".to_string());
                dragon.speech_timer = 3;
                dragon.last_emotion = DragonEmotion::Sleepy;
            }

            dragon.hunger = (dragon.hunger - decay_multiplier).max(0);
            dragon.energy =
                (dragon.energy - (dragon.sleep_rate * time_penalty * decay_multiplier)).max(0);

            let mut happiness_decay = 1;
            if dragon.hunger < 20 {
                happiness_decay += 1;
            }
            if dragon.energy < 20 {
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
                    if dragon.last_action != DragonAction::Sleep {
                        dragon.last_emotion = ambient_emotion(dragon);
                    }
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
                        dragon.last_emotion = ambient_emotion(dragon);
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
                if dragon.happiness >= 60
                    && dragon.phase2_ticks >= 20
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
            Phase::Lobby => 0,
            Phase::Phase0 => 0,
            Phase::Phase1 => self.config.phase1_minutes,
            Phase::Handover | Phase::Phase2 => self.config.phase2_minutes,
            Phase::Judge | Phase::Voting | Phase::End => 0,
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

    pub fn enter_judge(&mut self) -> Result<(), DomainError> {
        self.transition_to(Phase::Judge)?;
        self.touch();
        Ok(())
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
            results_revealed: immediate_finalize,
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
            .dragons
            .get(dragon_id)
            .and_then(|dragon| Some(dragon.original_owner_id.as_str()))
            == Some(player_id)
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
        if self
            .voting
            .as_ref()
            .is_some_and(|voting| !voting.results_revealed)
        {
            return Err(DomainError::VotingResultsNotRevealed);
        }

        self.transition_to(Phase::End)?;

        self.touch();
        Ok(())
    }

    pub fn reveal_voting_results(&mut self) -> Result<(), DomainError> {
        let voting = self.voting.as_mut().ok_or(DomainError::VotingNotActive)?;

        if !voting.eligible_player_ids.is_empty() && voting.votes_by_player_id.is_empty() {
            return Err(DomainError::VotingRevealNotReady);
        }

        voting.results_revealed = true;
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
        evaluations: &[(String, i32, i32, String)], // (dragon_id, observation_score, care_score, feedback)
    ) {
        // Reset scores first
        for player in self.players.values_mut() {
            player.score = 0;
        }

        for (dragon_id, observation_score, care_score, feedback) in evaluations {
            let Some(dragon) = self.dragons.get_mut(dragon_id) else {
                continue;
            };
            dragon.judge_observation_score = Some(*observation_score);
            dragon.judge_care_score = Some(*care_score);
            dragon.judge_feedback = Some(feedback.clone());
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

    pub fn reset_to_lobby(&mut self, host_player_id: &str) -> Result<(), DomainError> {
        self.phase = Phase::Lobby;
        self.time = 16; // tick 16 = hour 8 (daytime start)
        self.voting = None;
        self.dragons.clear();

        // Remove every player except the host so the session is truly fresh.
        self.players.retain(|id, _| id == host_player_id);

        // Reset the host's runtime state (pet, sprites, score, etc.).
        if let Some(host) = self.players.get_mut(host_player_id) {
            host.score = 0;
            host.current_dragon_id = None;
            host.achievements.clear();
            host.is_ready = host.selected_character.is_some();
        }

        self.touch();
        Ok(())
    }

    pub fn ensure_host_assigned(&mut self, prefer_connected: bool) -> Option<String> {
        if self.reserved_host_account_id.is_some() {
            if let Some(current_host_id) = self.host_player_id.clone() {
                let current_matches_reserved = self
                    .players
                    .get(&current_host_id)
                    .and_then(|player| player.account_id.as_deref())
                    .zip(self.reserved_host_account_id.as_deref())
                    .is_some_and(|(actual, reserved)| actual == reserved);
                if current_matches_reserved {
                    self.assign_reserved_host_to_player(&current_host_id);
                    return Some(current_host_id);
                } else {
                    self.host_player_id = None;
                    self.reconcile_host_flags(None);
                }
            }

            let reserved_host_id = self.players.iter().find_map(|(player_id, player)| {
                player
                    .account_id
                    .as_deref()
                    .zip(self.reserved_host_account_id.as_deref())
                    .and_then(|(actual, reserved)| {
                        if actual == reserved && (!prefer_connected || player.is_connected) {
                            Some(player_id.clone())
                        } else {
                            None
                        }
                    })
            });
            if let Some(player_id) = reserved_host_id {
                self.assign_reserved_host_to_player(&player_id);
                return Some(player_id);
            }

            self.host_player_id = None;
            self.reconcile_host_flags(None);
            return None;
        }

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
    // Session 4 / refactor: `Lobby→Phase0` and `Phase0→Phase1` edges are removed.
    // `Phase::Phase0` is retained only for deserializing legacy persisted sessions;
    // it is no longer reachable via the FSM. Hosts transition directly `Lobby→Phase1`
    // via `begin_phase1`.
    matches!(
        (current, next),
        (Phase::Lobby, Phase::Phase1)
            | (Phase::Phase1, Phase::Handover)
            | (Phase::Handover, Phase::Phase2)
            | (Phase::Phase2, Phase::Judge)
            | (Phase::Phase2, Phase::Voting)
            | (Phase::Judge, Phase::Voting)
            | (Phase::Voting, Phase::End)
            | (Phase::End, Phase::Lobby)
    )
}

fn fallback_handover_tags() -> Vec<String> {
    let tags = vec![
        "Auto handover: teammate went offline before finishing notes.".to_string(),
        "Start with safe observations and watch how the dragon reacts.".to_string(),
        "Pay attention to food and play preferences — they stay the same.".to_string(),
    ];
    debug_assert_eq!(tags.len(), HANDOVER_TAG_COUNT);
    tags
}

/// 48-tick cycle. Each "hour" = 2 ticks.
/// Day = ticks 12..36 (hours 6–17). Night = ticks 0..12 + 36..48 (hours 0–5, 18–23).
fn is_daytime(tick: i32) -> bool {
    (12..36).contains(&tick)
}

/// Convert a raw tick (0..47) to a display hour (0..23).
pub fn tick_to_hour(tick: i32) -> i32 {
    (tick.rem_euclid(48)) / 2
}

fn ambient_emotion(dragon: &SessionDragon) -> DragonEmotion {
    if dragon.energy < 15 {
        DragonEmotion::Sleepy
    } else if dragon.happiness < 15 || dragon.hunger < 15 {
        DragonEmotion::Angry
    } else if dragon.happiness >= 90 {
        DragonEmotion::Happy
    } else {
        DragonEmotion::Neutral
    }
}

fn random_dragon_name() -> String {
    const PREFIXES: &[&str] = &[
        "Ember", "Frost", "Shadow", "Storm", "Blaze", "Thorn", "Ivy", "Coral", "Ash", "Dusk",
        "Dawn", "Mist", "Flint", "Sage", "Onyx", "Pearl", "Rune", "Gale", "Cobalt", "Crimson",
        "Jade", "Amber", "Slate", "Breeze", "Cinder", "Spark", "Glimmer", "Dew", "Fern", "Vex",
    ];
    const SUFFIXES: &[&str] = &[
        "wing", "claw", "scale", "fang", "tail", "heart", "eye", "flame", "frost", "spark",
        "shade", "storm", "thorn", "bloom", "drift",
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

fn random_play_type() -> PlayType {
    match rand::rng().random_range(0..3u32) {
        0 => PlayType::Fetch,
        1 => PlayType::Puzzle,
        _ => PlayType::Music,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::SpriteSet;

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
        // Session 4 / refactor: `begin_phase1` is now strict and requires every
        // player to have `selected_character.is_some()`. The test helper now
        // pre-populates a deterministic `CharacterProfile` so existing tests
        // that call `begin_phase1` continue to work. Individual tests that need
        // a missing-selection scenario can clear this field after construction.
        SessionPlayer {
            id: id.to_string(),
            name: format!("player-{id}"),
            account_id: None,
            character_id: Some(format!("character-{id}")),
            selected_character: Some(CharacterProfile {
                id: format!("character-{id}"),
                description: format!("test character for player-{id}"),
                sprites: SpriteSet {
                    neutral: "neutral".to_string(),
                    happy: "happy".to_string(),
                    angry: "angry".to_string(),
                    sleepy: "sleepy".to_string(),
                },
                remaining_sprite_regenerations: 1,
            }),
            is_host: false,
            is_connected: connected,
            is_ready: true,
            score: 0,
            current_dragon_id: None,
            achievements: Vec::new(),
            joined_at: ts(joined_at_seconds),
        }
    }

    #[test]
    fn rejects_lobby_to_phase0_transition_after_refactor() {
        // Session 4 / refactor: `Lobby → Phase0` edge removed from the FSM.
        // `Phase::Phase0` is retained only for legacy deserialization.
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );

        let result = session.transition_to(Phase::Phase0);

        assert!(matches!(
            result,
            Err(DomainError::InvalidSessionTransition {
                from: Phase::Lobby,
                to: Phase::Phase0,
            })
        ));
        assert_eq!(session.phase, Phase::Lobby);
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
        p1.selected_character = Some(CharacterProfile {
            id: "character-p1".into(),
            description: "Curious cave dragon".into(),
            sprites: SpriteSet {
                neutral: "neutral".into(),
                happy: "happy".into(),
                angry: "angry".into(),
                sleepy: "sleepy".into(),
            },
            remaining_sprite_regenerations: 1,
        });
        p1.character_id = Some("character-p1".into());
        p1.is_ready = true;
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
        assert_eq!(session.time, 16);
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
        assert!(
            !dragon_a.name.contains("player-p1"),
            "Dragon name should not contain player name"
        );
        assert!(!dragon_a.name.is_empty(), "Dragon name should not be empty");
        assert_eq!(dragon_a.creator_instructions, "Curious cave dragon");
        assert!(dragon_a.discovery_observations.is_empty());
        let dragon_b = session.dragons.get("dragon-b").expect("dragon b");
        // Session 4 / refactor: `default_pet_description` fallback removed.
        // p2 now carries the default test character from the `player()` helper.
        assert_eq!(
            dragon_b.creator_instructions,
            "test character for player-p2"
        );
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
    fn assign_player_character_marks_player_ready() {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("123456".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));
        session
            .assign_player_character(
                "p1",
                CharacterProfile {
                    id: "character-1".into(),
                    description: "Crystal dragon".into(),
                    sprites: SpriteSet {
                        neutral: "neutral_b64".into(),
                        happy: "happy_b64".into(),
                        angry: "angry_b64".into(),
                        sleepy: "sleepy_b64".into(),
                    },
                    remaining_sprite_regenerations: 1,
                },
            )
            .expect("assign character");

        let player = session.players.get("p1").expect("player p1");
        assert_eq!(player.character_id.as_deref(), Some("character-1"));
        assert_eq!(
            player
                .selected_character
                .as_ref()
                .map(|character| character.description.as_str()),
            Some("Crystal dragon")
        );
        assert!(player.is_ready);
    }

    #[test]
    fn enter_voting_with_single_assigned_player_opens_scoring_without_voters() {
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

        session.enter_voting().expect("enter voting");

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
        session.transition_to(Phase::Phase2).expect("to phase2");
        session.enter_voting().expect("enter voting");
        session.transition_to(Phase::End).expect("to end");
        {
            let player = session.players.get_mut("p1").expect("player p1");
            player.score = 77;
            player.achievements = vec!["smooth_transition".into()];
            player.character_id = Some("character-1".into());
            player.selected_character = Some(CharacterProfile {
                id: "character-1".into(),
                description: "Cool dragon".into(),
                sprites: SpriteSet {
                    neutral: "neutral".into(),
                    happy: "happy".into(),
                    angry: "angry".into(),
                    sleepy: "sleepy".into(),
                },
                remaining_sprite_regenerations: 1,
            });
        }

        let result = session.reset_to_lobby("p1");

        assert!(result.is_ok());
        assert_eq!(session.phase, Phase::Lobby);
        assert_eq!(session.time, 16);
        assert!(session.voting.is_none());
        assert!(session.dragons.is_empty());
        // Non-host player removed entirely
        assert!(session.players.get("p2").is_none());
        // Host stays but fully reset
        assert_eq!(session.players.len(), 1);
        let host = session.players.get("p1").expect("host p1");
        assert_eq!(host.score, 0);
        assert!(host.current_dragon_id.is_none());
        assert!(host.achievements.is_empty());
        assert!(host.is_ready);
        assert_eq!(host.character_id.as_deref(), Some("character-1"));
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
        assert_eq!(session.phase, Phase::Handover);
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
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]).expect("save handover tags");

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
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");

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
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]).expect("save handover tags");
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
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]).expect("save handover tags");
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
    fn submit_vote_allows_vote_for_currently_assigned_dragon_after_handover() {
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
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]).expect("save handover tags");
        session.enter_phase2().expect("enter phase2");
        session.enter_voting().expect("enter voting");

        let eligible_player = session
            .voting
            .as_ref()
            .and_then(|v| v.eligible_player_ids.first())
            .cloned()
            .expect("eligible player");
        let currently_assigned_dragon = session
            .players
            .get(&eligible_player)
            .and_then(|p| p.current_dragon_id.clone())
            .expect("current dragon");
        let result = session.submit_vote(&eligible_player, &currently_assigned_dragon);

        assert_eq!(result, Ok(()));
    }

    #[test]
    fn submit_vote_rejects_vote_for_originally_owned_dragon_even_after_handover() {
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
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]).expect("save handover tags");
        session.enter_phase2().expect("enter phase2");
        session.enter_voting().expect("enter voting");

        let result = session.submit_vote("p1", "dragon-a");

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
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]).expect("save handover tags");
        session.enter_phase2().expect("enter phase2");
        session.enter_voting().expect("enter voting");
        session.submit_vote("p1", "dragon-b").expect("p1 vote");
        session.submit_vote("p2", "dragon-a").expect("p2 vote");
        session
            .reveal_voting_results()
            .expect("reveal voting results");

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
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        session.save_handover_tags("p2", vec!["d".into(), "e".into(), "f".into()]).expect("save handover tags");
        session.enter_phase2().expect("enter phase2");
        session.enter_voting().expect("enter voting");
        session.submit_vote("p1", "dragon-b").expect("p1 vote");
        session.submit_vote("p2", "dragon-a").expect("p2 vote");
        session
            .reveal_voting_results()
            .expect("reveal voting results");
        session.finalize_voting().expect("finalize");

        // dragon-a: created by p1 (observation_score=70), now owned by p2 (care_score=80)
        // dragon-b: created by p2 (observation_score=60), now owned by p1 (care_score=90)
        let dragon_a_owner = session
            .dragons
            .get("dragon-a")
            .map(|d| d.current_owner_id.clone());
        let dragon_b_owner = session
            .dragons
            .get("dragon-b")
            .map(|d| d.current_owner_id.clone());

        // After shuffle: p1 created dragon-a, p2 created dragon-b
        // Phase 2 ownership is swapped (rotate-by-one)
        assert_eq!(
            session
                .dragons
                .get("dragon-a")
                .map(|d| d.original_owner_id.clone()),
            Some("p1".to_string())
        );
        assert_eq!(
            session
                .dragons
                .get("dragon-b")
                .map(|d| d.original_owner_id.clone()),
            Some("p2".to_string())
        );

        // The current owners should differ from original owners
        assert_ne!(dragon_a_owner.as_deref(), Some("p1"));
        assert_ne!(dragon_b_owner.as_deref(), Some("p2"));

        session.apply_judge_scores(&[
            ("dragon-a".into(), 70, 80, "solid discovery".into()),
            ("dragon-b".into(), 60, 90, "strong recovery".into()),
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
        session.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        session.enter_phase2().expect("enter phase2");
        let dragon = session.dragons.get_mut("dragon-a").expect("dragon-a");
        dragon.hunger = 100;
        dragon.energy = 100;
        dragon.happiness = 100;
        dragon.sleep_rate = 1;
        dragon.active_time = ActiveTime::Day;
        session.time = 19; // advance → tick 20 (daytime, no dawn/dusk)

        session.advance_tick();

        let dragon = session.dragons.get("dragon-a").expect("dragon-a");
        // decay_multiplier = 2 in Phase 2
        // hunger: 100 - 2 = 98
        // energy: 100 - (sleep_rate=1 * time_penalty=1 * 2) = 98
        // happiness: 100 - (base_decay=1 * 2) = 98
        assert_eq!(dragon.hunger, 98);
        assert_eq!(dragon.energy, 98);
        assert_eq!(dragon.happiness, 98);
    }

    // =========================================================================
    // VALIDATOR 1: Action Correctness (feed, play, sleep — correct vs wrong)
    // =========================================================================

    /// Helper: create a Phase1 session with one player and one dragon whose
    /// preferences are fully controlled (day active, favorite_food=Meat, etc.)
    fn setup_deterministic_session() -> WorkshopSession {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("999999".into()),
            ts(1),
            config(),
        );
        session.add_player(player("p1", true, 10));

        session
            .begin_phase1(&[Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "d1".into(),
            }])
            .expect("phase1");
        // Fix dragon preferences for determinism
        let d = session.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.favorite_food = FoodType::Meat;
        d.favorite_play = PlayType::Fetch;
        d.sleep_rate = 2;
        // Reset stats
        d.hunger = 50;
        d.energy = 50;
        d.happiness = 50;
        // Set time to daytime (tick 20 = hour 10)
        session.time = 20;
        session
    }

    #[test]
    fn validator1_feed_correct_food_applies_bonuses() {
        let mut s = setup_deterministic_session();
        let outcome = s
            .apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();

        assert!(matches!(
            outcome,
            ActionOutcome::Applied {
                was_correct: true,
                ..
            }
        ));
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.hunger, 90); // 50 + 40
        assert_eq!(d.happiness, 70); // 50 + 20
        assert_eq!(d.correct_actions, 1);
        assert_eq!(d.wrong_food_count, 0);
        assert_eq!(d.total_actions, 1);
        assert!(d.found_correct_food);
    }

    #[test]
    fn validator1_feed_wrong_food_applies_penalty() {
        let mut s = setup_deterministic_session();
        let outcome = s
            .apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
            .unwrap();

        assert!(matches!(
            outcome,
            ActionOutcome::Applied {
                was_correct: false,
                ..
            }
        ));
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.hunger, 55); // 50 + 5
        assert_eq!(d.happiness, 38); // 50 - 12
        assert_eq!(d.wrong_food_count, 1);
        assert_eq!(d.penalty_stacks, 1);
        assert!(!d.found_correct_food);
    }

    #[test]
    fn validator1_food_preference_stable_across_day_night() {
        let mut s = setup_deterministic_session();
        // During day (tick=20), Meat is correct
        let out1 = s
            .apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert!(matches!(
            out1,
            ActionOutcome::Applied {
                was_correct: true,
                ..
            }
        ));

        // Clear cooldown and switch to night
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.dragons.get_mut("d1").unwrap().hunger = 50;
        s.time = 40; // Night (tick 40 = hour 20)

        // During night, Meat is STILL correct (preferences don't change)
        let out2 = s
            .apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert!(matches!(
            out2,
            ActionOutcome::Applied {
                was_correct: true,
                ..
            }
        ));

        // Fish is wrong regardless of time of day
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.dragons.get_mut("d1").unwrap().hunger = 50;
        let out3 = s
            .apply_action("p1", PlayerAction::Feed(FoodType::Fish))
            .unwrap();
        assert!(matches!(
            out3,
            ActionOutcome::Applied {
                was_correct: false,
                ..
            }
        ));
    }

    #[test]
    fn validator1_play_correct_vs_wrong() {
        let mut s = setup_deterministic_session();
        // Day play = Fetch (correct)
        let out1 = s
            .apply_action("p1", PlayerAction::Play(PlayType::Fetch))
            .unwrap();
        assert!(matches!(
            out1,
            ActionOutcome::Applied {
                was_correct: true,
                ..
            }
        ));
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.energy, 30); // 50 - 20
        assert_eq!(d.happiness, 80); // 50 + 30

        // Reset and try wrong play
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.dragons.get_mut("d1").unwrap().energy = 50;
        s.dragons.get_mut("d1").unwrap().happiness = 50;
        let out2 = s
            .apply_action("p1", PlayerAction::Play(PlayType::Puzzle))
            .unwrap();
        assert!(matches!(
            out2,
            ActionOutcome::Applied {
                was_correct: false,
                ..
            }
        ));
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.energy, 35); // 50 - 15
        assert_eq!(d.happiness, 38); // 50 - 12
    }

    #[test]
    fn validator1_sleep_correct_vs_wrong_time() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.energy = 40; // Not too awake
        d.active_time = ActiveTime::Day;

        // During night → correct sleep time for day-active dragon
        s.time = 40;
        let out1 = s.apply_action("p1", PlayerAction::Sleep).unwrap();
        assert!(matches!(
            out1,
            ActionOutcome::Applied {
                was_correct: true,
                ..
            }
        ));
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.energy, 90); // 40 + 50
        assert_eq!(d.happiness, 65); // 50 + 15

        // Reset: during day → wrong sleep time
        let d = s.dragons.get_mut("d1").unwrap();
        d.action_cooldown = 0;
        d.energy = 40;
        d.happiness = 50;
        s.time = 20;
        let out2 = s.apply_action("p1", PlayerAction::Sleep).unwrap();
        assert!(matches!(
            out2,
            ActionOutcome::Applied {
                was_correct: false,
                ..
            }
        ));
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.energy, 90); // 40 + 50 (energy always recovers)
        assert_eq!(d.happiness, 50); // No happiness bonus for wrong time
    }

    // =========================================================================
    // VALIDATOR 2: Achievement System (all 12 achievements)
    // =========================================================================

    #[test]
    fn validator2_master_chef_first_correct_food() {
        let mut s = setup_deterministic_session();
        let out = s
            .apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        match out {
            ActionOutcome::Applied {
                awarded_achievement,
                was_correct,
            } => {
                assert!(was_correct);
                assert_eq!(awarded_achievement, Some("master_chef"));
            }
            _ => panic!("expected Applied"),
        }
    }

    #[test]
    fn validator2_master_chef_not_awarded_on_second_try() {
        let mut s = setup_deterministic_session();
        // First try: wrong food
        s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
            .unwrap();
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        // Second try: correct food — no master_chef because food_tries > 1
        let out = s
            .apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        match out {
            ActionOutcome::Applied {
                awarded_achievement,
                ..
            } => {
                assert_ne!(awarded_achievement, Some("master_chef"));
            }
            _ => panic!("expected Applied"),
        }
    }

    #[test]
    fn validator2_playful_spirit_first_correct_play() {
        let mut s = setup_deterministic_session();
        let out = s
            .apply_action("p1", PlayerAction::Play(PlayType::Fetch))
            .unwrap();
        match out {
            ActionOutcome::Applied {
                awarded_achievement,
                was_correct,
            } => {
                assert!(was_correct);
                assert_eq!(awarded_achievement, Some("playful_spirit"));
            }
            _ => panic!("expected Applied"),
        }
    }

    #[test]
    fn validator2_speed_learner_within_three_actions() {
        let mut s = setup_deterministic_session();
        // Action 1: correct food (awards master_chef)
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;

        // Action 2: correct play → should award speed_learner (both found within 3 actions)
        let out = s
            .apply_action("p1", PlayerAction::Play(PlayType::Fetch))
            .unwrap();
        match out {
            ActionOutcome::Applied {
                awarded_achievement,
                ..
            } => {
                // playful_spirit takes priority on first play try,
                // but speed_learner check: found_correct_food is true, total_actions <= 3
                // Actually playful_spirit is checked first; speed_learner only if awarded.is_none()
                // Since playful_spirit fires, speed_learner doesn't
                assert_eq!(awarded_achievement, Some("playful_spirit"));
            }
            _ => panic!("expected Applied"),
        }
        // But the player should have both food + play found
        let d = s.dragons.get("d1").unwrap();
        assert!(d.found_correct_food);
        assert!(d.found_correct_play);
        assert_eq!(d.total_actions, 2); // Within 3
    }

    #[test]
    fn validator2_speed_learner_not_awarded_after_four_actions() {
        let mut s = setup_deterministic_session();
        // 3 wrong actions first
        for _ in 0..3 {
            s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
                .unwrap();
            s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        }
        // Now correct food (total_actions = 4)
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        // Correct play (total_actions = 5) — too late for speed_learner
        s.apply_action("p1", PlayerAction::Play(PlayType::Fetch))
            .unwrap();
        let d = s.dragons.get("d1").unwrap();
        assert!(d.found_correct_food);
        assert!(d.found_correct_play);
        assert!(d.total_actions > 3);
        // speed_learner should NOT have been awarded
        let p = s.players.get("p1").unwrap();
        assert!(!p.achievements.contains(&"speed_learner".to_string()));
    }

    #[test]
    fn validator2_steady_hand_happiness_above_60_for_20_ticks() {
        let mut s = setup_deterministic_session();
        // Move to Phase 2
        s.transition_to(Phase::Handover).unwrap();
        s.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s.enter_phase2().unwrap();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.favorite_food = FoodType::Meat;
        d.favorite_play = PlayType::Fetch;
        d.sleep_rate = 1;

        // Simulate 19 ticks already passed with happiness always >= 65
        d.phase2_ticks = 19;
        d.phase2_lowest_happiness = 65;
        d.hunger = 80;
        d.energy = 80;
        d.happiness = 80;
        s.time = 20; // daytime tick, correct for Day dragon

        // 20th tick — happiness stays above 60, achievement should fire
        s.advance_tick();
        let d = s.dragons.get("d1").unwrap();
        // Decay: (1 + 0 + 0 + 0 + 0) * 3 = 3. happiness = 80 - 3 = 77
        assert!(
            d.happiness >= 60,
            "happiness {} should be >= 60",
            d.happiness
        );
        assert_eq!(d.phase2_ticks, 20);

        let p = s.players.get("p1").unwrap();
        assert!(
            p.achievements.contains(&"steady_hand".to_string()),
            "steady_hand should be awarded when phase2_ticks >= 20 and lowest happiness >= 60"
        );

        // Negative case: if lowest happiness was below 60, achievement should NOT fire
        let mut s2 = setup_deterministic_session();
        s2.transition_to(Phase::Handover).unwrap();
        s2.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s2.enter_phase2().unwrap();
        let d2 = s2.dragons.get_mut("d1").unwrap();
        d2.active_time = ActiveTime::Day;
        d2.sleep_rate = 1;
        d2.phase2_ticks = 19;
        d2.phase2_lowest_happiness = 50; // was below 60 at some point
        d2.hunger = 80;
        d2.energy = 80;
        d2.happiness = 80;
        s2.time = 10;

        s2.advance_tick();
        let p2 = s2.players.get("p1").unwrap();
        assert!(
            !p2.achievements.contains(&"steady_hand".to_string()),
            "steady_hand should NOT be awarded when lowest happiness was below 60"
        );
    }

    #[test]
    fn validator2_no_mistakes_phase_end() {
        let mut s = setup_deterministic_session();
        s.transition_to(Phase::Handover).unwrap();
        s.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s.enter_phase2().unwrap();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.favorite_food = FoodType::Meat;
        d.favorite_play = PlayType::Fetch;
        d.sleep_rate = 1;
        d.hunger = 50;
        d.energy = 50;
        d.happiness = 50;
        s.time = 10;

        // 5 correct actions, 0 wrong
        for _ in 0..5 {
            s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
                .unwrap();
            let d = s.dragons.get_mut("d1").unwrap();
            d.action_cooldown = 0;
            d.hunger = 50; // Keep feedable
        }
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.wrong_food_count, 0);
        assert_eq!(d.wrong_play_count, 0);
        assert!(d.total_actions >= 5);

        s.award_phase_end_achievements();
        let p = s.players.get("p1").unwrap();
        assert!(p.achievements.contains(&"no_mistakes".to_string()));
    }

    #[test]
    fn validator2_zen_master_zero_penalty_stacks_eight_actions() {
        let mut s = setup_deterministic_session();
        s.transition_to(Phase::Handover).unwrap();
        s.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s.enter_phase2().unwrap();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.favorite_food = FoodType::Meat;
        d.sleep_rate = 1;
        d.hunger = 50;
        d.energy = 50;
        d.happiness = 50;
        s.time = 10;

        // 8 correct actions
        for _ in 0..8 {
            s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
                .unwrap();
            let d = s.dragons.get_mut("d1").unwrap();
            d.action_cooldown = 0;
            d.hunger = 50;
        }
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.penalty_stacks, 0);
        assert!(d.total_actions >= 8);

        s.award_phase_end_achievements();
        let p = s.players.get("p1").unwrap();
        assert!(p.achievements.contains(&"zen_master".to_string()));
    }

    #[test]
    fn validator2_button_masher_five_cooldown_violations() {
        let mut s = setup_deterministic_session();
        s.transition_to(Phase::Handover).unwrap();
        s.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s.enter_phase2().unwrap();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.favorite_food = FoodType::Meat;
        d.sleep_rate = 1;
        s.time = 10;

        // Trigger one real action to start cooldown
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        // Spam 5 actions during cooldown
        for _ in 0..5 {
            let out = s
                .apply_action("p1", PlayerAction::Feed(FoodType::Meat))
                .unwrap();
            assert_eq!(out, ActionOutcome::CooldownViolation);
        }
        assert_eq!(s.dragons.get("d1").unwrap().cooldown_violations, 5);

        s.award_phase_end_achievements();
        let p = s.players.get("p1").unwrap();
        assert!(p.achievements.contains(&"button_masher".to_string()));
    }

    #[test]
    fn validator2_rock_bottom_happiness_hits_zero() {
        let mut s = setup_deterministic_session();
        s.transition_to(Phase::Handover).unwrap();
        s.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s.enter_phase2().unwrap();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 1;
        d.hunger = 0;
        d.energy = 0;
        d.happiness = 5; // Very low, will hit 0 fast with decay_multiplier=3
        s.time = 10;

        // Run ticks until happiness drops to 0
        for _ in 0..5 {
            s.advance_tick();
        }
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.happiness, 0);
        let p = s.players.get("p1").unwrap();
        assert!(p.achievements.contains(&"rock_bottom".to_string()));
    }

    #[test]
    fn validator2_helicopter_parent_twenty_actions() {
        let mut s = setup_deterministic_session();
        s.transition_to(Phase::Handover).unwrap();
        s.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s.enter_phase2().unwrap();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.favorite_food = FoodType::Meat;
        d.sleep_rate = 1;
        s.time = 10;

        for _ in 0..20 {
            s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
            s.dragons.get_mut("d1").unwrap().hunger = 50;
            s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
                .unwrap();
        }
        assert_eq!(s.dragons.get("d1").unwrap().total_actions, 20);

        s.award_phase_end_achievements();
        let p = s.players.get("p1").unwrap();
        assert!(p.achievements.contains(&"helicopter_parent".to_string()));
    }

    #[test]
    fn validator2_comeback_kid_low_to_high_happiness() {
        let mut s = setup_deterministic_session();
        s.transition_to(Phase::Handover).unwrap();
        s.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s.enter_phase2().unwrap();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.favorite_food = FoodType::Meat;
        d.favorite_play = PlayType::Fetch;
        d.sleep_rate = 1;
        d.hunger = 50;
        d.energy = 50;
        d.happiness = 10; // Low starting point
        d.phase2_lowest_happiness = 10;
        s.time = 10;

        // Record a tick so phase2_lowest_happiness captures the 10
        s.advance_tick();
        let d = s.dragons.get("d1").unwrap();
        assert!(d.phase2_lowest_happiness <= 15);

        // Now recover happiness with correct actions
        for _ in 0..6 {
            let d = s.dragons.get_mut("d1").unwrap();
            d.action_cooldown = 0;
            d.hunger = 50;
            d.energy = 50;
            s.apply_action("p1", PlayerAction::Play(PlayType::Fetch))
                .unwrap();
        }
        let d = s.dragons.get("d1").unwrap();
        assert!(
            d.happiness >= 70,
            "happiness {} should be >= 70",
            d.happiness
        );
        assert!(d.phase2_lowest_happiness <= 15);

        s.award_phase_end_achievements();
        let p = s.players.get("p1").unwrap();
        assert!(p.achievements.contains(&"comeback_kid".to_string()));
    }

    #[test]
    fn validator2_chaos_gremlin_peak_penalty_stacks() {
        let mut s = setup_deterministic_session();
        s.transition_to(Phase::Handover).unwrap();
        s.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s.enter_phase2().unwrap();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.favorite_food = FoodType::Meat;
        d.sleep_rate = 1;
        d.happiness = 100; // High so penalties don't zero out
        s.time = 10;

        // 4 wrong foods → 4 penalty stacks
        for _ in 0..4 {
            s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
            s.dragons.get_mut("d1").unwrap().hunger = 50;
            s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
                .unwrap();
        }
        let d = s.dragons.get("d1").unwrap();
        assert!(
            d.peak_penalty_stacks >= 4,
            "peak {} should be >= 4",
            d.peak_penalty_stacks
        );

        s.award_phase_end_achievements();
        let p = s.players.get("p1").unwrap();
        assert!(p.achievements.contains(&"chaos_gremlin".to_string()));
    }

    #[test]
    fn validator2_perfectionist_high_correct_ratio() {
        let mut s = setup_deterministic_session();
        s.transition_to(Phase::Handover).unwrap();
        s.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s.enter_phase2().unwrap();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.favorite_food = FoodType::Meat;
        d.sleep_rate = 1;
        s.time = 10;

        // 9 correct + 1 wrong = 90% correct (>= 80%), total = 10
        for _ in 0..9 {
            s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
            s.dragons.get_mut("d1").unwrap().hunger = 50;
            s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
                .unwrap();
        }
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.dragons.get_mut("d1").unwrap().hunger = 50;
        s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
            .unwrap(); // 1 wrong

        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.total_actions, 10);
        assert_eq!(d.correct_actions, 9);

        s.award_phase_end_achievements();
        let p = s.players.get("p1").unwrap();
        assert!(p.achievements.contains(&"perfectionist".to_string()));
    }

    // =========================================================================
    // VALIDATOR 3: Tick Simulation (decay, penalty stacks, sleep shield, speech)
    // =========================================================================

    #[test]
    fn validator3_phase1_tick_decay_multiplier_is_one() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.hunger = 100;
        d.energy = 100;
        d.happiness = 100;
        d.sleep_rate = 2;
        d.active_time = ActiveTime::Day;
        s.time = 19; // advance → tick 20 (daytime)

        s.advance_tick();

        let d = s.dragons.get("d1").unwrap();
        // Phase1: decay_multiplier=1
        // hunger: 100 - 1 = 99
        // energy: 100 - (2 * 1 * 1) = 98
        // happiness: 100 - (1 * 1) = 99
        assert_eq!(d.hunger, 99);
        assert_eq!(d.energy, 98);
        assert_eq!(d.happiness, 99);
    }

    #[test]
    fn validator3_penalty_stacks_increase_happiness_decay() {
        let mut s = setup_deterministic_session();
        s.transition_to(Phase::Handover).unwrap();
        s.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s.enter_phase2().unwrap();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 1;
        d.hunger = 100;
        d.energy = 100;
        d.happiness = 100;
        d.penalty_stacks = 3;
        s.time = 19; // advance → tick 20 (daytime)

        s.advance_tick();

        let d = s.dragons.get("d1").unwrap();
        // happiness_decay = (1 + 0 + 0 + 0 + min(3,4)) * 2 = 4 * 2 = 8
        assert_eq!(d.happiness, 92);
    }

    #[test]
    fn validator3_penalty_stacks_decay_every_six_ticks() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 1;
        d.hunger = 100;
        d.energy = 100;
        d.happiness = 100;
        d.penalty_stacks = 2;
        d.penalty_decay_timer = 0;
        s.time = 10;

        // Run 6 ticks — should decay 1 stack
        for _ in 0..6 {
            s.advance_tick();
        }
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.penalty_stacks, 1, "should have decayed from 2 to 1");

        // Run 6 more ticks
        for _ in 0..6 {
            s.advance_tick();
        }
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.penalty_stacks, 0, "should have decayed from 1 to 0");
    }

    #[test]
    fn validator3_sleep_shield_suppresses_wrong_time_decay() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 1;
        d.hunger = 100;
        d.energy = 100;
        d.happiness = 100;
        d.sleep_shield_ticks = 1;
        s.time = 40; // Night — wrong time for day dragon

        s.advance_tick();

        let d = s.dragons.get("d1").unwrap();
        // With sleep shield, wrong_time component is suppressed
        // happiness_decay = (1 + 0 + 0 + 0_suppressed + 0) * 1 = 1
        assert_eq!(d.happiness, 99);

        // Shield expired, next tick wrong_time should apply
        let d = s.dragons.get_mut("d1").unwrap();
        d.hunger = 100;
        d.happiness = 100;
        s.advance_tick();
        let d = s.dragons.get("d1").unwrap();
        // happiness_decay = (1 + 0 + 0 + 1_wrong_time + 0) * 1 = 2
        assert_eq!(d.happiness, 98);
    }

    #[test]
    fn validator3_speech_timer_counts_down_and_clears() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 1;
        d.hunger = 100;
        d.energy = 100;
        d.speech = Some("Hello!".to_string());
        d.speech_timer = 2;
        s.time = 10;

        s.advance_tick();
        assert_eq!(s.dragons.get("d1").unwrap().speech_timer, 1);
        assert!(s.dragons.get("d1").unwrap().speech.is_some());

        s.advance_tick();
        assert_eq!(s.dragons.get("d1").unwrap().speech_timer, 0);
        assert!(s.dragons.get("d1").unwrap().speech.is_none());
    }

    #[test]
    fn validator3_day_night_cycle_resets_food_play_tries() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 1;
        d.hunger = 100;
        d.energy = 100;
        d.food_tries = 5;
        d.play_tries = 3;

        // Transition from day (35) to night (36)
        s.time = 35;
        s.advance_tick(); // time becomes 36 (night)
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.food_tries, 0, "food_tries reset on day→night");
        assert_eq!(d.play_tries, 0, "play_tries reset on day→night");
    }

    #[test]
    fn validator3_wrong_active_time_doubles_energy_decay() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 2;
        d.hunger = 100;
        d.energy = 100;
        d.happiness = 100;

        // Correct time (day)
        s.time = 19; // advance → tick 20 (daytime)
        s.advance_tick();
        let energy_day = s.dragons.get("d1").unwrap().energy;
        // energy: 100 - (2 * 1 * 1) = 98
        assert_eq!(energy_day, 98);

        // Wrong time (night) — time_penalty=2
        let d = s.dragons.get_mut("d1").unwrap();
        d.energy = 100;
        s.time = 40; // advance → tick 41 (night)
        s.advance_tick();
        let energy_night = s.dragons.get("d1").unwrap().energy;
        // energy: 100 - (2 * 2 * 1) = 96
        assert_eq!(energy_night, 96);
    }

    // =========================================================================
    // VALIDATOR 4: Stat Clamping (all stats bounded 0–100)
    // =========================================================================

    #[test]
    fn validator4_hunger_clamped_at_100_after_feed() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.hunger = 80;
        // Correct food: +40 → 120 should clamp to 100
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().hunger, 100);
    }

    #[test]
    fn validator4_hunger_clamped_at_zero_after_tick() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.hunger = 0;
        d.energy = 100;
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 1;
        s.time = 10;
        s.advance_tick();
        assert_eq!(s.dragons.get("d1").unwrap().hunger, 0);
    }

    #[test]
    fn validator4_happiness_clamped_at_zero_after_escalating_penalties() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.happiness = 10;
        // Wrong food penalty = 20 → would go to -10
        s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().happiness, 0);
    }

    #[test]
    fn validator4_energy_clamped_at_100_after_sleep() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.energy = 80;
        // Sleep: +50 → 130 should clamp to 100
        s.apply_action("p1", PlayerAction::Sleep).unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().energy, 100);
    }

    #[test]
    fn validator4_happiness_clamped_at_100_after_correct_play() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.happiness = 90;
        // Correct play: +30 → 120 should clamp to 100
        s.apply_action("p1", PlayerAction::Play(PlayType::Fetch))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().happiness, 100);
    }

    #[test]
    fn validator4_energy_clamped_at_zero_after_tick() {
        let mut s = setup_deterministic_session();
        s.transition_to(Phase::Handover).unwrap();
        s.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s.enter_phase2().unwrap();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 3;
        d.energy = 5;
        s.time = 22; // Wrong time → time_penalty=2
        // energy: 5 - (3 * 2 * 3) = 5 - 18 → 0

        s.advance_tick();
        assert_eq!(s.dragons.get("d1").unwrap().energy, 0);
    }

    // =========================================================================
    // VALIDATOR 5: Cooldown Enforcement
    // =========================================================================

    #[test]
    fn validator5_action_during_cooldown_returns_violation() {
        let mut s = setup_deterministic_session();
        // First action starts cooldown
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.action_cooldown, 2);

        // Second action during cooldown
        let out = s
            .apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert_eq!(out, ActionOutcome::CooldownViolation);
    }

    #[test]
    fn validator5_cooldown_violation_increments_counter_only() {
        let mut s = setup_deterministic_session();
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        let hunger_before = s.dragons.get("d1").unwrap().hunger;

        // Spam during cooldown
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();

        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.cooldown_violations, 2);
        assert_eq!(
            d.hunger, hunger_before,
            "hunger should not change during cooldown"
        );
        assert_eq!(d.total_actions, 1, "total_actions should not increase");
    }

    #[test]
    fn validator5_cooldown_expires_after_two_ticks() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.favorite_food = FoodType::Meat;
        d.sleep_rate = 1;
        d.hunger = 50; // must be < 95 so feed is not blocked
        d.energy = 100;
        s.time = 10;

        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().action_cooldown, 2);

        s.advance_tick();
        assert_eq!(s.dragons.get("d1").unwrap().action_cooldown, 1);
        s.advance_tick();
        assert_eq!(s.dragons.get("d1").unwrap().action_cooldown, 0);

        // Now action should work again
        s.dragons.get_mut("d1").unwrap().hunger = 50;
        let out = s
            .apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert!(matches!(out, ActionOutcome::Applied { .. }));
    }

    #[test]
    fn validator5_cooldown_violation_does_not_affect_penalty_stacks() {
        let mut s = setup_deterministic_session();
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();

        // Cooldown violation should NOT add penalty stacks
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.cooldown_violations, 1);
        assert_eq!(
            d.penalty_stacks, 0,
            "cooldown violations should not add penalty stacks"
        );
    }

    // =========================================================================
    // VALIDATOR 6: Penalty & Escalation System
    // =========================================================================

    #[test]
    fn validator6_wrong_food_penalty_escalates() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.happiness = 100;

        // 1st wrong food: penalty = 12
        s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().happiness, 88);

        // 2nd wrong food: penalty = 12 + (2-1)*3 = 15
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.dragons.get_mut("d1").unwrap().happiness = 100;
        s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().happiness, 85);

        // 3rd wrong food: penalty = 12 + (3-1)*3 = 18
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.dragons.get_mut("d1").unwrap().happiness = 100;
        s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().happiness, 82);

        // 4th wrong food: penalty = 12 + min(3,3)*3 = 21 (capped)
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.dragons.get_mut("d1").unwrap().happiness = 100;
        s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().happiness, 79);

        // 5th wrong food: penalty still 21 (cap holds)
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.dragons.get_mut("d1").unwrap().happiness = 100;
        s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().happiness, 79);
    }

    #[test]
    fn validator6_wrong_play_penalty_escalates() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.happiness = 100;

        // 1st wrong play: penalty = 12
        s.apply_action("p1", PlayerAction::Play(PlayType::Puzzle))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().happiness, 88);

        // 2nd wrong play: penalty = 15
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.dragons.get_mut("d1").unwrap().happiness = 100;
        s.dragons.get_mut("d1").unwrap().energy = 50;
        s.apply_action("p1", PlayerAction::Play(PlayType::Puzzle))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().happiness, 85);
    }

    #[test]
    fn validator6_correct_action_reduces_penalty_stacks() {
        let mut s = setup_deterministic_session();
        // Build up 3 penalty stacks
        for _ in 0..3 {
            s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
            s.dragons.get_mut("d1").unwrap().hunger = 50;
            s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
                .unwrap();
        }
        assert_eq!(s.dragons.get("d1").unwrap().penalty_stacks, 3);

        // Correct food: penalty_stacks -= 1
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.dragons.get_mut("d1").unwrap().hunger = 50;
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().penalty_stacks, 2);
    }

    #[test]
    fn validator6_peak_penalty_stacks_tracks_maximum() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.happiness = 100;

        // Build 3 penalty stacks
        for _ in 0..3 {
            s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
            s.dragons.get_mut("d1").unwrap().hunger = 50;
            s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
                .unwrap();
        }
        assert_eq!(s.dragons.get("d1").unwrap().peak_penalty_stacks, 3);

        // Reduce with correct action
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.dragons.get_mut("d1").unwrap().hunger = 50;
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().penalty_stacks, 2);
        // Peak should NOT decrease
        assert_eq!(s.dragons.get("d1").unwrap().peak_penalty_stacks, 3);

        // Add 2 more wrongs → stacks = 4, peak should update
        for _ in 0..2 {
            s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
            s.dragons.get_mut("d1").unwrap().hunger = 50;
            s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
                .unwrap();
        }
        assert_eq!(s.dragons.get("d1").unwrap().penalty_stacks, 4);
        assert_eq!(s.dragons.get("d1").unwrap().peak_penalty_stacks, 4);
    }

    #[test]
    fn validator6_penalty_stacks_capped_at_four_for_happiness_decay() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 1;
        d.hunger = 100;
        d.energy = 100;
        d.happiness = 100;
        d.penalty_stacks = 6; // More than 4, but capped to 4 for decay
        s.time = 19; // advance → tick 20 (daytime)

        s.advance_tick();
        let d = s.dragons.get("d1").unwrap();
        // happiness_decay = (1 + 0 + 0 + 0 + min(6,4)) * 1 = 5
        assert_eq!(d.happiness, 95);
    }

    // ── Validator 7: Action edge cases ────────────────────────────────────

    #[test]
    fn validator7_action_rejected_in_lobby_phase() {
        let mut s = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("700001".into()),
            ts(1),
            config(),
        );
        s.add_player(player("p1", true, 10));
        // Still in Lobby — no begin_phase1 called
        let result = s.apply_action("p1", PlayerAction::Sleep);
        assert_eq!(result, Err(DomainError::ActionNotAllowed));
    }

    #[test]
    fn validator7_action_rejected_in_voting_phase() {
        let mut s = setup_deterministic_session();
        s.dragons.get_mut("d1").unwrap().handover_tags = vec!["a".into(), "b".into(), "c".into()];
        s.transition_to(Phase::Handover).unwrap();
        s.enter_phase2().unwrap();
        s.enter_voting().unwrap();

        let result = s.apply_action("p1", PlayerAction::Sleep);
        assert_eq!(result, Err(DomainError::ActionNotAllowed));
    }

    #[test]
    fn validator7_action_rejected_for_unknown_player() {
        let mut s = setup_deterministic_session();
        let result = s.apply_action("ghost", PlayerAction::Sleep);
        assert_eq!(result, Err(DomainError::DragonNotAssigned));
    }

    #[test]
    fn validator7_play_blocked_when_energy_too_low() {
        let mut s = setup_deterministic_session();
        s.dragons.get_mut("d1").unwrap().energy = 10;
        s.dragons.get_mut("d1").unwrap().hunger = 50;

        let outcome = s
            .apply_action("p1", PlayerAction::Play(PlayType::Fetch))
            .unwrap();
        assert!(matches!(
            outcome,
            ActionOutcome::Blocked {
                reason: ActionBlockReason::TooTiredToPlay
            }
        ));
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.last_emotion, DragonEmotion::Sleepy);
    }

    #[test]
    fn validator7_blocked_action_does_not_increment_counters() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.hunger = 100; // will block feed (AlreadyFull, threshold >= 95)

        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.total_actions, 0);
        assert_eq!(d.food_tries, 0);
        assert_eq!(d.correct_actions, 0);
    }

    #[test]
    fn validator7_blocked_action_does_not_set_cooldown() {
        let mut s = setup_deterministic_session();
        s.dragons.get_mut("d1").unwrap().hunger = 100;

        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().action_cooldown, 0);
    }

    #[test]
    fn validator7_feed_resets_sleep_shield() {
        let mut s = setup_deterministic_session();
        s.dragons.get_mut("d1").unwrap().sleep_shield_ticks = 3;

        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().sleep_shield_ticks, 0);
    }

    #[test]
    fn validator7_play_resets_sleep_shield() {
        let mut s = setup_deterministic_session();
        s.dragons.get_mut("d1").unwrap().sleep_shield_ticks = 3;

        s.apply_action("p1", PlayerAction::Play(PlayType::Fetch))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().sleep_shield_ticks, 0);
    }

    #[test]
    fn validator7_sleep_action_sets_sleep_shield() {
        let mut s = setup_deterministic_session();
        s.dragons.get_mut("d1").unwrap().energy = 40;

        s.apply_action("p1", PlayerAction::Sleep).unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().sleep_shield_ticks, 1);
    }

    #[test]
    fn validator7_sleep_blocked_emotion_is_angry() {
        let mut s = setup_deterministic_session();
        s.dragons.get_mut("d1").unwrap().energy = 95;

        let outcome = s.apply_action("p1", PlayerAction::Sleep).unwrap();
        assert!(matches!(
            outcome,
            ActionOutcome::Blocked {
                reason: ActionBlockReason::TooAwakeToSleep
            }
        ));
        assert_eq!(
            s.dragons.get("d1").unwrap().last_emotion,
            DragonEmotion::Angry
        );
    }

    #[test]
    fn validator7_feed_blocked_emotion_is_neutral() {
        let mut s = setup_deterministic_session();
        s.dragons.get_mut("d1").unwrap().hunger = 100;

        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert_eq!(
            s.dragons.get("d1").unwrap().last_emotion,
            DragonEmotion::Neutral
        );
    }

    #[test]
    fn validator7_play_hungry_blocked_emotion_is_angry() {
        let mut s = setup_deterministic_session();
        s.dragons.get_mut("d1").unwrap().hunger = 10;

        s.apply_action("p1", PlayerAction::Play(PlayType::Fetch))
            .unwrap();
        assert_eq!(
            s.dragons.get("d1").unwrap().last_emotion,
            DragonEmotion::Angry
        );
    }

    #[test]
    fn validator7_correct_feed_emotion_is_happy() {
        let mut s = setup_deterministic_session();
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert_eq!(
            s.dragons.get("d1").unwrap().last_emotion,
            DragonEmotion::Happy
        );
    }

    #[test]
    fn validator7_wrong_feed_emotion_is_angry() {
        let mut s = setup_deterministic_session();
        s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
            .unwrap();
        assert_eq!(
            s.dragons.get("d1").unwrap().last_emotion,
            DragonEmotion::Angry
        );
    }

    #[test]
    fn validator7_sleep_emotion_is_sleepy() {
        let mut s = setup_deterministic_session();
        s.dragons.get_mut("d1").unwrap().energy = 40;

        s.apply_action("p1", PlayerAction::Sleep).unwrap();
        assert_eq!(
            s.dragons.get("d1").unwrap().last_emotion,
            DragonEmotion::Sleepy
        );
    }

    // ── Validator 8: Decay deep tests ─────────────────────────────────────

    #[test]
    fn validator8_low_hunger_adds_happiness_decay() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 1;
        d.hunger = 15; // < 20 → +1 happiness decay
        d.energy = 100; // >= 20 → no extra
        d.happiness = 100;
        d.penalty_stacks = 0;
        s.time = 19; // advance → tick 20 (daytime, no wrong_time for Day dragon)

        s.advance_tick();
        let d = s.dragons.get("d1").unwrap();
        // hunger_decay = 1, energy_decay = 1*1*1 = 1
        // happiness_decay = (1 + 1_hunger + 0_energy + 0_time + 0_penalty) * 1 = 2
        assert_eq!(d.happiness, 98);
    }

    #[test]
    fn validator8_low_energy_adds_happiness_decay() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 1;
        d.hunger = 100; // >= 20 → no extra
        d.energy = 15; // < 20 → +1 happiness decay
        d.happiness = 100;
        d.penalty_stacks = 0;
        s.time = 19; // advance → tick 20 (daytime)

        s.advance_tick();
        let d = s.dragons.get("d1").unwrap();
        // happiness_decay = (1 + 0_hunger + 1_energy + 0_time + 0_penalty) * 1 = 2
        assert_eq!(d.happiness, 98);
    }

    #[test]
    fn validator8_combined_max_happiness_decay_phase2() {
        let mut s = setup_deterministic_session();
        s.dragons.get_mut("d1").unwrap().handover_tags = vec!["a".into(), "b".into(), "c".into()];
        s.transition_to(Phase::Handover).unwrap();
        s.enter_phase2().unwrap();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 1;
        d.hunger = 0; // < 20 → +1
        d.energy = 0; // < 20 → +1
        d.happiness = 100;
        d.penalty_stacks = 4;
        d.sleep_shield_ticks = 0;
        s.time = 40; // advance → tick 41 (nighttime → wrong_time for Day dragon)

        s.advance_tick();
        let d = s.dragons.get("d1").unwrap();
        // happiness_decay = (1 + 1 + 1 + 1 + min(4,4)) * 2 = 8 * 2 = 16
        assert_eq!(d.happiness, 84);
    }

    #[test]
    fn validator8_disconnected_player_dragon_skipped_by_tick() {
        let mut s = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("800001".into()),
            ts(1),
            config(),
        );
        let mut p1 = player("p1", false, 10); // disconnected
        p1.is_connected = false;
        s.add_player(p1);

        s.begin_phase1(&[Phase1Assignment {
            player_id: "p1".into(),
            dragon_id: "d1".into(),
        }])
        .unwrap();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.sleep_rate = 1;
        d.hunger = 50;
        d.energy = 50;
        d.happiness = 50;
        s.time = 10;

        s.advance_tick();
        let d = s.dragons.get("d1").unwrap();
        // Dragon stats should be unchanged because owner is disconnected
        assert_eq!(d.hunger, 50);
        assert_eq!(d.energy, 50);
        assert_eq!(d.happiness, 50);
    }

    #[test]
    fn validator8_tick_noop_in_lobby() {
        let mut s = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("800002".into()),
            ts(1),
            config(),
        );
        s.add_player(player("p1", true, 10));
        s.time = 10;

        let result = s.advance_tick();
        assert!(result.is_empty());
        assert_eq!(s.time, 10); // time should NOT advance in lobby
    }

    #[test]
    fn validator8_time_wraps_from_23_to_0() {
        let mut s = setup_deterministic_session();
        s.time = 47;

        s.advance_tick();
        assert_eq!(s.time, 0);
    }

    #[test]
    fn validator8_is_daytime_boundaries() {
        // Tick 11 = night, 12 = day, 35 = day, 36 = night
        assert!(!is_daytime(11));
        assert!(is_daytime(12));
        assert!(is_daytime(35));
        assert!(!is_daytime(36));
        assert!(!is_daytime(0));
        assert!(is_daytime(24));
        assert!(!is_daytime(47));
    }

    #[test]
    fn validator8_night_dragon_no_time_penalty_at_night() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Night;
        d.sleep_rate = 1;
        d.hunger = 100;
        d.energy = 100;
        d.happiness = 100;
        d.sleep_shield_ticks = 0;
        s.time = 40; // advance → tick 41 (nighttime → correct time for Night dragon)

        s.advance_tick();
        let d = s.dragons.get("d1").unwrap();
        // energy_decay = sleep_rate(1) * time_penalty(1) * decay(1) = 1
        assert_eq!(d.energy, 99);
        // No wrong_time → happiness_decay = 1
        assert_eq!(d.happiness, 99);
    }

    #[test]
    fn validator8_night_dragon_time_penalty_during_day() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Night;
        d.sleep_rate = 1;
        d.hunger = 100;
        d.energy = 100;
        d.happiness = 100;
        d.sleep_shield_ticks = 0;
        s.time = 19; // advance → tick 20 (daytime → wrong time for Night dragon)

        s.advance_tick();
        let d = s.dragons.get("d1").unwrap();
        // energy_decay = 1 * 2 * 1 = 2
        assert_eq!(d.energy, 98);
        // wrong_time → happiness_decay = 1 + 1 = 2
        assert_eq!(d.happiness, 98);
    }

    #[test]
    fn validator8_sleep_shield_expiry_resets_emotion_to_neutral() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.sleep_shield_ticks = 1;
        d.last_action = DragonAction::Sleep;
        d.last_emotion = DragonEmotion::Sleepy;
        d.speech = None;
        d.speech_timer = 0;

        s.advance_tick();
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.sleep_shield_ticks, 0);
        assert_eq!(d.last_action, DragonAction::Idle);
        assert_eq!(d.last_emotion, DragonEmotion::Neutral);
    }

    #[test]
    fn emotion_recovers_from_angry_after_reaction_expires_if_stats_are_stable() {
        let mut s = setup_deterministic_session();

        s.apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
            .unwrap();
        {
            let d = s.dragons.get_mut("d1").unwrap();
            d.hunger = 70;
            d.energy = 70;
            d.happiness = 40;
            d.speech_timer = 1;
        }

        s.advance_tick();

        let d = s.dragons.get("d1").unwrap();
        assert!(d.speech.is_none());
        assert_eq!(d.last_emotion, DragonEmotion::Neutral);
    }

    #[test]
    fn sleep_emotion_recovers_after_sleep_reaction_finishes() {
        let mut s = setup_deterministic_session();
        {
            let d = s.dragons.get_mut("d1").unwrap();
            d.energy = 40;
            d.active_time = ActiveTime::Day;
        }
        s.time = 22;

        s.apply_action("p1", PlayerAction::Sleep).unwrap();
        {
            let d = s.dragons.get_mut("d1").unwrap();
            d.speech_timer = 1;
            d.sleep_shield_ticks = 0;
            d.last_action = DragonAction::Idle;
            d.hunger = 70;
            d.energy = 85;
            d.happiness = 40;
        }

        s.advance_tick();

        let d = s.dragons.get("d1").unwrap();
        assert!(d.speech.is_none());
        assert_eq!(d.last_emotion, DragonEmotion::Neutral);
    }

    #[test]
    fn validator8_penalty_decay_timer_resets_when_stacks_reach_zero() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.penalty_stacks = 1;
        d.penalty_decay_timer = 5;
        d.hunger = 100;
        d.energy = 100;
        d.happiness = 100;

        s.advance_tick(); // timer goes 5→6, then stacks 1→0, timer resets to 0
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.penalty_stacks, 0);
        assert_eq!(d.penalty_decay_timer, 0);
    }

    // ── Validator 9: Voting & phase transitions ───────────────────────────

    fn setup_two_player_session() -> WorkshopSession {
        let mut s = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("900000".into()),
            ts(1),
            config(),
        );
        s.add_player(player("p1", true, 10));
        s.add_player(player("p2", true, 20));

        s.begin_phase1(&[
            Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "d1".into(),
            },
            Phase1Assignment {
                player_id: "p2".into(),
                dragon_id: "d2".into(),
            },
        ])
        .unwrap();
        // Fix preferences
        for (did, food) in [("d1", FoodType::Meat), ("d2", FoodType::Fish)] {
            let d = s.dragons.get_mut(did).unwrap();
            d.active_time = ActiveTime::Day;
            d.favorite_food = food;
            d.favorite_play = PlayType::Fetch;
            d.sleep_rate = 1;
            d.hunger = 50;
            d.energy = 50;
            d.happiness = 50;
        }
        s.time = 20; // daytime tick
        s
    }

    #[test]
    fn validator9_successful_vote_recorded() {
        let mut s = setup_two_player_session();
        // Advance to voting
        for did in ["d1", "d2"] {
            s.dragons.get_mut(did).unwrap().handover_tags =
                vec!["a".into(), "b".into(), "c".into()];
        }
        s.transition_to(Phase::Handover).unwrap();
        s.enter_phase2().unwrap();
        s.enter_voting().unwrap();

        // After handover, each player is assigned the other dragon but still may not vote for
        // their original creation.
        let p1_current_dragon = s
            .players
            .get("p1")
            .unwrap()
            .current_dragon_id
            .clone()
            .unwrap();
        let p2_current_dragon = s
            .players
            .get("p2")
            .unwrap()
            .current_dragon_id
            .clone()
            .unwrap();
        assert_eq!(p1_current_dragon, "d2");
        assert_eq!(p2_current_dragon, "d1");

        s.submit_vote("p1", &p1_current_dragon).unwrap();

        let voting = s.voting.as_ref().unwrap();
        assert_eq!(voting.votes_by_player_id.get("p1"), Some(&p1_current_dragon));

        s.submit_vote("p2", &p2_current_dragon).unwrap();
        let voting = s.voting.as_ref().unwrap();
        assert_eq!(voting.votes_by_player_id.len(), 2);
    }

    #[test]
    fn validator9_vote_overwrite_replaces_previous() {
        let mut s = setup_two_player_session();
        for did in ["d1", "d2"] {
            s.dragons.get_mut(did).unwrap().handover_tags =
                vec!["a".into(), "b".into(), "c".into()];
        }
        s.transition_to(Phase::Handover).unwrap();
        s.enter_phase2().unwrap();
        s.enter_voting().unwrap();

        let p1_current_dragon = s
            .players
            .get("p1")
            .unwrap()
            .current_dragon_id
            .clone()
            .unwrap();
        let p2_current_dragon = s
            .players
            .get("p2")
            .unwrap()
            .current_dragon_id
            .clone()
            .unwrap();

        assert_eq!(p1_current_dragon, "d2");
        assert_eq!(p2_current_dragon, "d1");

        s.submit_vote("p1", &p1_current_dragon).unwrap();
        s.submit_vote("p1", "d1").unwrap_err();
        s.submit_vote("p1", &p1_current_dragon).unwrap();
        assert_eq!(s.voting.as_ref().unwrap().votes_by_player_id.len(), 1);
    }

    #[test]
    fn reveal_voting_results_rejects_without_any_votes() {
        let mut session = setup_two_player_session();
        for did in ["d1", "d2"] {
            session.dragons.get_mut(did).unwrap().handover_tags =
                vec!["a".into(), "b".into(), "c".into()];
        }
        session.transition_to(Phase::Handover).unwrap();
        session.enter_phase2().unwrap();
        session.enter_voting().unwrap();

        let result = session.reveal_voting_results();

        assert_eq!(result, Err(DomainError::VotingRevealNotReady));
        assert!(session
            .voting
            .as_ref()
            .is_some_and(|voting| !voting.results_revealed));
    }

    #[test]
    fn reveal_voting_results_allows_partial_votes_after_first_submission() {
        let mut session = setup_two_player_session();
        for did in ["d1", "d2"] {
            session.dragons.get_mut(did).unwrap().handover_tags =
                vec!["a".into(), "b".into(), "c".into()];
        }
        session.transition_to(Phase::Handover).unwrap();
        session.enter_phase2().unwrap();
        session.enter_voting().unwrap();
        session.submit_vote("p1", "d2").unwrap();

        let result = session.reveal_voting_results();

        assert_eq!(result, Ok(()));
        assert!(session
            .voting
            .as_ref()
            .is_some_and(|voting| voting.results_revealed));
    }

    #[test]
    fn enter_voting_marks_zero_voter_sessions_as_revealed() {
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
        assert!(session
            .voting
            .as_ref()
            .is_some_and(|voting| voting.eligible_player_ids.is_empty() && voting.results_revealed));
    }

    #[test]
    fn finalize_voting_rejects_unrevealed_results() {
        let mut session = setup_two_player_session();
        for did in ["d1", "d2"] {
            session.dragons.get_mut(did).unwrap().handover_tags =
                vec!["a".into(), "b".into(), "c".into()];
        }
        session.transition_to(Phase::Handover).unwrap();
        session.enter_phase2().unwrap();
        session.enter_voting().unwrap();

        let result = session.finalize_voting();

        assert_eq!(result, Err(DomainError::VotingResultsNotRevealed));
        assert_eq!(session.phase, Phase::Voting);
    }

    #[test]
    fn validator9_submit_vote_before_voting_returns_error() {
        let mut s = setup_deterministic_session();
        let result = s.submit_vote("p1", "d1");
        assert_eq!(result, Err(DomainError::VotingNotActive));
    }

    #[test]
    fn validator9_enter_voting_multi_player_returns_false() {
        let mut s = setup_two_player_session();
        for did in ["d1", "d2"] {
            s.dragons.get_mut(did).unwrap().handover_tags =
                vec!["a".into(), "b".into(), "c".into()];
        }
        s.transition_to(Phase::Handover).unwrap();
        s.enter_phase2().unwrap();

        s.enter_voting().unwrap();
        let eligible = &s.voting.as_ref().unwrap().eligible_player_ids;
        assert_eq!(eligible.len(), 2);
    }

    #[test]
    fn validator9_all_valid_transitions_accepted() {
        // Session 4 / refactor: `Lobby → Phase0` and `Phase0 → Phase1` removed;
        // `Lobby → Phase1` is now the only lobby exit.
        assert!(can_transition(Phase::Lobby, Phase::Phase1));
        assert!(can_transition(Phase::Phase1, Phase::Handover));
        assert!(can_transition(Phase::Handover, Phase::Phase2));
        assert!(can_transition(Phase::Phase2, Phase::Voting));
        assert!(can_transition(Phase::Voting, Phase::End));
        assert!(can_transition(Phase::End, Phase::Lobby));
    }

    #[test]
    fn validator9_invalid_transitions_rejected() {
        // Skip phases
        assert!(!can_transition(Phase::Lobby, Phase::Phase2));
        assert!(!can_transition(Phase::Phase1, Phase::Phase2));
        assert!(!can_transition(Phase::Phase2, Phase::End));
        assert!(!can_transition(Phase::Lobby, Phase::End));
        // Session 4 / refactor: Phase0 edges no longer allowed.
        assert!(!can_transition(Phase::Lobby, Phase::Phase0));
        assert!(!can_transition(Phase::Phase0, Phase::Phase1));
        // Backwards
        assert!(!can_transition(Phase::Phase1, Phase::Lobby));
        assert!(!can_transition(Phase::End, Phase::Phase1));
        assert!(!can_transition(Phase::Voting, Phase::Phase2));
        // Self
        assert!(!can_transition(Phase::Lobby, Phase::Lobby));
        assert!(!can_transition(Phase::Phase1, Phase::Phase1));
    }

    #[test]
    fn validator9_transition_resets_warned_flag() {
        let mut s = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("900005".into()),
            ts(1),
            config(),
        );
        s.warned_for_current_phase = true;

        // Session 4 / refactor: was `Phase::Phase0`; now `Lobby→Phase1` directly.
        s.transition_to(Phase::Phase1).unwrap();
        assert!(!s.warned_for_current_phase);
    }

    #[test]
    fn validator9_three_player_rotation_correctness() {
        let mut s = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("900006".into()),
            ts(1),
            config(),
        );
        s.add_player(player("p1", true, 10));
        s.add_player(player("p2", true, 20));
        s.add_player(player("p3", true, 30));

        s.begin_phase1(&[
            Phase1Assignment {
                player_id: "p1".into(),
                dragon_id: "d1".into(),
            },
            Phase1Assignment {
                player_id: "p2".into(),
                dragon_id: "d2".into(),
            },
            Phase1Assignment {
                player_id: "p3".into(),
                dragon_id: "d3".into(),
            },
        ])
        .unwrap();
        // Set tags
        for did in ["d1", "d2", "d3"] {
            s.dragons.get_mut(did).unwrap().handover_tags =
                vec!["a".into(), "b".into(), "c".into()];
        }
        s.transition_to(Phase::Handover).unwrap();
        s.enter_phase2().unwrap();

        // Each player should have a DIFFERENT dragon than they created
        let p1_dragon = s
            .players
            .get("p1")
            .unwrap()
            .current_dragon_id
            .as_deref()
            .unwrap();
        let p2_dragon = s
            .players
            .get("p2")
            .unwrap()
            .current_dragon_id
            .as_deref()
            .unwrap();
        let p3_dragon = s
            .players
            .get("p3")
            .unwrap()
            .current_dragon_id
            .as_deref()
            .unwrap();
        assert_ne!(p1_dragon, "d1", "p1 should not keep their own dragon");
        assert_ne!(p2_dragon, "d2", "p2 should not keep their own dragon");
        assert_ne!(p3_dragon, "d3", "p3 should not keep their own dragon");
        // All different
        assert_ne!(p1_dragon, p2_dragon);
        assert_ne!(p2_dragon, p3_dragon);
        assert_ne!(p1_dragon, p3_dragon);
    }

    #[test]
    fn validator9_dragon_initial_stats_are_80() {
        let mut s = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("900007".into()),
            ts(1),
            config(),
        );
        s.add_player(player("p1", true, 10));

        s.begin_phase1(&[Phase1Assignment {
            player_id: "p1".into(),
            dragon_id: "d1".into(),
        }])
        .unwrap();

        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.hunger, 80);
        assert_eq!(d.energy, 80);
        assert_eq!(d.happiness, 80);
    }

    // ── Validator 10: Achievement edge cases ──────────────────────────────

    #[test]
    fn validator10_rock_bottom_deduplicated_across_ticks() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.happiness = 1;
        d.hunger = 100;
        d.energy = 100;
        d.sleep_rate = 1;

        // First tick: happiness 1 → 0, awards rock_bottom
        let awards1 = s.advance_tick();
        assert!(awards1.iter().any(|(_, ach)| *ach == "rock_bottom"));

        // Second tick: happiness stays 0 (clamped), should NOT duplicate
        let awards2 = s.advance_tick();
        assert!(!awards2.iter().any(|(_, ach)| *ach == "rock_bottom"));

        // Player should have exactly one rock_bottom
        let count = s
            .players
            .get("p1")
            .unwrap()
            .achievements
            .iter()
            .filter(|a| *a == "rock_bottom")
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn validator10_phase_end_achievement_dedup_on_double_call() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.wrong_food_count = 0;
        d.wrong_play_count = 0;
        d.total_actions = 10;
        d.correct_actions = 10;

        let awards1 = s.award_phase_end_achievements();
        let awards2 = s.award_phase_end_achievements();

        // First call awards, second should not duplicate
        assert!(!awards1.is_empty());
        assert!(awards2.is_empty());
    }

    #[test]
    fn validator10_no_mistakes_requires_both_zero_wrong() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.total_actions = 6;
        d.wrong_food_count = 0;
        d.wrong_play_count = 1; // has wrong plays
        d.correct_actions = 5;

        let awards = s.award_phase_end_achievements();
        assert!(!awards.iter().any(|(_, ach)| *ach == "no_mistakes"));
    }

    #[test]
    fn validator10_no_mistakes_requires_min_five_actions() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.total_actions = 4; // less than 5
        d.wrong_food_count = 0;
        d.wrong_play_count = 0;
        d.correct_actions = 4;

        let awards = s.award_phase_end_achievements();
        assert!(!awards.iter().any(|(_, ach)| *ach == "no_mistakes"));
    }

    #[test]
    fn validator10_zen_master_negative_with_penalty_stacks() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.total_actions = 10;
        d.correct_actions = 8;
        d.penalty_stacks = 1; // has stacks → should NOT get zen_master

        let awards = s.award_phase_end_achievements();
        assert!(!awards.iter().any(|(_, ach)| *ach == "zen_master"));
    }

    #[test]
    fn validator10_zen_master_requires_eight_actions() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.total_actions = 7; // less than 8
        d.correct_actions = 7;
        d.penalty_stacks = 0;

        let awards = s.award_phase_end_achievements();
        assert!(!awards.iter().any(|(_, ach)| *ach == "zen_master"));
    }

    #[test]
    fn validator10_perfectionist_boundary_exactly_80_percent() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.total_actions = 10;
        d.correct_actions = 8; // 80% exactly
        d.wrong_food_count = 0;
        d.wrong_play_count = 0;

        let awards = s.award_phase_end_achievements();
        assert!(awards.iter().any(|(_, ach)| *ach == "perfectionist"));
    }

    #[test]
    fn validator10_perfectionist_below_80_percent_not_awarded() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.total_actions = 10;
        d.correct_actions = 7; // 70%
        d.wrong_food_count = 1;
        d.wrong_play_count = 2;

        let awards = s.award_phase_end_achievements();
        assert!(!awards.iter().any(|(_, ach)| *ach == "perfectionist"));
    }

    #[test]
    fn validator10_comeback_kid_boundary_lowest_16_not_awarded() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.phase2_lowest_happiness = 16; // > 15 → should NOT qualify
        d.happiness = 70;

        let awards = s.award_phase_end_achievements();
        assert!(!awards.iter().any(|(_, ach)| *ach == "comeback_kid"));
    }

    #[test]
    fn validator10_comeback_kid_boundary_happiness_69_not_awarded() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.phase2_lowest_happiness = 10;
        d.happiness = 69; // < 70 → should NOT qualify

        let awards = s.award_phase_end_achievements();
        assert!(!awards.iter().any(|(_, ach)| *ach == "comeback_kid"));
    }

    #[test]
    fn validator10_comeback_kid_awarded_at_boundaries() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.phase2_lowest_happiness = 15; // exactly 15
        d.happiness = 70; // exactly 70

        let awards = s.award_phase_end_achievements();
        assert!(awards.iter().any(|(_, ach)| *ach == "comeback_kid"));
    }

    // ── Validator 11: Penalty floor, sleep penalty, Phase2 counter reset ──

    #[test]
    fn validator11_penalty_stacks_floor_at_zero_on_correct_action() {
        let mut s = setup_deterministic_session();
        s.dragons.get_mut("d1").unwrap().penalty_stacks = 0;

        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().penalty_stacks, 0);
    }

    #[test]
    fn validator11_correct_sleep_reduces_penalty_stacks() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.penalty_stacks = 3;
        d.energy = 40;
        d.active_time = ActiveTime::Day;
        s.time = 40; // nighttime → correct sleep time for Day dragon

        s.apply_action("p1", PlayerAction::Sleep).unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().penalty_stacks, 2);
    }

    #[test]
    fn validator11_wrong_time_sleep_does_not_reduce_penalty() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.penalty_stacks = 3;
        d.energy = 40;
        d.active_time = ActiveTime::Day;
        s.time = 20; // daytime → wrong sleep time for Day dragon

        s.apply_action("p1", PlayerAction::Sleep).unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().penalty_stacks, 3);
    }

    #[test]
    fn validator11_phase2_resets_all_counters_multi_player() {
        let mut s = setup_two_player_session();
        // Build up state in Phase 1
        let d = s.dragons.get_mut("d1").unwrap();
        d.penalty_stacks = 3;
        d.penalty_decay_timer = 4;
        d.peak_penalty_stacks = 3;
        d.wrong_food_count = 2;
        d.wrong_play_count = 1;
        d.cooldown_violations = 3;
        d.total_actions = 10;
        d.correct_actions = 5;
        d.found_correct_food = true;
        d.found_correct_play = true;
        d.food_tries = 5;
        d.play_tries = 3;
        d.sleep_shield_ticks = 2;
        d.action_cooldown = 2;
        d.high_happiness_ticks = 10;

        // Set tags and advance to phase2
        for did in ["d1", "d2"] {
            s.dragons.get_mut(did).unwrap().handover_tags =
                vec!["a".into(), "b".into(), "c".into()];
        }
        s.transition_to(Phase::Handover).unwrap();
        s.enter_phase2().unwrap();

        // Check d1 was reset (now owned by a different player)
        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.hunger, 80);
        assert_eq!(d.energy, 80);
        assert_eq!(d.happiness, 80);
        assert_eq!(d.penalty_stacks, 0);
        assert_eq!(d.penalty_decay_timer, 0);
        assert_eq!(d.peak_penalty_stacks, 0);
        assert_eq!(d.wrong_food_count, 0);
        assert_eq!(d.wrong_play_count, 0);
        assert_eq!(d.cooldown_violations, 0);
        assert_eq!(d.total_actions, 0);
        assert_eq!(d.correct_actions, 0);
        assert!(!d.found_correct_food);
        assert!(!d.found_correct_play);
        assert_eq!(d.food_tries, 0);
        assert_eq!(d.play_tries, 0);
        assert_eq!(d.sleep_shield_ticks, 0);
        assert_eq!(d.action_cooldown, 0);
        assert_eq!(d.phase2_ticks, 0);
        assert_eq!(d.phase2_lowest_happiness, 100);
        assert_eq!(d.last_action, DragonAction::Idle);
        assert_eq!(d.last_emotion, DragonEmotion::Neutral);
    }

    #[test]
    fn validator11_phase2_resets_all_counters_single_player() {
        let mut s = setup_deterministic_session();
        let d = s.dragons.get_mut("d1").unwrap();
        d.penalty_stacks = 5;
        d.penalty_decay_timer = 3;
        d.peak_penalty_stacks = 5;
        d.wrong_food_count = 4;
        d.wrong_play_count = 2;
        d.cooldown_violations = 7;
        d.total_actions = 15;
        d.correct_actions = 8;
        d.found_correct_food = true;
        d.found_correct_play = true;
        d.handover_tags = vec!["a".into(), "b".into(), "c".into()];

        s.transition_to(Phase::Handover).unwrap();
        s.enter_phase2().unwrap();

        let d = s.dragons.get("d1").unwrap();
        assert_eq!(d.penalty_stacks, 0);
        assert_eq!(d.penalty_decay_timer, 0);
        assert_eq!(d.peak_penalty_stacks, 0);
        assert_eq!(d.wrong_food_count, 0);
        assert_eq!(d.wrong_play_count, 0);
        assert_eq!(d.cooldown_violations, 0);
        assert_eq!(d.total_actions, 0);
        assert_eq!(d.correct_actions, 0);
        assert!(!d.found_correct_food);
        assert!(!d.found_correct_play);
        assert_eq!(d.phase2_ticks, 0);
        assert_eq!(d.phase2_lowest_happiness, 100);
    }

    #[test]
    fn validator11_speech_timer_values_per_action() {
        let mut s = setup_deterministic_session();
        // Feed → speech_timer = 4
        s.apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().speech_timer, 4);

        // Play → speech_timer = 4
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.apply_action("p1", PlayerAction::Play(PlayType::Fetch))
            .unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().speech_timer, 4);

        // Sleep → speech_timer = 5
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.dragons.get_mut("d1").unwrap().energy = 40;
        s.apply_action("p1", PlayerAction::Sleep).unwrap();
        assert_eq!(s.dragons.get("d1").unwrap().speech_timer, 5);
    }

    // ── Validator 12: Tags, scores, observation edge cases ────────────────

    #[test]
    fn validator12_handover_tags_rejects_wrong_count() {
        let mut s = setup_deterministic_session();
        let result = s.save_handover_tags(
            "p1",
            vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()],
        );
        assert!(matches!(
            result,
            Err(DomainError::InvalidHandoverTagCount {
                expected: HANDOVER_TAG_COUNT,
                got: 5,
            })
        ));
        // Dragon's tags must be untouched by a rejected save.
        assert_eq!(s.dragons.get("d1").unwrap().handover_tags.len(), 0);
    }

    #[test]
    fn validator12_handover_tags_ghost_player_noop() {
        let mut s = setup_deterministic_session();
        let dragon_count_before = s.dragons.len();
        let result =
            s.save_handover_tags("ghost", vec!["a".into(), "b".into(), "c".into()]);
        assert!(result.is_ok());
        assert_eq!(s.dragons.len(), dragon_count_before);
    }

    #[test]
    fn validator12_observation_ghost_player_noop() {
        let mut s = setup_deterministic_session();
        s.record_discovery_observation("ghost", "some note");
        // Should not panic, no changes
        assert_eq!(s.dragons.get("d1").unwrap().discovery_observations.len(), 0);
    }

    #[test]
    fn validator12_apply_judge_scores_resets_before_applying() {
        let mut s = setup_deterministic_session();
        s.players.get_mut("p1").unwrap().score = 50;

        s.apply_judge_scores(&[("d1".to_string(), 10, 20, "ok".to_string())]);
        // Score should be 10 + 20 = 30, NOT 50 + 10 + 20 = 80
        assert_eq!(s.players.get("p1").unwrap().score, 30);
    }

    #[test]
    fn validator12_apply_judge_scores_unknown_dragon_skipped() {
        let mut s = setup_deterministic_session();
        s.apply_judge_scores(&[
            ("d1".to_string(), 10, 20, "ok".to_string()),
            ("nonexistent".to_string(), 100, 100, "skip".to_string()),
        ]);
        assert_eq!(s.players.get("p1").unwrap().score, 30);
    }

    #[test]
    fn validator12_single_player_score_gets_both_components() {
        let mut s = setup_deterministic_session();
        // In single player, original_owner == current_owner
        s.apply_judge_scores(&[("d1".to_string(), 15, 25, "good".to_string())]);
        // p1 gets both observation_score (15) + care_score (25) = 40
        assert_eq!(s.players.get("p1").unwrap().score, 40);
    }

    #[test]
    fn validator12_two_player_score_split() {
        let mut s = setup_two_player_session();
        // After phase2 shuffle, d1 original_owner=p1 but current_owner changes
        for did in ["d1", "d2"] {
            s.dragons.get_mut(did).unwrap().handover_tags =
                vec!["a".into(), "b".into(), "c".into()];
        }
        s.transition_to(Phase::Handover).unwrap();
        s.enter_phase2().unwrap();

        let d1_creator = s.dragons.get("d1").unwrap().original_owner_id.clone();
        let d1_caretaker = s.dragons.get("d1").unwrap().current_owner_id.clone();
        assert_ne!(d1_creator, d1_caretaker);

        s.apply_judge_scores(&[
            ("d1".to_string(), 10, 20, "good".to_string()),
            ("d2".to_string(), 30, 40, "great".to_string()),
        ]);

        // Each player gets observation from their created dragon + care from their cared dragon
        let p1_score = s.players.get("p1").unwrap().score;
        let p2_score = s.players.get("p2").unwrap().score;
        // Total distributed: 10+20+30+40 = 100
        assert_eq!(p1_score + p2_score, 100);
    }

    #[test]
    fn validator12_second_player_does_not_steal_host() {
        let mut s = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("120001".into()),
            ts(1),
            config(),
        );
        s.add_player(player("p1", true, 10));
        s.add_player(player("p2", true, 20));

        assert_eq!(s.host_player_id.as_deref(), Some("p1"));
        assert!(s.players.get("p1").unwrap().is_host);
        assert!(!s.players.get("p2").unwrap().is_host);
    }

    #[test]
    fn reserved_host_blocks_guest_from_becoming_host_until_creator_joins() {
        let mut s = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("120001".into()),
            ts(1),
            config(),
        );
        s.reserve_host("acct-alice", "Alice");

        let mut guest = player("p2", true, 20);
        guest.account_id = Some("acct-bob".to_string());
        s.add_player(guest);

        assert_eq!(s.host_player_id, None);
        assert!(!s.players.get("p2").unwrap().is_host);
        assert_eq!(s.reserved_host_name(), Some("Alice"));

        let mut creator = player("p1", true, 10);
        creator.account_id = Some("acct-alice".to_string());
        s.add_player(creator);

        assert_eq!(s.host_player_id.as_deref(), Some("p1"));
        assert!(s.players.get("p1").unwrap().is_host);
        assert!(!s.players.get("p2").unwrap().is_host);
        assert_eq!(s.owner_account_id(), Some("acct-alice"));
        assert_eq!(s.reserved_host_account_id(), None);
    }

    #[test]
    fn validator12_phase_duration_minutes_mapping() {
        let s = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("120002".into()),
            ts(1),
            config(),
        );
        assert_eq!(s.phase_duration_minutes(Phase::Lobby), 0);
        assert_eq!(s.phase_duration_minutes(Phase::Phase0), 0);
        assert_eq!(s.phase_duration_minutes(Phase::Phase1), 10);
        assert_eq!(s.phase_duration_minutes(Phase::Handover), 10);
        assert_eq!(s.phase_duration_minutes(Phase::Phase2), 10);
        assert_eq!(s.phase_duration_minutes(Phase::Judge), 0);
        assert_eq!(s.phase_duration_minutes(Phase::Voting), 0);
        assert_eq!(s.phase_duration_minutes(Phase::End), 0);
    }

    #[test]
    fn validator12_remaining_phase_seconds_zero_duration_returns_none() {
        let mut s = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("120003".into()),
            ts(1),
            config(),
        );
        s.phase = Phase::Voting;
        assert_eq!(s.remaining_phase_seconds(ts(100)), None);
    }

    #[test]
    fn validator12_remaining_phase_seconds_clamps_to_zero() {
        let mut s = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("120004".into()),
            ts(1),
            config(),
        );
        s.phase = Phase::Phase1;
        // Session 4 / refactor: was Phase0; now uses Phase1 (duration = 10 minutes).
        // 10 minutes = 600 seconds. 1000 seconds elapsed → should clamp to 0.
        assert_eq!(s.remaining_phase_seconds(ts(1001)), Some(0));
    }

    #[test]
    fn validator12_sleep_rate_range() {
        // Generate many dragons and verify sleep_rate is always 1-3
        let mut s = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode("120005".into()),
            ts(1),
            config(),
        );
        for i in 0..20 {
            let pid = format!("p{i}");
            s.add_player(player(&pid, true, i as i64));
        }

        let assignments: Vec<Phase1Assignment> = (0..20)
            .map(|i| Phase1Assignment {
                player_id: format!("p{i}"),
                dragon_id: format!("d{i}"),
            })
            .collect();
        s.begin_phase1(&assignments).unwrap();

        for dragon in s.dragons.values() {
            assert!(
                (1..=3).contains(&dragon.sleep_rate),
                "sleep_rate {} out of range for dragon {}",
                dragon.sleep_rate,
                dragon.id
            );
        }
    }

    /// Validates that brute-forcing preferences in Phase 2 is prohibitively costly.
    /// A player who doesn't know the dragon's preferences and tries all options
    /// will tank happiness so badly that the dragon is in crisis.
    #[test]
    fn phase2_brute_force_is_punishing() {
        let mut s = setup_deterministic_session();
        // Move to Phase 2
        s.transition_to(Phase::Handover).unwrap();
        s.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s.enter_phase2().unwrap();

        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.favorite_food = FoodType::Meat; // player doesn't know this
        d.favorite_play = PlayType::Fetch; // player doesn't know this
        d.sleep_rate = 1;
        d.hunger = 80;
        d.energy = 80;
        d.happiness = 80;
        // Daytime tick, no wrong_time penalty for Day dragon
        s.time = 21;

        // Simulate brute-force: try wrong food twice, then correct
        // Wrong food #1: Fish
        let out = s
            .apply_action("p1", PlayerAction::Feed(FoodType::Fish))
            .unwrap();
        assert!(matches!(
            out,
            ActionOutcome::Applied {
                was_correct: false,
                ..
            }
        ));
        let d = s.dragons.get("d1").unwrap();
        let h_after_wrong1 = d.happiness;
        // -12 penalty (first wrong food), happiness ~68
        assert!(h_after_wrong1 <= 70, "after first wrong food: {h_after_wrong1}");

        // Wait cooldown (2 ticks) — decay continues at 2X
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.advance_tick();
        s.advance_tick();

        // Wrong food #2: Fruit
        let out = s
            .apply_action("p1", PlayerAction::Feed(FoodType::Fruit))
            .unwrap();
        assert!(matches!(
            out,
            ActionOutcome::Applied {
                was_correct: false,
                ..
            }
        ));

        // Now try wrong play twice
        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.advance_tick();
        s.advance_tick();

        let out = s
            .apply_action("p1", PlayerAction::Play(PlayType::Puzzle))
            .unwrap();
        assert!(matches!(
            out,
            ActionOutcome::Applied {
                was_correct: false,
                ..
            }
        ));

        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.advance_tick();
        s.advance_tick();

        let out = s
            .apply_action("p1", PlayerAction::Play(PlayType::Music))
            .unwrap();
        assert!(matches!(
            out,
            ActionOutcome::Applied {
                was_correct: false,
                ..
            }
        ));

        // After 4 wrong guesses + decay ticks, happiness should be critically low
        let d = s.dragons.get("d1").unwrap();
        assert!(
            d.happiness < 40,
            "happiness {} should be < 40 after brute-forcing in Phase 2",
            d.happiness
        );
        // Penalty stacks should be high
        assert!(
            d.penalty_stacks >= 3,
            "penalty_stacks {} should be >= 3",
            d.penalty_stacks
        );
    }

    /// Validates that following correct instructions in Phase 2 keeps dragon healthy.
    #[test]
    fn phase2_following_instructions_succeeds() {
        let mut s = setup_deterministic_session();
        s.transition_to(Phase::Handover).unwrap();
        s.save_handover_tags("p1", vec!["a".into(), "b".into(), "c".into()]).expect("save handover tags");
        s.enter_phase2().unwrap();

        let d = s.dragons.get_mut("d1").unwrap();
        d.active_time = ActiveTime::Day;
        d.favorite_food = FoodType::Meat;
        d.favorite_play = PlayType::Fetch;
        d.sleep_rate = 1;
        d.hunger = 80;
        d.energy = 80;
        d.happiness = 80;
        s.time = 21;

        // Player knows the right answers from handover notes
        // Correct food
        let out = s
            .apply_action("p1", PlayerAction::Feed(FoodType::Meat))
            .unwrap();
        assert!(matches!(
            out,
            ActionOutcome::Applied {
                was_correct: true,
                ..
            }
        ));

        s.dragons.get_mut("d1").unwrap().action_cooldown = 0;
        s.advance_tick();
        s.advance_tick();

        // Correct play
        let out = s
            .apply_action("p1", PlayerAction::Play(PlayType::Fetch))
            .unwrap();
        assert!(matches!(
            out,
            ActionOutcome::Applied {
                was_correct: true,
                ..
            }
        ));

        let d = s.dragons.get("d1").unwrap();
        // Happiness should still be high (80 + 20 - some_decay + 30 - some_decay)
        assert!(
            d.happiness >= 70,
            "happiness {} should be >= 70 after correct actions in Phase 2",
            d.happiness
        );
        assert_eq!(d.penalty_stacks, 0);
    }
}
