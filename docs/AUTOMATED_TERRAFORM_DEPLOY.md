# Automated Terraform Deploy

Pushes to `main` use `.github/workflows/publish-image.yml` to:

- publish the production image to `ghcr.io` when app image inputs change, or reuse the current `main` digest for infra-only and smoke-test-only changes
- bootstrap Terraform state if needed
- apply `terraform/environments/production/foundation`
- apply `terraform/environments/production/platform`
- verify `/api/live`, `/api/ready`, and, when public edge verification is enabled, the deployed browser smoke

The production apply waits for the `CI` workflow for the same SHA to succeed before it touches Google Cloud.

Manual `workflow_dispatch` runs of `Publish Image` still publish an image, but the Terraform production apply only runs for `refs/heads/main`.

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
The bootstrap module intentionally uses a local backend path because it creates the remote state bucket that the other Terraform stacks use.
Keep that local bootstrap state file for operator-driven bootstrap changes and recovery.
If you change bootstrap IAM or Workload Identity settings after the first run, re-apply `terraform/bootstrap` with that saved local state and pass the same `state_bucket_name`, `github_repository_id`, and `github_repository_owner_id` values again before relying on automated deploys.

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

`TF_HOSTNAME_MODE=nip_io` is the zero-DNS default for a fresh project.
`TF_STATE_BUCKET_NAME` is optional and defaults to `<project-id>-tfstate`.
`TF_VERIFY_PUBLIC_EDGE` defaults to `true`; set it to `false` only for an intentionally partial rollout where public DNS or certificate readiness is managed outside the current run.
For `managed_dns` or `external_dns`, public HTTPS verification depends on DNS delegation or external DNS records outside this repo.

## Repository Secrets

- `GCP_WORKLOAD_IDENTITY_PROVIDER`
- `GCP_SERVICE_ACCOUNT_EMAIL`
- `TF_PRODUCTION_DB_PASSWORD`

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
