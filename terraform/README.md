## Dragon Shift Terraform

Production-ready Terraform for Dragon Shift on Google Cloud lives under this
directory. The layout follows the current application constraints:

- Kubernetes is the canonical production runtime.
- The app remains singleton-only and must run with exactly one replica.
- Cloud SQL is used as the managed Postgres system of record.
- Public traffic enters only through a Google Cloud external HTTP(S) load balancer
  backed by GKE ingress and Cloud Armor.

### Layout

- `bootstrap/` creates the GCS remote-state bucket.
- `modules/` contains reusable building blocks.
- `environments/production/foundation/` creates project services, networking,
  private service access, GKE Autopilot, and Cloud SQL.
- `environments/production/platform/` creates namespace-level app resources,
  DNS, ingress edge controls, uptime monitoring, and the Helm release.

### Why GKE instead of Cloud Run

This repository already defines Kubernetes and Helm as the canonical deployment
path, and the runtime is not horizontally scalable today. The app uses
WebSockets, in-process realtime ownership, singleton rollout expectations, and a
Kubernetes health/deploy workflow. GKE aligns with the existing production model
without inventing a second deployment architecture.

### Architecture Summary

- GKE Autopilot regional cluster in a dedicated VPC subnet
- Private GKE nodes with configurable control-plane authorized networks
- Cloud SQL for PostgreSQL with private IP only, backups, PITR, and deletion protection
- Secret Manager for application `DATABASE_URL`, mounted at runtime through the GKE Secret Manager CSI add-on
- Workload Identity Federation for GKE using the Kubernetes service account principal directly
- GKE ingress with managed certificate, static global IP, and Cloud Armor policy
- NEG-backed singleton service
- Cloud DNS public zone and A record for the production hostname
- Uptime checks and alerting policy for `/api/live` and `/api/ready`

### Apply Order

1. Bootstrap remote state.
2. Apply `foundation`.
3. Apply `platform`.

`platform` supports two control-plane access modes:

- direct endpoint mode using the foundation stack's cluster endpoint outputs; this
  requires the caller's egress IP to be included in `master_authorized_networks`
- kubeconfig mode using `kubeconfig_path` and optional `kubeconfig_context`,
  which is suitable for a Connect Gateway kubeconfig or another pre-approved
  kubeconfig flow

Example:

```bash
terraform -chdir=terraform/bootstrap init
terraform -chdir=terraform/bootstrap apply \
  -var='project_id=my-gcp-project' \
  -var='region=europe-west4'

terraform -chdir=terraform/environments/production/foundation init \
  -backend-config='bucket=<state-bucket-name>' \
  -backend-config='prefix=production/foundation'
terraform -chdir=terraform/environments/production/foundation apply -var-file=terraform.tfvars

terraform -chdir=terraform/environments/production/platform init \
  -backend-config='bucket=<state-bucket-name>' \
  -backend-config='prefix=production/platform'
terraform -chdir=terraform/environments/production/platform apply -var-file=terraform.tfvars
```

Example with a Connect Gateway kubeconfig:

```bash
gcloud container fleet memberships get-credentials <membership-name>
terraform -chdir=terraform/environments/production/platform apply \
  -var-file=terraform.tfvars \
  -var='kubeconfig_path=$HOME/.kube/config' \
  -var='kubeconfig_context=gke_<context-from-get-credentials>'
```

### Validation

```bash
terraform fmt -check -recursive terraform
terraform -chdir=terraform/bootstrap init -backend=false && terraform -chdir=terraform/bootstrap validate
terraform -chdir=terraform/environments/production/foundation init -backend=false && terraform -chdir=terraform/environments/production/foundation validate
terraform -chdir=terraform/environments/production/platform init -backend=false && terraform -chdir=terraform/environments/production/platform validate
```

### Required Inputs

Foundation requires at least:

- `project_id`
- `region`
- `cluster_name`
- `db_password`
- `db_password_version`
- `database_url_secret_version`
- `support_email`

Platform requires at least:

- `project_id`
- `region`
- `cluster_name`
- `hostname`
- `dns_zone_name`
- `dns_zone_dns_name`
- `image_repository`
- one of `image_digest` or `image_tag`

Platform optionally accepts:

- `cluster_location`
- `database_url_secret_id`
- `notification_channel_id`
- `kubeconfig_path`
- `kubeconfig_context`

### Important Constraints

- Keep `replicaCount=1`.
- Keep the Helm deployment strategy as `Recreate`.
- Do not expose the app through `Service.type=LoadBalancer`.
- Keep `postgresql.enabled=false` in cloud production.
- Cloud SQL uses private IP; no public database endpoint is created.

### Operational Notes

- Foundation intentionally separates long-lived project and data-plane resources
  from more frequently changing application deployment resources.
- Platform now reads the target cluster directly from Google APIs instead of the
  full foundation remote state, which narrows cross-stack access requirements.
- Set `master_authorized_networks` in the foundation stack to the operator or CI
  runner egress CIDRs that are allowed to reach the GKE public control-plane
  endpoint. The cluster module now disables broad Google public CIDR access by
  default.
- The platform stack waits for the Secret Manager CSI driver and the GKE ingress
  CRD APIs to become reachable before creating `SecretProviderClass`,
  `BackendConfig`, and `ManagedCertificate` resources. This reduces fresh-cluster
  first-apply races, but the caller still needs working Kubernetes API access.
  In kubeconfig mode the readiness gate uses `kubectl --raw` through the supplied
  kubeconfig; in direct endpoint mode it probes the cluster endpoint directly.
- Platform creates alerting primitives, but notification channel destinations are
  still operator-owned because they usually depend on environment-specific email,
  paging, or ticketing policies.
- Set `enable_uptime_checks=true` only after DNS delegation is complete and the
  managed certificate has become active, otherwise fresh environments will alert
  during expected cutover time.
- Terraform now uses write-only attributes for the Cloud SQL user password and
  Secret Manager secret payload so those values do not persist in Terraform state.
- Terraform 1.14 or later is required because the configuration relies on
  write-only resource attributes.
- The `DATABASE_URL` secret payload version now advances automatically when the
  rendered connection string changes, such as password rotation or Cloud SQL
  private IP replacement.
- Increment `db_password_version` when rotating the database password, and use
  `database_url_secret_version` only when you want to force an extra secret
  version even if the rendered payload is otherwise unchanged.
