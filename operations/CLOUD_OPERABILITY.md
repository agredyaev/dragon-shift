# Cloud Operability

This document defines what the repo owns and what operators own in production.

## Repo-owned
- application image and runtime config
- Helm chart in `helm/dragon-shift`
- Terraform under `terraform/`
- manual deploy, promote, and rollback workflow
- health endpoints and deploy smoke checks

## Operator-owned
- cluster, namespaces, ingress, and network policy
- DNS, TLS, and certificate rotation
- Cloud SQL lifecycle and backup policy
- runtime secrets and secret rotation
- monitoring, alerts, and on-call response
- restore execution and restore sign-off

## External Requirements
- expose the app only through a trusted ingress or gateway
- terminate TLS at the edge
- normalize forwarded-client headers before traffic reaches the app
- keep app replicas at 1
- use `postgresql.enabled=false` for cloud production

## Restore Baseline
- verify `/api/live` and `/api/ready`
- confirm the app still serves the expected user path
- run `smoke-restore-reconnect` from `platform/PERSISTENCE_VALIDATION.md`
- restore the database and restart or redeploy the app
- confirm reconnect, websocket reattach, and post-restore persistence

## Note
`platform/PERSISTENCE_VALIDATION.md` and `e2e/` are the repo-managed restore checks.
