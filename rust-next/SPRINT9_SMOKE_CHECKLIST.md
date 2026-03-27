# Sprint 9 Smoke Checklist

## Preconditions

- `rust-next` workspace is up to date and builds locally
- no legacy services are required for validation
- `app-server` is reachable at `http://127.0.0.1:4100` or another explicit base URL

## Baseline Validation

- `cargo test --manifest-path /Users/fingerbib/Project/dragon-switch/rust-next/Cargo.toml --workspace`
- confirm all workspace crates pass before runtime smoke begins

## Start Rust Runtime

- `cargo run --manifest-path /Users/fingerbib/Project/dragon-switch/rust-next/Cargo.toml -p app-server`
- wait for the server to bind locally before running smoke commands

## Required Runtime Smoke Commands

- `cargo run --manifest-path /Users/fingerbib/Project/dragon-switch/rust-next/Cargo.toml -p xtask -- smoke-phase1 --base-url http://127.0.0.1:4100`
- expected result: single-player host reaches `phase1` and receives a dragon

- `cargo run --manifest-path /Users/fingerbib/Project/dragon-switch/rust-next/Cargo.toml -p xtask -- smoke-judge-bundle --base-url http://127.0.0.1:4100`
- expected result: multiplayer session reaches `end` and judge bundle includes dragons plus artifacts

- `cargo run --manifest-path /Users/fingerbib/Project/dragon-switch/rust-next/Cargo.toml -p xtask -- smoke-offline-failover --base-url http://127.0.0.1:4100`
- expected result: websocket detach marks the original host offline, guest becomes host, guest starts `phase1`, original host can reconnect, and host-driven `reset` returns the session to `lobby`

## Manual Browser Checks

- start the Dioxus client through the Rust-only entrypoint: `cargo run --manifest-path /Users/fingerbib/Project/dragon-switch/rust-next/Cargo.toml -p xtask -- web`
- open the Dioxus client against the same Rust backend
- create a workshop as host
- join from a second browser session
- verify the UI auto-attaches realtime over `WebSocket`
- verify `Current session` / `Workshop controls` show the refreshed pixel-style shell rather than the old temporary bootstrap-only styling
- trigger `Start Phase 1` and confirm both browser sessions advance through pushed `StateUpdate` frames instead of local optimistic phase drift
- close the host browser tab and confirm the second player becomes host
- reconnect the original host and confirm it returns as a connected non-host player
- run `reset` from the reassigned host and confirm both players see `lobby`

## Cutover Gate

Proceed toward staging cutover only when all items below are true:

- workspace tests are green
- all three xtask smoke commands are green
- manual browser checks pass without any Node or TypeScript runtime in the path
- operators have the rollback plan on hand
