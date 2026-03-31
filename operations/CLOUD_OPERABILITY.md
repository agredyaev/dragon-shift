# Cloud Operability

This repository contains the application, Helm chart, deploy workflow, and a
production-oriented Terraform path for Google Cloud under `terraform/`.
Production operation still depends on a smaller set of external assets and
decisions that remain owned by platform or operations staff.

## Ownership Boundary

Repo-owned:

- application container image and app runtime configuration surface
- Helm chart in `helm/dragon-shift`
- Terraform under `terraform/` for GCP state bootstrap, foundation, and platform
- manual deploy, promote, and rollback workflow in `.github/workflows/deploy.yml`
- app health endpoints and deployed smoke checks described in `README.md`

Operator-owned:

- Kubernetes cluster, namespaces, network policy, and ingress or gateway setup
- DNS, public hostnames, TLS certificates, and certificate rotation
- managed Postgres or Cloud SQL provisioning and lifecycle policies
- runtime secret material and access review for production credentials
- monitoring, alerting, log retention, dashboards, and on-call escalation
- backup, PITR, restore execution, and restore verification scheduling

Cloud readiness should be evaluated against both sides of that boundary. The repo
can now provision the core GCP foundation and application platform for the
current supported production shape, but operators still own the access,
governance, approval, and restore practices around it.

## Required External Prerequisites

Before calling an environment production-ready, operators must provide the
following external dependencies.

### Ingress, TLS, and Backend Policy

- expose the app only through a trusted ingress or gateway; do not expose the app
  service or pods directly to the public internet
- terminate TLS at that trusted edge
- normalize or overwrite forwarded-client headers before traffic reaches the app
- enforce request or connection abuse controls such as rate limits
- configure backend behavior suitable for websocket traffic, health checks, and
  singleton rollout expectations
- keep app replicas at `1` and avoid overlap/surge behavior while the runtime
  remains single-authority

This repo includes portable Helm ingress support, but environment-specific cloud
objects such as managed certificates, static IPs, backend policies, gateway
resources, WAF settings, or controller-specific annotations remain operator-owned.

### Secret Management

- provide `DATABASE_URL` through `database.existingSecretName`,
  `database.existingSecretFile`, or another operator-managed secret source rather
  than committed values
- manage secret creation, rotation, access review, and incident response outside
  the repo
- provide any required registry pull credentials when GHCR access is private
- do not rely on browser-supplied third-party tokens for production workflows;
  long-lived integration credentials must move to server-side or operator-managed
  secret references

### Monitoring, Alerting, and Logging

- collect app, ingress, and database logs in centralized retention outside pod
  local storage
- alert on at least failed deploys, crash loops, readiness failures, elevated 5xx
  rates, and database availability issues
- maintain dashboards or equivalent views for request health, websocket/session
  behavior, and database saturation or storage risk
- ensure on-call ownership is explicit for deploy failures, runtime incidents, and
  restore decisions

The repository provides health endpoints and a deploy-time smoke test, but it does
not provision a full observability stack.

### Cloud SQL, Backups, and PITR

- use external managed Postgres such as Cloud SQL for cloud production and set
  `postgresql.enabled=false`
- enable automated backups and point-in-time recovery according to the target
  environment's recovery objectives
- verify that retention windows are long enough to cover detection and response
  time for bad deploys or operator mistakes
- assume Helm rollback does not reverse forward-only schema migrations; database
  recovery may require PITR or a corrective migration

This repo documents how to connect to managed Postgres, but backup policy and PITR
capability are operator-owned prerequisites.

## Restore Runbook Expectations

Operators should maintain an environment-specific restore runbook outside or
alongside this repo. For staging, the repository-contained baseline is the
validation sequence below. At minimum any environment runbook should define:

- who is authorized to declare a restore event and who executes it
- which backup or PITR target is selected and how that decision is recorded
- how application deploy state is coordinated with database restore timing
- how DNS, ingress, and secret dependencies are checked before reopening traffic
- how post-restore verification is performed and who signs off

### Staging Restore Validation Baseline

Use this when proving that a staging restore preserves restart, reconnect, and
session continuity for the current singleton runtime.

1. Record the restore target.
2. Confirm staging points at the intended Postgres instance and app URL.
3. Confirm the deployed app is healthy before the exercise:
   - `GET /api/live`
   - `GET /api/ready`
4. Confirm the normal user path still works before the restore:
   - run the deployed smoke used by the deploy workflow, or an equivalent staging user-path check
5. Start a restore validation checkpoint from this repo:

```bash
cargo run --manifest-path platform/Cargo.toml -p xtask -- smoke-restore-reconnect --base-url https://<staging-url> --database-url postgres://<staging-db> --restart-timeout-seconds 300
```

The command creates a real workshop, moves it to `phase1`, confirms persistence in
`workshop_sessions`, `session_artifacts`, and `player_identities`, then waits for an
observed app readiness outage and recovery while operators execute the restore.

6. While the command is waiting, execute the staging restore and restart or redeploy
   the app against the restored database.
7. Let the command finish. Treat success as proof that the restored environment:
   - returned to healthy `/api/live` and `/api/ready`
   - reconnected the original player over the public HTTP API
   - preserved the same session and player identity after restart
   - accepted a websocket attach after restart
   - accepted a new `SubmitObservation` command after restart
   - persisted the continued session back into Postgres with artifact growth
8. Re-run the deployed smoke or equivalent public user-path verification after the
   restore completes.
9. Record the session code, restore target timestamp, operator, and smoke outputs in
   the staging restore log.

### Minimum Post-Restore Verification

- app starts successfully and passes `/api/live` and `/api/ready`
- deployed smoke or equivalent user-path verification passes
- expected workshop/session data is present in the restored database
- reconnect behavior is verified through a real post-restart join using the original session
- websocket reattach is verified after restart
- command and persistence continuity are verified by successfully submitting one additional HTTP command and confirming artifact growth afterward

`platform/PERSISTENCE_VALIDATION.md` and `e2e/` are the repository-contained
verification inputs. Restore execution, backup selection, traffic coordination,
and sign-off remain operator responsibilities.

## Caveat

These docs bound the repository's cloud-readiness claims. Dragon Shift can be
deployed from this repo into Kubernetes, but safe cloud operation still depends on
the operator-owned prerequisites above being implemented and exercised.
