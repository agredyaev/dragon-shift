# Rust Architecture Principles

Dragon Shift uses a Rust-only modular monolith.

## Decisions
- Domain code stays separate from Axum, SQL, and Dioxus.
- Transport, persistence, and UI wrap that core.
- Real-time ownership stays with one session owner.
- Dependency injection is constructor-based.
- Domain errors are typed.
- Config is env-backed and typed.
- UI state stays separate from transport state.
- Tests cover core rules, adapters, and the main gameplay path.
