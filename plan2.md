# Plan 2 — Readiness Remediation

Source: consolidated must-fix findings from 10-validator readiness audit against `refactor.md` and `refactor-plan.md`. Ordered by blocking severity. Items are independently landable.

---

## 1. Duplicate-name signup error surfaces a clear "choose a different name" message

**Spec:** `refactor.md:50` — "If the account name already exists, return a clear message that the player must choose a different name."

**Current state:**
- `platform/app-server/src/auth.rs:270-286` — existing name + correct password → 200 login; existing name + wrong password → 401 `"invalid credentials"`.
- `platform/app-server/src/auth.rs:331-339` — `DuplicateAccountName` 409 reachable only via insert-race.
- No branch delivers the "name taken, pick another" message the spec requires.
- Frontend `NoticeBar` shows only generic 401 text.

**Target behavior:**
- Keep combined `/api/auth/signin` create-or-login endpoint (per locked plan §3).
- 401 response on wrong-password for existing name MUST include a structured body distinguishing "account exists, wrong password" from "account does not exist" sufficient for the client to render "This name is taken — enter the correct password or choose a different name."
- Frontend SignIn screen MUST render that message on the 401-for-existing-name case.

**Acceptance criteria:**
- `POST /api/auth/signin` with existing name + wrong password returns 401 with body `{ "error": "name_taken_wrong_password", "message": "That name is already registered. Enter the correct password or choose a different name." }` (or equivalent structured payload).
- `POST /api/auth/signin` with free name + valid password returns 201 as today.
- Timing-dummy argon2 verify on the unknown-name branch stays in place (security requirement, do not regress).
- Frontend `components/sign_in.rs` maps `name_taken_wrong_password` to the required copy in `NoticeBar`.
- Unit test: signin with existing name and wrong password returns the new error code.
- Unit test: signin with unknown name and any password returns a distinct generic error (so enumeration surface is unchanged from today — the new copy only appears after argon2 verify fails, which is the same branch that already distinguishes).

**Files expected to change:**
- `platform/app-server/src/auth.rs`
- `platform/crates/protocol/src/lib.rs` (if auth error payload is shared)
- `platform/app-web/src/components/sign_in.rs`
- `platform/app-web/src/flows.rs` or `api.rs` (error parsing)
- Test file under `platform/app-server/src/tests.rs` or equivalent

**Non-goals:**
- Do NOT split the endpoint into separate signup/signin paths.
- Do NOT add rate-limit or lockout changes in this item.
- Do NOT change cookie/session semantics.

---

## 2. CreateCharacter screen wires real sprite generation (no placeholder empty sprites)

**Spec:** `refactor.md:38-41` — "Character creation must be a separate capability outside the workshop flow" with the same character entity as before. `refactor-plan.md:45, 97` locks "CreateCharacter owns the description + sprite-generation UI currently in `phase0_view.rs`" and "Move sprite editor out of phase0_view.rs into create_character.rs".

**Current state:**
- `platform/app-web/src/components/create_character.rs:56-63` sends `SpriteSet { neutral: "", happy: "", angry: "", sleepy: "" }` with a comment acknowledging the placeholder.
- Backend endpoint `POST /api/characters/sprite-sheet` exists (`app-server/src/app.rs:232`) but is **workshop-scoped**: it requires `session_code` + `reconnect_token`, persists a `CharacterRecord`, mutates the session, and broadcasts. It cannot be called from account-scoped CreateCharacter.
- `phase0_view.rs` has already been deleted; Phase0 is unreachable in routing. No shared component to extract — the old sprite-generation flow must be reimplemented fresh in `create_character.rs` (CSS classes can be reused).

**Design correction (approved):** add a new account-scoped preview route rather than reshape the existing workshop-scoped one.

**Target behavior:**
- Backend adds `POST /api/characters/preview-sprites` which:
  - Requires a valid account session cookie (reuse `AccountSession` extractor).
  - Accepts `{ description: String }`.
  - Reuses the existing `generate_sprite_sheet_with_queue` helper (or whatever the current sprite-generation producer is).
  - Returns `{ sprites: SpriteSet }`.
  - Does NOT persist `CharacterRecord`, does NOT mutate any session, does NOT broadcast.
  - Is rate-limited on the same per-account queue as existing sprite generation.
- Frontend `CreateCharacter` flow:
  1. Player enters description.
  2. Frontend calls `/api/characters/preview-sprites`.
  3. Generated sprites render in the UI.
  4. Player can regenerate (new call) or confirm.
  5. On confirm, `POST /api/characters` is called with the populated `SpriteSet`.
- On sprite-generation failure: render a retryable error; save button disabled; never submit empty sprites.
- Visual style reuses the existing phase0 CSS classes (`phase0-card`, `phase0-textarea`, `phase0-generation-bar`, `phase0-action-button`, `phase0-sprite-frame`, `sprite-grid`, etc.).

**Acceptance criteria:**
- `create_character.rs` never constructs `SpriteSet` with empty strings. Verify via grep.
- Submit button is disabled until all four sprite URLs are populated.
- Error state for sprite-generation failure is rendered; submit remains disabled.
- New backend route `POST /api/characters/preview-sprites` exists, is cookie-gated via `AccountSession`, returns 401 without a valid session, and does not persist anything.
- Rate limiter on preview route shares the existing per-account sprite-generation quota (or adds a new one with explicit justification).
- Unit test: preview-sprites route returns 401 without cookie.
- Unit test: preview-sprites route returns sprites with a valid cookie (use the in-memory store/fake queue already used by other sprite tests).
- Unit test: preview-sprites does NOT create a `CharacterRecord` (assert record count unchanged).

**Files expected to change:**
- `platform/app-server/src/app.rs` (route registration)
- `platform/app-server/src/http.rs` (new handler; reuse generate_sprite_sheet_with_queue)
- `platform/crates/protocol/src/lib.rs` (new request/response types)
- `platform/app-web/src/components/create_character.rs` (UI + state)
- `platform/app-web/src/api.rs` (new client call)
- `platform/app-web/src/flows.rs` (wiring if needed)
- `platform/app-server/src/tests.rs` (new tests)

**Non-goals:**
- Do NOT redesign the sprite-generation UX; port existing look-and-feel.
- Do NOT modify the existing `POST /api/characters/sprite-sheet` request/response shape or behavior.
- Do NOT delete `/api/workshops/sprite-sheet` in this item (separate cleanup).
- Do NOT touch backend character persistence (`CharacterRecord` create/read unchanged).
- Do NOT change cookie/session semantics.

---

## Decision log

### Item 1 — Duplicate-name signup error

**Status:** COMPLETED (consensus 9/10)

- Round A: 4/5 READY (architecture flagged constant placement).
- Fix: moved `AUTH_ERR_NAME_TAKEN_WRONG_PASSWORD` into `platform/crates/protocol/src/lib.rs:23`.
- Round B: 5/5 READY.
- Backend: `auth.rs` helper `name_taken_wrong_password()` (373-388), routed from `Ok(false)` branch (284).
- Frontend: `map_signin_error` in `sign_in.rs` + NoticeBar mapping in `flows.rs`.
- Tests: 2 new unit tests in `tests.rs` (~10134-10231). Timing-dummy argon2 preserved on unknown-name branch.

### Item 2 — Account-scoped sprite preview route

**Status:** COMPLETED (consensus 8/10)

- Round A: 3/5 READY. Must-fix findings:
  - **Correctness (Medium):** stale-sprite save bug — textarea `oninput` did not invalidate `generated_sprites` on description edit, allowing submission of mismatched sprites.
  - **Architecture (High):** DRY violation — image-queue admission cascade (`try_acquire_owned / NoPermits / wait_for_image_job_turn / Closed`) duplicated across 3 sites in `http.rs`.
- Round B: 5/5 READY after fixes.

**Fix 1 (stale-sprite):** added `last_generated_for: Signal<Option<String>>` in `create_character.rs` (line ~70); success handler sets it to trimmed desc alongside `generated_sprites`; `oninput` invalidates both signals + resets status to Idle when `event.value().trim() != prev`. Trim-equivalence preserved (whitespace-only edits keep preview).

**Fix 2 (DRY Step A):** new helper `acquire_image_job_permit(state, on_queued)` + `ImageQueueAdmissionOutcome` enum in `http.rs` (~87-109). Consolidates admission ladder into one place. 3 call sites refactored: workshop sprite-sheet (~148), workshop single-image (~2326), account preview (~3028). `on_queued` closure lets workshop sprite-sheet emit `SpriteAtelierQueued` notice at the right moment; other sites pass `|| async {}`. Each caller maps `TimedOut` / `Unavailable` to its own response contract (HTTP status, notice level, fallback body).

**Fix 2 (DRY Step B):** intentionally SKIPPED with inline comment at account-preview call site. Sprite-generate-or-fallback block (~10 lines) differs between workshop (sends `Warning` notice via `sprite_sheet_fallback_with_notice`) and account preview (plain fallback clone). Factoring would either re-introduce a branch or leave two near-identical wrappers — correctly deferred under KISS.

**Artifacts:**
- Backend: `http.rs` handler (2951-3029), route registration (`app.rs:222`).
- Protocol: `CharacterSpritePreviewRequest/Response` (`protocol/src/lib.rs:993-1013`).
- Frontend: full rewrite of `create_character.rs`, new `preview_character_sprites` method in `api.rs`.
- Tests: 5 new unit tests in `tests.rs` (~11354-11557) — account-cookie required, sprites returned, no `CharacterRecord` created, empty-description rejected, 429 on rate-limit.

**Residual risks (accepted, not blocking):**
- No frontend component test for stale-sprite invalidation (would require Dioxus wasm test harness; not in plan2.md scope).
- No test locks in `SpriteAtelierQueued` notice emission via the new closure path (refactor preserved behavior; assumption verified by inspection).
- Shared `character_create_limiter` (20/hr) means heavy previewing can exhaust save budget within the hour. Plan2.md:62 explicitly endorsed the shared quota.
- Pre-existing red tree outside item 2 scope: `cargo fmt --check` drift in ~13 files, `cargo clippy` debt in `crates/domain`, `app-web` wasm build break on `web_sys::RequestInit::set_credentials`, 4 docker-dependent tests skipped in CI-less env. Confirmed pre-existing via stash/retry; tracked for future passes (not in plan2.md items 3-20 as written, but some overlap with item 16).

---

## Deferred items (NOT in this pass)

Tracked for future passes. Each remains unresolved:

3. Starter-lease uniqueness inside a workshop (`http.rs:251-266`).
4. WS handler `cookie.account_id == player.account_id` assertion (`ws.rs:195-214`).
5. Remove frontend `tags.len() != 3` check (`flows.rs:177`).
6. Remove frontend hardcoded phase minutes (`flows.rs:344-348`).
7. Strip character roster + Delete UI from AccountHome (Block 2 scope).
8. Split Voting/Judge/End screens (wire `voting_view.rs` or document merge).
9. Workshop list cap 50 + Postgres order by `created_at`.
10. Character-limit 409 → 400; "workshop already started" 400 → 409.
11. Endpoint path reconciliation with spec or plan §3 table update.
12. Remove `/workshops/sprite-sheet` and `/llm/images` dead routes.
13. Drop `hero` field from AuthRequest/accounts (or justify in plan).
14. Origin check on `/api/auth/*`.
15. Charset validation on `name`.
16. Missing unit/integration tests (starter lease, list endpoint, Postgres accounts, MissingSelectedCharacter HTTP, case-insensitive signup collision, rate limits, IDOR, Phase0 serde compat).
17. E2E rewrite (new helpers, delete phase0 helpers, new specs).
18. Rollback SQL for migration 0007.
19. Logging for rate-limit hits and join failures.
20. `WorkshopCreateConfig.phase0_minutes` `#[serde(default)]`.

---

## Execution model

- One implementer pass per item, landed in sequence (1 then 2).
- After each implementer pass, run 5 validator lenses (dynamic selection): plan conformance, completeness, correctness, architecture, security. Additional lenses (testing, contract, simplicity, operations) added when the changed artifacts trigger them.
- Consensus gate: 8 of 10 READY votes across two full-validator rounds (or equivalent within the selected lenses set). Loop until gate passes per item.
- Each validator runs in a fresh subagent session. Implementer in a separate session.
- Decision log recorded per item.
