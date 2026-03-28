# Rust Architecture Principles

## Confirmed Stack

- `Rust`
- `Axum`
- `Dioxus Web`
- `WebSocket`
- `Postgres`

## Primary Goal

Построить новый `Rust-only` модульный монолит в отдельном workspace, где:

- доменная логика независима от веб-фреймворка и UI;
- transport, persistence и UI являются внешними адаптерами;
- вся новая разработка идёт без правок legacy, пока не наступит cutover.

## 1. DDD в Rust: применять, но прагматично

`DDD` здесь применим, но в урезанном практическом виде.

### Что действительно стоит использовать

- `ubiquitous language` на уровне типов и модулей;
- доменные сущности, value objects и invariants;
- явные aggregate boundaries;
- application services поверх domain model;
- ports/adapters для persistence и transport.

### Что не нужно тащить без причины

- чрезмерно сложные domain services;
- искусственные repository abstractions на каждый struct;
- deep inheritance-style abstraction trees;
- over-engineered CQRS/Event Sourcing, если это не требуется прямо сейчас.

### Практическое правило

- `domain` должен быть чистым Rust-кодом без `Axum`, `SQLx`, `Dioxus`;
- `application` orchestration живёт рядом с transport boundary, но не внутри UI;
- инфраструктура подключается только через порты.

## 2. Рекомендуемая слоистость

### Domain layer

Содержит:

- `GameState`
- `Session`
- `Player`
- `Dragon`
- phase transitions
- tick logic
- validation rules
- client projection rules

Не содержит:

- SQL
- HTTP
- WebSocket implementation details
- browser APIs

### Application layer

Содержит:

- use cases
- command handlers
- orchestration
- transaction boundaries
- mapping между transport DTO и domain commands

### Infrastructure layer

Содержит:

- `Axum` routes
- `WebSocket` session hub
- `Postgres` repositories
- config loading
- tracing
- rate limiting

### UI layer

Содержит:

- Dioxus components
- page state
- browser storage adapter
- HTTP client
- WebSocket client

## 3. Crate Boundaries

### `crates/protocol`

Только wire contract и shared DTO:

- commands
- responses
- ws messages
- transport-safe view models

### `crates/domain`

Только бизнес-правила:

- phases
- ticks
- actions
- handover
- voting
- achievements
- judge bundle logic

### `crates/persistence`

Только хранилище:

- schema
- repositories
- state load/save
- artifact persistence
- identity persistence

### `crates/security`

Только security policies:

- origins
- rate limits
- payload validation
- sprite validation

### `crates/realtime`

Только realtime runtime:

- connection registry
- session fan-out
- attach/detach
- WebSocket event dispatch

### `app-server`

Composition root:

- собирает все crates;
- поднимает `Axum`;
- владеет конфигурацией и DI wiring;
- публикует HTTP и WebSocket endpoints.

### `app-web`

Frontend composition root:

- Dioxus app;
- screen composition;
- client-side state;
- commands и subscriptions.

## 4. DI в Rust: не через контейнер, а через composition

Классический DI container для Rust обычно не нужен.

### Рекомендуемый подход

- зависимости передаются через конструкторы;
- shared state инжектится через `Arc`;
- поведение абстрагируется через traits только там, где реально нужна подмена;
- wiring делается в `main.rs` или `app.rs`.

### Хорошо

- `GameService<R: SessionRepository>`
- `Arc<dyn SessionRepository + Send + Sync>` только если нужна runtime polymorphism
- явные зависимости в constructor signatures

### Плохо

- service locator;
- глобальные mutable singletons;
- traits ради одного implementation без тестовой ценности.

## 5. DRY: применять аккуратно

В Rust слишком агрессивный `DRY` часто ухудшает код.

### Правило

- дублирование domain-терминов плохо;
- дублирование простого glue-кода иногда нормально;
- не надо абстрагировать два почти одинаковых обработчика слишком рано.

### Практика

- сначала сделать 2-3 явных use case handlers;
- выделять общую abstraction только когда видно устойчивый повтор;
- не смешивать domain reuse и infrastructure reuse.

## 6. Error Handling

### Рекомендации

- в `domain` использовать typed errors;
- в `application` маппить их в use-case errors;
- в `transport` переводить в HTTP/WS-friendly responses;
- не использовать `anyhow` внутри core domain.

### Подход

- `thiserror` для библиотечных crates;
- `anyhow` допустим в binary entrypoints и tooling.

## 7. Async Boundaries

### Правило

- domain logic по возможности синхронная;
- `async` начинается на persistence, WebSocket, HTTP и внешних I/O.

Это упрощает:

- тестирование;
- переиспользование;
- предсказуемость invariants.

## 8. State and Concurrency

### Рекомендуемый принцип

- authoritative state на сессию должен иметь явного owner;
- конкурентный доступ не должен хаотично менять одну и ту же session state;
- лучше actor/session-task model, чем shared mutable state everywhere.

### Для этого проекта

Предпочтительно:

- `session actor` или `session task` на одну workshop session;
- команды сериализуются через mailbox;
- broadcast идёт из единого owner runtime.

## 9. Testing Strategy

### Unit tests

Для:

- phase transitions
- action effects
- vote validation
- host failover
- handover logic
- judge bundle logic

### Integration tests

Для:

- repository implementations
- Axum routes
- WebSocket attach flow
- reconnect flow

### E2E tests

Для:

- create/join
- gameplay loop
- phase progression
- voting
- reset

### Invariant tests

Отдельно держать проверки вроде:

- player cannot vote for forbidden target
- disconnected player does not decay unfairly
- host always exists if session has players
- phase transitions only happen from valid previous phase

## 10. Observability

Нужно закладывать сразу:

- `tracing`
- request IDs
- session code in structured logs
- player ID in structured logs where appropriate
- health/readiness endpoints
- metrics later, если понадобится

## 11. Config and Secrets

### Принципы

- config через env + typed config layer;
- secrets не хардкодить;
- browser-visible config отделять от server config;
- staging/prod differences задавать явно.

## 12. UI Architecture for Dioxus

### Рекомендации

- разделять screen state и transport state;
- DTO из `protocol` не смешивать с transient UI-only flags без адаптера;
- reusable components держать dumb, а orchestration в page-level modules;
- browser storage оборачивать adapter-слоем.

### Не делать

- весь app state в одном giant component;
- прямой сетевой код внутри маленьких presentational components;
- hidden business rules во frontend.

## 13. Migration Rules

Пока не наступил cutover:

- legacy код не редактируется без крайней необходимости;
- все новые фичи Rust-only мира реализуются в `platform/`;
- parity подтверждается тестами, а не ощущением;
- удаление legacy только после зелёного staging и cutover checklist.

## 14. Decision Summary

### Да

- modular monolith first
- pragmatic DDD
- constructor-based DI
- actor/session runtime for realtime
- typed domain errors
- transport adapters around pure core
- testable boundaries

### Нет

- dependency injection container as default
- over-abstraction
- framework leakage into domain
- premature DRY
- hidden global mutable state
- direct SQL/HTTP logic inside domain model
