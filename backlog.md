# Workshop Scalability Backlog

## Goal

Remove the current workshop command bottleneck without a big-bang rewrite.

The current failure mode is:

- all host and guest commands for one workshop contend on one expensive serialized path
- each command waits on storage, full apply, and fanout before completion
- the client reports timeouts before the server meaningfully communicates queue state
- phase deadlines are not server-owned, so lagging clients can block progression

This backlog introduces small, reversible steps that improve correctness first, then throughput, then architecture.

## Non-Goals

- no immediate full protocol rewrite
- no immediate removal of existing HTTP command endpoints
- no big-bang migration to event sourcing across the whole product
- no dependency on a single deploy to unlock all improvements

## Success Criteria

- host phase-control commands remain responsive under 30 guest clients
- handover no longer blocks indefinitely after deadline expiry
- client-visible command outcomes distinguish queued, applied, and rejected states
- workshop command latency is no longer dominated by per-request state reload and synchronous persistence
- active workshops tolerate guest activity bursts without starving host commands

## Workstreams

### 1. Command Envelope And Telemetry

#### Why

Current failures are hard to classify. We need per-command visibility before deeper changes.

#### Deliverables

- add a stable `command_id` to workshop command requests
- classify each command by `command_type` and `priority`
- log and measure:
  - `session_code`
  - `player_id`
  - `command_id`
  - `command_type`
  - `priority`
  - `received_at`
  - `queue_wait_ms`
  - `apply_ms`
  - `persist_ms`
  - `broadcast_ms`
  - `result`
- separate client-visible failure reasons:
  - client timeout
  - server validation conflict
  - server rate limit
  - transport failure

#### Files

- `platform/crates/protocol/src/lib.rs`
- `platform/app-server/src/http.rs`
- `platform/app-web/src/api.rs`

#### Tests

- protocol serialization test for `command_id`
- server request logging / command result test
- web client error classification tests

#### Acceptance Criteria

- a single load run can identify whether a failure was queueing, validation, rate limiting, or transport

### 2. Server-Owned Deadlines And Missing Handover State

#### Why

This fixes progression correctness and removes the infinite blocker case.

#### Deliverables

- add explicit handover completion state:
  - `Pending`
  - `Saved`
  - `Missing`
- add server-owned deadline fields for active phases
- add domain logic to resolve overdue handover state:
  - all `Pending` handovers become `Missing` after deadline expiry
- update phase progression rules:
  - before deadline: missing guest handovers block `StartPhase2`
  - after deadline: `Missing` guests no longer block `StartPhase2`
- expose missing players in notices and UI

#### Files

- `platform/crates/domain/src/lib.rs`
- `platform/crates/protocol/src/lib.rs`
- `platform/app-server/src/http.rs`
- `platform/app-web/src/components/handover_view.rs`

#### Tests

- domain test: overdue handover transitions pending to missing
- server test: `StartPhase2` blocked before deadline and allowed after deadline
- web view test: host sees `Pending`, `Saved`, and `Missing`

#### Acceptance Criteria

- a lagging guest cannot block progression indefinitely after handover deadline expiry

### 3. Workshop Coordinator Behind Existing HTTP API

#### Why

This is the first real throughput fix. Keep current endpoints, change only the execution model.

#### Deliverables

- introduce `WorkshopCoordinatorRegistry`
- introduce `WorkshopCoordinator` with:
  - one in-memory workshop state
  - one serialized command loop per workshop
  - one enqueue path for commands
- change `POST /api/workshops/command` to route commands into the coordinator instead of executing full request-time state reload and mutation directly
- keep current HTTP contract for now

#### Files

- new: `platform/app-server/src/workshop_coordinator.rs`
- `platform/app-server/src/http.rs`
- `platform/app-server/src/main.rs`
- `platform/app-server/src/ws.rs`

#### Tests

- coordinator unit tests for sequential apply
- integration test: multiple commands on one workshop preserve order
- integration test: host command and guest command operate on the same in-memory state without request-time reload drift

#### Acceptance Criteria

- active workshop state is loaded once into memory and reused across multiple commands

### 4. Coordinator Priority Queues

#### Why

Host control commands must not starve behind guest activity spam.

#### Deliverables

- classify workshop commands into queue classes:
  - `High`: `StartHandover`, `StartPhase2`, `EndGame`, `Archive`
  - `Medium`: `SaveHandoverTags`
  - `Low`: guest activity actions and observations
- implement queue scheduling so `High` commands preempt `Low` backlog fairly
- emit queue-depth metrics by priority

#### Files

- `platform/app-server/src/workshop_coordinator.rs`
- `platform/crates/protocol/src/lib.rs`

#### Tests

- unit test: high-priority command executes ahead of queued low-priority commands
- integration test: host `StartHandover` stays responsive during guest activity flood

#### Acceptance Criteria

- phase-control latency remains bounded even when low-priority guest commands are queued

### 5. Low-Priority Backpressure And Coalescing

#### Why

Guest spam should degrade gracefully instead of unboundedly inflating queue depth.

#### Deliverables

- cap queued low-priority commands per player
- drop or coalesce redundant low-priority actions when allowed by game semantics
- reject low-priority commands with explicit overload result instead of allowing unbounded queue growth
- record overload telemetry

#### Files

- `platform/app-server/src/workshop_coordinator.rs`
- `platform/crates/domain/src/lib.rs`
- `platform/crates/protocol/src/lib.rs`

#### Tests

- queue cap test per player
- coalescing test for repeated low-value actions
- explicit overload result test

#### Acceptance Criteria

- guest command pressure cannot grow queue depth without limit

### 6. Async Persistence Off The Critical Path

#### Why

Persistence should not determine command latency for active workshops.

#### Deliverables

- split command handling into:
  - fast path: validate, apply in memory, broadcast result
  - durable path: append event and/or snapshot asynchronously
- add `PersistenceWriter` with batching and retry
- flush snapshots periodically by:
  - command count threshold
  - elapsed time threshold
  - important phase transition boundary

#### Files

- new: `platform/app-server/src/persistence_writer.rs`
- `platform/app-server/src/workshop_coordinator.rs`
- `platform/crates/persistence/*`

#### Tests

- writer retry test
- snapshot cadence test
- integration test: command apply completes while persistence is delayed

#### Acceptance Criteria

- command latency remains stable when storage latency rises within tested bounds

### 7. Recovery From Snapshot Plus Event Tail

#### Why

Async persistence is only safe if active workshop state can be reconstructed after restart or ownership transfer.

#### Deliverables

- define snapshot format for workshop runtime state
- define durable event tail format for commands applied after the last snapshot
- implement coordinator bootstrap from latest snapshot plus unapplied event tail
- validate phase deadlines and missing-state resolution on recovery

#### Files

- `platform/crates/protocol/src/lib.rs`
- `platform/crates/persistence/*`
- `platform/app-server/src/workshop_coordinator.rs`
- `platform/app-server/src/persistence_writer.rs`

#### Tests

- restart recovery test from snapshot plus events
- recovery test with overdue handover deadline

#### Acceptance Criteria

- active workshop state recovers without losing phase correctness or handover status

### 8. Delta Broadcast Alongside Full Sync

#### Why

Full-state fanout on every command is expensive. Add smaller updates first without removing the reconnect path.

#### Deliverables

- define delta event types:
  - `phase_changed`
  - `observation_added`
  - `handover_status_changed`
  - `player_stats_changed`
  - `command_applied`
  - `archive_ready`
- send deltas on normal command flow
- keep full snapshot sync for reconnect and resubscribe paths
- update client reducer to apply deltas incrementally

#### Files

- `platform/crates/protocol/src/lib.rs`
- `platform/app-server/src/ws.rs`
- `platform/app-web/src/state.rs`
- `platform/app-web/src/flows.rs`

#### Tests

- delta reducer unit tests
- reconnect test still using full snapshot
- integration test: multiple guest actions update clients without full-state dependency

#### Acceptance Criteria

- normal command flow can update clients without sending the full workshop state every time

### 9. Fast Acknowledge For HTTP Commands

#### Why

Client timeouts should not imply failure when the command was accepted and queued.

#### Deliverables

- add `accepted` response mode for HTTP command submission
- return `202 Accepted` when a command is queued but not yet applied within a short sync budget
- use websocket delta events to deliver final outcome for queued commands
- preserve current `200` behavior for immediately applied commands during compatibility phase

#### Files

- `platform/app-server/src/http.rs`
- `platform/app-web/src/api.rs`
- `platform/app-web/src/flows.rs`

#### Tests

- server test: command returns `202` when queued
- web test: client handles queued command and later applies outcome from websocket

#### Acceptance Criteria

- host no longer sees a false failure solely because a command waited in queue longer than the HTTP response budget

### 10. WebSocket Command Ingress

#### Why

After queueing and result delivery are in place, a realtime ingress removes unnecessary request-response coupling.

#### Deliverables

- define websocket command envelope carrying:
  - `command_id`
  - `session_code`
  - `command_type`
  - `payload`
- route websocket commands into the same coordinator path as HTTP commands
- keep HTTP endpoint as compatibility shim during migration

#### Files

- `platform/app-server/src/ws.rs`
- `platform/app-web/src/flows.rs`
- `platform/crates/protocol/src/lib.rs`

#### Tests

- websocket command ingress integration test
- parity test: HTTP and WS commands produce the same workshop state transition

#### Acceptance Criteria

- workshop commands can be submitted and completed entirely through the realtime channel

### 11. Cross-Pod Workshop Ownership

#### Why

Per-workshop coordinators must remain singular at cluster scale.

#### Deliverables

- add workshop ownership lease or routing metadata
- ensure one logical coordinator owner per workshop across pods
- add forwarding or handoff path for non-owner command ingress
- define coordinator eviction and rehydrate behavior on ownership transfer

#### Files

- `platform/app-server/src/workshop_coordinator.rs`
- `platform/app-server/src/main.rs`
- `platform/crates/persistence/*`
- infrastructure-related config as needed

#### Tests

- ownership acquisition test
- ownership handoff test
- command routing test through non-owner instance

#### Acceptance Criteria

- one workshop never executes commands in more than one active coordinator at a time across the deployment

## Execution Order

Recommended delivery order:

1. Command Envelope And Telemetry
2. Server-Owned Deadlines And Missing Handover State
3. Workshop Coordinator Behind Existing HTTP API
4. Coordinator Priority Queues
5. Low-Priority Backpressure And Coalescing
6. Async Persistence Off The Critical Path
7. Recovery From Snapshot Plus Event Tail
8. Delta Broadcast Alongside Full Sync
9. Fast Acknowledge For HTTP Commands
10. WebSocket Command Ingress
11. Cross-Pod Workshop Ownership

## Milestones

### Milestone A: Correctness Under Load

Includes:

- telemetry
- server-owned deadlines
- missing handover state

Outcome:

- host progression no longer stalls forever
- failures become classifiable

### Milestone B: Throughput Improvement Without API Rewrite

Includes:

- in-memory workshop coordinator
- priority queues
- backpressure

Outcome:

- host commands stop competing equally with guest activity spam
- active workshop latency improves while preserving current client API

### Milestone C: Durable And Efficient Active Workshop Runtime

Includes:

- async persistence
- recovery model
- delta broadcast

Outcome:

- command latency is no longer dominated by persistence or full-state fanout

### Milestone D: Full Realtime Command Model

Includes:

- fast acknowledge over HTTP
- websocket command ingress
- cross-pod ownership

Outcome:

- queued and applied command states are explicit
- architecture scales beyond a single instance bottleneck

## Rollout Strategy

- gate each major runtime change behind a feature flag
- enable first for internal or test workshops only
- compare old path vs new path telemetry under identical load scenarios
- keep HTTP command compatibility until websocket ingress is proven stable
- keep full-state reconnect bootstrap even after delta broadcast is introduced

## Definition Of Done

The backlog is complete when all of the following are true:

- one active workshop uses an in-memory coordinator instead of per-request full reload/apply/persist
- phase deadlines are server-owned and overdue handovers resolve to `Missing`
- host phase-control commands are prioritized above guest activity noise
- low-priority guest traffic is subject to backpressure
- persistence is no longer on the critical path for command completion
- normal workshop updates use deltas, while reconnect still uses full sync
- clients can distinguish queued, applied, and rejected command outcomes
- workshop ownership is singular across pods
