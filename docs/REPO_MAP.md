# Repository Map

## Runtime
- `platform/` - Rust workspace for the app server, web client, shared crates, and tests
- `platform/app-server` - Axum HTTP/WebSocket entrypoint and runtime config
- `platform/app-web` - Dioxus frontend and UI state
- `platform/crates/domain` - core workshop domain types and rules
- `platform/crates/persistence` - Postgres schema, migrations, and database access
- `platform/crates/protocol` - API and realtime message types
- `platform/crates/realtime` - websocket/realtime coordination
- `platform/crates/security` - token and identity helpers
- `platform/xtask` - build and smoke-test automation

## Delivery
- `Dockerfile` - production image build
- `helm/dragon-shift` - Kubernetes deployment chart
- `.github/workflows/ci.yml` - repo checks and chart validation
- `.github/workflows/publish-image.yml` - GHCR image publishing
- `.github/workflows/deploy.yml` - manual deploy, promote, and rollback flow
- `e2e/` - deployed browser smoke and local restart proofs

## Infrastructure
- `terraform/bootstrap` - remote-state bucket bootstrap
- `terraform/environments/production/foundation` - GCP project, network, GKE, and Cloud SQL
- `terraform/environments/production/platform` - DNS, ingress edge, secrets, monitoring, and Helm release
- `terraform/modules/*` - reusable Terraform building blocks

## Supporting Docs
- `README.md` - project overview and canonical usage
- `docs/VARIABLE_CATALOG.md` - runtime, Helm, and Terraform input catalog
- `.env.example` - local environment variable reference
- `operations/CLOUD_OPERABILITY.md` - operator-owned cloud boundaries
- `platform/ARCHITECTURE.md` - runtime architecture notes
- `platform/rust-toolchain.toml` - pinned Rust toolchain and wasm target
- `platform/Cargo.toml` - workspace manifest and dependency versions
- `platform/PERSISTENCE_VALIDATION.md` - storage and migration checks
- `terraform/README.md` - GCP Terraform apply guidance and stack boundaries

## Top-Level Config
- `.github/workflows/*` - CI, publish, and deploy automation
- `.gitignore` - repo ignore rules
- `Dockerfile` - production image build
