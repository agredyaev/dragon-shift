# Architecture Plan: platform/app-web INP Reduction and Render Path Simplification

## Summary

Reduce interaction latency in `platform/app-web` by narrowing Dioxus signal subscriptions, memoizing render-time derived data, and simplifying expensive visual effects.

Primary target:

1. Restore INP from the current ~120 ms range toward the pre-refactor baseline.
2. Keep the existing modular architecture from `refactor_frontend.md`.
3. Preserve current app behavior and workshop flow.

This plan does not replace the previous refactor. It is a follow-up optimization pass.

## Problem Statement

The current app uses modular files, but hot UI paths still subscribe to coarse signals:

- `InputState` bundles all text inputs.
- `OperationState` bundles notice + pending flags.
- `IdentityState` bundles session identity + connection + API config.
- `game_state` is replaced as a whole on each server update.

As a result, one keystroke or one command can trigger rerenders across unrelated panels. Several components also recompute derived rows and labels directly in render.

The current background effect also animates large painted areas in a way that looks intentionally jittery.

## Goals

1. Minimize rerender fanout from keystrokes.
2. Minimize rerender fanout from pending state changes.
3. Move repeated derived computations out of the immediate render path.
4. Keep changes incremental, testable, and localized.
5. Make the animated background visually smooth.

## Non-Goals

1. No full rewrite to a different state model.
2. No routing or server protocol changes.
3. No speculative feature work.
4. No broad redesign of the component tree beyond what is needed for performance.

## Phase 1: Split Hot Input State

### Current issue

`InputState` is a single hot signal. Typing in one field invalidates subscribers that only need other fields.

### Target state

Replace `Signal<InputState>` in the UI with field-level signals:

- `Signal<String>` for `create_name`
- `Signal<String>` for `join_session_code`
- `Signal<String>` for `join_name`
- `Signal<String>` for `reconnect_session_code`
- `Signal<String>` for `reconnect_token`
- `Signal<String>` for `handover_tags_input`

### Notes

- Keep bootstrap and persistence behavior identical.
- Preserve helper logic and request building.
- Prefer explicit flow function signatures over rebuilding a new struct signal.

### Acceptance criteria

1. Keystrokes in create/join/reconnect/handover inputs only update the relevant field signal.
2. No single shared `InputState` signal remains in `App`.
3. Existing affected tests are updated and pass.

## Phase 2: Split Hot Operation State

### Current issue

`OperationState` combines unrelated reactive concerns. Updating a notice rerenders controls that only need pending flags, and vice versa.

### Target state

Replace `Signal<OperationState>` with narrower signals:

- `Signal<Option<PendingFlow>>`
- `Signal<Option<SessionCommand>>`
- `Signal<bool>` for judge bundle pending
- `Signal<Option<ShellNotice>>`

### Acceptance criteria

1. Notice-only updates do not rerender unrelated pending-only consumers.
2. Pending updates do not require a whole operation struct read.

## Phase 3: Narrow Identity Subscriptions

### Current issue

`IdentityState` still mixes hot and cold concerns.

### Target state

Split into narrower signals where useful:

- session identity / snapshot
- connection status
- coordinator
- API base URL
- realtime bootstrap attempted

### Acceptance criteria

1. Editing API base URL does not rerender unrelated session status consumers.
2. Connection status changes do not force unrelated config UI rerenders.

## Phase 4: Memoize Derived Render Data

### Current issue

Views such as voting, end results, archive, and some phase panels derive labels, rows, and sorted lists inside render.

### Target state

Use `use_memo` for repeated derived data that depends on signals and is reused by the same component render.

Priority targets:

- `VotingView`
- `EndView`
- `ArchivePanel`
- `SessionPanel` summary labels if still repeated

### Acceptance criteria

1. Expensive row builders are not rerun on unrelated signal changes.
2. Duplicate render-path computations are removed.

## Phase 5: Remove Unnecessary Subscriptions

### Current issue

Some code reads signals where it only needs a one-time snapshot or a non-reactive check.

### Target state

Apply Dioxus best practices:

- use `ReadSignal<T>` for read-only child props where appropriate
- use `peek()` / `with_peek()` when the value should not subscribe the component
- avoid broad `use_effect` dependencies when a one-shot or narrower dependency is enough

### Acceptance criteria

1. Components subscribe only to data they render reactively.
2. Effects do not create avoidable rerender loops or extra post-render work.

## Phase 6: Smooth Background Animation

### Current issue

The current background animates `background-position` on large full-screen layers and uses a step-based flicker overlay.

### Target state

Keep the visual style but reduce visible jitter:

- animate a dedicated layer with `transform` instead of animating full `background-position` on `body`
- replace `steps(2, end)` flicker with a smoother low-amplitude animation or reduce its strength
- avoid expensive full-screen blend effects when they do not materially improve the look

### Acceptance criteria

1. Background motion looks smooth during animation.
2. No visual regression in theme or readability.

## Execution Order

1. Phase 1: split hot input state
2. Phase 6: smooth background animation
3. Phase 2: split hot operation state
4. Phase 4: memoize derived render data
5. Phase 5: remove unnecessary subscriptions
6. Phase 3: narrow identity subscriptions if still needed after measurement

This order prioritizes the highest likely INP wins with the lowest architectural risk.

## Validation

For each phase:

1. `cargo build -p app-web`
2. `cargo build --target wasm32-unknown-unknown -p app-web`
3. `cargo test -p app-web`
4. `cargo run -p xtask -- build-web`
5. run app-server and confirm the app still loads
6. re-measure INP and Element Render Delay in Chrome after rebuild

## Final Acceptance Criteria

1. The app still builds and tests successfully.
2. Workshop flows still work end-to-end.
3. INP improves measurably versus the current post-refactor baseline.
4. Background animation is visibly smoother.
5. No unrelated refactor is mixed into the optimization work.
