use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub type SessionStage = u8;
pub type SpriteCatalog = BTreeMap<String, SpriteSet>;
pub type SessionPhaseConfig = BTreeMap<Phase, SessionPhaseSettings>;

pub const SPRITE_ATELIER_NOTICE_TITLE: &str = "Sprite Atelier";
pub const SPRITE_ATELIER_ACCEPTED_NOTICE_MESSAGE: &str =
    "The atelier accepted your dragon brief and is preparing a sketch slot.";
pub const SPRITE_ATELIER_QUEUED_NOTICE_MESSAGE: &str =
    "All easels are busy. Your dragon is waiting in the atelier queue.";
pub const SPRITE_ATELIER_DRAWING_NOTICE_MESSAGE: &str =
    "An easel is free. The atelier has started sketching your dragon.";
pub const SPRITE_ATELIER_FALLBACK_NOTICE_MESSAGE: &str =
    "The atelier prepared a reserve companion sprite sheet so you can keep playing.";

/// Error code returned by `POST /api/auth/signin` when the submitted name
/// exists but the password does not match. Shared wire constant so the
/// backend handler (`app-server/src/auth.rs`) and the frontend matcher
/// (`app-web/src/components/sign_in.rs`) cannot drift. Spec: `refactor.md:50`.
pub const AUTH_ERR_NAME_TAKEN_WRONG_PASSWORD: &str = "name_taken_wrong_password";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionNoticeCode {
    SpriteAtelierAccepted,
    SpriteAtelierQueued,
    SpriteAtelierDrawing,
    SpriteAtelierFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Lobby,
    Phase0,
    Phase1,
    Handover,
    Phase2,
    Judge,
    Voting,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoordinatorType {
    Node,
    Rust,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionCommand {
    Join,
    // Session 4 / refactor: `StartPhase0` removed. Character creation no longer
    // happens inside a workshop; hosts transition `Lobby→Phase1` directly via
    // `StartPhase1`.
    SelectCharacter,
    SubmitObservation,
    StartPhase1,
    StartHandover,
    SubmitTags,
    StartPhase2,
    Action,
    EndGame,
    StartVoting,
    SubmitVote,
    RevealVotingResults,
    ResetGame,
    EndSession,
    LeaveWorkshop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FoodType {
    Meat,
    Fruit,
    Fish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayType {
    Fetch,
    Puzzle,
    Music,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DragonAction {
    Feed,
    Play,
    Sleep,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DragonEmotion {
    Happy,
    Angry,
    Sleepy,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActiveTime {
    Day,
    Night,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpriteSet {
    pub neutral: String,
    pub happy: String,
    pub angry: String,
    pub sleepy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterProfile {
    pub id: String,
    pub description: String,
    pub sprites: SpriteSet,
    #[serde(default)]
    pub remaining_sprite_regenerations: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    pub id: String,
    pub name: String,
    pub is_host: bool,
    pub score: i32,
    pub current_dragon_id: Option<String>,
    pub achievements: Vec<String>,
    pub is_ready: bool,
    pub is_connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pet_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_sprites: Option<SpriteSet>,
    #[serde(default)]
    pub remaining_sprite_regenerations: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerPlayer {
    pub id: String,
    pub name: String,
    pub is_host: bool,
    pub score: i32,
    pub current_dragon_id: Option<String>,
    pub achievements: Vec<String>,
    pub is_ready: bool,
    pub is_connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pet_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_sprites: Option<SpriteSet>,
    #[serde(default)]
    pub remaining_sprite_regenerations: u8,
    pub joined_at: String,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DragonStats {
    pub hunger: i32,
    pub energy: i32,
    pub happiness: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DragonVisuals {
    pub base: i32,
    pub color_p: String,
    pub color_s: String,
    pub color_a: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DragonTraits {
    pub active_time: ActiveTime,
    pub favorite_food: FoodType,
    pub favorite_play: PlayType,
    pub sleep_rate: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dragon {
    pub id: String,
    pub name: String,
    pub visuals: DragonVisuals,
    pub original_owner_id: String,
    pub current_owner_id: String,
    pub creator_instructions: String,
    pub stats: DragonStats,
    pub traits: DragonTraits,
    pub discovery_observations: Vec<String>,
    pub handover_tags: Vec<String>,
    pub last_action: DragonAction,
    pub last_emotion: DragonEmotion,
    pub speech: Option<String>,
    pub speech_timer: i32,
    pub action_cooldown: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_sprites: Option<SpriteSet>,
    pub sleep_shield_ticks: i32,
    pub food_tries: i32,
    pub play_tries: i32,
    pub high_happiness_ticks: i32,
    pub phase2_ticks: i32,
    pub phase2_lowest_happiness: i32,
    pub wrong_food_count: i32,
    pub wrong_play_count: i32,
    pub cooldown_violations: i32,
    pub total_actions: i32,
    pub correct_actions: i32,
    pub penalty_stacks: i32,
    #[serde(default)]
    pub peak_penalty_stacks: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientDragon {
    pub id: String,
    pub name: String,
    pub visuals: DragonVisuals,
    pub original_owner_id: Option<String>,
    pub current_owner_id: Option<String>,
    pub stats: DragonStats,
    pub condition_hint: Option<String>,
    pub discovery_observations: Vec<String>,
    pub handover_tags: Vec<String>,
    pub last_action: DragonAction,
    pub last_emotion: DragonEmotion,
    pub speech: Option<String>,
    pub speech_timer: i32,
    pub action_cooldown: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_sprites: Option<SpriteSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_observation_score: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_care_score: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_feedback: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoteResult {
    pub dragon_id: String,
    pub votes: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerVotingState {
    pub eligible_player_ids: Vec<String>,
    pub votes_by_player_id: BTreeMap<String, String>,
    #[serde(default)]
    pub results_revealed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientVotingState {
    pub eligible_count: i32,
    pub submitted_count: i32,
    pub current_player_vote_dragon_id: Option<String>,
    #[serde(default)]
    pub results_revealed: bool,
    pub results: Option<Vec<VoteResult>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    pub id: String,
    pub code: String,
    pub created_at: String,
    pub updated_at: String,
    pub phase_started_at: String,
    pub host_player_id: Option<String>,
    pub settings: SessionSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionArtifactKind {
    SessionCreated,
    PlayerJoined,
    PlayerReconnected,
    PetProfileUpdated,
    CreatorInstructionsRecorded,
    PhaseChanged,
    DiscoveryObservationSaved,
    HandoverSaved,
    HandoverChainCompiled,
    ActionProcessed,
    VoteSubmitted,
    VotingFinalized,
    JudgeBundleGenerated,
    SessionReset,
    PlayerLeft,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionArtifactRecord {
    pub id: String,
    pub session_id: String,
    pub phase: Phase,
    pub step: SessionStage,
    pub kind: SessionArtifactKind,
    pub player_id: Option<String>,
    pub created_at: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPhaseSettings {
    pub step: SessionStage,
    pub label: String,
    pub description: String,
    pub duration_seconds: i32,
    pub allowed_commands: Vec<SessionCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSettings {
    pub phases: SessionPhaseConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerGameState {
    pub session: SessionMeta,
    pub phase: Phase,
    pub time: i32,
    pub players: BTreeMap<String, ServerPlayer>,
    pub dragons: BTreeMap<String, Dragon>,
    pub voting: Option<ServerVotingState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientGameState {
    pub session: SessionMeta,
    pub phase: Phase,
    pub time: i32,
    pub players: BTreeMap<String, Player>,
    pub dragons: BTreeMap<String, ClientDragon>,
    pub current_player_id: Option<String>,
    pub voting: Option<ClientVotingState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotice {
    pub level: NoticeLevel,
    pub title: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<SessionNoticeCode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSessionSnapshot {
    pub session_code: String,
    pub reconnect_token: String,
    pub player_id: String,
    pub coordinator_type: CoordinatorType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkshopCreateConfig {
    pub phase0_minutes: u32,
    pub phase1_minutes: u32,
    pub phase2_minutes: u32,
}

impl Default for WorkshopCreateConfig {
    fn default() -> Self {
        Self {
            phase0_minutes: 8,
            phase1_minutes: 8,
            phase2_minutes: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorkshopRequest {
    /// Ignored by the server (name is derived from the authenticated account).
    /// Kept as optional for backward compatibility with older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<WorkshopCreateConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinWorkshopRequest {
    pub session_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnect_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectCharacterRequest {
    pub character_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryObservationRequest {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPayload {
    #[serde(rename = "type")]
    pub action_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VotePayload {
    pub dragon_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkshopCommandRequest {
    pub session_code: String,
    pub reconnect_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_type: Option<CoordinatorType>,
    pub command: SessionCommand,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkshopJudgeBundleRequest {
    pub session_code: String,
    pub reconnect_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_type: Option<CoordinatorType>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgeActionTrace {
    pub player_id: String,
    pub player_name: String,
    pub phase: Phase,
    pub action_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_value: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resulting_stats: Option<DragonStats>,
    /// Whether the action matched the dragon's preference (None = blocked/sleep).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was_correct: Option<bool>,
    /// If the action was blocked, the reason (e.g. "already_full", "cooldown_violation").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgeHandoverChain {
    pub creator_instructions: String,
    pub discovery_observations: Vec<String>,
    pub handover_tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgePlayerSummary {
    pub player_id: String,
    pub name: String,
    pub score: i32,
    pub achievements: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgeDragonBundle {
    pub dragon_id: String,
    pub dragon_name: String,
    pub creator_player_id: String,
    pub creator_name: String,
    pub current_owner_id: String,
    pub current_owner_name: String,
    pub creative_vote_count: i32,
    pub final_stats: DragonStats,
    pub actual_active_time: ActiveTime,
    pub actual_favorite_food: FoodType,
    pub actual_favorite_play: PlayType,
    pub actual_sleep_rate: i32,
    pub handover_chain: JudgeHandoverChain,
    pub phase2_actions: Vec<JudgeActionTrace>,
    /// Summary stats for the Phase 2 caretaker's performance.
    pub total_actions: i32,
    pub correct_actions: i32,
    pub wrong_food_count: i32,
    pub wrong_play_count: i32,
    pub cooldown_violations: i32,
    pub penalty_stacks_at_end: i32,
    pub phase2_lowest_happiness: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgeBundle {
    pub session_id: String,
    pub session_code: String,
    pub current_phase: Phase,
    pub generated_at: String,
    pub artifact_count: i32,
    pub players: Vec<JudgePlayerSummary>,
    pub dragons: Vec<JudgeDragonBundle>,
}

// ---------------------------------------------------------------------------
// LLM provider types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProviderKind {
    VertexAi,
    ApiKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmProviderEntry {
    #[serde(rename = "type")]
    pub provider_type: LlmProviderKind,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env_var: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmJudgeRequest {
    pub session_code: String,
    pub reconnect_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_type: Option<CoordinatorType>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmJudgeEvaluation {
    pub summary: String,
    pub dragon_evaluations: Vec<LlmDragonEvaluation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmDragonEvaluation {
    pub dragon_id: String,
    pub dragon_name: String,
    pub observation_score: i32,
    pub care_score: i32,
    pub creativity_score: i32,
    pub feedback: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmJudgeSuccess {
    pub ok: bool,
    pub evaluation: LlmJudgeEvaluation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LlmJudgeResult {
    Success(LlmJudgeSuccess),
    Error(WorkshopError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmImageRequest {
    pub session_code: String,
    pub reconnect_token: String,
    pub dragon_id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_type: Option<CoordinatorType>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmImageSuccess {
    pub ok: bool,
    pub image_base64: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LlmImageResult {
    Success(LlmImageSuccess),
    Error(WorkshopError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpriteSheetRequest {
    pub session_code: String,
    pub reconnect_token: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterSpriteSheetRequest {
    pub session_code: String,
    pub reconnect_token: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CharacterCatalogRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpriteSheetSuccess {
    pub ok: bool,
    pub sprites: SpriteSet,
    #[serde(default)]
    pub fallback_used: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterSpriteSheetSuccess {
    pub ok: bool,
    pub character_id: String,
    pub sprites: SpriteSet,
    #[serde(default)]
    pub remaining_sprite_regenerations: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SpriteSheetResult {
    Success(SpriteSheetSuccess),
    Error(WorkshopError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CharacterSpriteSheetResult {
    Success(CharacterSpriteSheetSuccess),
    Error(WorkshopError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterCatalogSuccess {
    pub ok: bool,
    pub characters: Vec<CharacterProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CharacterCatalogResult {
    Success(CharacterCatalogSuccess),
    Error(WorkshopError),
}

// ---------------------------------------------------------------------------
// Workshop command result types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkshopCommandSuccess {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkshopJoinSuccess {
    pub ok: bool,
    #[serde(rename = "sessionCode")]
    pub session_code: String,
    #[serde(rename = "playerId")]
    pub player_id: String,
    #[serde(rename = "reconnectToken")]
    pub reconnect_token: String,
    #[serde(rename = "coordinatorType")]
    pub coordinator_type: CoordinatorType,
    pub state: ClientGameState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkshopCreateSuccess {
    pub ok: bool,
    pub session_code: String,
    pub host_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkshopJudgeBundleSuccess {
    pub ok: bool,
    pub bundle: JudgeBundle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkshopError {
    pub ok: bool,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvelope {
    pub session_code: String,
    pub player_id: String,
    pub reconnect_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_type: Option<CoordinatorType>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClientWsMessage {
    AttachSession(SessionEnvelope),
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::large_enum_variant)]
pub enum ServerWsMessage {
    StateUpdate(ClientGameState),
    Notice(SessionNotice),
    Error { message: String },
    Pong,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum WorkshopJoinResult {
    Success(WorkshopJoinSuccess),
    Error(WorkshopError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkshopCreateResult {
    Success(WorkshopCreateSuccess),
    Error(WorkshopError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkshopCommandResult {
    Success(WorkshopCommandSuccess),
    Error(WorkshopError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkshopJudgeBundleResult {
    Success(WorkshopJudgeBundleSuccess),
    Error(WorkshopError),
}

pub fn create_default_session_settings() -> SessionSettings {
    create_session_settings(&WorkshopCreateConfig::default())
}

pub fn create_session_settings(config: &WorkshopCreateConfig) -> SessionSettings {
    let mut phases = BTreeMap::new();
    phases.insert(
        Phase::Lobby,
        SessionPhaseSettings {
            step: 0,
            label: "Lobby - Waiting Room".to_string(),
            description:
                "Join the workshop, pick or create your character from the lobby, and wait for the host to begin discovery."
                    .to_string(),
            duration_seconds: 0,
            allowed_commands: vec![
                SessionCommand::Join,
                SessionCommand::SelectCharacter,
                SessionCommand::StartPhase1,
                SessionCommand::ResetGame,
                SessionCommand::LeaveWorkshop,
            ],
        },
    );
    phases.insert(
        Phase::Phase0,
        SessionPhaseSettings {
            step: 1,
            label: "Phase 0 - Character Atelier".to_string(),
            description:
                "Create or redraw a global character from the lobby before discovery begins."
                    .to_string(),
            duration_seconds: (config.phase0_minutes as i32) * 60,
            allowed_commands: vec![
                SessionCommand::SelectCharacter,
                SessionCommand::StartPhase1,
                SessionCommand::ResetGame,
                SessionCommand::LeaveWorkshop,
            ],
        },
    );
    phases.insert(
        Phase::Phase1,
        SessionPhaseSettings {
            step: 2,
            label: "Phase 1 - Discovery".to_string(),
            description:
                "Observe the pet, test assumptions, and discover what care patterns actually work."
                    .to_string(),
            duration_seconds: (config.phase1_minutes as i32) * 60,
            allowed_commands: vec![
                SessionCommand::Action,
                SessionCommand::SubmitObservation,
                SessionCommand::StartHandover,
                SessionCommand::ResetGame,
            ],
        },
    );
    phases.insert(
        Phase::Handover,
        SessionPhaseSettings {
            step: 3,
            label: "Phase 2 - Handover".to_string(),
            description:
                "Capture instructions and context so another teammate can inherit the pet."
                    .to_string(),
            duration_seconds: (config.phase2_minutes as i32) * 60,
            allowed_commands: vec![
                SessionCommand::SubmitTags,
                SessionCommand::StartPhase2,
                SessionCommand::ResetGame,
            ],
        },
    );
    phases.insert(
        Phase::Phase2,
        SessionPhaseSettings {
            step: 4,
            label: "Phase 2 - Shuffle & Care".to_string(),
            description:
                "Take over a reassigned pet and apply what the previous teammate documented."
                    .to_string(),
            duration_seconds: (config.phase2_minutes as i32) * 60,
            allowed_commands: vec![
                SessionCommand::Action,
                SessionCommand::EndGame,
                SessionCommand::ResetGame,
            ],
        },
    );
    phases.insert(
        Phase::Judge,
        SessionPhaseSettings {
            step: 5,
            label: "Judge Review".to_string(),
            description:
                "Review the judge's mechanics evaluation before opening the anonymous design vote."
                    .to_string(),
            duration_seconds: 0,
            allowed_commands: vec![SessionCommand::StartVoting, SessionCommand::ResetGame],
        },
    );
    phases.insert(
        Phase::Voting,
        SessionPhaseSettings {
            step: 6,
            label: "Phase 3 - Design Vote".to_string(),
            description: "Vote for the most creative pet design and wait for the host to reveal the final standings.".to_string(),
            duration_seconds: 0,
            allowed_commands: vec![
                SessionCommand::SubmitVote,
                SessionCommand::RevealVotingResults,
                SessionCommand::ResetGame,
                SessionCommand::EndSession,
            ],
        },
    );
    phases.insert(
        Phase::End,
        SessionPhaseSettings {
            step: 7,
            label: "Final Leaderboard".to_string(),
            description: "Reveal final scores, creator identities, and wrap up the workshop."
                .to_string(),
            duration_seconds: 0,
            allowed_commands: vec![SessionCommand::ResetGame, SessionCommand::LeaveWorkshop],
        },
    );

    SessionSettings { phases }
}

// ---------------------------------------------------------------------------
// Accounts & character ownership (refactor-plan.md §3)
// ---------------------------------------------------------------------------

/// Public-facing account profile. Never carries the password hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProfile {
    pub id: String,
    pub hero: String,
    pub name: String,
}

/// Combined sign-in / sign-up form submission. Server disambiguates:
/// - name free → create account
/// - name exists + password matches → login
/// - name exists + password mismatch → 401
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthRequest {
    pub hero: String,
    pub name: String,
    pub password: String,
}

/// Outcome of a successful `/api/auth` call. Cookie is set separately by the
/// HTTP layer; this body is the account snapshot for the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthResponse {
    pub account: AccountProfile,
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCharacterRequest {
    pub description: String,
    pub sprites: SpriteSet,
}

/// Request body for `POST /api/characters/preview-sprites`.
///
/// Account-scoped sprite-sheet preview. Unlike the workshop-scoped
/// `CharacterSpriteSheetRequest`, this carries no session credentials —
/// authentication is via the signed session cookie (`AccountSession`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterSpritePreviewRequest {
    pub description: String,
}

/// Response body for `POST /api/characters/preview-sprites`.
///
/// Returns the generated 4-frame sprite sheet without persisting anything.
/// The frontend holds this in memory and, on confirm, submits it via
/// `POST /api/characters`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterSpritePreviewResponse {
    pub sprites: SpriteSet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MyCharactersResponse {
    pub characters: Vec<CharacterProfile>,
    pub limit: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EligibleCharactersResponse {
    pub characters: Vec<CharacterProfile>,
}

/// One row in the "open workshops" list shown on AccountHome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenWorkshopSummary {
    pub session_code: String,
    pub host_name: String,
    pub player_count: u32,
    pub created_at: String,
    #[serde(default)]
    pub can_delete: bool,
}

/// Keyset cursor over the "open workshops" list. Pairs the RFC3339 `created_at`
/// timestamp with the `session_code` tie-breaker so pagination is stable when
/// two lobbies are created in the same tick. Ordering is DESC by `created_at`,
/// then ASC by `session_code` (both in-memory and Postgres backends agree).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenWorkshopCursor {
    pub created_at: String,
    pub session_code: String,
}

/// Request shape for `GET /api/workshops/open` is transported as query
/// params on the wire (`after_created_at` / `after_session_code` XOR
/// `before_created_at` / `before_session_code`). No dedicated request
/// struct is needed — the server parses those params directly and the
/// client builds them via query-string construction.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListOpenWorkshopsResponse {
    pub workshops: Vec<OpenWorkshopSummary>,
    /// Cursor to pass as `after=` for the next (older) page, if one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<OpenWorkshopCursor>,
    /// Cursor to pass as `before=` for the previous (newer) page, if one
    /// exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_cursor: Option<OpenWorkshopCursor>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_serializes_to_expected_wire_value() {
        let json = serde_json::to_string(&Phase::Phase1).expect("serialize phase");
        assert_eq!(json, "\"phase1\"");
    }

    #[test]
    fn invalid_phase_fails_to_deserialize() {
        let result = serde_json::from_str::<Phase>("\"phase_1\"");
        assert!(result.is_err());
    }

    #[test]
    fn join_request_allows_missing_optional_fields() {
        let request: JoinWorkshopRequest =
            serde_json::from_str(r#"{"sessionCode":"123456"}"#).expect("deserialize join request");

        assert_eq!(request.session_code, "123456");
        assert_eq!(request.name, None);
        assert_eq!(request.character_id, None);
        assert_eq!(request.reconnect_token, None);
    }

    #[test]
    fn invalid_command_fails_to_deserialize() {
        let result = serde_json::from_str::<SessionCommand>("\"launchMissiles\"");
        assert!(result.is_err());
    }

    #[test]
    fn action_payload_allows_missing_value() {
        let payload: ActionPayload =
            serde_json::from_str(r#"{"type":"sleep"}"#).expect("deserialize action payload");

        assert_eq!(payload.action_type, "sleep");
        assert_eq!(payload.value, None);
    }

    #[test]
    fn coordinator_type_serializes_to_expected_wire_value() {
        let json = serde_json::to_string(&CoordinatorType::Rust).expect("serialize coordinator");
        assert_eq!(json, "\"rust\"");
    }

    #[test]
    fn workshop_command_request_skips_missing_optional_fields() {
        let request = WorkshopCommandRequest {
            session_code: "123456".to_string(),
            reconnect_token: "token-1".to_string(),
            coordinator_type: None,
            command: SessionCommand::ResetGame,
            payload: None,
        };

        let value = serde_json::to_value(&request).expect("serialize command request");
        let object = value.as_object().expect("command request object");

        assert!(!object.contains_key("payload"));
        assert!(!object.contains_key("coordinatorType"));
        assert_eq!(
            object.get("command").and_then(|v| v.as_str()),
            Some("resetGame")
        );
    }

    #[test]
    fn untagged_command_result_parses_error_branch() {
        let result: WorkshopCommandResult = serde_json::from_str(r#"{"ok":false,"error":"boom"}"#)
            .expect("deserialize command result");

        match result {
            WorkshopCommandResult::Error(error) => assert_eq!(error.error, "boom"),
            WorkshopCommandResult::Success(_) => panic!("expected error result branch"),
        }
    }

    #[test]
    fn session_artifact_kind_serializes_to_snake_case() {
        let json = serde_json::to_string(&SessionArtifactKind::JudgeBundleGenerated)
            .expect("serialize artifact kind");
        assert_eq!(json, "\"judge_bundle_generated\"");
    }

    #[test]
    fn default_session_settings_cover_all_phases() {
        let settings = create_default_session_settings();

        assert_eq!(settings.phases.len(), 8);
        assert_eq!(
            settings
                .phases
                .get(&Phase::Lobby)
                .expect("lobby phase")
                .step,
            0
        );
        assert!(
            settings
                .phases
                .get(&Phase::Lobby)
                .expect("lobby phase")
                .allowed_commands
                .contains(&SessionCommand::Join)
        );
        assert!(
            settings
                .phases
                .get(&Phase::Lobby)
                .expect("lobby phase")
                .allowed_commands
                .contains(&SessionCommand::SelectCharacter)
        );
        assert!(
            settings
                .phases
                .get(&Phase::Judge)
                .expect("judge phase")
                .allowed_commands
                .contains(&SessionCommand::StartVoting)
        );
        assert!(
            settings
                .phases
                .get(&Phase::Voting)
                .expect("voting phase")
                .allowed_commands
                .contains(&SessionCommand::SubmitVote)
        );
    }

    #[test]
    fn character_sprite_sheet_request_skips_missing_character_id() {
        let request = CharacterSpriteSheetRequest {
            session_code: "123456".to_string(),
            reconnect_token: "reconnect-1".to_string(),
            description: "A moonlit dragon".to_string(),
            character_id: None,
        };

        let value = serde_json::to_value(&request).expect("serialize character sprite request");
        let object = value.as_object().expect("character sprite request object");

        assert_eq!(
            object.get("description").and_then(|value| value.as_str()),
            Some("A moonlit dragon")
        );
        assert!(!object.contains_key("characterId"));
    }
}
