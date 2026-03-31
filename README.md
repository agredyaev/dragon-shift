# Dragon Shift

Dragon Shift is a Rust-only multiplayer workshop game about handoffs, operational continuity, and support readiness. The current production path is a single `app-server` binary that serves the Dioxus web app, exposes the HTTP and WebSocket API, and persists workshop state in Postgres.

## Canonical project shape

- `platform/` - Rust workspace and canonical runtime
- `platform/app-server` - Axum composition root and static asset host
- `platform/app-web` - Dioxus Web frontend
- `platform/crates/*` - domain, protocol, persistence, security, realtime
- `helm/dragon-shift` - Rust-first Helm chart
- `terraform/` - GCP production infrastructure and deployment environments
- `Dockerfile` - Rust-only container build
- `docs/REPO_MAP.md` - compact file map for this repository

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
- `DATABASE_URL` or `DATABASE_URL_FILE` must be set
- `ALLOWED_ORIGINS` must explicitly match the public app origin
- `VITE_APP_URL` must match the externally visible base URL
- production is single-replica only today: keep `replicaCount=1`; multi-replica operation, horizontal scaling, and autoscaling are unsupported until the runtime gains distributed coordination for realtime ownership and socket fan-out

If `NODE_ENV=production` is set without `DATABASE_URL` or `DATABASE_URL_FILE`, `app-server` now fails fast during startup.

## Environment

Copy `.env.example` and set values appropriate for your environment.

- `APP_SERVER_BIND_ADDR` - bind address for Axum, e.g. `0.0.0.0:3000`
- `DATABASE_URL` - required in production unless `DATABASE_URL_FILE` is provided for durable workshop state
- `DATABASE_URL_FILE` - optional path to a file containing the database URL; useful for mounted secret volumes in Kubernetes
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

- canonical GitHub/GHCR namespace: `agredyaev/dragon-shift`

- `ci.yml` - validates the Rust workspace, Helm chart, and root production Docker image
- `publish-image.yml` - publishes the root production image to `ghcr.io/agredyaev/dragon-shift` on manual dispatch, on pushes to version tags matching `v*`, and on pushes to `main` only when `platform/**`, `helm/**`, `Dockerfile`, `.github/workflows/**`, or `README.md` changed
- `deploy.yml` - manually deploys, promotes, or rolls back a Helm release using GitHub environment approvals, then verifies `/api/live`, `/api/ready`, and the deployed Playwright smoke path

The CI workflow currently runs:

- `cargo check --workspace --all-targets`
- `cargo test --workspace`
- `cargo run --locked -p xtask -- build-web --out-dir /tmp/app-web-dist`
- browser restart-reconnect proof against local Postgres
- `helm lint ./helm/dragon-shift` with production-like required values
- `helm template ./helm/dragon-shift` with secret-backed database wiring
- `terraform fmt -check -recursive terraform`
- `terraform validate` for bootstrap, foundation, and platform
- `docker build .`

The web bundle and production image now use only Rust tooling: `cargo`, `xtask`, and `wasm-bindgen-cli`. The image publish workflow is intentionally limited to container publishing. It always pushes a requested version tag plus `sha-<short-sha>`, and only adds `latest` for `main` push publishes and version-tag publishes. Cluster deployment remains environment-driven: `deploy.yml` reads Kubernetes credentials and target-specific values from GitHub environment secrets/variables so credentials and mutable deploy values stay outside the repository. The workflow may publish convenience tags, but deployment should pin the resulting `sha256` digest via `image.digest` whenever possible.

Recommended validation before a manual image publish or release tag is to run the same checks that gate `main`: `cargo check --manifest-path platform/Cargo.toml --workspace --all-targets`, `cargo test --manifest-path platform/Cargo.toml --workspace`, `cargo run --manifest-path platform/Cargo.toml -p xtask -- build-web --out-dir /tmp/app-web-dist`, the browser restart proof, the documented `helm lint` and `helm template` commands for `./helm/dragon-shift`, `terraform fmt -check -recursive terraform`, `terraform validate` for bootstrap/foundation/platform, and `docker build -t dragon-shift:local .`. Treat `/api/live`, `/api/ready`, and deployed Playwright smoke as deploy validation from `deploy.yml`, not as part of the GHCR publish trigger.

Minimal GitHub environment setup for `deploy.yml`:

- create GitHub environments such as `staging` and `production`
- add required reviewers to those environments to enforce approval gates before the job starts
- set secret `KUBECONFIG_B64` to a base64-encoded kubeconfig for that environment
- set variable `IMAGE_REPOSITORY`, for example `ghcr.io/agredyaev/dragon-shift`
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
- `database.url`, `database.existingSecretName`, or `database.existingSecretFile`

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
docker buildx imagetools inspect ghcr.io/agredyaev/dragon-shift:v0.1.0
docker buildx imagetools inspect ghcr.io/agredyaev/dragon-shift:main
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

Service metadata is also configurable through `service.annotations`, which the GCP Terraform path uses for NEG and BackendConfig wiring on GKE.

Example render with a direct database URL:

```bash
helm template dragon-shift ./helm/dragon-shift --set image.repository=ghcr.io/agredyaev/dragon-shift --set image.digest=sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef --set app.allowedOrigins=https://dragon-shift.example.com --set app.viteAppUrl=https://dragon-shift.example.com --set postgresql.enabled=false --set database.url=postgres://user:pass@managed-postgres:5432/dragon_shift
```

Recommended cloud-production deployment uses a secret-backed `DATABASE_URL`:

```bash
kubectl create secret generic dragon-shift-app --from-literal=DATABASE_URL='postgres://user:pass@managed-postgres:5432/dragon_shift'
helm upgrade --install dragon-shift ./helm/dragon-shift --set image.repository=ghcr.io/agredyaev/dragon-shift --set image.digest=sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef --set app.allowedOrigins=https://dragon-shift.example.com --set app.viteAppUrl=https://dragon-shift.example.com --set postgresql.enabled=false --set database.existingSecretName=dragon-shift-app --set database.existingSecretKey=DATABASE_URL
```

On GKE or similar managed Kubernetes, point that secret at your managed Postgres or
Cloud SQL connection string and keep it out of committed values files.

For GKE Secret Manager CSI or similar file-mounted secret flows, you can instead
mount the secret as a file and point the chart at it:

```bash
helm upgrade --install dragon-shift ./helm/dragon-shift --set image.repository=ghcr.io/agredyaev/dragon-shift --set image.digest=sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef --set app.allowedOrigins=https://dragon-shift.example.com --set app.viteAppUrl=https://dragon-shift.example.com --set postgresql.enabled=false --set database.existingSecretFile=/var/run/secrets/dragon-shift/DATABASE_URL --set secretManager.enabled=true --set secretManager.secretProviderClassName=dragon-shift-database-url --set secretManager.mountPath=/var/run/secrets/dragon-shift
```

Cloud-operability prerequisites and the remaining operator-owned boundaries are
documented in `operations/CLOUD_OPERABILITY.md`. Production-ready GCP foundation
and platform Terraform now live under `terraform/`; treat the operability doc as
the boundary between repo-managed infrastructure/application deployment and the
still operator-owned concerns such as domain delegation, notification routing,
access governance, and restore execution.

## Terraform on GCP

The repository now includes a production-oriented Terraform path for Google
Cloud under `terraform/`.

- `terraform/bootstrap` - remote-state bucket bootstrap
- `terraform/environments/production/foundation` - project services, VPC,
  private-service access, regional GKE Autopilot, and Cloud SQL for Postgres
- `terraform/environments/production/platform` - Kubernetes namespace/runtime secret wiring,
  GKE ingress edge, Cloud Armor, DNS, uptime checks, and Helm release

Validation commands:

```bash
terraform fmt -check -recursive terraform
terraform -chdir=terraform/bootstrap init -backend=false && terraform -chdir=terraform/bootstrap validate
terraform -chdir=terraform/environments/production/foundation init -backend=false && terraform -chdir=terraform/environments/production/foundation validate
terraform -chdir=terraform/environments/production/platform init -backend=false && terraform -chdir=terraform/environments/production/platform validate
```

See `terraform/README.md` for the apply order, required variables, remote-state
setup, and the GCP architecture rationale.

## Operational References

- `terraform/README.md` - apply order, required variables, and GCP stack boundaries
- `operations/CLOUD_OPERABILITY.md` - operator-owned cloud prerequisites and restore expectations
- `platform/PERSISTENCE_VALIDATION.md` - storage and migration validation
- `e2e/` - browser restart and deployed smoke checks
- `.github/workflows/deploy.yml` - manual deploy, promote, and rollback automation
