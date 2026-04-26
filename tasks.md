# App Web Performance Tasks

Source audit: `front-check.md`

Execution rule for each implementation task: implement the smallest safe change, run focused verification, then run 5 independent validator lenses before moving to the next task.

Validator lenses per task:
- Requirements: checks the task matches `front-check.md` and this file.
- Correctness: checks edge cases and regressions.
- Performance: checks the change reduces the targeted bottleneck without adding new hot-path work.
- Functional Safety: checks gameplay/account behavior is preserved.
- UX/A11y or Network: selected by task area.

## Backlog

### T01. Guard Realtime Socket Generation And Disconnect On Exit

Status: Completed

Goal: prevent stale WebSocket callbacks or messages from mutating state after reconnect, leave, or logout.

Scope:
- `platform/app-web/src/realtime.rs`
- `platform/app-web/src/flows.rs`

Acceptance checks:
- Installing a new realtime socket invalidates callbacks from the previous socket.
- `leave_workshop` closes and clears the active realtime socket before local session state is cleared.
- `submit_logout_flow` closes and clears the active realtime socket before account/session state is cleared.
- Non-wasm builds still compile.

### T02. Add Fetch Timeout/Abort For WASM Requests

Status: Completed

Goal: prevent hung browser fetches from leaving UI permanently pending.

Scope:
- `platform/app-web/src/api.rs`
- `platform/app-web/Cargo.toml`

Acceptance checks:
- WASM fetch uses timeout/abort through one shared path.
- Existing backend error extraction is preserved.
- Long sprite-generation requests get a safe timeout budget.
- Non-WASM reqwest paths remain unchanged.

### T03. Sequence AccountHome Workshop Loads

Status: Completed

Goal: prevent stale poll responses from overwriting newer pager/create/delete refreshes.

Scope:
- `platform/app-web/src/components/account_home.rs`
- `platform/app-web/src/flows.rs`
- `platform/app-web/src/state.rs`

Acceptance checks:
- Poll, initial load, Prev, Next, create refresh, and delete fallback cannot overwrite newer paging state with stale data.
- Current `Before`/`After` cursor semantics are preserved.
- Polling remains active while AccountHome is mounted.

### T04. Add Explicit Loading States For Pre-Session Lists

Status: Completed

Goal: remove false empty states in AccountHome, PickCharacter, and Manage Dragons.

Scope:
- `platform/app-web/src/state.rs`
- `platform/app-web/src/components/account_home.rs`
- `platform/app-web/src/components/pick_character.rs`
- `platform/app-web/src/components/app_bar.rs`
- `platform/app-web/src/flows.rs`

Acceptance checks:
- Open workshops show loading before the first response.
- PickCharacter does not show random starter until the server confirms no eligible characters.
- Manage Dragons does not show stale empty copy while a fresh load is in flight.

### T05. Add Synchronous Duplicate-Submit Guards

Status: Completed

Goal: prevent double-click/tap from spawning duplicate async work before pending UI rerenders.

Scope:
- `platform/app-web/src/flows.rs`
- `platform/app-web/src/components/sign_in.rs`
- `platform/app-web/src/components/account_home.rs`
- `platform/app-web/src/components/pick_character.rs`
- `platform/app-web/src/components/phase1_view.rs`
- `platform/app-web/src/components/phase2_view.rs`
- `platform/app-web/src/components/app_bar.rs`

Acceptance checks:
- Sign-in/create/join/action/save/delete/rename/logout single-flight behavior is enforced.
- Retry after failure still works.
- Server-side permissions and cooldowns remain final authority.

### T06. Narrow Realtime Signal Mutations By Message Type

Status: Pending

Goal: avoid mutating unrelated signals for lightweight realtime messages.

Scope:
- `platform/app-web/src/realtime.rs`
- `platform/app-web/src/state.rs`

Acceptance checks:
- `Pong` updates only connection status.
- Notices/errors avoid unnecessary `game_state` writes.
- `StateUpdate` still applies atomically enough to preserve phase transitions.

### T07. Reduce Root Hot `game_state` Reads

Status: Pending

Goal: reduce broad root rerenders caused by clock/time/gameplay updates.

Scope:
- `platform/app-web/src/main.rs`

Acceptance checks:
- Phase routing still updates immediately.
- Clock/day-night state still updates in Phase1/Phase2.
- Pre-session screens are not invalidated by gameplay-only timer updates.

### T08. Add Sprite Image Attributes And Avoid Unneeded Lazy Loads

Status: Pending

Goal: reduce layout/decode cost for sprite-heavy rows with minimal behavior risk.

Scope:
- `platform/app-web/src/components/app_bar.rs`
- `platform/app-web/src/components/pick_character.rs`
- `platform/app-web/src/components/create_character.rs`
- `platform/app-web/src/components/end_view.rs`
- `platform/app-web/src/components/voting_view.rs`
- `platform/app-web/src/components/phase1_view.rs`
- `platform/app-web/src/components/phase2_view.rs`

Acceptance checks:
- Sprite images have explicit dimensions and async decode where supported.
- Lazy loading is used only for offscreen/list sprites, not critical current-dragon sprites.
- Alt text and all four emotion mappings are preserved.

### T09. Memoize Or Reuse Sprite URL Strings

Status: Pending

Goal: reduce repeated `data:image/png;base64,...` formatting and full `SpriteSet` cloning.

Scope:
- `platform/app-web/src/helpers.rs`
- `platform/app-web/src/components/*`

Acceptance checks:
- Sprite URL cache invalidates when sprite content changes.
- Voting rows do not clone full sprite sets unnecessarily.
- Create-character regeneration still shows new sprites.

### T10. Lazy-Compute End/Voting Rows By Visible Branch

Status: Pending

Goal: avoid computing hidden tab and overlay rows on every End/Voting render.

Scope:
- `platform/app-web/src/components/end_view.rs`
- `platform/app-web/src/helpers.rs`

Acceptance checks:
- Voting, score, design, and game-over branches compute only visible data.
- Existing sort order and tie-breakers are preserved.
- Vote reveal and host controls remain unchanged.

### T11. Precompute Score Summaries In One Pass

Status: Pending

Goal: replace per-player dragon scans with one score-summary pass.

Scope:
- `platform/app-web/src/helpers.rs`

Acceptance checks:
- Phase1/Phase2 score values and judge status labels remain identical.
- Existing helper tests still pass.
- Complexity is no longer O(players × dragons) for score-row derivation.

### T12. Reduce Phase1/Phase2 Stat DOM

Status: Pending

Goal: replace 60 segmented DOM nodes with cheaper pixel-equivalent bars.

Scope:
- `platform/app-web/src/components/phase1_view.rs`
- `platform/app-web/src/components/phase2_view.rs`
- `platform/app-web/static/style.css`

Acceptance checks:
- Numeric stat values are unchanged.
- Pixel visual style is preserved.
- Action availability and cooldown behavior are unchanged.

### T13. Remove Repeated Current Player/Dragon Derivation

Status: Pending

Goal: resolve current player/dragon once per Phase1/Phase2 render and avoid unnecessary clones.

Scope:
- `platform/app-web/src/components/phase1_view.rs`
- `platform/app-web/src/components/phase2_view.rs`

Acceptance checks:
- Missing-player/dragon fallback copy is preserved.
- Rendered observations, achievements, and handover notes keep order.
- No signal read guard is held across mutable updates.

### T14. Gate Heavy CSS Motion And Complete Reduced Motion

Status: Pending

Goal: reduce default paint/compositing work without removing visual meaning.

Scope:
- `platform/app-web/static/style.css`

Acceptance checks:
- Full-screen background/CRT motion is reduced on mobile and reduced-motion contexts.
- Crown bounce, action transitions, handover-note transforms, and stat transitions are covered by reduced motion.
- Decorative overlays keep `pointer-events: none`.

### T15. Replace Expensive CSS Filters On Hot Controls

Status: Pending

Goal: remove interaction-path `filter` usage from buttons/action buttons and reduce dragon filter cost.

Scope:
- `platform/app-web/static/style.css`

Acceptance checks:
- Hover/active/disabled affordances remain visible.
- Dragon remains visually separated from the background.
- Reduced-motion behavior is preserved.

### T16. Add WASM And Module Preload To Generated HTML

Status: Pending

Goal: discover JS/WASM earlier during startup.

Scope:
- `platform/xtask/src/main.rs`
- `platform/app-web/dist/index.html`

Acceptance checks:
- Generated preload URLs exactly match init URLs and cache tokens.
- App still boots with current generated loader.
- Build output is reproducible through xtask.

### T17. Move App-Web Assets Toward Content Hashes

Status: Pending

Goal: replace size/query-token cache busting with content-derived cache tokens or filenames.

Scope:
- `platform/xtask/src/main.rs`
- `platform/app-web/dist/index.html`

Acceptance checks:
- JS/WASM/CSS/font references are generated atomically.
- Same-size content changes produce different asset references.
- Rollback cannot reference missing assets.

### T18. Self-Host Runtime Icons Behind `poke_icon_url`

Status: Pending

Goal: remove runtime dependency on `raw.githubusercontent.com` for gameplay/result icons.

Scope:
- `platform/app-web/src/helpers.rs`
- `platform/app-web/static`
- `platform/app-web/dist`

Acceptance checks:
- Every current icon name maps to the same visual meaning.
- Existing `alt`, dimensions, and labels are preserved.
- UI remains usable if remote network is unavailable.

### T19. Fix Shell Scroll And Clock/App-Bar Overlap

Status: Pending

Goal: keep mobile content and timing feedback reachable.

Scope:
- `platform/app-web/static/style.css`
- `platform/app-web/src/main.rs`

Acceptance checks:
- All controls remain reachable at 320px and 390px widths.
- Clock/status does not cover app-bar/menu controls.
- Decorative horizontal overflow remains clipped.

### T20. Add Durable Input Labels And Native Submit Where Safe

Status: Pending

Goal: reduce keyboard/assistive-tech friction in forms.

Scope:
- `platform/app-web/src/components/sign_in.rs`
- `platform/app-web/src/components/create_character.rs`
- `platform/app-web/src/components/handover_view.rs`
- `platform/app-web/src/components/phase1_view.rs`

Acceptance checks:
- Inputs have visible or screen-reader-only names that persist after typing.
- Sign-in supports native submit without duplicate request risk.
- Multiline create-character textarea does not submit on Enter.

### T21. Add Dialog Focus Management

Status: Pending

Goal: make modal interactions fast and safe for keyboard users.

Scope:
- `platform/app-web/src/components/app_bar.rs`
- `platform/app-web/src/components/account_home.rs`

Acceptance checks:
- Initial focus moves into dialog on open.
- Tab stays within the dialog.
- Escape closes when no destructive operation is pending.
- Focus returns to the opener.

### T22. Add Action Focus Rings And Touch Target Audit Fixes

Status: Pending

Goal: make gameplay/account controls visibly focusable and easier to tap.

Scope:
- `platform/app-web/static/style.css`

Acceptance checks:
- `.action-btn:focus-visible` is visible and not clipped.
- App-bar menu controls meet mobile target sizing.
- Existing hover/active styles remain.

### T23. Add Leaderboard Mobile And Tooltip Access

Status: Pending

Goal: make score/judge details reachable on narrow screens and keyboard/touch.

Scope:
- `platform/app-web/static/style.css`
- `platform/app-web/src/components/end_view.rs`

Acceptance checks:
- Leaderboards do not clip important columns at 320px.
- Judge details are available without hover.
- Desktop grid alignment remains.

### T24. Centralize WASM JSON Response Handling

Status: Pending

Goal: reduce duplicated response parsing code and prepare for large-payload optimizations.

Scope:
- `platform/app-web/src/api.rs`

Acceptance checks:
- POST/GET/PATCH JSON success and backend-error behavior remain identical.
- Empty success responses remain supported.
- Non-WASM behavior remains aligned.

## Completed Cycles

- T01 completed.
- Implemented `disconnect_realtime()`, generation-guarded WebSocket callbacks, fail-closed bootstrap invalidation, and leave/logout disconnect calls.
- Verification passed: `cargo test -p app-web clear_session_identity -- --nocapture`; `cargo test -p app-web retained_flows_remain_linkable -- --nocapture`; `cargo test -p app-web reconnect_success_bootstraps_realtime -- --nocapture`; `cargo check -p app-web`; `cargo check -p app-web --target wasm32-unknown-unknown`.
- Validator cycle: requirements, correctness, performance, network safety, and regression evidence. Initial validators found failed-bootstrap stale-socket paths; implementation was corrected and final correctness/network reruns reported no findings.
- Residual risk: no browser-level test injects stale WebSocket callbacks after disconnect/reconnect; current coverage is static validation plus compile/native focused checks.
- T02 completed.
- Implemented shared WASM fetch timeout/abort path in `platform/app-web/src/api.rs`, enabled `AbortController`/`AbortSignal` features in `platform/app-web/Cargo.toml`, gave sprite preview a longer timeout budget, and softened timeout copy to note server-side completion may still happen.
- Verification passed: `cargo test -p app-web normalize_api_base_url -- --nocapture`; `cargo test -p app-web build_ws_url_maps_http_scheme_to_ws_endpoint -- --nocapture`; `cargo check -p app-web`; `cargo check -p app-web --target wasm32-unknown-unknown`.
- Validator cycle: requirements, correctness, performance, network safety, and regression evidence. No consensus blocker remained for the wasm timeout/abort implementation.
- Residual risk: no browser-level timeout runtime test yet; non-WASM timeout/error-parity remains a separate follow-up.
- T03 completed.
- Implemented request-generation sequencing for open workshops in `platform/app-web/src/state.rs` and `platform/app-web/src/flows.rs`, skipped poll during create/delete refresh windows in `platform/app-web/src/components/account_home.rs`, and protected `current_paging` from stale create/delete refresh writes.
- Verification passed: `cargo test -p app-web stale_open_workshops_response_is_ignored -- --nocapture`; `cargo test -p app-web delete_workshop_flow_falls_back_to_first_page_after_empty_non_first_reload -- --nocapture`; `cargo test -p app-web create_workshop_flow_refreshes_first_page_and_updates_current_paging -- --nocapture`; `cargo check -p app-web`; `cargo check -p app-web --target wasm32-unknown-unknown`.
- Validator cycle: requirements, correctness, performance, network safety, and regression evidence. Initial validators found delete/create paging races; implementation and tests were extended until final 5-lens signoff reported no findings.
- T04 completed.
- Implemented explicit loading/loaded/error state for open workshops, eligible characters, and account dragons; added request-generation guards and workshop-code scoping; preserved non-empty cached rows with refresh status; cleared/invalidate pre-session caches on logout/leave; guarded late command/archive responses; improved active-screen notices and list live regions; and added focused dialog/menu focus handling for changed Manage Dragons flows.
- Verification passed: `cargo test -p app-web -- --nocapture`; `cargo check -p app-web`; `cargo check -p app-web --target wasm32-unknown-unknown`.
- Validator cycle: requirements, correctness, performance, functional safety, and UX/a11y. Multiple rounds found first-paint loading gaps, stale eligible-character/workshop races, logout/cache invalidation collisions, app-bar/manage-dragons stale-action and focus issues; implementation was corrected until final 5-lens signoff reported no findings.
- Residual risk: no browser/e2e test verifies Dioxus first-paint timing, live-region announcements, or focus timing; current coverage is static validation plus native/wasm compile and unit tests.
- T05 completed.
- Implemented synchronous reservation/ticket helpers for `pending_flow`, `pending_command`, and `pending_judge_bundle` in `platform/app-web/src/state.rs`; UI handlers reserve before spawning async work for sign-in, create/join/save/delete/rename/logout, workshop commands, voting commands, and character/workshop destructive flows.
- Preserved direct async submit callers by reserving internally; guarded reserved completions by ticket generation and, for session command/archive work, by current session snapshot; kept logout pending through `api.logout().await`; preserved unrelated pending commands on first realtime attach while confirming matching state-completed commands; and made `RevealVotingResults` complete only when `voting.results_revealed` is true.
- Verification passed: `cargo fmt --package app-web --check`; `cargo test -p app-web -- --nocapture` (`74 passed`); `cargo check -p app-web`; `cargo check -p app-web --target wasm32-unknown-unknown`.
- Validator cycle: requirements, correctness, performance, functional safety, and UX/interaction. Initial validators found logout clearing too early, first realtime attach pending-command races, AppBar manage-dragons flow overlap, lobby leave blocking, stale realtime notices, and reveal-result false completion; implementation was corrected until final consensus reported no must-fix findings.
- Residual risk: no browser/e2e test exercises actual rapid double-click timing; current confidence is synchronous signal-state tests, static validator review, full app-web unit tests, and native/wasm compile.
