# Workshop Flow Refactor — Implementation Plan

Source spec: `refactor.md`. Analysis validated by three independent passes (architecture, screen-flow, business-rules). This document is the locked plan for implementation.

---

## 1. Requirements snapshot (locked)

### Accounts
- New `accounts` table: `id UUID PK`, `hero VARCHAR(64)`, `name VARCHAR(64)` with unique index on `lower(name)`, `password_hash VARCHAR(255)`, `created_at`, `updated_at`, `last_login_at`.
- Single `/api/auth` form supports **sign-in and sign-up**. Server disambiguates: name free → create account; name exists + password matches → login; name exists + wrong password → 401.
- Authentication: signed session cookie (`axum-extra` `cookie-signed`). Cookie payload: `account_id`. Lifespan: 14 days, `HttpOnly`, `SameSite=Lax`, `Secure` in prod.
- New env var: `SESSION_COOKIE_KEY` (64-byte hex, required in prod, dev-fallback generated at boot with warning).
- Hashing: argon2id, per-row 16-byte salt (encoded in hash), m=19456 KiB / t=2 / p=1. No pepper.
- Rate limits: account create 5/min/IP, login 10/min/IP, character create 20/hr/account.

### Characters
- Global `characters` table gains `owner_account_id UUID NULL REFERENCES accounts(id) ON DELETE CASCADE`. NULL = starter pool (current pre-generated rows).
- Max 5 owned characters per account. Enforced at service via `SELECT count(*) WHERE owner_account_id=$1` before insert; 400 on violation.
- Hard delete frees slots immediately.
- Deletion allowed anytime; active workshops already hold denormalized `CharacterProfile` snapshots in `SessionPlayer.selected_character`, so no cascade impact on running sessions.

### Workshops
- `GET /api/workshops?status=lobby` returns open workshops (phase == Lobby). Newest-first, cap 50.
- `POST /api/workshops/join` body becomes `{ session_code: String, character_id: Option<String> }`. Player name derived from logged-in account.
- New `GET /api/workshops/{code}/eligible-characters` returns the caller's characters; used to populate the PickCharacter screen.
- On create or join with zero owned characters, server **leases** a random starter: reads from `characters WHERE owner_account_id IS NULL`, samples without replacement among characters already leased in this workshop; if all 4 starters are used, reuse from the pool (explicit rule). Lease = set `SessionPlayer.selected_character` to a copy of the starter profile. No new `characters` row created. Leases do **not** count against the 5-per-account limit.
- Same-account dedup inside a workshop: if a connected player with same `account_id` is already present → 409 "already joined". Else treat as reconnect (rotate token).
- `Phase != Lobby` at join time → 409 "workshop already started".

### Phase 0 removal
- **Keep** `Phase::Phase0` enum variant and `WorkshopCreateConfig.phase0_minutes` field (serde default) for back-compat with persisted session blobs.
- **Drop** FSM edges `Lobby → Phase0` and `Phase0 → Phase1` from `can_transition`. `Lobby → Phase1` remains.
- **Remove** `SessionCommand::StartPhase0`.
- `begin_phase1` becomes strict: if any player has `selected_character.is_none()`, return new `DomainError::MissingSelectedCharacter { players }`. The `default_pet_description` fallback is deleted.
- `phase_duration_minutes` returns 0 for Phase0.

### Domain changes
- `SessionPlayer` gains `#[serde(default)] pub account_id: Option<String>`. `None` = legacy session. All new joins set it.
- Domain module adds `Account { id, hero, name, created_at }`, `AccountError`, and public constant `pub const MAX_CHARACTERS_PER_ACCOUNT: usize = 5`.

### Screens (frontend contract — frontend pass implements)
1. **SignIn** — hero + name + password form. Single submit button. Server endpoint `POST /api/auth`. On duplicate-with-wrong-password → NoticeBar.
2. **AccountHome** — header with account name + logout. Three blocks: "Create workshop" button (no inputs beyond existing `WorkshopCreateConfig`), "Create character" button (navigates to CreateCharacter screen), "Open workshops" list (polls `GET /api/workshops` every 5s; Join button per row). Own character count displayed as `n/5`.
3. **CreateCharacter** — owns the description + sprite-generation UI currently in `phase0_view.rs`. On save → `POST /api/accounts/me/characters`. Back to AccountHome.
4. **PickCharacter** — shown after clicking Join on an open workshop. Fetches `GET /api/workshops/{code}/eligible-characters`. Player selects one; if list is empty, a "Use a starter character" button calls Join with `character_id=None`. On select → `POST /api/workshops/join`.
5. **Session** — existing Lobby → Phase1 → Handover → Phase2 → Judge → Voting → End. Lobby gains host-only "Start Phase 1" button (dispatches `SessionCommand::StartPhase1` directly); inline `CharacterEditorBody` + `lobby_character_editor_open` signal are removed. Lobby and End both have "Leave workshop" → AccountHome (clears session snapshot, keeps cookie).
- Logout lives only on AccountHome (clears cookie via `POST /api/auth/logout` + clears session snapshot → SignIn).
- `WorkshopBrief` 4-card onboarding component is **deleted** (UX rule: no duplicated onboarding).
- Archive panel stays where it is (inside End view).

### Bootstrap decision
- No cookie → SignIn.
- Cookie + session snapshot in client storage → Session (attempt reconnect).
- Cookie + no snapshot → AccountHome.

### ShellScreen enum (frontend)
```rust
enum ShellScreen {
    SignIn,
    AccountHome,
    CreateCharacter,
    PickCharacter { workshop_code: String },
    Session,
}
```

### Auth boundary
- Cookie gates: `/api/auth/*`, `/api/accounts/me/*`, `/api/workshops` (GET list, POST create), `/api/workshops/{code}/eligible-characters`, `/api/workshops/join`.
- **Workshop commands** (`/api/workshops/command`) continue to authorize via `reconnect_token` only (unchanged from today). This preserves behavior when the cookie expires mid-workshop.
- **WebSocket** (`/api/workshops/ws`) reads the signed cookie on upgrade and additionally asserts `session.players[player_id].account_id == cookie.account_id` before attaching. Legacy sessions (where player has `account_id == None`) skip this check.

### Non-goals (explicit)
- Achievement aggregation across sessions.
- Character edit/delete UI (delete endpoint is shipped; UI is a follow-up).
- Email verification, password reset.
- Pepper / KMS.
- Cross-workshop WebSocket for live open-workshops updates (polling is enough).

---

## 2. Build order & verification

**Backend pass (this PR):**
1. `platform/crates/persistence/migrations/0007_accounts_and_ownership.sql`
2. `platform/crates/security/` — argon2 helpers (sync; callers use `spawn_blocking`).
3. `platform/crates/persistence/src/lib.rs` — `AccountRecord`, account CRUD, `characters.owner_account_id` column, list-by-owner, count-by-owner.
4. `platform/crates/protocol/src/lib.rs` — new types and `SessionCommand::StartPhase0` removal.
5. `platform/crates/domain/src/lib.rs` — `Account` entity, `MAX_CHARACTERS_PER_ACCOUNT`, `MissingSelectedCharacter` error, `SessionPlayer.account_id`, FSM edge removal, strict `begin_phase1`.
6. `platform/app-server/` — cookie key in `AppConfig`, `require_account` middleware, new handlers (`create_account`, `login`, `logout`, `list_my_characters`, `create_character`, `delete_character`, `list_open_workshops`, `list_eligible_characters`), refactored `create_workshop` / `join_workshop`, rate-limit registration, WS cookie check.
7. `platform/xtask/src/main.rs` — `smoke-sprite-load` uses new account/character flow and direct `Lobby→Phase1`.
8. Cargo tests: update `tests.rs` (strip Phase0 fixtures, add new endpoints), update `crates/domain` tests (Phase0 edge tests → Phase1 direct), update `crates/persistence/src/postgres_tests.rs` (add account round-trip, owner column), add new integration tests for account/character/open-workshops endpoints.
9. Verification: `cargo check --workspace --all-targets` green, `cargo test --workspace` green.

**Frontend pass (next conversation):**
10. `platform/app-web/src/state.rs` — extend `ShellScreen`, split identity clearing.
11. New components: `sign_in.rs`, `account_home.rs`, `create_character.rs`, `pick_character.rs`. Move sprite editor out of `phase0_view.rs` into `create_character.rs`, then delete `phase0_view.rs`.
12. Rewrite `hero.rs` usage; delete `workshop_brief.rs` + CSS.
13. `lobby_view.rs` host button + leave button. `end_view.rs` leave button retargets to AccountHome.
14. `main.rs` routing rebuild; remove `is_phase0` branch.
15. `flows.rs` — new auth/account/character/open-workshops flows; delete client-side `tags.len() != 3` check.
16. `api.rs` — new endpoints.
17. CSS cleanup in `static/style.css`.
18. Frontend unit tests in `helpers.rs` updated.
19. Verification: `cargo check -p app-web`, `xtask build-web`.

**E2E pass (next conversation):**
20. Rewrite `e2e/tests/gameplay-helpers.ts` (add `signUpAccount`, `logInAccount`, `createCharacter`, `joinOpenWorkshop`; delete `openCharacterCreation`, `saveDragonProfile`, `generateDragonSprites`).
21. Update `e2e-scenario.spec.ts`, `gameplay.spec.ts`, `restart-reconnect.spec.ts`, `view-validators.spec.ts`, `visual-validators.spec.ts` (drop home-screen & Phase0 rotations).
22. Verification: `npm ci && npm run test:deployed` against local kind.

---

## 3. API surface (new)

| Method | Path | Auth | Body | Response |
|---|---|---|---|---|
| POST | `/api/auth` | public | `{hero, name, password}` | 201 w/ cookie (new) OR 200 w/ cookie (login) OR 401 |
| POST | `/api/auth/logout` | cookie | — | 204, clears cookie |
| GET | `/api/accounts/me` | cookie | — | `AccountProfile` |
| GET | `/api/accounts/me/characters` | cookie | — | `{characters: Vec<CharacterProfile>, limit: 5}` |
| POST | `/api/accounts/me/characters` | cookie | `CreateCharacterRequest` | `CharacterProfile`; 400 if at limit |
| DELETE | `/api/accounts/me/characters/{id}` | cookie | — | 204; 404 if not owned |
| GET | `/api/workshops?status=lobby` | cookie | — | `ListOpenWorkshopsResponse` |
| POST | `/api/workshops` | cookie | `WorkshopCreateConfig` | `JoinSuccess` (player name from account) |
| GET | `/api/workshops/{code}/eligible-characters` | cookie | — | `{characters: Vec<CharacterProfile>}` |
| POST | `/api/workshops/join` | cookie | `{session_code, character_id: Option<String>}` | `JoinSuccess` or 409 |

Unchanged: `/api/workshops/command`, `/api/workshops/ws`, `/api/workshops/judge-bundle`, `/api/workshops/sprite-sheet`, `/api/characters/catalog`.

Removed: `SessionCommand::StartPhase0`.

---

## 4. Migration 0007 (sketch)

```sql
CREATE TABLE accounts (
    id UUID PRIMARY KEY,
    hero VARCHAR(64) NOT NULL,
    name VARCHAR(64) NOT NULL,
    password_hash VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    last_login_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX accounts_name_lower_idx ON accounts (LOWER(name));

ALTER TABLE characters ADD COLUMN owner_account_id UUID NULL REFERENCES accounts(id) ON DELETE CASCADE;
CREATE INDEX characters_owner_idx ON characters (owner_account_id) WHERE owner_account_id IS NOT NULL;
```

Starter rows remain with `owner_account_id = NULL` (the lease pool).

---

## 5. Residual risks

- Starter pool has only 4 characters today; a 5-zero-character workshop will reuse. Acceptable per explicit rule; product can grow the pool later.
- `SESSION_COOKIE_KEY` rotation not handled; rotating invalidates all sessions. Out of scope.
- `Phase::Phase0` variant retained for deserialization; any legacy session stuck in that phase would have no valid transition after the FSM change. Acceptable because no live production sessions persist through deploys of this scale.
- `workshop_brief` deletion removes a small amount of tutorial copy that some returning players may expect. Acceptable per UX rule.

---

## 6. Execution log

### Session 2 — partial data-layer pass (2026-04-19)

**Landed:**
- Baseline fix: `crates/domain/src/lib.rs` test module missing `use protocol::SpriteSet;`. Unrelated pre-existing breakage; fixed.
- `crates/persistence/migrations/0007_accounts_and_ownership.sql` — accounts table + `characters.owner_account_id` column + unique `LOWER(name)` index + partial index on owner.
- `crates/security/` — `argon2` 0.5 + `password-hash` 0.5 + `rand_core` 0.6 deps; `hash_password` / `verify_password` + `PasswordHashError` enum; 3 unit tests green. Params: Argon2id m=19456 KiB, t=2, p=1.
- `crates/protocol/src/lib.rs` — new additive types: `AccountProfile`, `AuthRequest`, `AuthResponse`, `CreateCharacterRequest`, `MyCharactersResponse`, `EligibleCharactersResponse`, `OpenWorkshopSummary`, `ListOpenWorkshopsResponse`.

**Plan deviations worth re-reading next session:**
1. **Schema types**: Plan §4 specified `UUID` / `TIMESTAMPTZ`. Existing migrations (0006) use `TEXT` for ids and timestamps. Migration 0007 follows the existing convention (`TEXT` primary key, `TEXT` timestamps). Account ids will be formatted UUIDs stored as TEXT. This matches how `characters.character_id` is already stored.
2. **Rate limiter crate**: Plan called for `tower_governor`. `crates/security` already provides `FixedWindowRateLimiter` — reuse it in `app-server` instead of adding a new dep. Update §1 rate-limits note accordingly when wiring handlers.
3. **`JoinWorkshopRequest`**: Plan §3 said body becomes `{ session_code, character_id }`. The existing `protocol::JoinWorkshopRequest` already has those fields plus `name`/`reconnect_token` as optional. Server will ignore client-supplied `name` (derive from cookie); keep the existing type and don't introduce a duplicate.
4. **Argon2 params constant location**: Argon2 params live inside `crates/security/src/lib.rs` as module constants (`ARGON2_MEMORY_KIB`, etc.), not exposed publicly. Only `hash_password` / `verify_password` are public. Good.
5. **`StartPhase0` removal & strict `begin_phase1` NOT done yet**: These are destructive and would break ~33 existing cargo tests + xtask. Deferred to Session 3 to land atomically with test rewrites.

**Not yet done (next session, in this order):**
1. Persistence: add `owner_account_id: Option<String>` to `CharacterRecord` (None for starters & legacy rows); add `AccountRecord`; extend `SessionStore` trait with `insert_account`, `find_account_by_name_lower`, `touch_last_login`, `list_characters_by_owner`, `count_characters_by_owner`, `delete_character_by_owner`; implement on `InMemorySessionStore` and `PostgresSessionStore`. Row-mapper needs to handle the new column for starters (NULL) and owned rows.
2. Domain: add `Account { id, hero, name, created_at }`, `AccountError`, `MAX_CHARACTERS_PER_ACCOUNT = 5`, `DomainError::MissingSelectedCharacter { players: Vec<String> }` variant (unused for now), `SessionPlayer.account_id: Option<String>` with `#[serde(default)]`.
3. Verify `cargo check --workspace --all-targets` still green.
4. App-server: cookie key in `AppConfig`, signed-cookie middleware, handlers (auth/logout/accounts/characters/workshops list+eligible), refactored `create_workshop` + `join_workshop`, rate-limit wiring (reuse `security::FixedWindowRateLimiter`), WS cookie check. **This is where `StartPhase0` gets removed and `begin_phase1` becomes strict** — must land together with tests.rs rewrite.
5. xtask smoke-sprite-load: use new account + character flow + direct `Lobby→Phase1`.
6. Rewrite `app-server/src/tests.rs` (~33 Phase0 refs), `crates/domain` tests (Phase0 edges → Phase1 direct), `crates/persistence/src/postgres_tests.rs` (account round-trip, owner column).
7. Verify `cargo check --workspace --all-targets` + `cargo test --workspace` green.
8. Run 6 validator lenses: requirements-fulfillment, requirements-drift, correctness, architect, testing, operations.

---

### Session 3 — persistence + domain data-layer pass (2026-04-19)

**Landed:**
- `crates/persistence/src/lib.rs`:
  - `CharacterRecord` gains `owner_account_id: Option<String>` (docs: `None` = starter pool / leasable; `Some` = owned).
  - New `AccountRecord { id, hero, name, password_hash, created_at, updated_at, last_login_at }`.
  - New `PersistenceError::DuplicateAccountName` variant.
  - `SessionStore` trait extended with 7 methods: `insert_account`, `find_account_by_name_lower`, `find_account_by_id`, `touch_last_login`, `list_characters_by_owner`, `count_characters_by_owner`, `delete_character_by_owner`.
  - `InMemorySessionStore`: new `accounts_by_id` field; full implementations of all 7 methods (case-insensitive name check in-memory; owner-filtered scans for characters).
  - `PostgresSessionStore`: full SQL implementations; new `account_from_row` helper; `character_from_row` reads `owner_account_id` via `try_get::<Option<String>, _>().ok().flatten()` (forward-compatible with pre-0007 rows in tests); `save_character_in_tx` / `load_character` / `list_characters` SQL updated to include `owner_account_id`. Constraint-name mapping: `accounts_name_lower_idx` → `DuplicateAccountName`.
- `crates/persistence/src/postgres_tests.rs`: `CharacterRecord` literal + 2 `SessionPlayer` literals updated for new fields.
- `app-server/src/http.rs`: 1 `CharacterRecord` literal and 2 `SessionPlayer` literals updated with `owner_account_id: None` / `account_id: None`. **Note**: the http.rs `CharacterRecord` construction is in the workshop-step-0 character creation flow that will be deleted in session 4.
- `app-server/src/tests.rs`: `AccountRecord` added to persistence imports; `FaultyStore: SessionStore` impl gains 7 delegate methods for the new trait methods. 1 `SessionPlayer` literal updated.
- `crates/domain/src/lib.rs`:
  - `SessionPlayer` gains `#[serde(default, skip_serializing_if = "Option::is_none")] pub account_id: Option<String>`.
  - New `pub const MAX_CHARACTERS_PER_ACCOUNT: usize = 5`.
  - New `Account { id, hero, name: String, created_at }` struct (distinct from persistence `AccountRecord`; no password hash).
  - New `AccountError` enum: `DuplicateName`, `InvalidCredentials`, `NotFound`, `CharacterLimitReached { max }`, `CharacterNotOwned`.
  - New `DomainError::MissingSelectedCharacter { players: Vec<String> }` variant (reserved for session 4; no production path constructs it yet).

**Verification:**
- `cargo check --workspace --all-targets` → green (warnings only, all pre-existing dead-code warnings in `app-web`/`app-server`).
- `cargo test -p security -p protocol -p domain --lib` → all pass (security 16, domain 137).
- `cargo test -p persistence --lib` → 26 passed, 5 failed. All 5 failures are the pre-existing docker-dependent `postgres_tests::postgres_tests::*` suite that also fails at baseline without a live Postgres. In-memory persistence tests all pass.

**Plan deviations / notes for session 4:**
1. `SessionPlayer.account_id` uses `#[serde(default, skip_serializing_if = "Option::is_none")]` (both attrs). Plan §7 only mentioned `#[serde(default)]`; the additional `skip_serializing_if` keeps wire-format compact and matches the existing `character_id`/`selected_character` convention in the same struct.
2. Existing `SessionStore::list_characters` still returns all rows (starters + owned). The "starter-pool only" filter needed by `GET /api/characters/catalog` will live in the app-server handler in session 4, not in persistence. Avoids changing existing trait semantics.
3. `save_character_in_tx` INSERT / ON CONFLICT UPDATE now writes `owner_account_id`. If any code path calls `save_character` on an existing starter row (owner_account_id IS NULL) and passes `None`, the UPDATE is a no-op for that column. Safe.
4. `account_from_row` parses `created_at` / `updated_at` / `last_login_at` as RFC3339 TEXT to match the 0007 migration's `TEXT` timestamp convention (locked in session 2, deviation note 1).
5. Domain `Account` is deliberately **not** a re-export of `AccountRecord`; keeping the boundary means handlers can construct domain `Account` values without leaking the password hash into domain/session state.

**Still deferred to session 4 (destructive, must land atomically with test rewrites):**
- Remove `SessionCommand::StartPhase0`.
- Remove FSM edges `Lobby→Phase0` and `Phase0→Phase1` from `can_transition`.
- Make `begin_phase1` strict: return `DomainError::MissingSelectedCharacter { players }` when any player lacks `selected_character`. Delete `default_pet_description` fallback.
- Rewrite ~33 Phase0 references in `app-server/src/tests.rs` and the Phase0-edge tests in `crates/domain`.
- App-server: cookie key in `AppConfig`, signed-cookie middleware, all new handlers (auth/logout/accounts/characters/workshops list+eligible), refactored `create_workshop` / `join_workshop` (derive name from cookie; ignore client `name`), starter-lease logic on zero-character join, rate-limit wiring via existing `security::FixedWindowRateLimiter`, WS cookie check.
- Starter-pool filtering in the `/api/characters/catalog` handler (use `list_characters` + filter `owner_account_id.is_none()`).
- Delete the workshop-step-0 character creation flow in `app-server/src/http.rs` (including the `CharacterRecord` literal patched here).
- xtask `smoke-sprite-load`: update to new account + character flow + direct `Lobby→Phase1`.
- `postgres_tests.rs`: add account round-trip and owner-column tests.
- Run 6 orchestrator validator lenses.

---

### Session 4 — destructive pass, Checkpoint 1 (2026-04-19)

**Scope:** domain destructive + tests.rs rewrites. Goal: `cargo check --workspace --all-targets` green and all non-docker tests passing, with character-creation no longer in the workshop lifecycle.

**Landed:**
- `crates/protocol/src/lib.rs`: removed `SessionCommand::StartPhase0` variant (`Phase::Phase0` enum variant retained for legacy deserialize only).
- `crates/domain/src/lib.rs`:
  - FSM: removed edges `Lobby→Phase0` and `Phase0→Phase1` from `can_transition`. Phase0 is now unreachable (no inbound edges).
  - `begin_phase1` is strict: returns `DomainError::MissingSelectedCharacter { players }` when any player lacks `selected_character`; now transitions directly `Lobby→Phase1`.
  - `default_pet_description` neutered (kept as compile-time shim, no production callers).
  - Phase-transition tests rewritten for the new graph.
  - **137/137 domain tests pass.**
- `xtask/src/main.rs`: removed the `StartPhase0` step from smoke-sprite-load (will be further rewritten in Checkpoint 3 to use the full account flow).
- `app-server/src/http.rs`:
  - `SessionCommand::StartPhase0` match arm deleted.
  - `StartPhase1` guard tightened (rejects non-`Lobby` phases); previous auto-assign fallback via `pick_random_character_profile` removed.
- `app-server/src/tests.rs`:
  - Deleted `setup_phase0_body` helper; added `setup_phase1_body`.
  - Added two seeding helpers:
    - async `seed_selected_characters(state, session_code)` — mutates the cached `WorkshopSession` via `state.sessions.lock()` so every player has a `selected_character` and `is_ready=true`. Used by HTTP-driven tests.
    - sync `seed_selected_characters_on_session(&mut session)` — same shape, for tests that construct a `WorkshopSession` by value. Used by pure-domain-shaped tests in this file.
  - Bulk rewrites via two Python scripts (`/tmp/rewrite_tests.py`, `/tmp/inject_seeds.py`) covering 10 for-loop and 11 singleton sites.
  - Manual rewrites:
    - 6 Rust-level `SessionCommand::StartPhase0` call sites fixed, 5 `session.transition_to(protocol::Phase::Phase0)` direct-mutation sites deleted.
    - `workshop_command_does_not_leave_mutated_cache_when_persisted_command_write_fails` — seeded via the store (not the cache) so the subsequent `reload_cached_session` pass preserves the seeded character.
    - Redundant follow-up `StartPhase1` block in `postgres_restart_reload_and_reconnect_keep_presence_runtime_only` deleted.
  - Bulk dedupe script (`/tmp/strip_phase0_blocks.py`) removed 15 duplicate HTTP `StartPhase1` round-trips that the earlier bulk rename had produced (original flow was phase0-then-phase1; both got renamed to phase1, so the first was a no-op → 422).
  - Deleted two tests that assert pre-refactor semantics which are no longer reachable:
    - `to_client_game_state_propagates_custom_sprites_to_dragon` — asserted that a player without a selected_character produced a dragon with `custom_sprites: None`. Post-refactor every player has a selected_character; per user direction, custom_sprites will be sourced from the character database and the "no sprites" branch is not needed.
    - `llm_images_times_out_while_waiting_for_shared_queue_capacity` — exercised `/api/llm/images` which is gated to `Phase::Phase0`. Feature will be redesigned in a later pass; test deleted per user direction.
  - Removed now-unused `LlmImageResult` import.

**Verification:**
- `cargo check --workspace --all-targets` → green (warnings only; all pre-existing dead-code warnings in `http.rs` sprite/catalog reject helpers).
- `cargo test -p domain --lib` → 137 passed.
- `cargo test -p app-server` → 128 passed, 4 failed. All 4 failures are the pre-existing docker-dependent postgres suite (`docker run for Postgres test container failed`); not refactor-related. Docker daemon is not running in the current environment.
- `cargo test --workspace --exclude app-server` → other crates: 51 + 137 pass; persistence 26 passed / 5 failed (same docker env issue).
- Net: **all non-docker tests pass after the destructive pass**.

**Plan deviations / notes for Checkpoint 2:**
1. **Seeding strategy split**: ended up needing both an async cache-mutating helper and a sync by-value helper because different test classes construct sessions differently. The async helper has a hidden failure mode — it mutates the cache but any downstream `reload_cached_session` (which most command handlers run) re-reads the store and wipes the seed. The rule is: if a test seeds a custom store with `store.inner.save_session`, seed the character via `seed_selected_characters_on_session` **before** saving. If a test builds the session purely via HTTP (`POST /api/workshops`), seed via the async helper after create. This is documented inline in tests.rs.
2. **Dead code warnings** accumulating in `app-server/src/http.rs`: `too_many_sprite_sheet_requests`, `reject_disallowed_sprite_sheet_origin`, `bad_sprite_sheet_request`, `bad_character_catalog_request`, `internal_sprite_sheet_error`, plus `pick_random_character_profile`. Clean up in Checkpoint 2 when handlers are refactored.
3. **`/api/llm/images` endpoint** still has `if session.phase != protocol::Phase::Phase0 { ... }` guard. Endpoint is now unreachable in normal flow (Phase0 not entered). Decide in Checkpoint 2 whether to gate on Phase1 or remove the endpoint entirely pending wider LLM-image redesign.
4. **`custom_sprites` field on `DragonDto` / `ClientPlayer`**: still present in protocol + helpers.rs propagation. Per user direction, sprites will be sourced from the character database going forward — this is a wider simplification deferred beyond Checkpoint 2.

**Still deferred to Checkpoint 2:**
- Add `axum-extra` with `cookie-signed` feature.
- `AppConfig.cookie_key` + `SESSION_COOKIE_KEY` env (dev fallback = random 64-byte key at boot + WARN log; prod detection reuses `is_production`).
- Signed-cookie middleware + `AccountSession` typed extractor.
- Handlers: auth/logout/accounts/characters/workshops list+eligible.
- Refactor `create_workshop` + `join_workshop` (derive name from cookie, ignore client `name`, zero-character join leases a starter).
- Starter-pool filter in `/api/characters/catalog` handler.
- Rate limiters: auth (existing), character-create (20/hr/account keyed by `account_id`).
- WS handler: soft cookie check (accept cookie OR `reconnect_token`) to keep the two-pass refactor compatible with the current frontend.

**Still deferred to Checkpoint 3:**
- xtask `smoke-sprite-load`: full account flow.
- `postgres_tests.rs`: account round-trip + `owner_account_id` column.
- Final update to this execution log.
- Run 6 validator lenses.

---

### Session 4 — Checkpoint 2-a (2026-04-19): cookie key + auth/logout + security hardening

**Scope:** land the auth foundation (dependency + cookie key + extractor + signin/logout endpoints) so Checkpoint 2-b/c/d can build protected handlers on top. Land security hardening surfaced by the post-2-a validator pass before any new handlers go in.

**Landed — infrastructure:**
- `Cargo.toml` (workspace): added `axum-extra = { version = "0.10", features = ["cookie", "cookie-signed"] }`.
- `app-server/Cargo.toml`: wired the workspace dep.
- `app-server/src/app.rs`:
  - `AppConfig` gains `cookie_key: Key` + `is_production: bool`. Manual `Debug` redacts the cookie key.
  - `load_config` parses `SESSION_COOKIE_KEY` (base64 STANDARD / URL-SAFE / NO-PAD, ≥64 decoded bytes). Prod: hard error if unset or empty. Dev: `Key::generate()` fallback + `tracing::warn!("SESSION_COOKIE_KEY not set; generated ephemeral key — sessions will not survive restarts")`.
  - `FromRef<AppState> for Key` wired so `SignedCookieJar<Key>` extractors work.
  - `/api/auth/signin` and `/api/auth/logout` nested routes registered.

**Landed — auth module (`app-server/src/auth.rs`, 358 lines):**
- `SESSION_COOKIE_NAME = "ds_session"` constant.
- `build_session_cookie(account_id, is_production)` — `HttpOnly`, `SameSite=Lax`, `Path=/`, `Secure` gated on prod.
- `build_logout_cookie(is_production)` — matching attributes so `SignedCookieJar::remove` clears browser entry.
- `AccountSession { account: domain::Account }` extractor via `FromRequestParts<AppState>`. Resolves cookie → account lookup; maps missing cookie / empty value / unknown account / store error to `AuthRejection::{Unauthenticated, UnknownAccount, Internal}`.
- `account_from_record` / `account_profile` helpers (domain boundary; password hash never leaves persistence).
- `signin` handler: trim hero/name, length bounds (1-64 / 1-64 / 8-256), `find_account_by_name_lower` → create-or-login. Maps `PersistenceError::DuplicateAccountName` → 409 (race branch). Issues signed cookie on 201/200.
- `logout` handler: idempotent 204; always emits a removal Set-Cookie via `jar.remove`.

**Landed — security hardening (prompted by 10-validator post-2-a pass):**
- **HIGH-1 fix** — `hash_password` and `verify_password` now run inside `tokio::task::spawn_blocking` via new `hash_password_blocking` / `verify_password_blocking` helpers. argon2id is ~60ms of pure CPU at the configured params (m=19456 / t=2 / p=1); running inline blocked a Tokio worker per signin. `crates/security/src/lib.rs` already documents the `MUST use spawn_blocking` requirement on `hash_password` (line 30) and `verify_password` (line 49); the callers now honour it.
- **HIGH-2 fix** — timing side-channel closed on the unknown-name branch of `signin`. Added `TIMING_DUMMY_HASH: LazyLock<String>` at module scope (lazily hashes a constant sentinel once per process). When `find_account_by_name_lower` returns `Ok(None)` the handler now runs `verify_password_blocking(password, TIMING_DUMMY_HASH.clone())` before falling through to the create path, equalising latency with the known-name-wrong-password branch. An attacker can no longer distinguish "name free" from "name taken + wrong password" by measuring response time at the lookup stage. (The create path itself still runs `hash_password` on success, so a 201 response is asymmetric vs a 401 — but status code already signals that outcome, so no additional oracle is introduced.)
- Kept `spawn_blocking` closure signature at `FnOnce() -> Result<…>` and mapped `JoinError` to the matching `PasswordHashError` variant (`HashFailure` / `MalformedHash`) so panics in the worker surface as 500 without double-logging.

**Landed — tests (`app-server/src/tests.rs`):**
- Existing 6 smoke tests re-verified green under the new blocking/timing-equalised implementation:
  - `signin_creates_new_account_and_sets_cookie`
  - `signin_logs_in_existing_account_when_password_matches`
  - `signin_rejects_wrong_password_for_existing_name`
  - `signin_rejects_short_password`
  - `logout_returns_no_content_and_clears_cookie`
  - `logout_is_idempotent_without_prior_cookie`
- New P1 test added from the validator-pass gap list:
  - `signin_is_case_insensitive_on_login` — creates "Alice", logs in as "ALICE", expects 200 + `created: false` + response payload retains stored casing ("Alice"). Guards against a regression where the handler short-circuits to a raw case-sensitive compare.
- 2 config tests added/updated for cookie-key plumbing: `load_config_requires_cookie_key_in_production` and `load_config_rejects_short_cookie_key` (dev path). Existing prod-path tests set `SESSION_COOKIE_KEY` to a `fake_cookie_key_base64()` fixture to avoid failing on the new hard-error branch.
- `ScopedEnvVar::unset` helper retained from earlier in 2-a.

**P1 tests deferred by design:**
- `signin_rejects_tampered_cookie` — needs a protected endpoint. Will land with `GET /api/accounts/me` in Checkpoint 2-b as the extractor's first real consumer.
- Concurrent-create race → 409 handler-level test — not deterministically reproducible without a persistence fault-injection hook. The `DuplicateAccountName` mapping is already covered at the persistence layer (constraint-name test in `postgres_tests.rs`).

**Verification:**
- `cargo check --workspace --all-targets` → green. Warnings: `AccountSession` + `AuthRejection` marked dead-code (consumed by C2-b), 5 pre-existing `http.rs` helper warnings, `default_pet_description` shim, 5 app-web judge-bundle warnings — all pre-existing / expected.
- `cargo test -p app-server --bin app-server -- signin_ logout_` → 7/7 pass (baseline 6 + new case-insensitive).
- `cargo test --workspace --no-fail-fast` →
  - app-server 137 pass / 4 fail (docker) — **+1 vs post-2-a baseline (136 → 137), 0 regressions**.
  - domain 137/0, persistence 26/5 (docker), app-web 51/0, protocol 11/0, realtime 6/0, security 16/0, xtask 15/0.
  - All 9 failures are the pre-existing docker-dependent postgres suite; unchanged since C1.

**Validator-pass findings carried into Checkpoint 2-b (not fixed in 2-a):**
1. **API contract / validator #4 #1**: `POST /api/characters/sprite-sheet` (http.rs:2278) is still workshop-scoped (`session_code` + `reconnect_token`). The new CreateCharacter screen is account-scoped. **Decide in 2-b**: re-gate the existing endpoint to `AccountSession`, or add a sibling account-scoped `/api/accounts/me/characters/sprite-preview`. Leaning re-gate (single source of truth, simpler frontend).
2. **Plan drift / validator #4**: plan §3 originally named the route `POST /api/auth`; server ships `/api/auth/signin`. Leave the endpoint name (clearer semantics, create-or-login); section §3 will be updated inline during 2-b when the full route table is revised.
3. **Design smell / validator #5 #4 (MEDIUM)**: `AccountError::InvalidCredentials` and `DuplicateName` variants exist in `domain::AccountError` but `auth.rs` emits raw JSON responses instead of routing through the enum. Decide in 2-b whether to narrow the enum to `CharacterOwnershipError` (auth stays HTTP-raw) or route auth through it. Leaning narrow the enum — auth error mapping is naturally HTTP-shaped (401/409/500) and adding a layer buys nothing.
4. **Doc drift / validator #2 #15 (LOW)**: `AuthRejection::UnknownAccount` doc-comment at auth.rs:113-116 promises to clear the stale cookie, but `IntoResponse` doesn't emit a removal Set-Cookie. Either update the comment or add the cookie-clearing behaviour in 2-b (trivial — return `(StatusCode, jar.remove(…), Json)` instead of plain tuple). Leaning fix the behaviour (low-cost hardening).
5. **UX / validator #2 #13 (LOW)**: signin length bounds use `.len()` (bytes) on `hero`/`name`/`password`. Emoji / CJK hit the 64-byte cap at much lower visible char counts. Switch to `.chars().count()` in 2-b when other validation cleans up.
6. **Hardening / validator #2 #6 (LOW)**: cookie value is a signed plaintext UUID. Consider `PrivateCookieJar` (encrypted) in a later pass. Not a 2-b blocker.
7. **Persistence / validator #7 #4 (LOW)**: Postgres `insert_account` error mapping matches on constraint name only, not SQLSTATE 23505. Brittle to index rename. Optional belt-and-suspenders in 2-b or 2-d.
8. **Clippy regression / validator #6**: `cargo clippy --workspace --all-targets -- -D warnings` fails with 7 style lints in `app-web` (1× `if_same_then_else` end_view.rs:62, 5× `redundant_closure` handover_view.rs:16-19 + phase1_view.rs:18, 1× `manual_range_contains` main.rs:73). Frontend is out of session-4 scope per locked decision #11, but these need addressing before any frontend work can land a green clippy pass. Logged; fix out-of-band or in a session-5 frontend pass.
9. **Process flag / validator #9 #10**: working tree has ~723 insertions / 233 deletions in `app-web/src/` not mentioned anywhere in this log. Contradicts locked decision #11 ("Frontend out of scope for Session 4"). **Unresolved** — likely WIP from a prior unrelated conversation; treated as orthogonal to refactor work. Do not commit these alongside 2-a changes without explicit user review.
10. **Known Phase0 dead code / validator #8**: `/api/llm/images` at http.rs:2140/2177 is guarded by `if session.phase != Phase::Phase0 { ... }` which is now unreachable → endpoint rejects 100% of calls. Scheduled for C2-d decision (gate to Phase1 or remove pending LLM-image redesign).

**Scope remaining in Checkpoint 2 (b/c/d):**
- **2-b**: `GET /api/accounts/me`, `POST /api/characters` (with 5-per-account limit), `GET /api/characters/mine`, `DELETE /api/characters/:id`, `GET /api/workshops/open` (needs new persistence `list_open_workshops()` for `Phase::Lobby` sessions), `GET /api/workshops/:code/eligible-characters`, starter-pool filter on `list_character_catalog`. Decide sprite-sheet endpoint scoping (item 1 above). Land deferred P1 cookie-tampering test against `/api/accounts/me`. Fix `AuthRejection::UnknownAccount` cookie-clearing (item 4).
- **2-c**: refactor `create_workshop` + `join_workshop` to derive hero/name from `AccountSession` (ignore client-supplied `name` per locked decision #9); implement starter-lease on zero-character join (copy `CharacterProfile` into `SessionPlayer.selected_character`, do **not** transfer `owner_account_id`).
- **2-d**: character-create rate limiter (20/hr/account via existing `security::FixedWindowRateLimiter`); WS handler soft cookie check (accept cookie OR `reconnect_token`); decide `/api/llm/images` fate; final `cargo test` sweep + plan log + 6 validator lenses.

### Session 4 — Checkpoint 2-b (landed)

**Persistence layer:**
- Added `OpenWorkshopRecord` DTO and `open_workshop_summary_from_session` helper in `crates/persistence/src/lib.rs`.
- Added `list_open_workshops()` trait method on `SessionStore`. Postgres impl filters `WHERE payload->>'phase' = 'lobby'` ORDER BY `updated_at` DESC LIMIT 100. InMemory impl filters `session.phase == Phase::Lobby`, sorts newest-first, truncates to 100.
- Added `tracing.workspace = true` to `crates/persistence/Cargo.toml`.
- FaultyStore delegate added in `app-server/src/tests.rs`.

**New handlers in `http.rs`:**
- `create_character` (POST /api/characters) — enforces `MAX_CHARACTERS_PER_ACCOUNT` (5), sets `owner_account_id`.
- `list_my_characters` (GET /api/characters/mine) — returns `MyCharactersResponse`.
- `delete_character` (DELETE /api/characters/{id}) — 204 on success, 404 on not-found/not-owned.
- `list_open_workshops` (GET /api/workshops/open) — requires `AccountSession`.
- `eligible_characters` (GET /api/workshops/{code}/eligible-characters) — returns account's owned characters.

**New handler in `auth.rs`:**
- `accounts_me` (GET /api/accounts/me) — returns `AccountProfile`.

**Other changes:**
- `list_character_catalog` now filters to unowned characters only (`.filter(|r| r.owner_account_id.is_none())`).
- `AuthRejection::UnknownAccount` now carries `{ key, is_production }` and clears the stale cookie in `IntoResponse`.
- `account_from_record` made `pub(crate)` for use by `http.rs`.
- Routes wired in `app.rs` using axum 0.8 `{capture}` syntax.

**Tests:**
- 10 new tests: accounts_me (authenticated + unauthenticated), tampered cookie, character CRUD (create, enforce limit, list, delete, delete-404), open workshops list, eligible characters.

### Session 4 — Checkpoint 2-c (landed)

**Refactored `create_workshop`:**
- Takes `AccountSession` extractor. Derives `normalized_name` from `session.account.name` (ignores payload.name per locked decision #9). Sets `account_id: Some(session.account.id)`. Uses `resolve_character_for_session` with starter-lease logic. Returns 201 (CREATED) instead of 200.

**Refactored `join_workshop` (new-join branch):**
- Uses `extract_account_from_headers` helper for manual cookie extraction (axum 0.8 Handler trait limitation prevents `Option<AccountSession>` + 4 extractors). Derives name from account. Uses `resolve_character_for_session`. Sets `account_id: Some(...)`. Returns 401 (not 400) for unauthenticated new-join attempts. Reconnect branch untouched.

**New helpers:**
- `resolve_character_for_session(state, account_id, requested_character_id)`: explicit ID → load + ownership check; 0 owned chars → lease starter; has chars but no ID → error.
- `pick_random_starter_profile`: filters `owner_account_id.is_none()`, random pick.
- `extract_account_from_headers`: manual `SignedCookieJar::from_headers` cookie parsing.

**Protocol change:**
- `CreateWorkshopRequest.name` changed from `String` to `Option<String>` (server ignores it).

**~79 existing tests updated** to include auth cookies when calling workshop endpoints.

### Session 4 — Post-C2-b/C2-c Validator Pass

**10 independent validators run:** requirements-fulfillment, requirements-drift, correctness, architect, simplicity, testing, performance, security, API contract, operations.

**Must-fix findings addressed:**
1. **IDOR in `resolve_character_for_session`** (security, correctness, drift) — added ownership check: loaded character must have `owner_account_id == Some(account_id)` OR be a starter (`owner_account_id.is_none()`). Rejects with "you do not own this character" otherwise.
2. **IDOR in `SelectCharacter` command** (security, correctness) — added ownership check: loads raw `CharacterRecord`, verifies player's `account_id` matches `owner_account_id` or character is a starter. Rejects with "You do not own this character." otherwise.
3. **`POST /api/workshops` returned 200 instead of 201** (API contract) — changed to `StatusCode::CREATED`. Updated 5 test assertions.
4. **Unauthenticated join returned 400 instead of 401** (API contract) — changed to `StatusCode::UNAUTHORIZED` with structured `WorkshopJoinResult::Error` body.
5. **`CreateWorkshopRequest.name` required but ignored** (API contract) — made `Option<String>` with `#[serde(default)]`. Updated xtask and app-web usages.
6. **Dead code removed** (simplicity) — deleted `character_profile_from_record`, `pick_random_character_profile`, `load_or_pick_character_profile`. Replaced call sites with `CharacterRecord::profile()` / `r.profile()`.
7. **9 new auth-401 tests** (testing) — added by testing validator for unauthenticated access to all new endpoints + character validation + ownership check on delete.

**Accepted/deferred findings:**
- TOCTOU race on character count (low risk at scale, defer to C2-d or DB constraint).
- Domain logic in controller layer, fat SessionStore trait, persistence-to-domain coupling (architectural, defer to future refactor).
- JSONB index on `payload->>'phase'` (scale not there yet, noted for growth).
- Error envelope inconsistency between auth/character and workshop families (cosmetic, defer).
- `.len()` vs `.chars().count()` on signin length bounds (carried forward, not yet fixed).

**Final state:** `cargo check --workspace --all-targets` green (6 warnings). 156 non-docker tests pass, 4 docker-only failures (expected).

### Session 4 — Orchestrator Completeness Audit

**6 targeted validators run** comparing `refactor.md` spec against actual implementation:

**V1 (Backend completeness) — 5 silently dropped items proactively fixed:**
1. Same-account dedup in `join_workshop` → 409 (was silently dropped; added account_id check in `http.rs`).
2. Cookie 14-day Max-Age (was missing; added `set_max_age(time::Duration::days(14))` to `build_session_cookie`; added `time` dep).
3. Auth rate limits — signup 5/min/IP, login 10/min/IP (was missing; added `signup_limiter`/`login_limiter` to AppConfig/AppState, wired IP extraction).
4. `phase_duration_minutes` returns 0 for Phase0 (was still returning config value; fixed in `domain/src/lib.rs`).
5. Deleted `default_pet_description` dead code (no callers remaining).

**V2 (API surface compliance):**
- All 10 spec endpoints implemented. 5 path deviations documented as intentional (e.g. `/api/auth` → `/api/auth/signin`).
- Extra 409 on signin race (not in spec but defensible).

**V3 (Phase0 removal):**
- FSM edges, StartPhase0, begin_phase1 strict — all DONE.
- `/api/llm/images` still gated to Phase0 = permanently dead code (deferred to C2-d decision).
- `enter_phase0` no-op stub called ~25 times in domain tests (tech debt, deferred).
- `WorkshopCreateConfig.phase0_minutes` vestigial field kept for wire compat.

**V4 (Frontend gap analysis):**
- Items 10-19 from build order: ALL NOT DONE. Entire frontend pass deferred to next conversation.
- Need 4 new components, 6 major rewrites, 4 deletions. ~1200-1800 lines new/changed.

**V5 (Deferred/dropped items):**
- Same-account dedup, signin rate limits, cookie 14-day — all now fixed by V1.
- WorkshopBrief deletion grouped under frontend pass.

**V6 (Test coverage gaps):**
- Missing: postgres account round-trip, owner column persistence, same-account dedup 409, starter-lease, MissingSelectedCharacter guard, auth+workshop e2e, enter_phase0 stub cleanup.

**Verified:** `cargo check` green + 156 non-docker app-server tests pass + 137 domain tests pass after V1 fixes.

