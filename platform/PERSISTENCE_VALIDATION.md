# Persistence Validation

## Purpose

This document explains how the Rust-only `platform` runtime writes workshop state into Postgres and how to verify that behavior end-to-end.

## Persisted tables

- `workshop_sessions`
  - one row per workshop
  - `payload` stores the full serialized `WorkshopSession`
  - `session_code` is the lookup key used for lazy rehydrate after restart

- `session_artifacts`
  - append-only trail of workshop events
  - stores phase transitions, observations, reconnects, votes, resets, and judge bundle generation

- `player_identities`
  - reconnect token mapping for player recovery
  - stores `session_id`, `player_id`, `created_at`, and `last_seen_at`

## When writes happen

`app-server` persists data on:

- workshop creation
- player join
- reconnect
- websocket disconnect state changes
- phase transitions
- discovery observations
- handover tag submission
- phase 2 actions
- voting
- reset

## Atomicity and restore policy

The runtime now treats some persistence writes as logical groups rather than unrelated best-effort operations.

- workshop creation persists session snapshot, reconnect identity, and creation artifact together
- player join persists session snapshot, reconnect identity, and join artifact together
- reconnect rotation persists updated session snapshot, rotated reconnect identity, revocation of the previous token, and reconnect artifact together
- command/disconnect paths that produce a session mutation and artifact persist those together

For Postgres, those grouped operations are executed inside one SQL transaction.

Restore-time transient-state rule:

- persisted `WorkshopSession` payload may still contain transient fields such as `SessionPlayer.is_connected`
- those fields are not authoritative after restart/reload
- when a session is reloaded into cache, all restored player connectivity is normalized back to `false`
- live connectivity is rebuilt only from current runtime activity such as reconnect or websocket attach

## Local verification

1. Start Postgres.
2. Start `app-server` with `DATABASE_URL` pointing at that Postgres instance.
3. Run:

```bash
cargo run --manifest-path platform/Cargo.toml -p xtask -- smoke-persistence --base-url http://127.0.0.1:4100 --database-url postgres://user:pass@127.0.0.1:5432/dragon_shift
```

## What `smoke-persistence` proves

The smoke:

- creates a workshop over HTTP
- triggers `StartPhase1`
- queries `workshop_sessions` by `session_code`
- checks that persisted `payload.phase == "phase1"`
- counts matching `session_artifacts`
- verifies that `player_identities` contains the reconnect token created during workshop bootstrap

## Expected result

Successful output includes:

- `persistedPhase: phase1`
- `artifactCount >= 2`
- `tablesChecked: ["workshop_sessions", "session_artifacts", "player_identities"]`

## Operational note

This validates durable persistence of workshop state and reconnect identity, but it does not make realtime ownership distributed. The runtime is still intended to run as a single authoritative replica unless a separate coordination layer is introduced.
