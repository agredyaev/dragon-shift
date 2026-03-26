# Rust-Only Migration Roadmap

## Target

- `Rust`
- `Axum`
- `Dioxus Web`
- `WebSocket`
- `Postgres`

## Migration Rules

- Вся новая работа ведётся только в `rust-next/`
- Legacy-файлы не редактируются, пока не наступит cutover
- Командный канал остаётся через `HTTP`
- Поток состояния идёт через `WebSocket`
- Истина домена переносится в Rust раньше UI

## Sprint 0 - Workspace Freeze

### Goals

- зафиксировать целевой стек
- разрезать будущую систему на crates
- подготовить отдельный workspace без влияния на legacy

### Deliverables

- `rust-next/Cargo.toml`
- `rust-next/ARCHITECTURE.md`
- `rust-next/ROADMAP.md`
- каркас `app-server`, `app-web`, `crates/*`

### Definition of Done

- новый workspace существует отдельно
- legacy не изменён
- структура crates утверждена

## Sprint 1 - Protocol First

### Goals

- перенести shared contract из `src/shared/game.ts` в Rust
- сделать Rust-типизацию центром всех новых модулей

### Scope

- `Phase`
- `SessionCommand`
- `ServerGameState`
- `ClientGameState`
- `Player`
- `Dragon`
- `Voting`
- `JudgeBundle`
- wire messages для HTTP и WebSocket

### Deliverables

- crate `protocol`
- DTO для команд и ответов
- базовые client/server message types

### Definition of Done

- новые backend и frontend модули могут зависеть только от Rust protocol types
- новый код не ссылается на TypeScript types

## Sprint 2 - Domain Parity

### Goals

- перенести игровую логику в Rust
- сделать Rust единственным authoritative носителем бизнес-правил

### Scope

- phase state machine
- tick loop
- rules для `feed`, `play`, `sleep`
- handover chain
- host failover
- vote validation
- judge bundle assembly
- client projection/privacy filtering

### Deliverables

- crate `domain`
- unit tests на transitions и invariants
- integration tests на ключевые сценарии

### Definition of Done

- все фазы моделируются в Rust
- доменная логика не зависит от Axum, Dioxus и Postgres

## Sprint 3 - Persistence and Security

### Goals

- вынести хранение состояния и валидации в Rust

### Scope

- sessions
- reconnect identities
- sprites
- artifacts
- migrations
- health checks
- origin policy
- rate limiting
- session code validation
- sprite validation

### Deliverables

- crate `persistence`
- crate `security`
- Postgres schema bootstrap

### Definition of Done

- Rust backend может сам хранить и восстанавливать сессии
- security checks выполняются внутри Rust stack

## Sprint 4 - Realtime Transport

### Goals

- убрать `Socket.IO`
- построить новый `WebSocket` transport

### Scope

- attach/detach session flow
- heartbeat
- state broadcast
- notices
- achievements
- reconnect over ws/http contract

### Deliverables

- crate `realtime`
- transport message model
- session broadcast runtime

### Definition of Done

- новый transport не использует `Socket.IO`
- сервер пушит состояние через обычный `WebSocket`

## Sprint 5 - Axum Authoritative Backend

### Goals

- собрать единый Rust backend
- заменить staged Rust path на основной runtime

### Scope

- create/join/command/judge-bundle HTTP API
- runtime composition
- session ownership
- app config
- tracing and health endpoints

### Deliverables

- `app-server`
- composition root для всех crates
- единый Rust-only backend entrypoint

### Definition of Done

- жизненный цикл игры проходит через один Rust backend
- Node coordinator функционально больше не нужен

## Sprint 6 - Dioxus UI Shell

### Goals

- перенести UI shell и session flows на Dioxus

### Scope

- app bootstrap
- create/join/reconnect
- session persistence in browser
- toasts
- connection status
- command client
- WebSocket client

### Deliverables

- `app-web`
- Dioxus root app
- базовые layout и app state containers

### Definition of Done

- пользователь может создать и подключить сессию через новый Rust UI
- frontend не зависит от React/Vite runtime

## Sprint 7 - Gameplay Screens

### Goals

- перенести все игровые экраны и пользовательский сценарий

### Scope

- lobby
- phase1
- handover
- phase2
- voting
- end
- judge bundle panel
- pixel UI components

### Deliverables

- полный Dioxus screen flow
- визуальный parity минимум по UX и состояниям

### Definition of Done

- весь пользовательский сценарий проходит через Dioxus UI
- не осталось обязательных React screens для core flow

## Sprint 8 - Tooling and Media

### Goals

- убрать TS-only tooling из критического пути

### Scope

- замена `generate-sprites.ts`
- media ingestion
- cargo-based dev scripts

### Deliverables

- `xtask` или `media` crate
- Rust CLI для вспомогательных задач

### Definition of Done

- для разработки и сборки не нужны Bun/npm scripts

## Sprint 9 - Parity, Staging, Cutover

### Goals

- подтвердить полную замену legacy стека

### Required Test Matrix

- create workshop
- join workshop
- reconnect
- start phase1
- actions
- discovery notes
- handover
- phase2 reassignment
- voting
- judge bundle
- reset
- host failover
- offline behavior

### Deliverables

- browser E2E
- staging deployment
- smoke checklist
- rollback plan

### Definition of Done

- staging проходит полный сценарий без Node/TS path
- production готов к cutover

## Sprint 10 - Legacy Purge

### Goals

- удалить legacy после подтверждённого cutover

### Remove

- `src/`
- `server/`
- `server.ts`
- `axum-gateway/`
- `package.json`
- `package-lock.json`
- `bun.lock`
- `tsconfig.json`
- `vite.config.ts`
- `tests/**/*.ts`
- `generate-sprites.ts`

### Deliverables

- Rust-only repo
- Rust-only CI/CD
- новый Helm chart
- переписанный README

### Definition of Done

- в репозитории нет production-useful TypeScript legacy
- deploy и local dev идут только через Rust toolchain
