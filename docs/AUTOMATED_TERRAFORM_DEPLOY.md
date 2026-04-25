# Automated Terraform Deploy

Pushes to `main` use `.github/workflows/publish-image.yml` to:

- publish the production image to `ghcr.io` when app image inputs change, or reuse the current `main` digest for infra-only and smoke-test-only changes
- bootstrap Terraform state if needed
- apply `terraform/environments/production/foundation`
- apply `terraform/environments/production/platform`
- verify `/api/live`, `/api/ready`, and, when public edge verification is enabled, the deployed browser smoke

The production apply waits for the `CI` workflow for the same SHA to succeed before it touches Google Cloud.

Manual `workflow_dispatch` runs of `Publish Image` still publish an image, but the Terraform production apply only runs for `refs/heads/main`.
Direct Helm deploys through `.github/workflows/deploy.yml` are non-production only; production is Terraform-managed.

## Bootstrap Once

Create the Terraform state bucket and GitHub OIDC identity with local operator credentials before the first automated run.

```bash
terraform -chdir=terraform/bootstrap init -reconfigure \
  -backend-config="path=$(pwd)/terraform/bootstrap/<gcp-project-id>.bootstrap.tfstate"
terraform -chdir=terraform/bootstrap apply -auto-approve \
  -var="project_id=<gcp-project-id>" \
  -var="region=<gcp-region>" \
  -var="github_repository_id=<numeric-repo-id>" \
  -var="github_repository_owner_id=<numeric-owner-id>"
```

The bootstrap outputs provide `GCP_WORKLOAD_IDENTITY_PROVIDER` and `GCP_SERVICE_ACCOUNT_EMAIL`.
`TF_PRODUCTION_DB_PASSWORD` is a separate operator-managed secret and is not emitted by Terraform.
The default GitHub ref allowlist is `refs/heads/main`.
The default GitHub event allowlist is `push` plus `workflow_dispatch` for manual `Publish Image` re-runs on `main`.
The bootstrap module intentionally uses a local backend path because it creates the remote state bucket that the other Terraform stacks use.
Keep that local bootstrap state file for operator-driven bootstrap changes and recovery.
If you change bootstrap IAM or Workload Identity settings after the first run, re-apply `terraform/bootstrap` with that saved local state and pass the same `state_bucket_name`, `github_repository_id`, and `github_repository_owner_id` values again before relying on automated deploys.
The default GitHub Actions Terraform roles now include `roles/iam.serviceAccountAdmin` and `roles/resourcemanager.projectIamAdmin` because the production platform stack creates a runtime GSA for Vertex AI and grants it `roles/aiplatform.user`.

## Repository Variables

- `GCP_PROJECT_ID`
- `GCP_REGION`
- `TF_SUPPORT_EMAIL`
- `TF_STATE_BUCKET_NAME`
- `TF_HOSTNAME_MODE`
- `TF_HOSTNAME`
- `TF_DNS_ZONE_NAME`
- `TF_DNS_ZONE_DNS_NAME`
- `TF_NIP_IO_LABEL`
- `TF_ENABLE_CLOUD_ARMOR`
- `TF_ENABLE_UPTIME_CHECKS`
- `TF_NOTIFICATION_CHANNEL_ID`
- `TF_EXTRA_MASTER_AUTHORIZED_CIDRS`
- `TF_VERIFY_PUBLIC_EDGE`
- `TF_GOOGLE_CLOUD_PROJECT`
- `TF_GOOGLE_CLOUD_LOCATION`
- `TF_LLM_PROVIDER_TYPE`
- `TF_LLM_JUDGE_MODEL`
- `TF_LLM_IMAGE_MODEL`
- `TF_RUST_LOG`
- `TF_SPRITE_QUEUE_TIMEOUT_SECONDS`
- `TF_IMAGE_JOB_MAX_CONCURRENCY`

`TF_HOSTNAME_MODE=nip_io` is the zero-DNS default for a fresh project.
`TF_STATE_BUCKET_NAME` is optional and defaults to `<project-id>-tfstate`.
`TF_VERIFY_PUBLIC_EDGE` defaults to `true`; set it to `false` only for an intentionally partial rollout where public DNS or certificate readiness is managed outside the current run.
`TF_NOTIFICATION_CHANNEL_ID` is optional; when unset, the production apply uses the notification channel output from the foundation stack.
`TF_GOOGLE_CLOUD_PROJECT` and `TF_GOOGLE_CLOUD_LOCATION` are optional overrides; by default the runtime uses `GCP_PROJECT_ID` and `GCP_REGION` for Vertex AI.
`TF_LLM_PROVIDER_TYPE` defaults to `vertex_ai`.
For `managed_dns` or `external_dns`, public HTTPS verification depends on DNS delegation or external DNS records outside this repo.

## Repository Secrets

- `GCP_WORKLOAD_IDENTITY_PROVIDER`
- `GCP_SERVICE_ACCOUNT_EMAIL`
- `TF_PRODUCTION_DB_PASSWORD`
- `TF_GEMINI_API_KEY`

## Secret Manager Secrets (operator-managed, out-of-band)

The production platform stack reads these Google Secret Manager secrets at apply time and
projects each into a Kubernetes Secret consumed by the app pod. Operators must create and
populate them **before** the first production apply; the Terraform apply fails otherwise.

- `dragon-shift-production-database-url` - runtime `DATABASE_URL`; version is bumped through
  `TF_DATABASE_URL_SECRET_VERSION` on the foundation stack.
- `dragon-shift-production-session-cookie-key` - base64-encoded random bytes (>=64 decoded
  bytes) used to sign and encrypt session cookies. Generate once per environment with
  `openssl rand -base64 64 | tr -d '\n'` and store as the first version of this secret:
  ```bash
  openssl rand -base64 64 | tr -d '\n' \
    | gcloud secrets create dragon-shift-production-session-cookie-key \
        --project "<gcp-project-id>" \
        --replication-policy=automatic \
        --data-file=-
  ```
  Rotate by adding a new secret version; the platform apply re-reads `latest` on every run
  and triggers a rollout when the projected Kubernetes Secret value changes.

## Migration Rollback

`platform/crates/persistence/migrations/0007_accounts_and_ownership.sql` adds the `accounts`
table and the `characters.owner_account_id` FK. A matching rollback script lives at
`platform/crates/persistence/migrations/0007_accounts_and_ownership.down.sql`.

Rollback procedure (production):

1. Roll the app container image back to the pre-0007 tag (`helm upgrade --set image.tag=<prev>`)
   or re-deploy the previous digest through Terraform to stop writing to `accounts`.
2. Take a logical backup of the affected tables:
   ```bash
   pg_dump --data-only --table=accounts --table=characters \
     "$DATABASE_URL" > ./0007-pre-rollback.sql
   ```
3. Apply the rollback SQL (manual, not run by sqlx migrate):
   ```bash
   psql "$DATABASE_URL" -v ON_ERROR_STOP=1 \
     -f platform/crates/persistence/migrations/0007_accounts_and_ownership.down.sql
   ```
4. Verify the app `/api/ready` returns 200 and character listings still load.

Rollback is destructive: all rows in `accounts` are lost and every character becomes
un-owned. Only execute it with a current backup and coordinated image rollback.

## Local Apply Path

The same bootstrap and verification flow can be run locally:

```bash
GCP_PROJECT_ID=<gcp-project-id> \
GCP_REGION=<gcp-region> \
TF_SUPPORT_EMAIL=<operator-email> \
TF_STATE_BUCKET_NAME=<optional-custom-state-bucket> \
TF_PRODUCTION_DB_PASSWORD=<strong-password> \
IMAGE_REPOSITORY=ghcr.io/agredyaev/dragon-shift \
IMAGE_DIGEST=<sha256:digest> \
bash ./operations/terraform-apply.sh
```

Prerequisites for local use:

- `terraform >= 1.14`
- authenticated `gcloud` with access to the target project
- `kubectl`, `helm`, `npm`, and Playwright browser dependencies available

The script resolves the current public IPv4 for `master_authorized_networks`, preserves extra operator CIDRs from `TF_EXTRA_MASTER_AUTHORIZED_CIDRS`, bootstraps the state bucket when needed, applies both Terraform stacks, verifies rollout health through the cluster, and optionally requires public HTTPS plus the deployed Playwright smoke when `TF_VERIFY_PUBLIC_EDGE=true`.

## Deploy Workarounds

This section documents configuration workarounds applied during the initial production deployment. Each item explains the problem, the fix, and how to revert when the underlying issue is resolved.

### 1. Cloud Armor disabled — `TF_ENABLE_CLOUD_ARMOR=false`

**Problem:** The GCP project `rna-workshop2` has `SECURITY_POLICY_RULES` quota set to `0.0 globally`. Terraform fails with:
```
Error waiting for Creating SecurityPolicy "dragon-shift-production":
Quota 'SECURITY_POLICY_RULES' exceeded. Limit: 0.0 globally.
```

**Workaround:** Set the repository variable `TF_ENABLE_CLOUD_ARMOR=false`. The Terraform platform stack uses `count = var.enable_cloud_armor ? 1 : 0` on the `google_compute_security_policy.app` resource and conditionally omits the `securityPolicy` block from the `BackendConfig` spec. All other infrastructure (Ingress, TLS, health checks, connection draining) continues to function.

**Impact:** No per-IP rate limiting at the load-balancer edge. The application still enforces its own in-process rate limits (`createRateLimitMax`, `joinRateLimitMax`, etc.) via Helm values.

**Revert:** Request a Cloud Armor quota increase in the GCP Console, then set `TF_ENABLE_CLOUD_ARMOR=true` and re-deploy.

### 2. GKE Autopilot `WORKLOADS` monitoring component removed

**Problem:** The `google_container_cluster` resource included `WORKLOADS` in `monitoring_config.enable_components`, which is not supported by GKE Autopilot. Terraform returned a generic `400 Bad Request: invalid argument`.

**Fix (permanent):** Removed `WORKLOADS` from the list in `terraform/modules/gke_autopilot/main.tf`. Only `SYSTEM_COMPONENTS` and `STORAGE` remain. This is the correct configuration for Autopilot clusters and does not need to be reverted.

### 3. Service Networking IAM role added to bootstrap

**Problem:** The Terraform service account lacked permission to create VPC peering for Private Service Access. The apply failed with:
```
Error 403: Permission denied to add peering for service
'servicenetworking.googleapis.com'
```

**Fix (permanent):** Added `roles/servicenetworking.networksAdmin` to the SA roles in `terraform/bootstrap/variables.tf`. After changing bootstrap IAM, the bootstrap stack was re-applied with the saved local state (see **Bootstrap Once** above).

### 4. Workflow dispatch deploy support

**Problem:** The `publish-image.yml` deploy job only ran for `push` events. Manual re-deploys via `workflow_dispatch` were impossible without creating empty commits.

**Fix (permanent):** Updated the deploy job condition to `(github.event_name == 'push' || github.event_name == 'workflow_dispatch') && github.ref == 'refs/heads/main'`. Also gated the CI-wait step inside the deploy job with `if: github.event_name == 'push'` since manual dispatches are intentional operator actions that do not require gating on CI completion.

### 5. Vertex AI bootstrap IAM for the GitHub Terraform deployer

**Problem:** The production platform stack now creates the application GSA `dragon-shift-app` and binds `roles/aiplatform.user` for Workload Identity based Vertex AI access. Existing bootstrap state that predates this change leaves the GitHub Actions Terraform service account without permission to create service accounts or edit project IAM, which causes production apply failures such as:
```
Error creating service account: googleapi: Error 403: Permission 'iam.serviceAccounts.create' denied
```

**Fix (permanent):** Added `roles/iam.serviceAccountAdmin` and `roles/resourcemanager.projectIamAdmin` to `terraform/bootstrap/variables.tf`. After pulling this change, re-apply `terraform/bootstrap` with the original local bootstrap state before re-running `Publish Image` for production.
