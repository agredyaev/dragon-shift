# Dragon Shift

Dragon Shift is a Rust-only multiplayer workshop game about handoffs, operational continuity, and support readiness. The current production path is a single `app-server` binary that serves the Dioxus web app, exposes the HTTP and WebSocket API, and persists workshop state in Postgres.

## Canonical project shape

- `platform/` - Rust workspace and canonical runtime
- `platform/app-server` - Axum composition root and static asset host
- `platform/app-web` - Dioxus Web frontend
- `platform/crates/*` - domain, protocol, persistence, security, realtime
- `helm/dragon-shift` - Rust-first Helm chart
- `Dockerfile` - Rust-only container build

## Confirmed stack

- Rust
- Axum
- Dioxus Web
- WebSocket
- Postgres
- Helm

## Runtime characteristics

- 6-digit workshop codes
- reconnect-token based player recovery
- same-origin aware frontend API bootstrap in production
- static frontend assets served directly by `app-server`
- durable workshop persistence through `DATABASE_URL`
- `/api/live` and `/api/ready` health endpoints
- automatic host failover on disconnect
- single authoritative realtime runtime inside the app instance

## Production requirements

- `NODE_ENV=production`
- `DATABASE_URL` must be set
- `ALLOWED_ORIGINS` must explicitly match the public app origin
- `VITE_APP_URL` must match the externally visible base URL
- keep `replicaCount=1` unless you add distributed coordination for realtime ownership and socket fan-out

If `NODE_ENV=production` is set without `DATABASE_URL`, `app-server` now fails fast during startup.

## Environment

Copy `.env.example` and set values appropriate for your environment.

- `APP_SERVER_BIND_ADDR` - bind address for Axum, e.g. `0.0.0.0:3000`
- `DATABASE_URL` - required in production for durable workshop state
- `ALLOWED_ORIGINS` - explicit comma-separated origin allowlist
- `VITE_APP_URL` - public base URL used for same-origin validation/bootstrap
- `RUST_SESSION_CODE_PREFIX` - optional single-digit code prefix override
- `VITE_GEMINI_API_KEY` - optional browser-side key for sprite generation

## Local development

1. Install Rust toolchain from `platform/rust-toolchain.toml`.
2. Install `wasm-bindgen-cli` if you want to build the browser bundle locally:

```bash
cargo install wasm-bindgen-cli --version 0.2.114 --locked
```

3. Run workspace checks:

```bash
cargo check --manifest-path platform/Cargo.toml --workspace
```

4. Build the frontend bundle:

```bash
cargo run --manifest-path platform/Cargo.toml -p xtask -- build-web
```

5. Start the backend:

```bash
cargo run --manifest-path platform/Cargo.toml -p app-server
```

## Validation

- Workspace compile:

```bash
cargo check --manifest-path platform/Cargo.toml --workspace
```

- Workspace tests:

```bash
cargo test --manifest-path platform/Cargo.toml --workspace
```

- Rust smoke commands:

```bash
cargo run --manifest-path platform/Cargo.toml -p xtask -- smoke-phase1 --base-url http://127.0.0.1:4100
cargo run --manifest-path platform/Cargo.toml -p xtask -- smoke-judge-bundle --base-url http://127.0.0.1:4100
cargo run --manifest-path platform/Cargo.toml -p xtask -- smoke-offline-failover --base-url http://127.0.0.1:4100
```

## Container build

```bash
docker build -t dragon-shift:local .
```

The root `Dockerfile` builds the frontend bundle from `platform/app-web` and the backend binary from `platform/app-server`, then ships them together in a single runtime image.

## CI/CD

GitHub Actions workflows now live under `.github/workflows`:

- `ci.yml` - validates the Rust workspace, Helm chart, and root production Docker image
- `publish-image.yml` - publishes the root production image to `ghcr.io/<owner>/<repo>` on version tags or manual dispatch

The CI workflow currently runs:

- `cargo check --workspace --all-targets`
- `cargo test --workspace`
- `cargo run --locked -p xtask -- build-web --out-dir /tmp/app-web-dist`
- `helm lint ./helm/dragon-shift` with production-like required values
- `helm template ./helm/dragon-shift` with secret-backed database wiring
- `docker build .`

The web bundle and production image now use only Rust tooling: `cargo`, `xtask`, and `wasm-bindgen-cli`. The image publish workflow is intentionally limited to container publishing. Cluster deployment remains a separate Helm step because Kubernetes credentials and environment-specific values should stay outside the repository.

## Helm deploy path

The canonical deploy path is `helm/dragon-shift`.

Required production inputs:

- `image.repository`
- `image.tag`
- `app.allowedOrigins`
- `app.viteAppUrl`
- `database.url` or `database.existingSecretName`

Portable chart defaults now avoid assuming a specific ingress controller. The chart also supports:

- `serviceAccount.create` / `serviceAccount.name`
- `imagePullSecrets`
- `podAnnotations`
- `podSecurityContext`
- `securityContext`
- `nodeSelector`
- `tolerations`
- `affinity`

Example render:

```bash
helm template dragon-shift ./helm/dragon-shift --set image.repository=ghcr.io/your-org/dragon-shift-rust --set image.tag=latest --set app.allowedOrigins=https://dragon-shift.example.com --set app.viteAppUrl=https://dragon-shift.example.com --set database.url=postgres://user:pass@postgres:5432/dragon_shift
```

Example secret-backed deployment:

```bash
kubectl create secret generic dragon-shift-app --from-literal=DATABASE_URL='postgres://user:pass@postgres:5432/dragon_shift'
helm upgrade --install dragon-shift ./helm/dragon-shift --set image.repository=ghcr.io/your-org/dragon-shift-rust --set image.tag=latest --set app.allowedOrigins=https://dragon-shift.example.com --set app.viteAppUrl=https://dragon-shift.example.com --set database.existingSecretName=dragon-shift-app --set database.existingSecretKey=DATABASE_URL
```

## Postgres persistence verification

The Rust runtime persists data into three tables:

- `workshop_sessions` - current full `WorkshopSession` snapshot in `payload`
- `session_artifacts` - append-only workshop artifact trail
- `player_identities` - reconnect token to player/session mapping

Executable verification command:

```bash
cargo run --manifest-path platform/Cargo.toml -p xtask -- smoke-persistence --base-url http://127.0.0.1:4100 --database-url postgres://user:pass@127.0.0.1:5432/dragon_shift
```

The smoke does the following:

- creates a workshop through the public HTTP API
- transitions it into `Phase1`
- reads Postgres directly
- verifies rows exist in `workshop_sessions`, `session_artifacts`, and `player_identities`
- asserts the persisted session phase is `phase1`

## Operational note

The current runtime is intentionally single-replica for correctness. Postgres now gives durable workshop persistence, but realtime connection ownership still lives in-process. Horizontal scaling requires an additional coordination layer.
