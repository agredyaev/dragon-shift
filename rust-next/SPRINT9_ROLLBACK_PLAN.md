# Sprint 9 Rollback Plan

## Rollback Triggers

Rollback the Rust-only cutover immediately if at least one condition is observed:

- create, join, reconnect, or command flows fail for active users
- `WebSocket` state streaming becomes unstable or stale
- host failover or offline recovery behaves incorrectly
- judge bundle generation becomes incomplete or unavailable
- staging smoke checklist fails after deploy

## Primary Goal

Restore user traffic to the last known good legacy production path without mutating or deleting Rust-side data that may be needed for investigation.

## Preparation Before Cutover

- keep the legacy deployment healthy and runnable
- preserve the previous ingress or gateway routing configuration
- keep database backup and restore procedures verified
- export the current Rust release identifier and config used for cutover
- have the Sprint 9 smoke checklist results attached to the release record

## Rollback Procedure

### 1. Stop New Traffic to Rust Runtime

- remove Rust-only app instances from public traffic
- restore ingress or gateway routing to the last known good legacy target
- verify health probes succeed on the restored path

### 2. Freeze Further Rust Changes

- halt the rollout pipeline for the failing Rust release
- preserve application logs, tracing output, and release metadata
- record the exact failing scenario and timestamp

### 3. Validate User Recovery on Legacy

- confirm create and join work through the restored path
- confirm reconnect works for active sessions where possible
- confirm a minimal end-to-end workshop scenario succeeds

### 4. Preserve Investigation Inputs

- save Rust-side logs and smoke command outputs
- capture the failing session codes and player ids
- keep database state available for postmortem analysis

## Post-Rollback Checks

- public traffic is fully served by legacy again
- no new user traffic is routed to `rust-next`
- operators can reproduce the failure in staging or local smoke before retrying cutover
- a new cutover is blocked until the root cause is fixed and the smoke checklist passes again

## Retry Gate

Attempt another Rust-only cutover only after all items below are true:

- root cause is identified and fixed
- workspace tests are green
- `smoke-phase1` passes
- `smoke-judge-bundle` passes
- `smoke-offline-failover` passes
- manual browser checks pass on staging
