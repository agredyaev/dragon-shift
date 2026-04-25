use chrono::{DateTime, Utc};
use domain::WorkshopSession;
use protocol::{CharacterProfile, OpenWorkshopCursor, SessionArtifactRecord, SpriteSet};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::RwLock;
use thiserror::Error;

#[cfg(test)]
mod postgres_tests;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("session store lock poisoned")]
    LockPoisoned,
    #[error("duplicate artifact id {artifact_id}")]
    DuplicateArtifactId { artifact_id: String },
    #[error(
        "stale session write rejected for session {session_code} ({session_id}) at {attempted_updated_at}"
    )]
    StaleSessionWrite {
        session_id: String,
        session_code: String,
        attempted_updated_at: String,
    },
    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session lease acquisition timed out for {session_code}")]
    SessionLeaseTimeout { session_code: String },
    #[error("realtime connection {connection_id} has been retired")]
    RetiredRealtimeConnection { connection_id: String },
    #[error("account name already in use")]
    DuplicateAccountName,
    #[error("character limit reached ({max} per account)")]
    CharacterLimitReached { max: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerIdentity {
    pub session_id: String,
    pub player_id: String,
    pub reconnect_token: String,
    pub created_at: String,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerIdentityMatch {
    pub session_id: String,
    pub player_id: String,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionUpdateNotification {
    pub kind: String,
    pub session_code: String,
    pub updated_at: Option<String>,
    pub payload_fingerprint: Option<String>,
    pub connection_id: Option<String>,
    pub replica_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealtimeConnectionRegistration {
    pub session_code: String,
    pub player_id: String,
    pub connection_id: String,
    pub replica_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealtimeConnectionClaim {
    pub replaced: Option<RealtimeConnectionRegistration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealtimeConnectionRestore {
    pub restored: bool,
    pub replaced: Option<RealtimeConnectionRegistration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSpriteDefaults {
    pub key: String,
    pub sprites: SpriteSet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterRecord {
    pub id: String,
    pub description: String,
    pub sprites: SpriteSet,
    pub remaining_sprite_regenerations: u8,
    pub created_at: String,
    pub updated_at: String,
    /// `None` = starter pool (leasable), `Some(account_id)` = owned by that account.
    /// Added in migration 0007; pre-0007 rows read back as `None`.
    #[doc(hidden)]
    pub owner_account_id: Option<String>,
}

impl CharacterRecord {
    pub fn profile(&self) -> CharacterProfile {
        CharacterProfile {
            id: self.id.clone(),
            description: self.description.clone(),
            sprites: self.sprites.clone(),
            remaining_sprite_regenerations: self.remaining_sprite_regenerations,
        }
    }
}

/// Summary row returned by `SessionStore::list_open_workshops`. Captures
/// only what AccountHome needs to render the lobby list. Intentionally a
/// plain persistence DTO so the service layer can map it to the protocol's
/// `OpenWorkshopSummary` without leaking `WorkshopSession` internals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWorkshopRecord {
    pub session_code: String,
    pub host_name: String,
    pub player_count: u32,
    pub created_at: String,
    pub owner_account_id: Option<String>,
    pub has_non_owner_players: bool,
}

/// Paging direction for `SessionStore::list_open_workshops`. Cursors reuse
/// `protocol::OpenWorkshopCursor` verbatim (same `{created_at, session_code}`
/// shape) so the HTTP boundary is a trivial passthrough and the types can't
/// drift apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenWorkshopsPaging {
    First,
    After(protocol::OpenWorkshopCursor),
    Before(protocol::OpenWorkshopCursor),
}

/// Page returned by `SessionStore::list_open_workshops`. `rows` is already
/// truncated to at most `OPEN_WORKSHOPS_PAGE_SIZE` and ordered DESC by
/// `(created_at, session_code ASC)`. The `has_more_*` flags let callers
/// synthesize next/prev cursors without another query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWorkshopsPage {
    pub rows: Vec<OpenWorkshopRecord>,
    /// True iff a strictly older row exists beyond the last returned row.
    pub has_more_after: bool,
    /// True iff a strictly newer row exists before the first returned row.
    pub has_more_before: bool,
}

/// Page size for the "open workshops" list. Also the internal fetch is
/// `PAGE_SIZE + 1` to let the impl compute `has_more_after` / `has_more_before`.
pub const OPEN_WORKSHOPS_PAGE_SIZE: usize = 4;

/// Row in the `accounts` table (migration 0007). Never carries plaintext
/// password; only the argon2id PHC hash produced by `security::hash_password`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRecord {
    pub id: String,
    pub hero: String,
    pub name: String,
    pub password_hash: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_login_at: Option<String>,
}

pub const TIMEOUT_COMPANION_SPRITE_KEY: &str = "timeout_companion";

const TIMEOUT_COMPANION_NEUTRAL_SPRITE: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAApElEQVR42mNgGAWjYBQMFfD/asJ/WqrHaxA6Rpavjvn/nxT1VHEAyFJcmKoOwGYYPkfgUz80Q2BQOADZYHwWY3MIjE2WxWum/v+PjEmxHIbRzaDYAXJycmCMy0J0eZo4AATwOQBZnuoOGNAoePd4Hcl4eDvg/6S1ePk0dYCNbgBOPHyiAB2Q4wCqtAfQsxYxFqPrGV4OwCZPqh6qOoBaekbByAUAnY5G6OllDUkAAAAASUVORK5CYII=";
const TIMEOUT_COMPANION_HAPPY_SPRITE: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAvElEQVR42mNgGAWjYBQMFfD/asJ/WqrHaxA6Rpavjvn/nxT1VHEAyFJcmKoOwGYYPkfgUz80Q2BQOADZYHwWY3MIjE2WxWum/v+PjEmxHIbRzaDYAXJycmCMy0J0eZo4AATwOQBZnuoOsNENIBjsyGqo6oB3j9eBMcgCGBsdo8vRxAGkYJo54P+ktVgtRBcfviGAKy1QNQ2gA1yJDhmjy1OlPYCezYgJenQ9w8sB2ORJ1UNVB1BLzygYuQAAmLxC6PcDH7YAAAAASUVORK5CYII=";
const TIMEOUT_COMPANION_ANGRY_SPRITE: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAArklEQVR42mNgGAWjYBQMFfD/asJ/WqrHaxA6Rpavjvn/nxT1VHEAyFJcmKoOwGYYPkfgUz80Q2BQOADZYHwWY3MIjE2WxWum/v+PjG10A8CYkMXI6tDNoMgBMAvk5ORwWo4uRxMHgIP3P5YgxyJGMwcQi6nqgHeP15GMRx1Acwf8T3ZHoenmAJCFuDDVHIAOyIkCqrQH0LMWMRaj6xleDsAmT6oeqjqAWnpGwcgFAAfTTcoNBh/0AAAAAElFTkSuQmCC";
const TIMEOUT_COMPANION_SLEEPY_SPRITE: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAApElEQVR42mNgGAWjYBQMFfD/asJ/WqrHaxA6Rpavjvn/nxT1VHEAyFJcmKoOwGYYPkfgUz80Q2BQOADZYHwWY3MIjE2WxWum/v+PjEmxHIbRzaDYATU7CFuKrIaqDgAZjIyxWYwuT/UQGNAoePd4Hcl41AHD1wE2ugFYMVUdgA7QDUe2EJvlIEyV9gB61iIm6NH1DC8HYJMnVQ9VHUAtPaNg5AIAXXpP4NPsjugAAAAASUVORK5CYII=";

pub fn timeout_companion_defaults() -> AppSpriteDefaults {
    AppSpriteDefaults {
        key: TIMEOUT_COMPANION_SPRITE_KEY.to_string(),
        sprites: SpriteSet {
            neutral: TIMEOUT_COMPANION_NEUTRAL_SPRITE.to_string(),
            happy: TIMEOUT_COMPANION_HAPPY_SPRITE.to_string(),
            angry: TIMEOUT_COMPANION_ANGRY_SPRITE.to_string(),
            sleepy: TIMEOUT_COMPANION_SLEEPY_SPRITE.to_string(),
        },
    }
}

pub fn starter_character_defaults() -> Vec<CharacterRecord> {
    let defaults = timeout_companion_defaults().sprites;
    vec![
        CharacterRecord {
            id: "starter_violet_crystal".to_string(),
            description:
                "A violet crystal dragon with lantern eyes and a careful, observant posture."
                    .to_string(),
            sprites: defaults.clone(),
            remaining_sprite_regenerations: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            owner_account_id: None,
        },
        CharacterRecord {
            id: "starter_moss_forest".to_string(),
            description:
                "A mossy forest dragon with fern-like frills and a warm trail-guide demeanor."
                    .to_string(),
            sprites: defaults.clone(),
            remaining_sprite_regenerations: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            owner_account_id: None,
        },
        CharacterRecord {
            id: "starter_sunset_coral".to_string(),
            description:
                "A coral sunset dragon with tide-polished scales and a bright show-off streak."
                    .to_string(),
            sprites: defaults.clone(),
            remaining_sprite_regenerations: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            owner_account_id: None,
        },
        CharacterRecord {
            id: "starter_midnight_moon".to_string(),
            description:
                "A midnight moon dragon with silver horns, soft wings, and a nocturnal calm."
                    .to_string(),
            sprites: defaults,
            remaining_sprite_regenerations: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            owner_account_id: None,
        },
    ]
}

pub const REALTIME_CONNECTION_TTL_SECONDS: i64 = 15;
pub const REALTIME_CONNECTION_TTL: std::time::Duration =
    std::time::Duration::from_secs(REALTIME_CONNECTION_TTL_SECONDS as u64);

impl SessionUpdateNotification {
    pub fn session_state_changed(session: &WorkshopSession) -> Self {
        Self {
            kind: "session_state_changed".to_string(),
            session_code: session.code.0.clone(),
            updated_at: Some(session.updated_at.to_rfc3339()),
            payload_fingerprint: Some(session_payload_fingerprint(session)),
            connection_id: None,
            replica_id: None,
        }
    }

    pub fn realtime_connection_replaced(registration: &RealtimeConnectionRegistration) -> Self {
        Self {
            kind: "realtime_connection_replaced".to_string(),
            session_code: registration.session_code.clone(),
            updated_at: None,
            payload_fingerprint: None,
            connection_id: Some(registration.connection_id.clone()),
            replica_id: Some(registration.replica_id.clone()),
        }
    }

    pub fn workshop_deleted(session_code: &str) -> Self {
        Self {
            kind: "workshop_deleted".to_string(),
            session_code: session_code.to_string(),
            updated_at: None,
            payload_fingerprint: None,
            connection_id: None,
            replica_id: None,
        }
    }

    pub fn to_payload(&self) -> Result<String, PersistenceError> {
        let mut payload = serde_json::Map::new();
        payload.insert(
            "kind".to_string(),
            serde_json::Value::String(self.kind.clone()),
        );
        payload.insert(
            "sessionCode".to_string(),
            serde_json::Value::String(self.session_code.clone()),
        );
        if let Some(updated_at) = &self.updated_at {
            payload.insert(
                "updatedAt".to_string(),
                serde_json::Value::String(updated_at.clone()),
            );
        }
        if let Some(payload_fingerprint) = &self.payload_fingerprint {
            payload.insert(
                "payloadFingerprint".to_string(),
                serde_json::Value::String(payload_fingerprint.clone()),
            );
        }
        if let Some(connection_id) = &self.connection_id {
            payload.insert(
                "connectionId".to_string(),
                serde_json::Value::String(connection_id.clone()),
            );
        }
        if let Some(replica_id) = &self.replica_id {
            payload.insert(
                "replicaId".to_string(),
                serde_json::Value::String(replica_id.clone()),
            );
        }
        Ok(serde_json::Value::Object(payload).to_string())
    }

    pub fn to_legacy_payload(&self) -> String {
        self.session_code.clone()
    }

    pub fn to_publish_payloads(&self) -> Result<[String; 2], PersistenceError> {
        Ok([self.to_payload()?, self.to_legacy_payload()])
    }
}

fn sanitize_runtime_presence(mut session: WorkshopSession) -> WorkshopSession {
    for player in session.players.values_mut() {
        player.is_connected = false;
    }
    session
}

fn session_payload_fingerprint(session: &WorkshopSession) -> String {
    let payload = serde_json::to_string(&sanitize_runtime_presence(session.clone()))
        .expect("serialize session for fingerprint");
    let mut hash = 14695981039346656037_u64;
    for byte in payload.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("{hash:016x}")
}

fn parse_lease_deadline(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn active_realtime_connection_cutoff() -> DateTime<Utc> {
    Utc::now() - chrono::Duration::seconds(REALTIME_CONNECTION_TTL_SECONDS)
}

/// Map a `WorkshopSession` to the summary record returned by
/// `list_open_workshops`. Empty lobbies are allowed before the reserved
/// creator explicitly joins, so the summary falls back to the reserved host
/// name when no concrete host player exists yet.
fn open_workshop_summary_from_session(session: &WorkshopSession) -> Option<OpenWorkshopRecord> {
    let host_name = session
        .host_player_id
        .as_ref()
        .and_then(|id| session.players.get(id))
        .map(|player| player.name.clone())
        .or_else(|| session.reserved_host_name().map(str::to_string))?;
    let owner_account_id = session
        .owner_account_id()
        .or_else(|| session.reserved_host_account_id());
    let has_non_owner_players = session
        .players
        .values()
        .any(|player| player.account_id.as_deref() != owner_account_id);
    Some(OpenWorkshopRecord {
        session_code: session.code.0.clone(),
        host_name,
        player_count: session.players.len() as u32,
        created_at: session.created_at.to_rfc3339(),
        owner_account_id: owner_account_id.map(str::to_string),
        has_non_owner_players,
    })
}

fn is_older_open_workshop_cursor(
    created_at: &str,
    session_code: &str,
    cursor: &OpenWorkshopCursor,
) -> bool {
    created_at < cursor.created_at.as_str()
        || (created_at == cursor.created_at.as_str() && session_code > cursor.session_code.as_str())
}

fn is_newer_open_workshop_cursor(
    created_at: &str,
    session_code: &str,
    cursor: &OpenWorkshopCursor,
) -> bool {
    created_at > cursor.created_at.as_str()
        || (created_at == cursor.created_at.as_str() && session_code < cursor.session_code.as_str())
}

pub trait SessionStore: Send + Sync {
    fn init(&self) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn health_check(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>>;
    fn load_session_by_code(
        &self,
        session_code: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<WorkshopSession>, PersistenceError>> + Send + '_>>;
    fn save_session(
        &self,
        session: &WorkshopSession,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn append_session_artifact(
        &self,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn list_session_artifacts(
        &self,
        session_id: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Vec<SessionArtifactRecord>, PersistenceError>> + Send + '_>,
    >;
    fn create_player_identity(
        &self,
        identity: &PlayerIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn find_player_identity(
        &self,
        session_code: &str,
        reconnect_token: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<PlayerIdentityMatch>, PersistenceError>> + Send + '_>,
    >;
    fn touch_player_identity(
        &self,
        reconnect_token: &str,
        last_seen_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn revoke_player_identity(
        &self,
        reconnect_token: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn save_session_with_artifact(
        &self,
        session: &WorkshopSession,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn save_session_with_identity_and_artifact(
        &self,
        session: &WorkshopSession,
        identity: &PlayerIdentity,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn replace_player_identity_and_save_session_with_artifact(
        &self,
        previous_reconnect_token: &str,
        next_identity: &PlayerIdentity,
        session: &WorkshopSession,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn acquire_session_lease(
        &self,
        session_code: &str,
        lease_id: &str,
        expires_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>>;
    fn release_session_lease(
        &self,
        session_code: &str,
        lease_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn renew_session_lease(
        &self,
        session_code: &str,
        lease_id: &str,
        expires_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>>;
    fn renew_realtime_connection(
        &self,
        connection_id: &str,
        replica_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>>;
    fn claim_realtime_connection(
        &self,
        registration: &RealtimeConnectionRegistration,
    ) -> Pin<Box<dyn Future<Output = Result<RealtimeConnectionClaim, PersistenceError>> + Send + '_>>;
    fn restore_realtime_connection(
        &self,
        registration: &RealtimeConnectionRegistration,
    ) -> Pin<
        Box<dyn Future<Output = Result<RealtimeConnectionRestore, PersistenceError>> + Send + '_>,
    >;
    fn release_realtime_connection(
        &self,
        connection_id: &str,
        replica_id: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    >;
    fn take_retired_realtime_connection(
        &self,
        connection_id: &str,
        replica_id: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    >;
    fn list_realtime_connections(
        &self,
        session_code: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    >;
    fn load_app_sprite_defaults(
        &self,
        key: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<AppSpriteDefaults>, PersistenceError>> + Send + '_>,
    >;
    fn load_character(
        &self,
        character_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<CharacterRecord>, PersistenceError>> + Send + '_>>;
    fn list_characters(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CharacterRecord>, PersistenceError>> + Send + '_>>;
    fn save_character(
        &self,
        character: &CharacterRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    /// Atomically enforce a per-owner character cap and insert. Intended for
    /// owner-scoped character creation where the cap (`max`) must be checked
    /// under the same lock as the insert to prevent a TOCTOU race between
    /// `count_characters_by_owner` and `save_character`.
    ///
    /// Returns `CharacterLimitReached` if the owner already has `max` rows.
    /// Callers must set `character.owner_account_id = Some(owner_id)`.
    fn save_character_enforcing_cap(
        &self,
        character: &CharacterRecord,
        max: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;
    fn publish_session_notification(
        &self,
        notification: &SessionUpdateNotification,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;

    // -----------------------------------------------------------------
    // Accounts (added in migration 0007 / session 3)
    // -----------------------------------------------------------------

    /// Insert a new account row. Returns `DuplicateAccountName` if the
    /// case-insensitive unique index on `accounts(LOWER(name))` is violated.
    fn insert_account(
        &self,
        account: &AccountRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;

    /// Look up an account by case-insensitive name. Returns `None` if no match.
    fn find_account_by_name_lower(
        &self,
        name: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<AccountRecord>, PersistenceError>> + Send + '_>>;

    /// Look up an account by id. Returns `None` if the id is unknown.
    fn find_account_by_id(
        &self,
        account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<AccountRecord>, PersistenceError>> + Send + '_>>;

    /// Stamp the `last_login_at` column with the supplied RFC3339 timestamp.
    /// No-op if the account does not exist (idempotent best-effort).
    fn touch_last_login(
        &self,
        account_id: &str,
        now: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>>;

    // -----------------------------------------------------------------
    // Owner-aware character queries (added in migration 0007 / session 3)
    // -----------------------------------------------------------------

    /// List characters owned by the given account, oldest-first.
    /// Does NOT return starter-pool rows (`owner_account_id IS NULL`).
    fn list_characters_by_owner(
        &self,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CharacterRecord>, PersistenceError>> + Send + '_>>;

    /// Count characters owned by the given account. Used by the service layer
    /// to enforce `domain::MAX_CHARACTERS_PER_ACCOUNT`.
    fn count_characters_by_owner(
        &self,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<u32, PersistenceError>> + Send + '_>>;

    /// Delete a character owned by the given account. Returns `true` if a row
    /// was deleted, `false` if the id was not found or belonged to another
    /// account (the service layer maps `false` → 404).
    fn delete_character_by_owner(
        &self,
        character_id: &str,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>>;

    fn delete_lobby_workshop_by_owner(
        &self,
        session_code: &str,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>>;

    fn delete_realtime_connections_for_session(
        &self,
        session_code: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    >;

    /// List sessions currently in `Phase::Lobby` for AccountHome's "open
    /// workshops" list. Ordered DESC by `created_at` with `session_code ASC`
    /// as a stable tie-breaker so the UI surfaces the freshest lobbies.
    /// Paginated via a bidirectional keyset cursor on `(created_at,
    /// session_code)`; callers pass `OpenWorkshopsPaging::First` for the
    /// initial page, `After(cursor)` to move to older rows, or
    /// `Before(cursor)` to move back toward newer rows. Page size is
    /// `OPEN_WORKSHOPS_PAGE_SIZE`. Only returns summary fields; callers
    /// needing full session state should still go through `load_session_by_code`.
    fn list_open_workshops(
        &self,
        paging: OpenWorkshopsPaging,
    ) -> Pin<Box<dyn Future<Output = Result<OpenWorkshopsPage, PersistenceError>> + Send + '_>>;
}

#[derive(Debug, Default)]
pub struct InMemorySessionStore {
    sessions_by_code: RwLock<HashMap<String, WorkshopSession>>,
    sessions_by_id: RwLock<HashMap<String, WorkshopSession>>,
    artifacts_by_session_id: RwLock<HashMap<String, Vec<SessionArtifactRecord>>>,
    identities_by_token: RwLock<HashMap<String, PlayerIdentity>>,
    app_sprite_defaults_by_key: RwLock<HashMap<String, AppSpriteDefaults>>,
    characters_by_id: RwLock<HashMap<String, CharacterRecord>>,
    accounts_by_id: RwLock<HashMap<String, AccountRecord>>,
    session_leases: RwLock<HashMap<String, (String, String)>>,
    realtime_connections_by_id:
        RwLock<HashMap<String, (RealtimeConnectionRegistration, DateTime<Utc>)>>,
    realtime_connection_by_session_player: RwLock<HashMap<(String, String), String>>,
    retired_realtime_connections: RwLock<HashMap<String, String>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        let store = Self::default();
        store
            .seed_app_sprite_defaults(timeout_companion_defaults())
            .expect("seed in-memory timeout companion sprites");
        for character in starter_character_defaults() {
            store
                .characters_by_id
                .write()
                .expect("seed in-memory characters")
                .insert(character.id.clone(), character);
        }
        store
    }

    pub fn seed_app_sprite_defaults(
        &self,
        defaults: AppSpriteDefaults,
    ) -> Result<(), PersistenceError> {
        self.app_sprite_defaults_by_key
            .write()
            .map_err(|_| PersistenceError::LockPoisoned)?
            .insert(defaults.key.clone(), defaults);
        Ok(())
    }
}

pub struct PostgresSessionStore {
    pool: sqlx::PgPool,
}

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

impl PostgresSessionStore {
    pub async fn connect(
        database_url: &str,
        max_connections: u32,
    ) -> Result<Self, PersistenceError> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    async fn save_session_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        session: &WorkshopSession,
    ) -> Result<(), PersistenceError> {
        let sanitized_session = sanitize_runtime_presence(session.clone());
        let payload = serde_json::to_value(&sanitized_session)?;
        let notification = SessionUpdateNotification::session_state_changed(&sanitized_session);
        let result = sqlx::query(
            "
                INSERT INTO workshop_sessions (session_id, session_code, payload, updated_at)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (session_id) DO UPDATE SET
                    session_code = EXCLUDED.session_code,
                    payload = EXCLUDED.payload,
                    updated_at = EXCLUDED.updated_at
                WHERE workshop_sessions.updated_at::timestamptz < EXCLUDED.updated_at::timestamptz
                    OR (
                        workshop_sessions.updated_at::timestamptz = EXCLUDED.updated_at::timestamptz
                        AND workshop_sessions.payload::text < EXCLUDED.payload::text
                    )
                ",
        )
        .bind(sanitized_session.id.to_string())
        .bind(&sanitized_session.code.0)
        .bind(sqlx::types::Json(&payload))
        .bind(sanitized_session.updated_at.to_rfc3339())
        .execute(&mut **tx)
        .await?;

        if result.rows_affected() == 0 {
            return Err(PersistenceError::StaleSessionWrite {
                session_id: sanitized_session.id.to_string(),
                session_code: sanitized_session.code.0.clone(),
                attempted_updated_at: sanitized_session.updated_at.to_rfc3339(),
            });
        }

        for payload in notification.to_publish_payloads()? {
            sqlx::query("SELECT pg_notify('session_updates', $1)")
                .bind(payload)
                .execute(&mut **tx)
                .await?;
        }
        Ok(())
    }

    async fn save_character_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        character: &CharacterRecord,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            "
                INSERT INTO characters (
                    character_id,
                    description,
                    neutral_sprite,
                    happy_sprite,
                    angry_sprite,
                    sleepy_sprite,
                    remaining_sprite_regenerations,
                    created_at,
                    updated_at,
                    owner_account_id
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                ON CONFLICT (character_id) DO UPDATE SET
                    description = EXCLUDED.description,
                    neutral_sprite = EXCLUDED.neutral_sprite,
                    happy_sprite = EXCLUDED.happy_sprite,
                    angry_sprite = EXCLUDED.angry_sprite,
                    sleepy_sprite = EXCLUDED.sleepy_sprite,
                    remaining_sprite_regenerations = EXCLUDED.remaining_sprite_regenerations,
                    created_at = EXCLUDED.created_at,
                    updated_at = EXCLUDED.updated_at,
                    owner_account_id = EXCLUDED.owner_account_id
            ",
        )
        .bind(&character.id)
        .bind(&character.description)
        .bind(&character.sprites.neutral)
        .bind(&character.sprites.happy)
        .bind(&character.sprites.angry)
        .bind(&character.sprites.sleepy)
        .bind(i16::from(character.remaining_sprite_regenerations))
        .bind(&character.created_at)
        .bind(&character.updated_at)
        .bind(&character.owner_account_id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    fn character_from_row(row: &sqlx::postgres::PgRow) -> CharacterRecord {
        use sqlx::Row;

        CharacterRecord {
            id: row.get("character_id"),
            description: row.get("description"),
            sprites: SpriteSet {
                neutral: row.get("neutral_sprite"),
                happy: row.get("happy_sprite"),
                angry: row.get("angry_sprite"),
                sleepy: row.get("sleepy_sprite"),
            },
            remaining_sprite_regenerations: row
                .get::<i16, _>("remaining_sprite_regenerations")
                .clamp(0, u8::MAX as i16) as u8,
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            owner_account_id: row
                .try_get::<Option<String>, _>("owner_account_id")
                .ok()
                .flatten(),
        }
    }

    fn account_from_row(row: &sqlx::postgres::PgRow) -> AccountRecord {
        use sqlx::Row;

        AccountRecord {
            id: row.get("account_id"),
            hero: row.get("hero"),
            name: row.get("name"),
            password_hash: row.get("password_hash"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            last_login_at: row.get("last_login_at"),
        }
    }

    async fn seed_default_characters(&self) -> Result<(), PersistenceError> {
        let mut tx = self.pool.begin().await?;
        for character in starter_character_defaults() {
            Self::save_character_in_tx(&mut tx, &character).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn append_session_artifact_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        artifact: &SessionArtifactRecord,
    ) -> Result<(), PersistenceError> {
        let payload = serde_json::to_value(artifact)?;
        sqlx::query(
            "
                INSERT INTO session_artifacts (id, session_id, created_at, payload)
                VALUES ($1, $2, $3, $4)
                ",
        )
        .bind(&artifact.id)
        .bind(&artifact.session_id)
        .bind(&artifact.created_at)
        .bind(sqlx::types::Json(&payload))
        .execute(&mut **tx)
        .await
        .map_err(|error| match error {
            sqlx::Error::Database(database_error)
                if database_error.constraint() == Some("session_artifacts_pkey") =>
            {
                PersistenceError::DuplicateArtifactId {
                    artifact_id: artifact.id.clone(),
                }
            }
            other => PersistenceError::Sqlx(other),
        })?;
        Ok(())
    }

    async fn create_player_identity_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        identity: &PlayerIdentity,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            "
                INSERT INTO player_identities (reconnect_token, session_id, player_id, created_at, last_seen_at)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (reconnect_token) DO UPDATE SET
                    session_id = EXCLUDED.session_id,
                    player_id = EXCLUDED.player_id,
                    created_at = EXCLUDED.created_at,
                    last_seen_at = EXCLUDED.last_seen_at
                ",
        )
        .bind(&identity.reconnect_token)
        .bind(&identity.session_id)
        .bind(&identity.player_id)
        .bind(&identity.created_at)
        .bind(&identity.last_seen_at)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn revoke_player_identity_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        reconnect_token: &str,
    ) -> Result<(), PersistenceError> {
        sqlx::query("DELETE FROM player_identities WHERE reconnect_token = $1")
            .bind(reconnect_token)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    async fn acquire_session_lease_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        session_code: &str,
        lease_id: &str,
        expires_at: &str,
    ) -> Result<bool, PersistenceError> {
        let result = sqlx::query(
            "
                INSERT INTO session_leases (session_code, lease_id, expires_at)
                VALUES ($1, $2, $3::timestamptz)
                ON CONFLICT (session_code) DO UPDATE SET
                    lease_id = EXCLUDED.lease_id,
                    expires_at = EXCLUDED.expires_at
                WHERE session_leases.expires_at::timestamptz <= NOW()
                    OR session_leases.lease_id = EXCLUDED.lease_id
            ",
        )
        .bind(session_code)
        .bind(lease_id)
        .bind(expires_at)
        .execute(&mut **tx)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn release_session_lease_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        session_code: &str,
        lease_id: &str,
    ) -> Result<(), PersistenceError> {
        sqlx::query("DELETE FROM session_leases WHERE session_code = $1 AND lease_id = $2")
            .bind(session_code)
            .bind(lease_id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    async fn renew_session_lease_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        session_code: &str,
        lease_id: &str,
        expires_at: &str,
    ) -> Result<bool, PersistenceError> {
        let result = sqlx::query(
            "
                UPDATE session_leases
                SET expires_at = $3::timestamptz
                WHERE session_code = $1 AND lease_id = $2
            ",
        )
        .bind(session_code)
        .bind(lease_id)
        .bind(expires_at)
        .execute(&mut **tx)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn renew_realtime_connection_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        connection_id: &str,
        replica_id: &str,
    ) -> Result<bool, PersistenceError> {
        let result = sqlx::query(
            "
                UPDATE realtime_connections
                SET updated_at = NOW()
                WHERE connection_id = $1
                  AND replica_id = $2
                  AND updated_at > NOW() - INTERVAL '15 seconds'
            ",
        )
        .bind(connection_id)
        .bind(replica_id)
        .execute(&mut **tx)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn cleanup_realtime_runtime_state_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            "
                DELETE FROM realtime_connections
                WHERE updated_at <= NOW() - INTERVAL '15 seconds'
            ",
        )
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    async fn lock_realtime_session_player_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        session_code: &str,
        player_id: &str,
    ) -> Result<(), PersistenceError> {
        let lock_key = format!("{session_code}:{player_id}");
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(lock_key)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    async fn claim_realtime_connection_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        registration: &RealtimeConnectionRegistration,
        allow_retired_connection: bool,
    ) -> Result<RealtimeConnectionClaim, PersistenceError> {
        use sqlx::Row;

        Self::cleanup_realtime_runtime_state_in_tx(tx).await?;
        Self::lock_realtime_session_player_in_tx(
            tx,
            &registration.session_code,
            &registration.player_id,
        )
        .await?;

        if allow_retired_connection {
            sqlx::query(
                "DELETE FROM retired_realtime_connections WHERE connection_id = $1 AND replica_id = $2",
            )
            .bind(&registration.connection_id)
            .bind(&registration.replica_id)
            .execute(&mut **tx)
            .await?;
        } else {
            let fenced =
                sqlx::query("SELECT 1 FROM retired_realtime_connections WHERE connection_id = $1")
                    .bind(&registration.connection_id)
                    .fetch_optional(&mut **tx)
                    .await?
                    .is_some();
            if fenced {
                return Err(PersistenceError::RetiredRealtimeConnection {
                    connection_id: registration.connection_id.clone(),
                });
            }
        }

        let replaced = sqlx::query(
            "
                DELETE FROM realtime_connections
                WHERE session_code = $1 AND player_id = $2 AND connection_id <> $3
                RETURNING session_code, player_id, connection_id, replica_id
            ",
        )
        .bind(&registration.session_code)
        .bind(&registration.player_id)
        .bind(&registration.connection_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(|row| RealtimeConnectionRegistration {
            session_code: row.get("session_code"),
            player_id: row.get("player_id"),
            connection_id: row.get("connection_id"),
            replica_id: row.get("replica_id"),
        });

        if let Some(replaced) = replaced.as_ref() {
            sqlx::query(
                "
                    INSERT INTO retired_realtime_connections (connection_id, replica_id, retired_at)
                    VALUES ($1, $2, NOW())
                    ON CONFLICT (connection_id) DO UPDATE SET
                        replica_id = EXCLUDED.replica_id,
                        retired_at = EXCLUDED.retired_at
                ",
            )
            .bind(&replaced.connection_id)
            .bind(&replaced.replica_id)
            .execute(&mut **tx)
            .await?;
        }

        sqlx::query(
            "
                INSERT INTO realtime_connections (connection_id, session_code, player_id, replica_id, updated_at)
                VALUES ($1, $2, $3, $4, NOW())
                ON CONFLICT (connection_id) DO UPDATE SET
                    session_code = EXCLUDED.session_code,
                    player_id = EXCLUDED.player_id,
                    replica_id = EXCLUDED.replica_id,
                    updated_at = NOW()
            ",
        )
        .bind(&registration.connection_id)
        .bind(&registration.session_code)
        .bind(&registration.player_id)
        .bind(&registration.replica_id)
        .execute(&mut **tx)
        .await?;

        Ok(RealtimeConnectionClaim { replaced })
    }
}

impl SessionStore for PostgresSessionStore {
    fn init(&self) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        Box::pin(async move {
            MIGRATOR.run(&self.pool).await?;
            self.seed_default_characters().await?;
            Ok(())
        })
    }

    fn health_check(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        Box::pin(async move {
            sqlx::query("SELECT 1").fetch_one(&self.pool).await?;
            Ok(true)
        })
    }

    fn load_session_by_code(
        &self,
        session_code: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<WorkshopSession>, PersistenceError>> + Send + '_>>
    {
        let session_code = session_code.to_string();
        Box::pin(async move {
            let row = sqlx::query("SELECT payload FROM workshop_sessions WHERE session_code = $1")
                .bind(&session_code)
                .fetch_optional(&self.pool)
                .await?;
            let Some(row) = row else {
                return Ok(None);
            };
            use sqlx::Row;
            let payload: sqlx::types::Json<serde_json::Value> = row.get("payload");
            let session = serde_json::from_value(payload.0)?;
            Ok(Some(sanitize_runtime_presence(session)))
        })
    }

    fn save_session(
        &self,
        session: &WorkshopSession,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let session = sanitize_runtime_presence(session.clone());
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            Self::save_session_in_tx(&mut tx, &session).await?;
            tx.commit().await?;
            Ok(())
        })
    }

    fn append_session_artifact(
        &self,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let artifact = artifact.clone();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            Self::append_session_artifact_in_tx(&mut tx, &artifact).await?;
            tx.commit().await?;
            Ok(())
        })
    }

    fn list_session_artifacts(
        &self,
        session_id: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Vec<SessionArtifactRecord>, PersistenceError>> + Send + '_>,
    > {
        let session_id = session_id.to_string();
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT payload FROM session_artifacts WHERE session_id = $1 ORDER BY created_at ASC, id ASC",
            )
            .bind(&session_id)
            .fetch_all(&self.pool)
            .await?;
            rows.into_iter()
                .map(|row| {
                    use sqlx::Row;
                    let payload: sqlx::types::Json<serde_json::Value> = row.get("payload");
                    serde_json::from_value(payload.0).map_err(PersistenceError::from)
                })
                .collect()
        })
    }

    fn create_player_identity(
        &self,
        identity: &PlayerIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let identity = identity.clone();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            Self::create_player_identity_in_tx(&mut tx, &identity).await?;
            tx.commit().await?;
            Ok(())
        })
    }

    fn find_player_identity(
        &self,
        session_code: &str,
        reconnect_token: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<PlayerIdentityMatch>, PersistenceError>> + Send + '_>,
    > {
        let session_code = session_code.to_string();
        let reconnect_token = reconnect_token.to_string();
        Box::pin(async move {
            let row = sqlx::query(
                "
                SELECT identities.session_id, identities.player_id, identities.last_seen_at
                FROM player_identities identities
                INNER JOIN workshop_sessions sessions ON sessions.session_id = identities.session_id
                WHERE identities.reconnect_token = $1 AND sessions.session_code = $2
                ",
            )
            .bind(&reconnect_token)
            .bind(&session_code)
            .fetch_optional(&self.pool)
            .await?;
            use sqlx::Row;
            Ok(row.map(|row| PlayerIdentityMatch {
                session_id: row.get("session_id"),
                player_id: row.get("player_id"),
                last_seen_at: row.get("last_seen_at"),
            }))
        })
    }

    fn touch_player_identity(
        &self,
        reconnect_token: &str,
        last_seen_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let reconnect_token = reconnect_token.to_string();
        let last_seen_at = last_seen_at.to_string();
        Box::pin(async move {
            sqlx::query(
                "UPDATE player_identities SET last_seen_at = $2 WHERE reconnect_token = $1",
            )
            .bind(&reconnect_token)
            .bind(&last_seen_at)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
    }

    fn revoke_player_identity(
        &self,
        reconnect_token: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let reconnect_token = reconnect_token.to_string();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            Self::revoke_player_identity_in_tx(&mut tx, &reconnect_token).await?;
            tx.commit().await?;
            Ok(())
        })
    }

    fn save_session_with_artifact(
        &self,
        session: &WorkshopSession,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let session = session.clone();
        let artifact = artifact.clone();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            Self::save_session_in_tx(&mut tx, &session).await?;
            Self::append_session_artifact_in_tx(&mut tx, &artifact).await?;
            tx.commit().await?;
            Ok(())
        })
    }

    fn save_session_with_identity_and_artifact(
        &self,
        session: &WorkshopSession,
        identity: &PlayerIdentity,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let session = session.clone();
        let identity = identity.clone();
        let artifact = artifact.clone();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            Self::save_session_in_tx(&mut tx, &session).await?;
            Self::create_player_identity_in_tx(&mut tx, &identity).await?;
            Self::append_session_artifact_in_tx(&mut tx, &artifact).await?;
            tx.commit().await?;
            Ok(())
        })
    }

    fn replace_player_identity_and_save_session_with_artifact(
        &self,
        previous_reconnect_token: &str,
        next_identity: &PlayerIdentity,
        session: &WorkshopSession,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let previous_reconnect_token = previous_reconnect_token.to_string();
        let next_identity = next_identity.clone();
        let session = session.clone();
        let artifact = artifact.clone();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            Self::save_session_in_tx(&mut tx, &session).await?;
            Self::create_player_identity_in_tx(&mut tx, &next_identity).await?;
            Self::revoke_player_identity_in_tx(&mut tx, &previous_reconnect_token).await?;
            Self::append_session_artifact_in_tx(&mut tx, &artifact).await?;
            tx.commit().await?;
            Ok(())
        })
    }

    fn acquire_session_lease(
        &self,
        session_code: &str,
        lease_id: &str,
        expires_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        let session_code = session_code.to_string();
        let lease_id = lease_id.to_string();
        let expires_at = expires_at.to_string();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let acquired =
                Self::acquire_session_lease_in_tx(&mut tx, &session_code, &lease_id, &expires_at)
                    .await?;
            tx.commit().await?;
            Ok(acquired)
        })
    }

    fn release_session_lease(
        &self,
        session_code: &str,
        lease_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let session_code = session_code.to_string();
        let lease_id = lease_id.to_string();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            Self::release_session_lease_in_tx(&mut tx, &session_code, &lease_id).await?;
            tx.commit().await?;
            Ok(())
        })
    }

    fn renew_session_lease(
        &self,
        session_code: &str,
        lease_id: &str,
        expires_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        let session_code = session_code.to_string();
        let lease_id = lease_id.to_string();
        let expires_at = expires_at.to_string();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let renewed =
                Self::renew_session_lease_in_tx(&mut tx, &session_code, &lease_id, &expires_at)
                    .await?;
            tx.commit().await?;
            Ok(renewed)
        })
    }

    fn renew_realtime_connection(
        &self,
        connection_id: &str,
        replica_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        let connection_id = connection_id.to_string();
        let replica_id = replica_id.to_string();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let renewed =
                Self::renew_realtime_connection_in_tx(&mut tx, &connection_id, &replica_id).await?;
            tx.commit().await?;
            Ok(renewed)
        })
    }

    fn claim_realtime_connection(
        &self,
        registration: &RealtimeConnectionRegistration,
    ) -> Pin<Box<dyn Future<Output = Result<RealtimeConnectionClaim, PersistenceError>> + Send + '_>>
    {
        let registration = registration.clone();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let claim =
                Self::claim_realtime_connection_in_tx(&mut tx, &registration, false).await?;
            tx.commit().await?;
            Ok(claim)
        })
    }

    fn restore_realtime_connection(
        &self,
        registration: &RealtimeConnectionRegistration,
    ) -> Pin<
        Box<dyn Future<Output = Result<RealtimeConnectionRestore, PersistenceError>> + Send + '_>,
    > {
        let registration = registration.clone();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            Self::cleanup_realtime_runtime_state_in_tx(&mut tx).await?;
            Self::lock_realtime_session_player_in_tx(
                &mut tx,
                &registration.session_code,
                &registration.player_id,
            )
            .await?;

            let current_owner: Option<(String, String)> = sqlx::query_as(
                "
                    SELECT connection_id, replica_id
                    FROM realtime_connections
                    WHERE session_code = $1 AND player_id = $2
                ",
            )
            .bind(&registration.session_code)
            .bind(&registration.player_id)
            .fetch_optional(&mut *tx)
            .await?;

            let restored = match current_owner {
                Some((connection_id, replica_id))
                    if connection_id != registration.connection_id
                        || replica_id != registration.replica_id =>
                {
                    false
                }
                _ => {
                    Self::claim_realtime_connection_in_tx(&mut tx, &registration, true).await?;
                    true
                }
            };
            tx.commit().await?;
            Ok(RealtimeConnectionRestore {
                restored,
                replaced: None,
            })
        })
    }

    fn release_realtime_connection(
        &self,
        connection_id: &str,
        replica_id: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        let connection_id = connection_id.to_string();
        let replica_id = replica_id.to_string();
        Box::pin(async move {
            use sqlx::Row;

            let row = sqlx::query(
                "
                    DELETE FROM realtime_connections
                    WHERE connection_id = $1 AND replica_id = $2
                    RETURNING session_code, player_id, connection_id, replica_id
                ",
            )
            .bind(&connection_id)
            .bind(&replica_id)
            .fetch_optional(&self.pool)
            .await?;

            Ok(row.map(|row| RealtimeConnectionRegistration {
                session_code: row.get("session_code"),
                player_id: row.get("player_id"),
                connection_id: row.get("connection_id"),
                replica_id: row.get("replica_id"),
            }))
        })
    }

    fn take_retired_realtime_connection(
        &self,
        connection_id: &str,
        replica_id: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        let connection_id = connection_id.to_string();
        let replica_id = replica_id.to_string();
        Box::pin(async move {
            use sqlx::Row;

            let row = sqlx::query(
                "
                    DELETE FROM retired_realtime_connections
                    WHERE connection_id = $1 AND replica_id = $2
                    RETURNING connection_id, replica_id
                ",
            )
            .bind(&connection_id)
            .bind(&replica_id)
            .fetch_optional(&self.pool)
            .await?;

            Ok(row.map(|row| RealtimeConnectionRegistration {
                session_code: String::new(),
                player_id: String::new(),
                connection_id: row.get("connection_id"),
                replica_id: row.get("replica_id"),
            }))
        })
    }

    fn list_realtime_connections(
        &self,
        session_code: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        let session_code = session_code.to_string();
        Box::pin(async move {
            use sqlx::Row;

            let cutoff = active_realtime_connection_cutoff();

            sqlx::query("DELETE FROM realtime_connections WHERE updated_at <= $1::timestamptz")
                .bind(cutoff.to_rfc3339())
                .execute(&self.pool)
                .await?;

            let rows = sqlx::query(
                "
                    SELECT session_code, player_id, connection_id, replica_id
                    FROM realtime_connections
                    WHERE session_code = $1 AND updated_at > $2::timestamptz
                    ORDER BY connection_id ASC
                ",
            )
            .bind(&session_code)
            .bind(cutoff.to_rfc3339())
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|row| RealtimeConnectionRegistration {
                    session_code: row.get("session_code"),
                    player_id: row.get("player_id"),
                    connection_id: row.get("connection_id"),
                    replica_id: row.get("replica_id"),
                })
                .collect())
        })
    }

    fn publish_session_notification(
        &self,
        notification: &SessionUpdateNotification,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let notification = notification.clone();
        Box::pin(async move {
            for payload in notification.to_publish_payloads()? {
                sqlx::query("SELECT pg_notify('session_updates', $1)")
                    .bind(payload)
                    .execute(&self.pool)
                    .await?;
            }
            Ok(())
        })
    }

    fn load_app_sprite_defaults(
        &self,
        key: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<AppSpriteDefaults>, PersistenceError>> + Send + '_>,
    > {
        let key = key.to_string();
        Box::pin(async move {
            use sqlx::Row;

            let row = sqlx::query(
                "SELECT neutral_sprite, happy_sprite, angry_sprite, sleepy_sprite FROM app_sprite_defaults WHERE sprite_key = $1",
            )
            .bind(&key)
            .fetch_optional(&self.pool)
            .await?;

            Ok(row.map(|row| AppSpriteDefaults {
                key,
                sprites: SpriteSet {
                    neutral: row.get("neutral_sprite"),
                    happy: row.get("happy_sprite"),
                    angry: row.get("angry_sprite"),
                    sleepy: row.get("sleepy_sprite"),
                },
            }))
        })
    }

    fn load_character(
        &self,
        character_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<CharacterRecord>, PersistenceError>> + Send + '_>>
    {
        let character_id = character_id.to_string();
        Box::pin(async move {
            let row = sqlx::query(
                "
                    SELECT
                        character_id,
                        description,
                        neutral_sprite,
                        happy_sprite,
                        angry_sprite,
                        sleepy_sprite,
                        remaining_sprite_regenerations,
                        created_at,
                        updated_at,
                        owner_account_id
                    FROM characters
                    WHERE character_id = $1
                ",
            )
            .bind(&character_id)
            .fetch_optional(&self.pool)
            .await?;

            Ok(row.map(|row| Self::character_from_row(&row)))
        })
    }

    fn list_characters(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CharacterRecord>, PersistenceError>> + Send + '_>>
    {
        Box::pin(async move {
            let rows = sqlx::query(
                "
                    SELECT
                        character_id,
                        description,
                        neutral_sprite,
                        happy_sprite,
                        angry_sprite,
                        sleepy_sprite,
                        remaining_sprite_regenerations,
                        created_at,
                        updated_at,
                        owner_account_id
                    FROM characters
                    ORDER BY created_at ASC, character_id ASC
                ",
            )
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|row| Self::character_from_row(&row))
                .collect())
        })
    }

    fn save_character(
        &self,
        character: &CharacterRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let character = character.clone();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            Self::save_character_in_tx(&mut tx, &character).await?;
            tx.commit().await?;
            Ok(())
        })
    }

    /// Atomic per-owner cap enforcement. Serializes concurrent creates for
    /// the same owner by taking a `FOR UPDATE` row lock on the owner's
    /// `accounts` row, then counting the owner's existing characters inside
    /// that lock, inserting only if under `max`, and committing. Concurrent
    /// creates for the same owner block on the account row lock and are
    /// processed one at a time; different owners never contend.
    fn save_character_enforcing_cap(
        &self,
        character: &CharacterRecord,
        max: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let character = character.clone();
        Box::pin(async move {
            use sqlx::Row;
            let owner = match character.owner_account_id.clone() {
                Some(id) => id,
                None => {
                    // Defensive: cap enforcement only makes sense for owned
                    // characters. Fall back to the plain save path.
                    let mut tx = self.pool.begin().await?;
                    Self::save_character_in_tx(&mut tx, &character).await?;
                    tx.commit().await?;
                    return Ok(());
                }
            };
            let mut tx = self.pool.begin().await?;
            // Lock the owner's account row. Any other in-flight cap-enforcing
            // create for the same owner blocks here until we commit.
            sqlx::query("SELECT 1 FROM accounts WHERE account_id = $1 FOR UPDATE")
                .bind(&owner)
                .fetch_optional(&mut *tx)
                .await?;
            let row = sqlx::query(
                "SELECT COUNT(*)::BIGINT AS cnt FROM characters WHERE owner_account_id = $1",
            )
            .bind(&owner)
            .fetch_one(&mut *tx)
            .await?;
            let count: i64 = row.get("cnt");
            if count.max(0) as u32 >= max {
                // Rollback by dropping tx without commit.
                return Err(PersistenceError::CharacterLimitReached { max });
            }
            Self::save_character_in_tx(&mut tx, &character).await?;
            tx.commit().await?;
            Ok(())
        })
    }

    fn insert_account(
        &self,
        account: &AccountRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let account = account.clone();
        Box::pin(async move {
            sqlx::query(
                "
                    INSERT INTO accounts (
                        account_id,
                        hero,
                        name,
                        password_hash,
                        created_at,
                        updated_at,
                        last_login_at
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7)
                ",
            )
            .bind(&account.id)
            .bind(&account.hero)
            .bind(&account.name)
            .bind(&account.password_hash)
            .bind(&account.created_at)
            .bind(&account.updated_at)
            .bind(&account.last_login_at)
            .execute(&self.pool)
            .await
            .map_err(|error| match error {
                sqlx::Error::Database(db_error)
                    if db_error.constraint() == Some("accounts_name_lower_idx") =>
                {
                    PersistenceError::DuplicateAccountName
                }
                other => PersistenceError::Sqlx(other),
            })?;
            Ok(())
        })
    }

    fn find_account_by_name_lower(
        &self,
        name: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<AccountRecord>, PersistenceError>> + Send + '_>>
    {
        let name = name.to_string();
        Box::pin(async move {
            let row = sqlx::query(
                "
                    SELECT account_id, hero, name, password_hash,
                           created_at, updated_at, last_login_at
                    FROM accounts
                    WHERE LOWER(name) = LOWER($1)
                ",
            )
            .bind(&name)
            .fetch_optional(&self.pool)
            .await?;
            Ok(row.map(|row| Self::account_from_row(&row)))
        })
    }

    fn find_account_by_id(
        &self,
        account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<AccountRecord>, PersistenceError>> + Send + '_>>
    {
        let account_id = account_id.to_string();
        Box::pin(async move {
            let row = sqlx::query(
                "
                    SELECT account_id, hero, name, password_hash,
                           created_at, updated_at, last_login_at
                    FROM accounts
                    WHERE account_id = $1
                ",
            )
            .bind(&account_id)
            .fetch_optional(&self.pool)
            .await?;
            Ok(row.map(|row| Self::account_from_row(&row)))
        })
    }

    fn touch_last_login(
        &self,
        account_id: &str,
        now: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let account_id = account_id.to_string();
        let now = now.to_string();
        Box::pin(async move {
            sqlx::query(
                "
                    UPDATE accounts
                    SET last_login_at = $2, updated_at = $2
                    WHERE account_id = $1
                ",
            )
            .bind(&account_id)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
    }

    fn list_characters_by_owner(
        &self,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CharacterRecord>, PersistenceError>> + Send + '_>>
    {
        let owner = owner_account_id.to_string();
        Box::pin(async move {
            let rows = sqlx::query(
                "
                    SELECT
                        character_id,
                        description,
                        neutral_sprite,
                        happy_sprite,
                        angry_sprite,
                        sleepy_sprite,
                        remaining_sprite_regenerations,
                        created_at,
                        updated_at,
                        owner_account_id
                    FROM characters
                    WHERE owner_account_id = $1
                    ORDER BY created_at ASC, character_id ASC
                ",
            )
            .bind(&owner)
            .fetch_all(&self.pool)
            .await?;
            Ok(rows
                .into_iter()
                .map(|row| Self::character_from_row(&row))
                .collect())
        })
    }

    fn count_characters_by_owner(
        &self,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<u32, PersistenceError>> + Send + '_>> {
        let owner = owner_account_id.to_string();
        Box::pin(async move {
            use sqlx::Row;
            let row = sqlx::query(
                "SELECT COUNT(*)::BIGINT AS cnt FROM characters WHERE owner_account_id = $1",
            )
            .bind(&owner)
            .fetch_one(&self.pool)
            .await?;
            let count: i64 = row.get("cnt");
            Ok(count.max(0) as u32)
        })
    }

    fn delete_character_by_owner(
        &self,
        character_id: &str,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        let character_id = character_id.to_string();
        let owner = owner_account_id.to_string();
        Box::pin(async move {
            let result = sqlx::query(
                "DELETE FROM characters WHERE character_id = $1 AND owner_account_id = $2",
            )
            .bind(&character_id)
            .bind(&owner)
            .execute(&self.pool)
            .await?;
            Ok(result.rows_affected() > 0)
        })
    }

    fn delete_lobby_workshop_by_owner(
        &self,
        session_code: &str,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        let session_code = session_code.to_string();
        let owner = owner_account_id.to_string();
        Box::pin(async move {
            let mut tx = self.pool.begin().await?;
            let row = sqlx::query(
                "
                    SELECT session_id, payload
                    FROM workshop_sessions
                    WHERE session_code = $1
                    FOR UPDATE
                ",
            )
            .bind(&session_code)
            .fetch_optional(&mut *tx)
            .await?;

            let Some(row) = row else {
                tx.rollback().await?;
                return Ok(false);
            };

            use sqlx::Row;
            let session_id: String = row.get("session_id");
            let payload: sqlx::types::Json<serde_json::Value> = row.get("payload");
            let session: WorkshopSession = match serde_json::from_value(payload.0) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(%error, %session_code, "malformed workshop payload during delete");
                    tx.rollback().await?;
                    return Ok(false);
                }
            };

            let session_owner = session
                .owner_account_id()
                .or_else(|| session.reserved_host_account_id());
            let can_delete = session.phase == protocol::Phase::Lobby
                && session.players.is_empty()
                && session_owner == Some(owner.as_str());
            if !can_delete {
                tx.rollback().await?;
                return Ok(false);
            }

            sqlx::query("DELETE FROM player_identities WHERE session_id = $1")
                .bind(&session_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM session_artifacts WHERE session_id = $1")
                .bind(&session_id)
                .execute(&mut *tx)
                .await?;
            let result = sqlx::query("DELETE FROM workshop_sessions WHERE session_id = $1")
                .bind(&session_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(result.rows_affected() > 0)
        })
    }

    fn delete_realtime_connections_for_session(
        &self,
        session_code: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        let session_code = session_code.to_string();
        Box::pin(async move {
            use sqlx::Row;

            let mut tx = self.pool.begin().await?;
            let realtime_connections = sqlx::query(
                "
                    DELETE FROM realtime_connections
                    WHERE session_code = $1
                    RETURNING session_code, player_id, connection_id, replica_id
                ",
            )
            .bind(&session_code)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|row| RealtimeConnectionRegistration {
                session_code: row.get("session_code"),
                player_id: row.get("player_id"),
                connection_id: row.get("connection_id"),
                replica_id: row.get("replica_id"),
            })
            .collect::<Vec<_>>();

            for registration in &realtime_connections {
                sqlx::query(
                    "
                        INSERT INTO retired_realtime_connections (connection_id, replica_id, retired_at)
                        VALUES ($1, $2, NOW())
                        ON CONFLICT (connection_id) DO UPDATE SET
                            replica_id = EXCLUDED.replica_id,
                            retired_at = EXCLUDED.retired_at
                    ",
                )
                .bind(&registration.connection_id)
                .bind(&registration.replica_id)
                .execute(&mut *tx)
                .await?;
            }

            tx.commit().await?;
            Ok(realtime_connections)
        })
    }

    fn list_open_workshops(
        &self,
        paging: OpenWorkshopsPaging,
    ) -> Pin<Box<dyn Future<Output = Result<OpenWorkshopsPage, PersistenceError>> + Send + '_>>
    {
        Box::pin(async move {
            // Ordering: DESC by `payload->>'created_at'` with `session_code`
            // ASC as tie-breaker. `created_at` is RFC3339 UTC (all writers go
            // through `to_rfc3339`), so TEXT lex-compare gives the correct
            // chronological order without a schema migration. `player_count`
            // and `host_name` are still derived from the JSON payload because
            // the lobby set is expected to be small; this query is cold
            // relative to the command-handler hot paths.
            //
            // Keyset predicate uses Postgres's native tuple `<` / `>`
            // operators on `(payload->>'created_at', session_code)`. We fetch
            // `PAGE_SIZE + 1` rows to detect whether another page exists on
            // the requested side without a second query.
            use sqlx::Row;
            let fetch_limit = (OPEN_WORKSHOPS_PAGE_SIZE + 1) as i64;

            // Outcomes depend on direction; keep the SQL as three small
            // parameterized variants rather than stitching strings with
            // bound placeholders at runtime.
            let (rows, is_before) = match &paging {
                OpenWorkshopsPaging::First => {
                    let rows = sqlx::query(
                        "
                            SELECT payload
                            FROM workshop_sessions
                            WHERE payload->>'phase' = 'lobby'
                            ORDER BY payload->>'created_at' DESC, session_code ASC
                            LIMIT $1
                        ",
                    )
                    .bind(fetch_limit)
                    .fetch_all(&self.pool)
                    .await?;
                    (rows, false)
                }
                OpenWorkshopsPaging::After(cursor) => {
                    // Strictly older than the DESC/ASC sort key:
                    // `(created_at DESC, session_code ASC)`.
                    let rows = sqlx::query(
                        "
                            SELECT payload
                            FROM workshop_sessions
                            WHERE payload->>'phase' = 'lobby'
                              AND (
                                    payload->>'created_at' < $1
                                 OR (payload->>'created_at' = $1 AND session_code > $2)
                              )
                            ORDER BY payload->>'created_at' DESC, session_code ASC
                            LIMIT $3
                        ",
                    )
                    .bind(&cursor.created_at)
                    .bind(&cursor.session_code)
                    .bind(fetch_limit)
                    .fetch_all(&self.pool)
                    .await?;
                    (rows, false)
                }
                OpenWorkshopsPaging::Before(cursor) => {
                    // Strictly newer than the DESC/ASC sort key. We
                    // query in the opposite order to grab the rows closest
                    // to the cursor, then reverse so the final page is in
                    // the canonical DESC form.
                    let rows = sqlx::query(
                        "
                            SELECT payload
                            FROM workshop_sessions
                            WHERE payload->>'phase' = 'lobby'
                              AND (
                                    payload->>'created_at' > $1
                                 OR (payload->>'created_at' = $1 AND session_code < $2)
                              )
                            ORDER BY payload->>'created_at' ASC, session_code DESC
                            LIMIT $3
                        ",
                    )
                    .bind(&cursor.created_at)
                    .bind(&cursor.session_code)
                    .bind(fetch_limit)
                    .fetch_all(&self.pool)
                    .await?;
                    (rows, true)
                }
            };
            // Deserialize + project to OpenWorkshopRecord.
            let mut summaries: Vec<OpenWorkshopRecord> = Vec::with_capacity(rows.len());
            for row in rows {
                let payload: sqlx::types::Json<serde_json::Value> = row.get("payload");
                let session: WorkshopSession = match serde_json::from_value(payload.0) {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(%error, "skipping malformed workshop_sessions.payload");
                        continue;
                    }
                };
                if let Some(summary) = open_workshop_summary_from_session(&session) {
                    summaries.push(summary);
                }
            }
            // For `Before`, we asked the DB in ASC order; restore DESC.
            if is_before {
                summaries.reverse();
            }
            // Compute has_more_* based on whether we fetched the +1 sentinel,
            // then truncate back to the page size. For Before we truncate
            // from the front (drop the oldest extra) to keep the page
            // adjacent to the cursor.
            let fetched_extra = summaries.len() > OPEN_WORKSHOPS_PAGE_SIZE;
            match &paging {
                OpenWorkshopsPaging::First => {
                    if fetched_extra {
                        summaries.truncate(OPEN_WORKSHOPS_PAGE_SIZE);
                    }
                    Ok(OpenWorkshopsPage {
                        rows: summaries,
                        has_more_after: fetched_extra,
                        has_more_before: false,
                    })
                }
                OpenWorkshopsPaging::After(_) => {
                    if fetched_extra {
                        summaries.truncate(OPEN_WORKSHOPS_PAGE_SIZE);
                    }
                    Ok(OpenWorkshopsPage {
                        rows: summaries,
                        has_more_after: fetched_extra,
                        has_more_before: true,
                    })
                }
                OpenWorkshopsPaging::Before(_) => {
                    if fetched_extra {
                        // After the reverse above, rows sit in DESC order:
                        // index 0 is the NEWEST (farthest from the cursor)
                        // and the last index is the row flush against the
                        // cursor. To keep the page-size rows adjacent to the cursor
                        // — matching the in-memory `skip(total - take)`
                        // semantics — we drop the extra from the FRONT, not
                        // the back. A `truncate(PAGE_SIZE)` here would
                        // silently lose the row immediately newer than the
                        // cursor and it would be unreachable via any
                        // subsequent Prev (its own cursor sits one tier
                        // further up).
                        let drop = summaries.len() - OPEN_WORKSHOPS_PAGE_SIZE;
                        summaries.drain(0..drop);
                    }
                    Ok(OpenWorkshopsPage {
                        rows: summaries,
                        has_more_after: true,
                        has_more_before: fetched_extra,
                    })
                }
            }
        })
    }
}

impl SessionStore for InMemorySessionStore {
    fn init(&self) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        Box::pin(async move { Ok(()) })
    }

    fn health_check(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        Box::pin(async move { Ok(true) })
    }

    fn load_session_by_code(
        &self,
        session_code: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<WorkshopSession>, PersistenceError>> + Send + '_>>
    {
        let session_code = session_code.to_string();
        Box::pin(async move {
            let guard = self
                .sessions_by_code
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            Ok(guard
                .get(&session_code)
                .cloned()
                .map(sanitize_runtime_presence))
        })
    }

    fn save_session(
        &self,
        session: &WorkshopSession,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let session = sanitize_runtime_presence(session.clone());
        Box::pin(async move {
            let candidate_payload = serde_json::to_string(&session)?;

            {
                let sessions_by_code = self
                    .sessions_by_code
                    .read()
                    .map_err(|_| PersistenceError::LockPoisoned)?;
                if sessions_by_code
                    .get(&session.code.0)
                    .is_some_and(|existing| {
                        existing.updated_at > session.updated_at
                            || (existing.updated_at == session.updated_at
                                && serde_json::to_string(existing).is_ok_and(|existing_payload| {
                                    existing_payload >= candidate_payload
                                }))
                    })
                {
                    return Err(PersistenceError::StaleSessionWrite {
                        session_id: session.id.to_string(),
                        session_code: session.code.0.clone(),
                        attempted_updated_at: session.updated_at.to_rfc3339(),
                    });
                }
            }

            {
                let sessions_by_id = self
                    .sessions_by_id
                    .read()
                    .map_err(|_| PersistenceError::LockPoisoned)?;
                if sessions_by_id
                    .get(&session.id.to_string())
                    .is_some_and(|existing| {
                        existing.updated_at > session.updated_at
                            || (existing.updated_at == session.updated_at
                                && serde_json::to_string(existing).is_ok_and(|existing_payload| {
                                    existing_payload >= candidate_payload
                                }))
                    })
                {
                    return Err(PersistenceError::StaleSessionWrite {
                        session_id: session.id.to_string(),
                        session_code: session.code.0.clone(),
                        attempted_updated_at: session.updated_at.to_rfc3339(),
                    });
                }
            }

            {
                let mut sessions_by_code = self
                    .sessions_by_code
                    .write()
                    .map_err(|_| PersistenceError::LockPoisoned)?;
                if let Some(previous) =
                    sessions_by_code.insert(session.code.0.clone(), session.clone())
                {
                    let mut sessions_by_id = self
                        .sessions_by_id
                        .write()
                        .map_err(|_| PersistenceError::LockPoisoned)?;
                    sessions_by_id.remove(&previous.id.to_string());
                    sessions_by_id.insert(session.id.to_string(), session.clone());
                    return Ok(());
                }
            }

            let mut sessions_by_id = self
                .sessions_by_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            sessions_by_id.insert(session.id.to_string(), session.clone());
            Ok(())
        })
    }

    fn append_session_artifact(
        &self,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let artifact = artifact.clone();
        Box::pin(async move {
            let mut guard = self
                .artifacts_by_session_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            if guard
                .values()
                .flatten()
                .any(|existing| existing.id == artifact.id)
            {
                return Err(PersistenceError::DuplicateArtifactId {
                    artifact_id: artifact.id,
                });
            }
            let artifacts = guard.entry(artifact.session_id.clone()).or_default();
            artifacts.push(artifact);
            Ok(())
        })
    }

    fn list_session_artifacts(
        &self,
        session_id: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Vec<SessionArtifactRecord>, PersistenceError>> + Send + '_>,
    > {
        let session_id = session_id.to_string();
        Box::pin(async move {
            let guard = self
                .artifacts_by_session_id
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            Ok(guard.get(&session_id).cloned().unwrap_or_default())
        })
    }

    fn create_player_identity(
        &self,
        identity: &PlayerIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let identity = identity.clone();
        Box::pin(async move {
            let mut guard = self
                .identities_by_token
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            guard.insert(identity.reconnect_token.clone(), identity);
            Ok(())
        })
    }

    fn find_player_identity(
        &self,
        session_code: &str,
        reconnect_token: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<PlayerIdentityMatch>, PersistenceError>> + Send + '_>,
    > {
        let session_code = session_code.to_string();
        let reconnect_token = reconnect_token.to_string();
        Box::pin(async move {
            let identity = {
                let identities = self
                    .identities_by_token
                    .read()
                    .map_err(|_| PersistenceError::LockPoisoned)?;
                identities.get(&reconnect_token).cloned()
            };

            let Some(identity) = identity else {
                return Ok(None);
            };

            let sessions = self
                .sessions_by_id
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let Some(session) = sessions.get(&identity.session_id) else {
                return Ok(None);
            };
            if session.code.0 != session_code {
                return Ok(None);
            }

            Ok(Some(PlayerIdentityMatch {
                session_id: identity.session_id,
                player_id: identity.player_id,
                last_seen_at: identity.last_seen_at,
            }))
        })
    }

    fn touch_player_identity(
        &self,
        reconnect_token: &str,
        last_seen_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let reconnect_token = reconnect_token.to_string();
        let last_seen_at = last_seen_at.to_string();
        Box::pin(async move {
            let mut guard = self
                .identities_by_token
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            if let Some(identity) = guard.get_mut(&reconnect_token) {
                identity.last_seen_at = last_seen_at;
            }
            Ok(())
        })
    }

    fn revoke_player_identity(
        &self,
        reconnect_token: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let reconnect_token = reconnect_token.to_string();
        Box::pin(async move {
            let mut guard = self
                .identities_by_token
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            guard.remove(&reconnect_token);
            Ok(())
        })
    }

    fn save_session_with_artifact(
        &self,
        session: &WorkshopSession,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let session = session.clone();
        let artifact = artifact.clone();
        Box::pin(async move {
            let previous_session = self.load_session_by_code(&session.code.0).await?;
            self.save_session(&session).await?;
            if let Err(error) = self.append_session_artifact(&artifact).await {
                rollback_in_memory_session(self, &session, previous_session).await?;
                return Err(error);
            }
            Ok(())
        })
    }

    fn save_session_with_identity_and_artifact(
        &self,
        session: &WorkshopSession,
        identity: &PlayerIdentity,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let session = session.clone();
        let identity = identity.clone();
        let artifact = artifact.clone();
        Box::pin(async move {
            let previous_session = self.load_session_by_code(&session.code.0).await?;
            let previous_identity = self
                .find_player_identity(&session.code.0, &identity.reconnect_token)
                .await?;
            self.save_session(&session).await?;
            self.create_player_identity(&identity).await?;
            if let Err(error) = self.append_session_artifact(&artifact).await {
                rollback_in_memory_identity(
                    self,
                    &identity.reconnect_token,
                    previous_identity,
                    &session,
                )
                .await?;
                rollback_in_memory_session(self, &session, previous_session).await?;
                return Err(error);
            }
            Ok(())
        })
    }

    fn replace_player_identity_and_save_session_with_artifact(
        &self,
        previous_reconnect_token: &str,
        next_identity: &PlayerIdentity,
        session: &WorkshopSession,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let previous_reconnect_token = previous_reconnect_token.to_string();
        let next_identity = next_identity.clone();
        let session = session.clone();
        let artifact = artifact.clone();
        Box::pin(async move {
            let previous_session = self.load_session_by_code(&session.code.0).await?;
            let revoked_identity = {
                let identities = self
                    .identities_by_token
                    .read()
                    .map_err(|_| PersistenceError::LockPoisoned)?;
                identities.get(&previous_reconnect_token).cloned()
            };
            self.save_session(&session).await?;
            self.create_player_identity(&next_identity).await?;
            self.revoke_player_identity(&previous_reconnect_token)
                .await?;
            if let Err(error) = self.append_session_artifact(&artifact).await {
                rollback_in_memory_replace_identity(
                    self,
                    &previous_reconnect_token,
                    revoked_identity,
                    &next_identity,
                )
                .await?;
                rollback_in_memory_session(self, &session, previous_session).await?;
                return Err(error);
            }
            Ok(())
        })
    }

    fn acquire_session_lease(
        &self,
        session_code: &str,
        lease_id: &str,
        expires_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        let session_code = session_code.to_string();
        let lease_id = lease_id.to_string();
        let expires_at = expires_at.to_string();
        Box::pin(async move {
            let mut leases = self
                .session_leases
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let acquired = match leases.get(&session_code) {
                Some((existing_lease_id, existing_expires_at)) => {
                    existing_lease_id == &lease_id
                        || parse_lease_deadline(existing_expires_at)
                            .is_some_and(|existing_expires_at| existing_expires_at <= Utc::now())
                }
                None => true,
            };
            if acquired {
                leases.insert(session_code, (lease_id, expires_at));
            }
            Ok(acquired)
        })
    }

    fn release_session_lease(
        &self,
        session_code: &str,
        lease_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let session_code = session_code.to_string();
        let lease_id = lease_id.to_string();
        Box::pin(async move {
            let mut leases = self
                .session_leases
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            if leases
                .get(&session_code)
                .is_some_and(|(existing_lease_id, _)| existing_lease_id == &lease_id)
            {
                leases.remove(&session_code);
            }
            Ok(())
        })
    }

    fn renew_session_lease(
        &self,
        session_code: &str,
        lease_id: &str,
        expires_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        let session_code = session_code.to_string();
        let lease_id = lease_id.to_string();
        let expires_at = expires_at.to_string();
        Box::pin(async move {
            let mut leases = self
                .session_leases
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let Some((existing_lease_id, existing_expires_at)) = leases.get_mut(&session_code)
            else {
                return Ok(false);
            };
            if existing_lease_id != &lease_id {
                return Ok(false);
            }
            *existing_expires_at = expires_at;
            Ok(true)
        })
    }

    fn renew_realtime_connection(
        &self,
        connection_id: &str,
        replica_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        let connection_id = connection_id.to_string();
        let replica_id = replica_id.to_string();
        Box::pin(async move {
            let mut connections_by_id = self
                .realtime_connections_by_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let cutoff = active_realtime_connection_cutoff();
            let Some((registration, updated_at)) = connections_by_id.get_mut(&connection_id) else {
                return Ok(false);
            };
            if registration.replica_id != replica_id || *updated_at <= cutoff {
                return Ok(false);
            }
            *updated_at = Utc::now();
            Ok(true)
        })
    }

    fn claim_realtime_connection(
        &self,
        registration: &RealtimeConnectionRegistration,
    ) -> Pin<Box<dyn Future<Output = Result<RealtimeConnectionClaim, PersistenceError>> + Send + '_>>
    {
        let registration = registration.clone();
        Box::pin(async move {
            let mut connections_by_id = self
                .realtime_connections_by_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let mut by_session_player = self
                .realtime_connection_by_session_player
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let mut retired_connections = self
                .retired_realtime_connections
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let cutoff = active_realtime_connection_cutoff();
            let stale_connection_ids = connections_by_id
                .iter()
                .filter(|(_, (_, updated_at))| *updated_at <= cutoff)
                .map(|(connection_id, _)| connection_id.clone())
                .collect::<Vec<_>>();
            for stale_connection_id in stale_connection_ids {
                if let Some((stale_registration, _)) =
                    connections_by_id.remove(&stale_connection_id)
                {
                    by_session_player.remove(&(
                        stale_registration.session_code,
                        stale_registration.player_id,
                    ));
                }
            }

            if retired_connections.contains_key(&registration.connection_id) {
                return Err(PersistenceError::RetiredRealtimeConnection {
                    connection_id: registration.connection_id.clone(),
                });
            }

            let key = (
                registration.session_code.clone(),
                registration.player_id.clone(),
            );
            if let Some((existing_registration, _)) =
                connections_by_id.get(&registration.connection_id)
                && (existing_registration.session_code != key.0
                    || existing_registration.player_id != key.1)
            {
                by_session_player.remove(&(
                    existing_registration.session_code.clone(),
                    existing_registration.player_id.clone(),
                ));
            }
            let replaced = by_session_player
                .insert(key, registration.connection_id.clone())
                .and_then(|previous_connection_id| {
                    if previous_connection_id == registration.connection_id {
                        None
                    } else {
                        connections_by_id
                            .remove(&previous_connection_id)
                            .map(|(registration, _)| registration)
                    }
                });

            if let Some(replaced) = replaced.as_ref() {
                retired_connections
                    .insert(replaced.connection_id.clone(), replaced.replica_id.clone());
            }

            connections_by_id.insert(
                registration.connection_id.clone(),
                (registration, Utc::now()),
            );
            Ok(RealtimeConnectionClaim { replaced })
        })
    }

    fn restore_realtime_connection(
        &self,
        registration: &RealtimeConnectionRegistration,
    ) -> Pin<
        Box<dyn Future<Output = Result<RealtimeConnectionRestore, PersistenceError>> + Send + '_>,
    > {
        let registration = registration.clone();
        Box::pin(async move {
            let mut connections_by_id = self
                .realtime_connections_by_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let mut by_session_player = self
                .realtime_connection_by_session_player
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let mut retired_connections = self
                .retired_realtime_connections
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let cutoff = active_realtime_connection_cutoff();

            let stale_connection_ids = connections_by_id
                .iter()
                .filter(|(_, (_, updated_at))| *updated_at <= cutoff)
                .map(|(connection_id, _)| connection_id.clone())
                .collect::<Vec<_>>();
            for stale_connection_id in stale_connection_ids {
                if let Some((stale_registration, _)) =
                    connections_by_id.remove(&stale_connection_id)
                {
                    by_session_player.remove(&(
                        stale_registration.session_code,
                        stale_registration.player_id,
                    ));
                }
            }

            let key = (
                registration.session_code.clone(),
                registration.player_id.clone(),
            );
            if let Some(current_connection_id) = by_session_player.get(&key)
                && current_connection_id != &registration.connection_id
            {
                return Ok(RealtimeConnectionRestore {
                    restored: false,
                    replaced: None,
                });
            }

            if retired_connections.get(&registration.connection_id)
                != Some(&registration.replica_id)
            {
                return Ok(RealtimeConnectionRestore {
                    restored: false,
                    replaced: None,
                });
            }
            retired_connections.remove(&registration.connection_id);

            if let Some((existing_registration, _)) =
                connections_by_id.get(&registration.connection_id)
                && (existing_registration.session_code != key.0
                    || existing_registration.player_id != key.1)
            {
                by_session_player.remove(&(
                    existing_registration.session_code.clone(),
                    existing_registration.player_id.clone(),
                ));
            }
            let replaced = by_session_player
                .insert(key, registration.connection_id.clone())
                .and_then(|previous_connection_id| {
                    if previous_connection_id == registration.connection_id {
                        None
                    } else {
                        connections_by_id
                            .remove(&previous_connection_id)
                            .map(|(registration, _)| registration)
                    }
                });

            if let Some(replaced) = replaced.as_ref() {
                retired_connections
                    .insert(replaced.connection_id.clone(), replaced.replica_id.clone());
            }

            connections_by_id.insert(
                registration.connection_id.clone(),
                (registration, Utc::now()),
            );
            Ok(RealtimeConnectionRestore {
                restored: true,
                replaced,
            })
        })
    }

    fn release_realtime_connection(
        &self,
        connection_id: &str,
        replica_id: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        let connection_id = connection_id.to_string();
        let replica_id = replica_id.to_string();
        Box::pin(async move {
            let mut connections_by_id = self
                .realtime_connections_by_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let mut by_session_player = self
                .realtime_connection_by_session_player
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;

            let removed = connections_by_id
                .remove(&connection_id)
                .map(|(registration, _)| registration)
                .filter(|registration| registration.replica_id == replica_id);
            if let Some(registration) = &removed {
                by_session_player.remove(&(
                    registration.session_code.clone(),
                    registration.player_id.clone(),
                ));
            }
            Ok(removed)
        })
    }

    fn take_retired_realtime_connection(
        &self,
        connection_id: &str,
        replica_id: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        let connection_id = connection_id.to_string();
        let replica_id = replica_id.to_string();
        Box::pin(async move {
            let mut retired_connections = self
                .retired_realtime_connections
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;

            if retired_connections.get(&connection_id) == Some(&replica_id) {
                retired_connections.remove(&connection_id);
                return Ok(Some(RealtimeConnectionRegistration {
                    session_code: String::new(),
                    player_id: String::new(),
                    connection_id,
                    replica_id,
                }));
            }
            Ok(None)
        })
    }

    fn list_realtime_connections(
        &self,
        session_code: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        let session_code = session_code.to_string();
        Box::pin(async move {
            let cutoff = active_realtime_connection_cutoff();
            let connections = self
                .realtime_connections_by_id
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            Ok(connections
                .values()
                .filter(|(registration, updated_at)| {
                    registration.session_code == session_code && *updated_at > cutoff
                })
                .map(|(registration, _)| registration.clone())
                .collect())
        })
    }

    fn publish_session_notification(
        &self,
        _notification: &SessionUpdateNotification,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        Box::pin(async move { Ok(()) })
    }

    fn load_app_sprite_defaults(
        &self,
        key: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<AppSpriteDefaults>, PersistenceError>> + Send + '_>,
    > {
        let key = key.to_string();
        Box::pin(async move {
            let defaults = self
                .app_sprite_defaults_by_key
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?
                .get(&key)
                .cloned();
            Ok(defaults)
        })
    }

    fn load_character(
        &self,
        character_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<CharacterRecord>, PersistenceError>> + Send + '_>>
    {
        let character_id = character_id.to_string();
        Box::pin(async move {
            let character = self
                .characters_by_id
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?
                .get(&character_id)
                .cloned();
            Ok(character)
        })
    }

    fn list_characters(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CharacterRecord>, PersistenceError>> + Send + '_>>
    {
        Box::pin(async move {
            let mut characters = self
                .characters_by_id
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?
                .values()
                .cloned()
                .collect::<Vec<_>>();
            characters.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.id.cmp(&right.id))
            });
            Ok(characters)
        })
    }

    fn save_character(
        &self,
        character: &CharacterRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let character = character.clone();
        Box::pin(async move {
            self.characters_by_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?
                .insert(character.id.clone(), character);
            Ok(())
        })
    }

    fn save_character_enforcing_cap(
        &self,
        character: &CharacterRecord,
        max: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let character = character.clone();
        Box::pin(async move {
            // Hold the write lock across count + insert so concurrent creates
            // for the same owner serialize here, mirroring the Postgres
            // `FOR UPDATE` path.
            let mut characters = self
                .characters_by_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            if let Some(owner_id) = character.owner_account_id.as_deref() {
                let count = characters
                    .values()
                    .filter(|c| c.owner_account_id.as_deref() == Some(owner_id))
                    .count() as u32;
                if count >= max {
                    return Err(PersistenceError::CharacterLimitReached { max });
                }
            }
            characters.insert(character.id.clone(), character);
            Ok(())
        })
    }

    fn insert_account(
        &self,
        account: &AccountRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let account = account.clone();
        Box::pin(async move {
            let mut accounts = self
                .accounts_by_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let lower_name = account.name.to_lowercase();
            if accounts
                .values()
                .any(|existing| existing.name.to_lowercase() == lower_name)
            {
                return Err(PersistenceError::DuplicateAccountName);
            }
            accounts.insert(account.id.clone(), account);
            Ok(())
        })
    }

    fn find_account_by_name_lower(
        &self,
        name: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<AccountRecord>, PersistenceError>> + Send + '_>>
    {
        let name_lower = name.to_lowercase();
        Box::pin(async move {
            let accounts = self
                .accounts_by_id
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            Ok(accounts
                .values()
                .find(|account| account.name.to_lowercase() == name_lower)
                .cloned())
        })
    }

    fn find_account_by_id(
        &self,
        account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<AccountRecord>, PersistenceError>> + Send + '_>>
    {
        let account_id = account_id.to_string();
        Box::pin(async move {
            Ok(self
                .accounts_by_id
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?
                .get(&account_id)
                .cloned())
        })
    }

    fn touch_last_login(
        &self,
        account_id: &str,
        now: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        let account_id = account_id.to_string();
        let now = now.to_string();
        Box::pin(async move {
            if let Some(account) = self
                .accounts_by_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?
                .get_mut(&account_id)
            {
                account.last_login_at = Some(now.clone());
                account.updated_at = now;
            }
            Ok(())
        })
    }

    fn list_characters_by_owner(
        &self,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CharacterRecord>, PersistenceError>> + Send + '_>>
    {
        let owner = owner_account_id.to_string();
        Box::pin(async move {
            let mut characters = self
                .characters_by_id
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?
                .values()
                .filter(|record| record.owner_account_id.as_deref() == Some(owner.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            characters.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.id.cmp(&right.id))
            });
            Ok(characters)
        })
    }

    fn count_characters_by_owner(
        &self,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<u32, PersistenceError>> + Send + '_>> {
        let owner = owner_account_id.to_string();
        Box::pin(async move {
            let count = self
                .characters_by_id
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?
                .values()
                .filter(|record| record.owner_account_id.as_deref() == Some(owner.as_str()))
                .count();
            Ok(count as u32)
        })
    }

    fn delete_character_by_owner(
        &self,
        character_id: &str,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        let character_id = character_id.to_string();
        let owner = owner_account_id.to_string();
        Box::pin(async move {
            let mut characters = self
                .characters_by_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            match characters.get(&character_id) {
                Some(record) if record.owner_account_id.as_deref() == Some(owner.as_str()) => {
                    characters.remove(&character_id);
                    Ok(true)
                }
                _ => Ok(false),
            }
        })
    }

    fn delete_lobby_workshop_by_owner(
        &self,
        session_code: &str,
        owner_account_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        let session_code = session_code.to_string();
        let owner = owner_account_id.to_string();
        Box::pin(async move {
            let mut sessions_by_code = self
                .sessions_by_code
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let Some(session) = sessions_by_code.get(&session_code) else {
                return Ok(false);
            };
            let session_owner = session
                .owner_account_id()
                .or_else(|| session.reserved_host_account_id());
            let can_delete = session.phase == protocol::Phase::Lobby
                && session.players.is_empty()
                && session_owner == Some(owner.as_str());
            if !can_delete {
                return Ok(false);
            }

            let session_id = session.id.to_string();
            sessions_by_code.remove(&session_code);
            drop(sessions_by_code);

            self.sessions_by_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?
                .remove(&session_id);
            self.artifacts_by_session_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?
                .remove(&session_id);
            self.identities_by_token
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?
                .retain(|_, identity| identity.session_id != session_id);
            Ok(true)
        })
    }

    fn delete_realtime_connections_for_session(
        &self,
        session_code: &str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<RealtimeConnectionRegistration>, PersistenceError>>
                + Send
                + '_,
        >,
    > {
        let session_code = session_code.to_string();
        Box::pin(async move {
            let mut connections_by_id = self
                .realtime_connections_by_id
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let mut by_session_player = self
                .realtime_connection_by_session_player
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            let mut retired_connections = self
                .retired_realtime_connections
                .write()
                .map_err(|_| PersistenceError::LockPoisoned)?;

            let connection_ids = connections_by_id
                .iter()
                .filter(|(_, (registration, _))| registration.session_code == session_code)
                .map(|(connection_id, _)| connection_id.clone())
                .collect::<Vec<_>>();

            let mut registrations = Vec::with_capacity(connection_ids.len());
            for connection_id in connection_ids {
                if let Some((registration, _)) = connections_by_id.remove(&connection_id) {
                    by_session_player.remove(&(
                        registration.session_code.clone(),
                        registration.player_id.clone(),
                    ));
                    retired_connections.insert(
                        registration.connection_id.clone(),
                        registration.replica_id.clone(),
                    );
                    registrations.push(registration);
                }
            }

            Ok(registrations)
        })
    }

    fn list_open_workshops(
        &self,
        paging: OpenWorkshopsPaging,
    ) -> Pin<Box<dyn Future<Output = Result<OpenWorkshopsPage, PersistenceError>> + Send + '_>>
    {
        Box::pin(async move {
            let guard = self
                .sessions_by_code
                .read()
                .map_err(|_| PersistenceError::LockPoisoned)?;
            // Collect candidate lobbies and sort DESC by `created_at` with
            // `session_code` ASC as tie-breaker. Matches the Postgres impl.
            let mut summaries: Vec<OpenWorkshopRecord> = guard
                .values()
                .filter(|session| session.phase == protocol::Phase::Lobby)
                .filter_map(open_workshop_summary_from_session)
                .collect();
            summaries.sort_by(|left, right| {
                right
                    .created_at
                    .cmp(&left.created_at)
                    .then_with(|| left.session_code.cmp(&right.session_code))
            });
            drop(guard);

            // Apply keyset filter in the sorted (DESC) list. For After we
            // keep rows strictly older than cursor; for Before we keep rows
            // strictly newer than cursor (which sit before the cursor in
            // the DESC list).
            let filtered: Vec<OpenWorkshopRecord> = match &paging {
                OpenWorkshopsPaging::First => summaries,
                OpenWorkshopsPaging::After(cursor) => summaries
                    .into_iter()
                    .filter(|row| {
                        is_older_open_workshop_cursor(
                            row.created_at.as_str(),
                            row.session_code.as_str(),
                            cursor,
                        )
                    })
                    .collect(),
                OpenWorkshopsPaging::Before(cursor) => summaries
                    .into_iter()
                    .filter(|row| {
                        is_newer_open_workshop_cursor(
                            row.created_at.as_str(),
                            row.session_code.as_str(),
                            cursor,
                        )
                    })
                    .collect(),
            };

            // For Before we need the page-size rows *immediately* newer than the
            // cursor. The filtered slice is still DESC, so those rows are the
            // tail. Symmetrically: fetch `PAGE_SIZE + 1` on the relevant
            // side, then truncate to `PAGE_SIZE`, and compute has_more_*.
            let page = match &paging {
                OpenWorkshopsPaging::First => {
                    let has_more_after = filtered.len() > OPEN_WORKSHOPS_PAGE_SIZE;
                    let mut rows = filtered;
                    rows.truncate(OPEN_WORKSHOPS_PAGE_SIZE);
                    OpenWorkshopsPage {
                        rows,
                        has_more_after,
                        has_more_before: false,
                    }
                }
                OpenWorkshopsPaging::After(_) => {
                    let has_more_after = filtered.len() > OPEN_WORKSHOPS_PAGE_SIZE;
                    let mut rows = filtered;
                    rows.truncate(OPEN_WORKSHOPS_PAGE_SIZE);
                    OpenWorkshopsPage {
                        rows,
                        has_more_after,
                        has_more_before: true,
                    }
                }
                OpenWorkshopsPaging::Before(_) => {
                    // Take the tail (rows closest to the cursor) of the
                    // DESC-sorted filtered list.
                    let total = filtered.len();
                    let take = OPEN_WORKSHOPS_PAGE_SIZE.min(total);
                    let has_more_before = total > OPEN_WORKSHOPS_PAGE_SIZE;
                    let start = total - take;
                    let rows: Vec<_> = filtered.into_iter().skip(start).collect();
                    OpenWorkshopsPage {
                        rows,
                        has_more_after: true,
                        has_more_before,
                    }
                }
            };
            Ok(page)
        })
    }
}

async fn rollback_in_memory_session(
    store: &InMemorySessionStore,
    session: &WorkshopSession,
    previous_session: Option<WorkshopSession>,
) -> Result<(), PersistenceError> {
    let mut sessions_by_code = store
        .sessions_by_code
        .write()
        .map_err(|_| PersistenceError::LockPoisoned)?;
    let mut sessions_by_id = store
        .sessions_by_id
        .write()
        .map_err(|_| PersistenceError::LockPoisoned)?;

    match previous_session {
        Some(previous_session) => {
            sessions_by_code.insert(previous_session.code.0.clone(), previous_session.clone());
            sessions_by_id.insert(previous_session.id.to_string(), previous_session);
        }
        None => {
            sessions_by_code.remove(&session.code.0);
            sessions_by_id.remove(&session.id.to_string());
        }
    }
    Ok(())
}

async fn rollback_in_memory_identity(
    store: &InMemorySessionStore,
    reconnect_token: &str,
    previous_identity: Option<PlayerIdentityMatch>,
    session: &WorkshopSession,
) -> Result<(), PersistenceError> {
    let mut identities = store
        .identities_by_token
        .write()
        .map_err(|_| PersistenceError::LockPoisoned)?;
    if let Some(previous_identity) = previous_identity {
        identities.insert(
            reconnect_token.to_string(),
            PlayerIdentity {
                session_id: previous_identity.session_id,
                player_id: previous_identity.player_id,
                reconnect_token: reconnect_token.to_string(),
                created_at: session.updated_at.to_rfc3339(),
                last_seen_at: previous_identity.last_seen_at,
            },
        );
    } else {
        identities.remove(reconnect_token);
    }
    Ok(())
}

async fn rollback_in_memory_replace_identity(
    store: &InMemorySessionStore,
    previous_reconnect_token: &str,
    revoked_identity: Option<PlayerIdentity>,
    next_identity: &PlayerIdentity,
) -> Result<(), PersistenceError> {
    let mut identities = store
        .identities_by_token
        .write()
        .map_err(|_| PersistenceError::LockPoisoned)?;
    identities.remove(&next_identity.reconnect_token);
    if let Some(revoked_identity) = revoked_identity {
        identities.insert(previous_reconnect_token.to_string(), revoked_identity);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use domain::{SessionCode, SessionPlayer, WorkshopSession};
    use protocol::{OpenWorkshopCursor, Phase, SessionArtifactKind};
    use serde_json::json;
    use uuid::Uuid;

    fn config() -> protocol::WorkshopCreateConfig {
        protocol::WorkshopCreateConfig {
            phase0_minutes: 5,
            phase1_minutes: 10,
            phase2_minutes: 10,
        }
    }

    fn ts(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).expect("valid timestamp")
    }

    fn session(code: &str, phase: Phase, updated_at_seconds: i64) -> WorkshopSession {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode(code.to_string()),
            ts(updated_at_seconds),
            config(),
        );
        session.phase = phase;
        session.updated_at = ts(updated_at_seconds);
        session
    }

    fn session_order_marker(session: &WorkshopSession) -> String {
        serde_json::to_string(session).expect("serialize session ordering marker")
    }

    fn artifact(session_id: &str, id: &str, created_at: &str) -> SessionArtifactRecord {
        SessionArtifactRecord {
            id: id.to_string(),
            session_id: session_id.to_string(),
            phase: Phase::Lobby,
            step: 0,
            kind: SessionArtifactKind::SessionCreated,
            player_id: None,
            created_at: created_at.to_string(),
            payload: json!({ "id": id }),
        }
    }

    fn identity(session_id: &str, player_id: &str, reconnect_token: &str) -> PlayerIdentity {
        PlayerIdentity {
            session_id: session_id.to_string(),
            player_id: player_id.to_string(),
            reconnect_token: reconnect_token.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_seen_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn connected_session(code: &str, updated_at_seconds: i64) -> WorkshopSession {
        let mut session = session(code, Phase::Lobby, updated_at_seconds);
        session.add_player(SessionPlayer {
            id: "player-1".to_string(),
            name: "Alice".to_string(),
            account_id: None,
            character_id: Some("character-1".to_string()),
            selected_character: Some(protocol::CharacterProfile {
                id: "character-1".to_string(),
                description: "Alice's workshop dragon".to_string(),
                sprites: SpriteSet {
                    neutral: "neutral".to_string(),
                    happy: "happy".to_string(),
                    angry: "angry".to_string(),
                    sleepy: "sleepy".to_string(),
                },
                remaining_sprite_regenerations: 1,
            }),
            is_host: true,
            is_connected: true,
            is_ready: true,
            score: 0,
            current_dragon_id: None,
            achievements: Vec::new(),
            joined_at: ts(updated_at_seconds),
        });
        session
    }

    #[tokio::test]
    async fn load_missing_session_returns_none() {
        let store = InMemorySessionStore::new();

        let session = store
            .load_session_by_code("missing")
            .await
            .expect("load missing session");

        assert_eq!(session, None);
    }

    #[tokio::test]
    async fn save_and_load_session_roundtrip() {
        let store = InMemorySessionStore::new();
        let saved = session("123456", Phase::Lobby, 1);

        store.save_session(&saved).await.expect("save session");
        let loaded = store
            .load_session_by_code("123456")
            .await
            .expect("load session")
            .expect("session exists");

        assert_eq!(loaded, saved);
    }

    #[tokio::test]
    async fn save_and_load_character_roundtrip() {
        let store = InMemorySessionStore::new();
        let character = CharacterRecord {
            id: "character-1".to_string(),
            description: "A violet crystal dragon".to_string(),
            sprites: SpriteSet {
                neutral: "neutral".to_string(),
                happy: "happy".to_string(),
                angry: "angry".to_string(),
                sleepy: "sleepy".to_string(),
            },
            remaining_sprite_regenerations: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            owner_account_id: None,
        };

        store
            .save_character(&character)
            .await
            .expect("save character");

        let loaded = store
            .load_character("character-1")
            .await
            .expect("load character")
            .expect("character exists");

        assert_eq!(loaded, character);
        assert_eq!(loaded.profile().id, "character-1");
    }

    #[tokio::test]
    async fn save_and_load_session_clears_durable_connectivity_flags() {
        let store = InMemorySessionStore::new();
        let saved = connected_session("123456", 1);

        store.save_session(&saved).await.expect("save session");
        let loaded = store
            .load_session_by_code("123456")
            .await
            .expect("load session")
            .expect("session exists");

        assert!(
            loaded.players.values().all(|player| !player.is_connected),
            "persisted sessions must not round-trip runtime connection presence"
        );
    }

    #[tokio::test]
    async fn save_session_overwrites_existing_session_by_code() {
        let store = InMemorySessionStore::new();
        let first = session("123456", Phase::Lobby, 1);
        let second = session("123456", Phase::Phase1, 2);

        store
            .save_session(&first)
            .await
            .expect("save first session");
        store
            .save_session(&second)
            .await
            .expect("save second session");
        let loaded = store
            .load_session_by_code("123456")
            .await
            .expect("load session")
            .expect("session exists");

        assert_eq!(loaded.phase, Phase::Phase1);
        assert_eq!(loaded.updated_at, ts(2));
    }

    #[tokio::test]
    async fn save_session_rejects_stale_write_by_updated_at() {
        let store = InMemorySessionStore::new();
        let mut current = session("123456", Phase::Lobby, 2);
        let mut stale = current.clone();
        current.phase = Phase::Phase1;

        store
            .save_session(&current)
            .await
            .expect("save current session");

        stale.phase = Phase::Handover;
        let error = store
            .save_session(&stale)
            .await
            .expect_err("stale session write should be rejected");

        assert!(matches!(error, PersistenceError::StaleSessionWrite { .. }));

        let loaded = store
            .load_session_by_code("123456")
            .await
            .expect("load session")
            .expect("session exists");
        assert_eq!(loaded.phase, Phase::Phase1);
        assert_eq!(loaded.updated_at, ts(2));
    }

    #[tokio::test]
    async fn save_session_uses_deterministic_tiebreaker_when_updated_at_matches() {
        let store = InMemorySessionStore::new();
        let mut lower_order = session("123456", Phase::Lobby, 2);
        lower_order.host_player_id = Some("host-a".to_string());
        let mut higher_order = lower_order.clone();
        higher_order.host_player_id = Some("host-z".to_string());

        assert!(session_order_marker(&lower_order) < session_order_marker(&higher_order));

        store
            .save_session(&lower_order)
            .await
            .expect("save lower-order session first");
        store
            .save_session(&higher_order)
            .await
            .expect("save higher-order session with matching updated_at");

        let loaded = store
            .load_session_by_code("123456")
            .await
            .expect("load session")
            .expect("session exists");

        assert_eq!(loaded.host_player_id, Some("host-z".to_string()));
        assert_eq!(loaded.updated_at, ts(2));
    }

    #[tokio::test]
    async fn grouped_session_write_rejects_stale_session_without_appending_artifact() {
        let store = InMemorySessionStore::new();
        let mut stale = session("123456", Phase::Lobby, 2);
        stale.host_player_id = Some("host-a".to_string());
        let mut current = stale.clone();
        current.host_player_id = Some("host-z".to_string());

        assert!(session_order_marker(&stale) < session_order_marker(&current));

        store
            .save_session(&current)
            .await
            .expect("save current session");

        let stale_artifact = artifact(&stale.id.to_string(), "a-stale", "2026-01-01T00:00:02Z");
        let error = store
            .save_session_with_artifact(&stale, &stale_artifact)
            .await
            .expect_err("stale grouped session write should be rejected");

        assert!(matches!(error, PersistenceError::StaleSessionWrite { .. }));

        let loaded = store
            .load_session_by_code("123456")
            .await
            .expect("load session")
            .expect("session exists");
        assert_eq!(loaded.host_player_id, Some("host-z".to_string()));

        let artifacts = store
            .list_session_artifacts(&stale.id.to_string())
            .await
            .expect("list artifacts");
        assert!(artifacts.is_empty());
    }

    #[tokio::test]
    async fn grouped_in_memory_write_keeps_identity_and_artifact_atomic() {
        let store = InMemorySessionStore::new();
        let existing_session = session("111111", Phase::Lobby, 1);
        let conflicting_artifact = artifact(
            &existing_session.id.to_string(),
            "a1",
            "2026-01-01T00:00:01Z",
        );
        let target_session = session("222222", Phase::Lobby, 2);
        let identity = identity(&target_session.id.to_string(), "player-1", "token-1");
        let artifact = artifact(&target_session.id.to_string(), "a1", "2026-01-01T00:00:02Z");

        store
            .save_session(&existing_session)
            .await
            .expect("seed existing session before grouped write");
        store
            .append_session_artifact(&conflicting_artifact)
            .await
            .expect("seed conflicting artifact id");

        let error = store
            .save_session_with_identity_and_artifact(&target_session, &identity, &artifact)
            .await
            .expect_err("conflicting grouped write should fail atomically");

        assert!(matches!(
            error,
            PersistenceError::DuplicateArtifactId { artifact_id } if artifact_id == "a1"
        ));

        let found_identity = store
            .find_player_identity("222222", "token-1")
            .await
            .expect("find grouped identity after failed write");
        assert!(
            found_identity.is_none(),
            "failed grouped write should not leave a persisted identity behind"
        );

        let loaded_session = store
            .load_session_by_code("222222")
            .await
            .expect("load grouped session after failed write");
        assert!(
            loaded_session.is_none(),
            "failed grouped write should not leave a persisted session behind"
        );

        let artifacts = store
            .list_session_artifacts(&target_session.id.to_string())
            .await
            .expect("list artifacts after failed grouped write");
        assert!(
            artifacts.is_empty(),
            "failed grouped write should not leave target-session artifacts behind"
        );
    }

    #[tokio::test]
    async fn health_check_is_true_for_memory_store() {
        let store = InMemorySessionStore::new();

        let health = store.health_check().await.expect("health check");

        assert!(health);
    }

    #[tokio::test]
    async fn appended_artifacts_are_listed_in_append_order() {
        let store = InMemorySessionStore::new();
        store
            .append_session_artifact(&artifact("session-1", "a1", "2026-01-01T00:00:00Z"))
            .await
            .expect("append first artifact");
        store
            .append_session_artifact(&artifact("session-1", "a2", "2026-01-01T00:00:01Z"))
            .await
            .expect("append second artifact");

        let artifacts = store
            .list_session_artifacts("session-1")
            .await
            .expect("list artifacts");

        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].id, "a1");
        assert_eq!(artifacts[1].id, "a2");
    }

    #[tokio::test]
    async fn find_player_identity_returns_none_for_missing_token() {
        let store = InMemorySessionStore::new();
        let session = session("123456", Phase::Lobby, 1);
        store.save_session(&session).await.expect("save session");

        let found = store
            .find_player_identity("123456", "missing-token")
            .await
            .expect("find identity");

        assert_eq!(found, None);
    }

    #[tokio::test]
    async fn revoke_player_identity_removes_existing_identity() {
        let store = InMemorySessionStore::new();
        let session = session("123456", Phase::Lobby, 1);
        store.save_session(&session).await.expect("save session");
        store
            .create_player_identity(&identity(&session.id.to_string(), "player-1", "token-1"))
            .await
            .expect("create identity");
        store
            .revoke_player_identity("token-1")
            .await
            .expect("revoke identity");

        let found = store
            .find_player_identity("123456", "token-1")
            .await
            .expect("find identity");

        assert_eq!(found, None);
    }

    #[tokio::test]
    async fn touch_player_identity_updates_last_seen_without_changing_match() {
        let store = InMemorySessionStore::new();
        let session = session("123456", Phase::Lobby, 1);
        store.save_session(&session).await.expect("save session");
        store
            .create_player_identity(&identity(&session.id.to_string(), "player-1", "token-1"))
            .await
            .expect("create identity");
        store
            .touch_player_identity("token-1", "2026-01-01T00:00:05Z")
            .await
            .expect("touch identity");

        let found = store
            .find_player_identity("123456", "token-1")
            .await
            .expect("find identity");

        assert_eq!(
            found,
            Some(PlayerIdentityMatch {
                session_id: session.id.to_string(),
                player_id: "player-1".to_string(),
                last_seen_at: "2026-01-01T00:00:05Z".to_string(),
            })
        );
    }

    #[test]
    fn session_update_notification_serializes_legacy_payload_for_rollout_compatibility() {
        let session = session("123456", Phase::Phase1, 42);

        let payload =
            SessionUpdateNotification::session_state_changed(&session).to_legacy_payload();

        assert_eq!(payload, "123456");
    }

    #[test]
    fn session_update_notification_serializes_typed_payload_with_updated_at() {
        let session = session("123456", Phase::Phase1, 42);

        let payload = SessionUpdateNotification::session_state_changed(&session)
            .to_payload()
            .expect("serialize notification");
        let payload: serde_json::Value =
            serde_json::from_str(&payload).expect("parse payload json");

        assert_eq!(payload["kind"], "session_state_changed");
        assert_eq!(payload["sessionCode"], "123456");
        assert_eq!(payload["updatedAt"], "1970-01-01T00:00:42+00:00");
        assert!(payload["payloadFingerprint"].as_str().is_some());
        assert!(payload.get("connectionId").is_none());
        assert!(payload.get("replicaId").is_none());
    }

    #[test]
    fn session_update_notification_prefers_typed_payload_before_legacy_for_rollout() {
        let session = session("123456", Phase::Phase1, 42);
        let notification = SessionUpdateNotification::session_state_changed(&session);

        let typed_payload = notification
            .to_payload()
            .expect("serialize typed notification");
        let legacy_payload = notification.to_legacy_payload();

        assert_ne!(typed_payload, legacy_payload);
        assert!(typed_payload.contains("updatedAt"));
    }

    #[test]
    fn realtime_replacement_notification_serializes_connection_metadata() {
        let payload = SessionUpdateNotification::realtime_connection_replaced(
            &RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            },
        )
        .to_payload()
        .expect("serialize replacement notification");
        let payload: serde_json::Value =
            serde_json::from_str(&payload).expect("parse replacement notification json");

        assert_eq!(payload["kind"], "realtime_connection_replaced");
        assert_eq!(payload["sessionCode"], "123456");
        assert_eq!(payload["connectionId"], "conn-1");
        assert_eq!(payload["replicaId"], "replica-a");
    }

    #[tokio::test]
    async fn in_memory_session_lease_is_exclusive() {
        let store = InMemorySessionStore::new();

        assert!(
            store
                .acquire_session_lease("123456", "lease-a", "2099-01-01T00:00:05Z")
                .await
                .expect("acquire first lease")
        );
        assert!(
            !store
                .acquire_session_lease("123456", "lease-b", "2099-01-01T00:00:04Z")
                .await
                .expect("reject overlapping lease")
        );
        assert!(
            !store
                .acquire_session_lease("123456", "lease-b", "2099-01-01T00:00:06Z")
                .await
                .expect("reject later overlapping lease")
        );
        assert!(
            store
                .renew_session_lease("123456", "lease-a", "2099-01-01T00:00:06Z")
                .await
                .expect("renew existing lease")
        );
    }

    #[tokio::test]
    async fn in_memory_realtime_claim_replaces_previous_owner_for_same_player() {
        let store = InMemorySessionStore::new();

        let first = store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("claim first connection");
        let second = store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-2".to_string(),
                replica_id: "replica-b".to_string(),
            })
            .await
            .expect("claim second connection");

        assert_eq!(first.replaced, None);
        assert_eq!(
            second.replaced,
            Some(RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
        );

        let registrations = store
            .list_realtime_connections("123456")
            .await
            .expect("list realtime registrations");
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].connection_id, "conn-2");
    }

    #[tokio::test]
    async fn stale_in_memory_realtime_connections_are_filtered_from_reads() {
        let store = InMemorySessionStore::new();

        store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("claim realtime connection");

        {
            let mut connections = store
                .realtime_connections_by_id
                .write()
                .expect("lock realtime connections");
            let (_, updated_at) = connections
                .get_mut("conn-1")
                .expect("stored connection exists");
            *updated_at =
                Utc::now() - chrono::Duration::seconds(REALTIME_CONNECTION_TTL_SECONDS + 1);
        }

        let registrations = store
            .list_realtime_connections("123456")
            .await
            .expect("list realtime registrations");
        assert!(
            registrations.is_empty(),
            "stale realtime registrations must not remain visible"
        );
    }

    #[tokio::test]
    async fn deleting_in_memory_realtime_connections_for_session_retires_all_connections() {
        let store = InMemorySessionStore::new();

        store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("claim first realtime connection");
        store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-2".to_string(),
                connection_id: "conn-2".to_string(),
                replica_id: "replica-b".to_string(),
            })
            .await
            .expect("claim second realtime connection");

        let deleted = store
            .delete_realtime_connections_for_session("123456")
            .await
            .expect("delete session realtime connections");

        assert_eq!(deleted.len(), 2);
        assert!(
            store
                .list_realtime_connections("123456")
                .await
                .expect("list cleared realtime connections")
                .is_empty()
        );
        assert!(
            store
                .take_retired_realtime_connection("conn-1", "replica-a")
                .await
                .expect("take retired first connection")
                .is_some()
        );
        assert!(
            store
                .take_retired_realtime_connection("conn-2", "replica-b")
                .await
                .expect("take retired second connection")
                .is_some()
        );
    }

    #[tokio::test]
    async fn in_memory_realtime_claim_reusing_same_connection_clears_stale_reverse_mapping() {
        let store = InMemorySessionStore::new();

        store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("claim initial connection");
        store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "654321".to_string(),
                player_id: "player-2".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("reuse same connection for another player");

        let reclaimed = store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-2".to_string(),
                replica_id: "replica-b".to_string(),
            })
            .await
            .expect("claim original player on new connection");

        assert_eq!(reclaimed.replaced, None);
        let original_session = store
            .list_realtime_connections("123456")
            .await
            .expect("list original session connections");
        assert_eq!(original_session.len(), 1);
        assert_eq!(original_session[0].connection_id, "conn-2");
        let replacement_session = store
            .list_realtime_connections("654321")
            .await
            .expect("list replacement session connections");
        assert_eq!(replacement_session.len(), 1);
        assert_eq!(replacement_session[0].connection_id, "conn-1");
    }

    #[tokio::test]
    async fn retired_in_memory_realtime_connection_cannot_reclaim_until_restored() {
        let store = InMemorySessionStore::new();

        store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("claim initial realtime connection");
        store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-2".to_string(),
                replica_id: "replica-b".to_string(),
            })
            .await
            .expect("replace initial realtime connection");

        let reclaim_error = store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect_err("retired connection must not reclaim ownership");
        assert!(matches!(
            reclaim_error,
            PersistenceError::RetiredRealtimeConnection { connection_id } if connection_id == "conn-1"
        ));

        let released = store
            .release_realtime_connection("conn-2", "replica-b")
            .await
            .expect("release replacement realtime connection");
        assert_eq!(
            released,
            Some(RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-2".to_string(),
                replica_id: "replica-b".to_string(),
            })
        );

        let restored = store
            .restore_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("restore retired realtime connection");
        assert!(restored.restored);
        assert_eq!(restored.replaced, None);

        let registrations = store
            .list_realtime_connections("123456")
            .await
            .expect("list realtime registrations after restore");
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].connection_id, "conn-1");
    }

    #[tokio::test]
    async fn restore_realtime_connection_does_not_override_newer_owner() {
        let store = InMemorySessionStore::new();

        store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("claim initial realtime connection");
        store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-2".to_string(),
                replica_id: "replica-b".to_string(),
            })
            .await
            .expect("replace initial realtime connection");
        store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-3".to_string(),
                replica_id: "replica-c".to_string(),
            })
            .await
            .expect("newer owner should replace second connection");

        let restored = store
            .restore_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("restore should no-op when newer owner exists");
        assert!(!restored.restored);

        let registrations = store
            .list_realtime_connections("123456")
            .await
            .expect("list realtime registrations after skipped restore");
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].connection_id, "conn-3");
        assert_eq!(registrations[0].replica_id, "replica-c");
    }

    #[tokio::test]
    async fn taking_retired_in_memory_realtime_connection_consumes_fence() {
        let store = InMemorySessionStore::new();

        store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
            .await
            .expect("claim initial realtime connection");
        store
            .claim_realtime_connection(&RealtimeConnectionRegistration {
                session_code: "123456".to_string(),
                player_id: "player-1".to_string(),
                connection_id: "conn-2".to_string(),
                replica_id: "replica-b".to_string(),
            })
            .await
            .expect("replace initial realtime connection");

        let taken = store
            .take_retired_realtime_connection("conn-1", "replica-a")
            .await
            .expect("take retired realtime connection");
        assert_eq!(
            taken,
            Some(RealtimeConnectionRegistration {
                session_code: String::new(),
                player_id: String::new(),
                connection_id: "conn-1".to_string(),
                replica_id: "replica-a".to_string(),
            })
        );
        assert!(
            store
                .take_retired_realtime_connection("conn-1", "replica-a")
                .await
                .expect("retired fence is consumed")
                .is_none()
        );
    }

    // ---------------------------------------------------------------------
    // Open-workshops pagination (plan2 item 9)
    // ---------------------------------------------------------------------

    /// Build a Lobby session with a host player named "Alice-<code>" and a
    /// caller-supplied `created_at`. `created_at` is what the paging keyset
    /// orders on; the `updated_at` stamp is irrelevant for pagination.
    fn lobby_session_at(code: &str, created_at_seconds: i64) -> WorkshopSession {
        let mut session = WorkshopSession::new(
            Uuid::new_v4(),
            SessionCode(code.to_string()),
            ts(created_at_seconds),
            config(),
        );
        session.phase = Phase::Lobby;
        let host_id = format!("host-{code}");
        session.host_player_id = Some(host_id.clone());
        session.add_player(SessionPlayer {
            id: host_id,
            name: format!("Alice-{code}"),
            account_id: None,
            character_id: None,
            selected_character: None,
            is_host: true,
            is_connected: true,
            is_ready: true,
            score: 0,
            current_dragon_id: None,
            achievements: Vec::new(),
            joined_at: ts(created_at_seconds),
        });
        session
    }

    async fn seed_lobbies(store: &InMemorySessionStore, count: usize, start_seconds: i64) {
        // Stagger `created_at` so every row sorts to a unique position. The
        // oldest row is at `start_seconds`; newest at
        // `start_seconds + count - 1`.
        for i in 0..count {
            let code = format!("{:06}", 100_000 + i);
            let session = lobby_session_at(&code, start_seconds + i as i64);
            store
                .save_session(&session)
                .await
                .expect("seed lobby session");
        }
    }

    #[tokio::test]
    async fn list_open_workshops_first_page_returns_page_size_newest_desc() {
        let store = InMemorySessionStore::new();
        seed_lobbies(&store, 120, 1_000).await;

        let page = store
            .list_open_workshops(OpenWorkshopsPaging::First)
            .await
            .expect("first page");

        assert_eq!(page.rows.len(), OPEN_WORKSHOPS_PAGE_SIZE);
        assert!(page.has_more_after);
        assert!(!page.has_more_before);
        // Strict DESC on (created_at, session_code ASC). With unique
        // created_ats the session_code tie-breaker doesn't kick in here;
        // see the dedicated test below.
        for pair in page.rows.windows(2) {
            assert!(pair[0].created_at > pair[1].created_at);
        }
        // Newest seeded row is `start + count - 1 = 1000 + 119 = 1119`.
        let newest = DateTime::from_timestamp(1_119, 0).unwrap().to_rfc3339();
        assert_eq!(page.rows.first().unwrap().created_at, newest);
    }

    #[tokio::test]
    async fn list_open_workshops_after_next_page_continues_desc() {
        let store = InMemorySessionStore::new();
        seed_lobbies(&store, 120, 1_000).await;

        let first = store
            .list_open_workshops(OpenWorkshopsPaging::First)
            .await
            .expect("first page");
        let cursor = {
            let last = first.rows.last().unwrap();
            OpenWorkshopCursor {
                created_at: last.created_at.clone(),
                session_code: last.session_code.clone(),
            }
        };

        let page2 = store
            .list_open_workshops(OpenWorkshopsPaging::After(cursor.clone()))
            .await
            .expect("after page");

        assert_eq!(page2.rows.len(), OPEN_WORKSHOPS_PAGE_SIZE);
        assert!(page2.has_more_after);
        assert!(page2.has_more_before);
        // Strict DESC and strictly older than the first page's last row.
        assert!(page2.rows.first().unwrap().created_at < cursor.created_at);
        for pair in page2.rows.windows(2) {
            assert!(pair[0].created_at > pair[1].created_at);
        }

        // Page 3 should still return a full page while older rows remain.
        let cursor3 = {
            let last = page2.rows.last().unwrap();
            OpenWorkshopCursor {
                created_at: last.created_at.clone(),
                session_code: last.session_code.clone(),
            }
        };
        let page3 = store
            .list_open_workshops(OpenWorkshopsPaging::After(cursor3))
            .await
            .expect("page 3");
        assert_eq!(page3.rows.len(), OPEN_WORKSHOPS_PAGE_SIZE);
        assert!(page3.has_more_after);
        assert!(page3.has_more_before);
    }

    #[tokio::test]
    async fn list_open_workshops_before_returns_page_symmetric_to_after() {
        let store = InMemorySessionStore::new();
        seed_lobbies(&store, 120, 1_000).await;

        // Walk forward to page 3, then Prev back: the returned slice must
        // match page 2 exactly.
        let page1 = store
            .list_open_workshops(OpenWorkshopsPaging::First)
            .await
            .expect("page 1");
        let p1_last = page1.rows.last().unwrap();
        let page2 = store
            .list_open_workshops(OpenWorkshopsPaging::After(OpenWorkshopCursor {
                created_at: p1_last.created_at.clone(),
                session_code: p1_last.session_code.clone(),
            }))
            .await
            .expect("page 2");
        let p2_last = page2.rows.last().unwrap();
        let page3 = store
            .list_open_workshops(OpenWorkshopsPaging::After(OpenWorkshopCursor {
                created_at: p2_last.created_at.clone(),
                session_code: p2_last.session_code.clone(),
            }))
            .await
            .expect("page 3");

        // Now Prev from page 3's first row.
        let p3_first = page3.rows.first().unwrap();
        let prev = store
            .list_open_workshops(OpenWorkshopsPaging::Before(OpenWorkshopCursor {
                created_at: p3_first.created_at.clone(),
                session_code: p3_first.session_code.clone(),
            }))
            .await
            .expect("prev page");

        assert_eq!(prev.rows, page2.rows);
        assert!(prev.has_more_after);
        assert!(prev.has_more_before);
    }

    #[tokio::test]
    async fn list_open_workshops_orders_ties_by_session_code_asc() {
        let store = InMemorySessionStore::new();
        // Two lobbies with identical created_at but different codes.
        let a = lobby_session_at("AAAAAA", 2_000);
        let b = lobby_session_at("BBBBBB", 2_000);
        store.save_session(&a).await.expect("save a");
        store.save_session(&b).await.expect("save b");

        let page = store
            .list_open_workshops(OpenWorkshopsPaging::First)
            .await
            .expect("first page");

        // DESC on created_at, but both are equal — tie-breaker sorts by
        // session_code ASC within the same created_at.
        assert_eq!(page.rows.len(), 2);
        assert_eq!(page.rows[0].session_code, "AAAAAA");
        assert_eq!(page.rows[1].session_code, "BBBBBB");
    }

    #[tokio::test]
    async fn list_open_workshops_after_cursor_keeps_equal_timestamp_older_code_side() {
        let store = InMemorySessionStore::new();
        for code in ["AAAAAA", "BBBBBB", "CCCCCC"] {
            store
                .save_session(&lobby_session_at(code, 2_000))
                .await
                .expect("save tied lobby");
        }

        let page = store
            .list_open_workshops(OpenWorkshopsPaging::After(OpenWorkshopCursor {
                created_at: "1970-01-01T00:33:20+00:00".to_string(),
                session_code: "BBBBBB".to_string(),
            }))
            .await
            .expect("after page");

        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].session_code, "CCCCCC");
    }

    #[tokio::test]
    async fn list_open_workshops_before_cursor_keeps_equal_timestamp_newer_code_side() {
        let store = InMemorySessionStore::new();
        for code in ["AAAAAA", "BBBBBB", "CCCCCC"] {
            store
                .save_session(&lobby_session_at(code, 2_000))
                .await
                .expect("save tied lobby");
        }

        let page = store
            .list_open_workshops(OpenWorkshopsPaging::Before(OpenWorkshopCursor {
                created_at: "1970-01-01T00:33:20+00:00".to_string(),
                session_code: "BBBBBB".to_string(),
            }))
            .await
            .expect("before page");

        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].session_code, "AAAAAA");
    }

    #[tokio::test]
    async fn list_open_workshops_first_page_with_exactly_page_size_rows_has_no_more_after() {
        // Boundary: no `+1` sentinel row beyond the page size. The page must
        // fill exactly and both has_more_* flags must be false.
        let store = InMemorySessionStore::new();
        seed_lobbies(&store, OPEN_WORKSHOPS_PAGE_SIZE, 1_000).await;

        let page = store
            .list_open_workshops(OpenWorkshopsPaging::First)
            .await
            .expect("first page");

        assert_eq!(page.rows.len(), OPEN_WORKSHOPS_PAGE_SIZE);
        assert!(
            !page.has_more_after,
            "no older rows exist beyond the last row on the page"
        );
        assert!(!page.has_more_before, "First page has no newer rows");
    }

    #[tokio::test]
    async fn list_open_workshops_first_page_with_exactly_page_size_plus_one_rows_signals_more() {
        // Boundary: exactly one extra sentinel. First page must truncate to
        // page size and flag more_after; a subsequent After(last) must yield the
        // lone remaining row with more_after = false.
        let store = InMemorySessionStore::new();
        seed_lobbies(&store, OPEN_WORKSHOPS_PAGE_SIZE + 1, 1_000).await;

        let page = store
            .list_open_workshops(OpenWorkshopsPaging::First)
            .await
            .expect("first page");
        assert_eq!(page.rows.len(), OPEN_WORKSHOPS_PAGE_SIZE);
        assert!(page.has_more_after);

        let last = page.rows.last().unwrap();
        let next = store
            .list_open_workshops(OpenWorkshopsPaging::After(OpenWorkshopCursor {
                created_at: last.created_at.clone(),
                session_code: last.session_code.clone(),
            }))
            .await
            .expect("after page");
        assert_eq!(next.rows.len(), 1);
        assert!(!next.has_more_after);
    }

    #[tokio::test]
    async fn list_open_workshops_excludes_non_lobby_sessions() {
        // Regression lock: a typo in the phase filter would silently return
        // Playing/Finished sessions in the open-workshops feed.
        let store = InMemorySessionStore::new();
        // 3 lobby sessions at ts 1000..1003.
        for i in 0..3 {
            let code = format!("LBBY{:02}", i);
            let session = lobby_session_at(&code, 1_000 + i);
            store.save_session(&session).await.expect("save lobby");
        }
        // 2 non-lobby sessions at later ts (so they would sort first if
        // erroneously included).
        for i in 0..2 {
            let code = format!("PLAY{:02}", i);
            let mut session = lobby_session_at(&code, 2_000 + i);
            session.phase = Phase::Phase1;
            store.save_session(&session).await.expect("save non-lobby");
        }

        let page = store
            .list_open_workshops(OpenWorkshopsPaging::First)
            .await
            .expect("first page");
        assert_eq!(page.rows.len(), 3);
        for row in &page.rows {
            assert!(row.session_code.starts_with("LBBY"));
        }
    }
}
