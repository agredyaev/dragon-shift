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

### Item 3 — Starter-lease uniqueness inside a workshop

**Status:** COMPLETED (consensus 12/14, gate ≥8/14)

- Round A (7 lenses): 5 READY / 2 NOT READY.
  - **Correctness (Medium):** `resolve_character_for_session` explicit-characterId branch (`http.rs:249-269`) ignored `excluded_character_ids`, letting a client pass `{characterId: X}` with X already leased to another seated player.
  - **Security (High):** same root cause escalated — attacker observes victim starter id from `GameState` broadcast, replays it via explicit-id join to duplicate the seat during voting/judging, biasing outcomes.
  - Architect, Drift, Completeness, Testing, Simplicity: READY.
- Fix: 12-line guard added inside the explicit branch of `resolve_character_for_session` (`http.rs:262-276`): if `is_starter && excluded_character_ids.contains(character_id)` → `Err("that starter is already taken in this workshop")` → mapped to HTTP 400. Owned-by-requester path untouched (owned chars cannot collide by construction).
- Round B (7 lenses): 7 READY / 0 NOT READY.

**Artifacts:**
- Backend: `http.rs` helpers (241-314), `create_workshop` passes `&BTreeSet::new()` (738-744), `join_workshop` builds exclusion set under `state.sessions.lock()` and handles `Ok(None)` → HTTP 400 `"no starter available"` (994-1065).
- Shape chosen: `excluded_character_ids: &BTreeSet<String>` threaded as parameter (keeps lock discipline caller-side).
- Tests: 3 new unit tests in `tests.rs` (~11606-11765):
  - `join_workshop_assigns_distinct_starters_to_two_accounts`
  - `join_workshop_returns_error_when_all_starters_leased_in_session`
  - `join_workshop_rejects_explicit_starter_already_leased` (closes Round A bypass).

**Residual risks (accepted, not blocking):**
- **TOCTOU** between exclusion snapshot and write lease (`http.rs:1001-1017`) — two concurrent new-join requests could both observe starter X free and both seat. Documented in-code; requires racing window; impact limited to accidental duplication (not targeted impersonation, since pre-seat). Fix would require taking write lease before snapshot — out of scope for item 3.
- Tri-state `Result<Option<CharacterProfile>, String>` + re-parse of `requested_character_id` at `http.rs:1058-1062` is mildly overloaded (readability smell, not correctness risk). Simplicity lens kept READY. Defer to future consolidation pass.
- Non-existent `characterId` silently seats player with no pet (pre-existing behavior at `http.rs:279 → 1075`, unchanged by this item).
- Exhaustion test asserts status-only across fills 2-4, not mid-pool distinctness (Testing Low, implicitly covered by distinct-assignment test).

**Committed in:** `feat(platform): land plan2 item 3 starter uniqueness` (baseline `390760e` + Round A fix).

### Item 4 — Bind WS identity to authenticated cookie

**Status:** COMPLETED (consensus 13/14, gate ≥8/14)

- Round A (7 lenses): 6 READY / 1 NOT READY.
  - **Correctness (Medium):** hard-close missing — on `attach_ws_session` Err, code sent `ServerWsMessage::Error` frame but left socket open for retry. Must-fix: queue `WsOutbound::Close` + `continue` after Error frame, mirroring retired-connection path at `ws.rs:316-326`.
  - Drift, Completeness, Architect, Security, Testing, Simplicity: READY.
- Fix: in `handle_workshop_ws` Err arm (`ws.rs:337-356`), after `send_ws_message` of Error frame, queue `outbound_tx.send(WsOutbound::Close)` and `continue` — mirrors retired-connection path (:316-326) exactly. All attach failures now terminal; `WsOutbound::Close` breaks select at `ws.rs:252`. Two reject tests (`ws_attach_rejects_account_mismatch_cookie`, `ws_attach_rejects_missing_cookie_for_account_owned_player`) updated to assert `socket.next()` after Error yields `None | Close | Err` (regression lock on hard-close).
- Round B (7 lenses): 7 READY / 0 NOT READY.

**Artifacts:**
- Backend: `ws.rs` upgrade handler extracts `cookie_account_id: Option<String>` from `SignedCookieJar` (`:205-224`, rejecting tampered cookies at `:218-220` with 401 pre-upgrade). Threaded via `handle_workshop_ws` (`:231-237`, call site `:330`) into `attach_ws_session` (signature at `:716-723`, check block `:772-812`). Check runs inside same `state.sessions.lock()` as `player.is_connected` mutation (no TOCTOU). `tracing::warn!` on rejection logs `session_code`, `player_id`, `expected_account_id`, `observed_account` (literal `"mismatch"`/`"none"`, no cookie bytes).
- Legacy anonymous bypass: `if let Some(expected_account_id) = player.account_id.as_deref()` — players with `account_id: None` skip check, preserving pre-auth fixture flow.
- Tests: 4 new tests in `tests.rs` (~1595-1770): `ws_attach_rejects_account_mismatch_cookie`, `ws_attach_rejects_missing_cookie_for_account_owned_player`, `ws_attach_allows_anonymous_player_without_cookie` (with fixture precondition assert `account_id.is_none()`), `ws_attach_accepts_matching_account_cookie`. 2 new helpers: `ws_request_with_cookie`, `connect_raw_ws_with_cookie`. 11 pre-existing WS tests updated to thread owner cookie through the new `_with_cookie` helpers (mechanical fix — account-owned players created via `test_auth_cookie` now require matching cookie on WS upgrade).
- Full suite `cargo test -p app-server`: 177/177 pass.

**Residual risks (accepted, not blocking):**
- **Architect (Medium, deferred):** WS cookie extraction (`ws.rs:205-224`) re-implements half of `AccountSession::from_request_parts` (`auth.rs:114-150`) but intentionally skips `find_account_by_id`. Consequence: a signed cookie for a *deleted* account passes the WS attach if `player.account_id` still matches, while the HTTP path would reject. Consolidate into a shared `signed_cookie_account_id` helper before the next auth-touching item.
- **Testing Medium (pre-existing gap, not regression):** no test covers the tampered-cookie 401 branch on the WS upgrade route (`ws.rs:218-220`). `signin_rejects_tampered_cookie` covers HTTP; WS equivalent missing. Not item 4 scope.
- **Testing Low:** hard-close assertions use `socket.next().await` without a timeout — a regression leaving the socket open would stall tests until runtime cancels rather than failing fast. Consider wrapping in `tokio::time::timeout`.
- **Residual attack vector:** stolen reconnect_token + stolen signed cookie → still accepted by design. Cookie is HttpOnly + Signed + SameSite=Lax, raising the bar. Documented.
- **Log hygiene:** `expected_account_id` (raw account UUID) logged on mismatch — acceptable operational data, not a credential. Attacker-controlled account ids are collapsed to `"mismatch"` literal to avoid log poisoning.

### Item 5 — Enforce "exactly 3 handover tags" invariant in domain

**Status:** COMPLETED (consensus 13/14, gate ≥8/14)

- Round A (7 lenses): 6 READY / 1 NOT READY.
  - **Architecture (High):** invariant scattered across 3 layers — `== 3` in `http.rs` SubmitTags arm, `.take(3)` silent truncation inside domain `save_handover_tags`, `< 3` duplicated in `enter_phase2`. No single source of truth.
  - Drift, Completeness, Correctness, Security, Testing, Simplicity: READY.
- Path decision (user): **Path A — DDD-aligned fallible domain method** (over Path B thin-fix).
- Fix: consolidate via a const + fallible domain method; strip frontend pre-check.
- Round B (7 lenses): 7 READY / 0 NOT READY.

**Artifacts:**
- Domain (`crates/domain/src/lib.rs`): `pub const HANDOVER_TAG_COUNT: usize = 3` (`:11-15`), new `DomainError::InvalidHandoverTagCount { expected, got }` variant (`:200-201`), `save_handover_tags` rewritten as `Result<(), DomainError>` — count check before any player/dragon lookup, `.take(3)` truncation removed (`:387-410`). `enter_phase2` preconditions reference the const (`:422`, `:434`). `fallback_handover_tags` gains `debug_assert_eq!` guard (`:1234-1242`).
- HTTP (`app-server/src/http.rs:1590-1603`): `SubmitTags` arm pattern-matches `InvalidHandoverTagCount { expected, got }` → `bad_command_request(format!("Exactly {expected} handover notes are required (got {got})."))`. Mirrors `StartPhase2 → MissingHandoverTags` peer at `:1627-1633` exactly. Copy changed from "rules" to "notes".
- Frontend (`app-web/src/flows.rs:174-178`): 4-line `tags.len() != 3` pre-check deleted — frontend now purely submits, server is sole validator. Removes refactor.md line 20 violation ("Do not place business rules in frontend").
- Tests:
  - Repurposed `validator12_handover_tags_truncated_to_three` → `validator12_handover_tags_rejects_wrong_count` (domain `:4471-4495`): 5-tag input → `Err(InvalidHandoverTagCount)`, asserts `handover_tags.len() == 0` (no partial mutation).
  - `validator12_handover_tags_ghost_player_noop`: payload bumped 1→3 tags; Ok(()) ghost path coverage preserved.
  - `workshop_command_rejects_submit_tags_with_wrong_count` (`app-server/src/tests.rs:6235-6293`): asserts 400 + exact copy `"Exactly 3"` + `"handover notes"` for both 2-tag and 4-tag sub-cases.
  - Mechanical: ~30 existing callers suffixed with `.expect("save handover tags")` — all statically 3-element, all inside `#[cfg(test)]` modules (audited: zero production call sites).
- Suites: `cargo test -p domain` 137/137, `cargo test -p app-server` 178/178.
- Diff stat: 4 files, +220 / -56.

**Residual risks (accepted, not blocking):**
- **Completeness (Low):** literal `3` survives in UI display copy (`app-web/src/helpers.rs:514,521,524` — "{n} / 3 handover rules saved", countdown hint), `app-web/src/components/handover_view.rs:64` ("Provide exactly 3 key rules" static label), `xtask/src/main.rs:1208` (judge-bundle observed-invariant smoke check), and `e2e/tests/restart-reconnect.spec.ts:109,132`. These are cosmetic / observation, not enforcement; a future `HANDOVER_TAG_COUNT` bump would need coordinated copy updates. Explicitly out of Item 5 scope (domain/HTTP boundary).
- **Testing (Low):** no 0-tag (empty payload) sub-case — covered by the general `!= 3` branch via 2/4/5-tag tests. No independent test for "ghost player + wrong count" — count check precedes ghost lookup in impl, so covered by `validator12_handover_tags_rejects_wrong_count` transitively. No integration test asserting frontend pre-check was removed — acceptable, since server rejection is authoritatively tested and removal is pure deletion.
- **Simplicity (Low):** `workshop_command_rejects_submit_tags_with_wrong_count` duplicates ~60 lines of create/seed/startPhase1/startHandover boilerplate from the sibling success test. Helper `seed_handover_ready()` could halve it — orthogonal cleanup, defer.
- **Simplicity (Low):** `debug_assert_eq!` in `fallback_handover_tags` is arguably noise (the preceding `vec![...]` literal is self-evidently length 3), but guards future edits without runtime cost.

**Committed in:** `feat(platform): close plan2 item 5 enforce handover-tag count in domain` (applied on top of `cd0574e`).

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
