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
- reconnect-token based player recovery with sliding inactivity expiry
- same-origin aware frontend API bootstrap in production
- static frontend assets served directly by `app-server`
- durable workshop persistence through `DATABASE_URL`
- `/api/live` and `/api/ready` public health endpoints
- automatic host failover on disconnect
- single authoritative realtime runtime inside the app instance

## Production requirements

- `NODE_ENV=production`
- `DATABASE_URL` must be set
- `ALLOWED_ORIGINS` must explicitly match the public app origin
- `VITE_APP_URL` must match the externally visible base URL
- production is single-replica only today: keep `replicaCount=1`; multi-replica operation, horizontal scaling, and autoscaling are unsupported until the runtime gains distributed coordination for realtime ownership and socket fan-out

If `NODE_ENV=production` is set without `DATABASE_URL`, `app-server` now fails fast during startup.

## Environment

Copy `.env.example` and set values appropriate for your environment.

- `APP_SERVER_BIND_ADDR` - bind address for Axum, e.g. `0.0.0.0:3000`
- `DATABASE_URL` - required in production for durable workshop state
- `ALLOWED_ORIGINS` - explicit comma-separated origin allowlist
- `VITE_APP_URL` - public base URL used for same-origin validation/bootstrap
- `RUST_SESSION_CODE_PREFIX` - optional single-digit code prefix override
- `TRUST_X_FORWARDED_FOR` - optional `true` to trust `X-Forwarded-For` for rate-limit identity behind a trusted proxy; default `false`
- `CREATE_RATE_LIMIT_MAX` - optional per-IP per-minute limit for workshop creation; default `20`
- `JOIN_RATE_LIMIT_MAX` - optional per-IP per-minute limit for workshop join/reconnect requests; default `40`
- `COMMAND_RATE_LIMIT_MAX` - optional per-IP per-minute limit for workshop command requests; default `120`
- `WEBSOCKET_RATE_LIMIT_MAX` - optional per-IP per-minute limit shared by websocket upgrades and inbound client messages; default `300`
- `RECONNECT_TOKEN_TTL_SECONDS` - optional sliding inactivity TTL for reconnect tokens; default `43200` (12 hours)
- `DATABASE_POOL_SIZE` - optional Postgres connection pool size for the app-server runtime; default `10`

Reconnect-token policy today:

- reconnect tokens are still bearer credentials and remain stored plaintext in `player_identities`
- inactivity expiry is enforced across reconnect join, workshop commands, judge-bundle requests, and websocket attach
- a successful reconnect join rotates the reconnect token and revokes the previous token
- other successful authenticated uses refresh `last_seen_at` without rotation to preserve browser UX
- expired tokens are treated as invalid and revoked opportunistically when checked

## Local development

1. Install Rust `1.94.1` from `platform/rust-toolchain.toml`.
2. Install `wasm-bindgen-cli` if you want to build the browser bundle locally:

```bash
cargo install wasm-bindgen-cli --version 0.2.115 --locked
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

- Browser e2e:

```bash
npm --prefix e2e run install:browsers
npm --prefix e2e test
npm --prefix e2e run test:mobile
```

The Playwright suite now includes `chromium` plus a `mobile-safari` project so the automated e2e flow exercises one mobile viewport and one non-Chromium engine without adding a larger browser matrix.

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
- `publish-image.yml` - publishes the root production image to `ghcr.io/<owner>/dragon-switch` on manual dispatch, on pushes to version tags matching `v*`, and on pushes to `main` only when `platform/**`, `helm/**`, `Dockerfile`, `.github/workflows/**`, or `README.md` changed
- `deploy.yml` - manually deploys, promotes, or rolls back a Helm release using GitHub environment approvals, then verifies `/api/live`, `/api/ready`, and the deployed Playwright smoke path

The CI workflow currently runs:

- `cargo check --workspace --all-targets`
- `cargo test --workspace`
- `cargo run --locked -p xtask -- build-web --out-dir /tmp/app-web-dist`
- `helm lint ./helm/dragon-shift` with production-like required values
- `helm template ./helm/dragon-shift` with secret-backed database wiring
- `docker build .`

The web bundle and production image now use only Rust tooling: `cargo`, `xtask`, and `wasm-bindgen-cli`. The image publish workflow is intentionally limited to container publishing. It always pushes a requested version tag plus `sha-<short-sha>`, and only adds `latest` for `main` push publishes and version-tag publishes. Cluster deployment remains environment-driven: `deploy.yml` reads Kubernetes credentials and target-specific values from GitHub environment secrets/variables so credentials and mutable deploy values stay outside the repository. The workflow may publish convenience tags, but deployment should pin the resulting `sha256` digest via `image.digest` whenever possible.

Recommended validation before a manual image publish or release tag is to run the same checks that gate `main`: `cargo check --manifest-path platform/Cargo.toml --workspace --all-targets`, `cargo test --manifest-path platform/Cargo.toml --workspace`, `cargo run --manifest-path platform/Cargo.toml -p xtask -- build-web --out-dir /tmp/app-web-dist`, the documented `helm lint` and `helm template` commands for `./helm/dragon-shift`, and `docker build -t dragon-shift:local .`. Treat `/api/live`, `/api/ready`, and deployed Playwright smoke as deploy validation from `deploy.yml`, not as part of the GHCR publish trigger.

Minimal GitHub environment setup for `deploy.yml`:

- create GitHub environments such as `staging` and `production`
- add required reviewers to those environments to enforce approval gates before the job starts
- set secret `KUBECONFIG_B64` to a base64-encoded kubeconfig for that environment
- set variable `IMAGE_REPOSITORY`, for example `ghcr.io/<owner>/dragon-switch`
- set variable `KUBE_NAMESPACE`
- set variable `APP_ALLOWED_ORIGINS`
- set variable `APP_VITE_APP_URL`
- set variable `VERIFY_URL` to the public URL checked after deployment
- ensure the target cluster can pull the selected image; if GHCR is private, configure `imagePullSecrets` or equivalent cluster/node registry auth before using the workflow

Optional environment variables:

- `HELM_VALUES_FILE` to add an in-repo values file such as `helm/dragon-shift/values.gateway-example.yaml`
- `DATABASE_SECRET_NAME` and `DATABASE_SECRET_KEY` when the release should wire `DATABASE_URL` from an existing Kubernetes secret
- `PORT_FORWARD_SERVICE` when the chart service name differs from the default `dragon-shift-dragon-shift`

Manual workflow usage:

1. Publish or identify the image you want to deploy.
2. Run `Deploy` with `action=deploy` for the first environment, usually `staging`.
3. After verification passes, run `Deploy` again with `action=promote` for the next environment and the same digest.
4. If the deployment regresses, run `Deploy` with `action=rollback` for the affected environment and provide the explicit Helm `rollback_revision` shown by `helm history`.

`deploy` and `promote` intentionally use the same Helm path. Promotion is represented by re-deploying the same image reference into the next GitHub environment so approvals, kubeconfig, namespace, and verification remain environment-specific.

## Helm deploy path

The canonical deploy path is `helm/dragon-shift`.

The chart defaults assume the current singleton runtime model: `replicaCount=1`
and `deploymentStrategy.type=Recreate` so upgrades do not overlap authoritative
app pods. Multi-replica production, horizontal scaling, and autoscaling are
currently unsupported by the runtime and should not be enabled. Overriding
`deploymentStrategy.type` away from `Recreate` is unsupported and the chart now
fails rendering if you try.

Required production inputs:

- `image.repository`
- `image.digest` preferred, or `image.tag` when you intentionally want a mutable reference
- `app.allowedOrigins`
- `app.viteAppUrl`
- `database.url` or `database.existingSecretName`

Production secret handling note:

- do not use browser-supplied `imageGeneratorToken` or `judgeToken` in production
- production `app-server` rejects create-workshop requests that include those long-lived third-party tokens
- if you need those integrations in production, move credentials to server-side configuration or another secret reference model instead of sending plaintext tokens from browser to API to database
- the create form token fields are supported only for local/dev workflows

For cloud production, use an external managed Postgres instance such as Cloud SQL and
set `postgresql.enabled=false`. The bundled chart Postgres is suitable for local or
single-node dev/test use, but its default chart configuration is not a production
durability, backup, or PITR story.

Cloud production must run behind a trusted ingress or gateway. Do not expose the
app service or pods directly to the public internet. The public edge must:

- terminate TLS
- normalize or overwrite forwarded-client headers before they reach the app
- enforce edge rate limits and connection/request abuse controls
- remain the only public entry point, with the app kept private behind it

Only enable `app.trustForwardedFor=true` when that trusted edge owns header
normalization. Leave it `false` for direct-client or otherwise untrusted paths.

When `image.digest` is set, the chart renders `image.repository@image.digest` and ignores `image.tag`.

To resolve a published digest from GHCR for deployment, for example after a `v0.1.0` release or `main` publish:

```bash
docker buildx imagetools inspect ghcr.io/<owner>/dragon-switch:v0.1.0
docker buildx imagetools inspect ghcr.io/<owner>/dragon-switch:main
```

Use the reported `Digest:` value as `image.digest` in Helm.

Portable chart defaults now avoid assuming a specific ingress controller. The chart also supports:

- `serviceAccount.create` / `serviceAccount.name`
- `imagePullSecrets`
- `podAnnotations`
- `podSecurityContext`
- `securityContext`
- `nodeSelector`
- `tolerations`
- `affinity`

The chart values surface intentionally does not expose browser-only AI token knobs. The current Helm templates wire the app runtime controls shown under `app.*`, but not deprecated or unsupported browser-side token injection.

Example render with a direct database URL:

```bash
helm template dragon-shift ./helm/dragon-shift --set image.repository=ghcr.io/your-org/dragon-switch --set image.digest=sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef --set app.allowedOrigins=https://dragon-shift.example.com --set app.viteAppUrl=https://dragon-shift.example.com --set postgresql.enabled=false --set database.url=postgres://user:pass@managed-postgres:5432/dragon_shift
```

Recommended cloud-production deployment uses a secret-backed `DATABASE_URL`:

```bash
kubectl create secret generic dragon-shift-app --from-literal=DATABASE_URL='postgres://user:pass@managed-postgres:5432/dragon_shift'
helm upgrade --install dragon-shift ./helm/dragon-shift --set image.repository=ghcr.io/your-org/dragon-switch --set image.digest=sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef --set app.allowedOrigins=https://dragon-shift.example.com --set app.viteAppUrl=https://dragon-shift.example.com --set postgresql.enabled=false --set database.existingSecretName=dragon-shift-app --set database.existingSecretKey=DATABASE_URL
```

On GKE or similar managed Kubernetes, point that secret at your managed Postgres or
Cloud SQL connection string and keep it out of committed values files.

Cloud-operability prerequisites that are not fully represented in this repository
are documented in `operations/CLOUD_OPERABILITY.md`. Treat that file as the
boundary between repo-owned app deploy assets and operator-owned cloud
infrastructure, secret management, observability, and restore readiness.

## Automated deploy and rollback

`deploy.yml` is the repository-contained automation path for deploy, promotion, and rollback.

Deploy/promotion behavior:

- requires a manual `workflow_dispatch`
- uses the selected GitHub environment so approval rules gate staging or production changes
- applies `helm upgrade --install --wait --atomic`
- expects an explicit image reference; use `use_digest=true` for `sha256:...` digests and prefer that in production
- verifies the deployed app through `/api/live`, `/api/ready`, and `npm --prefix e2e run test:deployed -- --project=chromium`

Rollback behavior:

- the workflow runs `helm history` first so operators can confirm recent revisions in the job log
- `action=rollback` executes `helm rollback <release> <rollback_revision>` for the explicit revision supplied by the operator
- post-rollback verification re-runs the same rollout, health, and deployed-smoke checks

Rollback guidance:

- prefer rolling back the Helm release when the problem is application behavior and the prior revision is still valid
- prefer deploying a new corrective image instead of rollback when a bad image has already been promoted across multiple environments and you want a single forward fix
- do not expect Helm rollback to undo forward-only database migrations; if a migrated release must be reversed after schema changes land, use the migration guidance below and restore from backup/PITR or ship a corrective migration

## Postgres persistence verification

The Rust runtime persists data into three tables:

- `workshop_sessions` - current full `WorkshopSession` snapshot in `payload`
- `session_artifacts` - append-only workshop artifact trail
- `player_identities` - reconnect token to player/session mapping

Executable verification command:

```bash
cargo run --manifest-path platform/Cargo.toml -p xtask -- smoke-persistence --base-url http://127.0.0.1:4100 --database-url postgres://user:pass@127.0.0.1:5432/dragon_shift
cargo run --manifest-path platform/Cargo.toml -p xtask -- smoke-persistence-restart --base-url http://127.0.0.1:4101 --database-url postgres://user:pass@127.0.0.1:5432/dragon_shift
cargo run --manifest-path platform/Cargo.toml -p xtask -- smoke-restore-reconnect --base-url https://staging.example.com --database-url postgres://user:pass@staging-postgres:5432/dragon_shift --restart-timeout-seconds 300
```

The smoke does the following:

- creates a workshop through the public HTTP API
- transitions it into `Phase1`
- reads Postgres directly
- verifies rows exist in `workshop_sessions`, `session_artifacts`, and `player_identities`
- asserts the persisted session phase is `phase1`

The restart-backed variant additionally:

- builds and launches `app-server` itself on the requested `--base-url`
- stops the process and starts a fresh `app-server` against the same database
- reconnects through the public HTTP API with the original reconnect token
- verifies the recovered session stays in `phase1` with the same session/player identity
- submits a new observation after restart and confirms both reconnect continuity and persisted artifact growth

Treat `smoke-persistence-restart` as a pre-release validation gate for any change that affects persistence, reconnect identity, restart behavior, or app-server recovery semantics.

Use a base URL/port that is not already occupied when running `smoke-persistence-restart`, because the command manages the backend process itself.

There is now a browser-level companion proof for the remaining restart/reconnect report item. It runs a real local `app-server` against Postgres, forces an actual process restart, reconnects through the browser UI with the saved reconnect token, and proves realtime continuity by advancing the same in-progress workshop after reconnect:

```bash
cd e2e
TEST_DATABASE_URL=postgres://user:pass@127.0.0.1:5432/dragon_shift npm run test:restart-local
```

`smoke-restore-reconnect` is the operator-assisted staging restore check. It:

- creates a workshop and persists it to Postgres
- waits for operators to restore the staging database and restart or redeploy the app
- requires an observed `/api/ready` outage followed by recovery, so it proves a real restart happened during the exercise
- reconnects with the original session after recovery
- verifies websocket attach still works after restart
- submits one additional observation and confirms persisted artifact growth afterward

Use `smoke-restore-reconnect` for staging restore drills where the app process is managed outside the command itself.

Ignored `persistence` crate integration tests can also run against the same PostgreSQL database without a separate test database. They create and drop an isolated schema per test via `search_path`:

```bash
TEST_DATABASE_URL=postgres://user:pass@127.0.0.1:5432/dragon_shift \
TEST_DATABASE_SCHEMA=persistence_itest \
cargo test --manifest-path platform/Cargo.toml -p persistence -- --ignored
```

`TEST_DATABASE_SCHEMA` is optional and only changes the schema name prefix. Each test still uses its own unique schema under that prefix.

## Postgres schema migrations

Postgres schema changes now use versioned SQL migrations under `platform/crates/persistence/migrations`.
`PostgresSessionStore::init()` applies any pending migrations through `sqlx` and records applied versions in `_sqlx_migrations`.
App startup still runs pending migrations before serving traffic, so the runtime database role must have the required DDL privileges and startup will fail fast if migration application fails.

Operational flow:

- add a new numbered `.sql` file in `platform/crates/persistence/migrations` for every schema change
- do not edit or reorder a migration that may already have been applied in another environment
- deploy the updated app; startup will apply any pending migrations before normal request handling

Rollback expectation:

- treat schema migrations as forward-only by default
- if a release must be reversed after a migration is applied, ship a new corrective migration or restore the database from backup/PITR rather than editing migration history

## Operational note

The current runtime is intentionally single-replica for correctness. Postgres now gives durable workshop persistence, but realtime connection ownership still lives in-process. Multi-replica production, horizontal scaling, and autoscaling are unsupported today. Horizontal scaling requires an additional coordination layer.

## Deploy Workflow

The repository includes a manual `.github/workflows/deploy.yml` workflow for `deploy`, `promote`, and `rollback`.

- GitHub Environment protection rules provide the approval gates.
- Workflow concurrency is per-environment, so overlapping deploys to the same target do not race.
- `rollback` requires an explicit Helm revision.
- Post-deploy verification checks rollout status, `/api/live`, `/api/ready`, and a deployed Playwright smoke run.
- If post-deploy smoke fails after a successful Helm upgrade, the workflow fails and an operator must decide whether to run rollback or ship a forward fix.

Environment setup must include cluster pull access for the selected image. If GHCR images are private, configure `imagePullSecrets` or equivalent cluster/node registry auth before using the workflow.

Rollback caveat:

- Helm rollback does not undo forward-only database migrations
- if a migration has already been applied, rollback may require a corrective migration or backup/PITR restore rather than only reverting the Helm release
