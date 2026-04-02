# Dragon Shift

[![Rust](https://img.shields.io/badge/Rust-1.78%2B-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Dioxus](https://img.shields.io/badge/Dioxus-Frontend-7C3AED?logo=rust&logoColor=white)](https://dioxuslabs.com/)
[![Axum](https://img.shields.io/badge/Axum-API%20%2B%20WebSocket-000000?logo=rust&logoColor=white)](https://github.com/tokio-rs/axum)
[![PostgreSQL](https://img.shields.io/badge/PostgreSQL-Persistence-316192?logo=postgresql&logoColor=white)](https://www.postgresql.org/)
[![Docker](https://img.shields.io/badge/Docker-Image-2496ED?logo=docker&logoColor=white)](https://www.docker.com/)
[![Terraform](https://img.shields.io/badge/Terraform-Infrastructure-844FBA?logo=terraform&logoColor=white)](https://www.terraform.io/)
[![Kubernetes](https://img.shields.io/badge/Kubernetes-GKE-326CE5?logo=kubernetes&logoColor=white)](https://kubernetes.io/)
[![Google Cloud](https://img.shields.io/badge/Google%20Cloud-Production-4285F4?logo=googlecloud&logoColor=white)](https://cloud.google.com/)

Dragon Shift is a real-time multiplayer workshop game built in Rust.
It ships as a browser client and Rust backend, persists game state in PostgreSQL, and is deployed on Google Cloud through Terraform and Helm.

## What it does
- Serves the game UI in the browser and the game API from one container image.
- Streams live game updates over WebSocket.
- Stores persistent game and account data in PostgreSQL.
- Runs production traffic behind GKE ingress with TLS.
- Packages the runtime and deployment stack for reproducible cloud releases.

## Core Stack
- `platform/` is a Rust workspace with these modules:
  - `app-server` - Axum HTTP and WebSocket entrypoint, request handling, runtime config, and serving the built frontend assets.
  - `app-web` - Dioxus frontend and browser-side UI state.
  - `crates/domain` - game rules, core types, and domain validation.
  - `crates/persistence` - Postgres schema, migrations, and database access.
  - `crates/protocol` - API payloads and realtime message types.
  - `crates/realtime` - session ownership and realtime coordination.
  - `crates/security` - token and identity helpers.
  - `xtask` - build, packaging, and smoke-test automation.
- `helm/dragon-shift` for the Kubernetes deployment, ingress, secrets, and certificate wiring.
- `terraform/` for GCP bootstrap, network, GKE, Cloud SQL, and platform infrastructure.

## Deployment Model
- `terraform/bootstrap` creates the remote state bucket.
- `terraform/environments/production/foundation` provisions the project services, VPC, GKE, and Cloud SQL.
- `terraform/environments/production/platform` wires DNS, ingress, TLS, monitoring, and the Helm release.

## Docs
- Cloud ownership and operator responsibilities: `docs/CLOUD_OPERABILITY.md`
- Terraform stack and apply order: `terraform/README.md`
- Repo map: `docs/REPO_MAP.md`
