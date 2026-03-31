# Dragon Shift Terraform

Terraform here provisions the GCP production path for Dragon Shift.

## Layout
- `bootstrap/` - remote-state bucket
- `environments/production/foundation/` - project services, network, GKE, Cloud SQL
- `environments/production/platform/` - DNS, ingress edge, monitoring, Helm release

## Order
1. Bootstrap remote state.
2. Apply `foundation`.
3. Apply `platform`.

## Constraints
- Kubernetes is the production runtime.
- The app stays single-replica.
- Cloud SQL uses private IP only.
- `postgresql.enabled=false` in cloud production.
- Use `master_authorized_networks` or `kubeconfig_path` for cluster access.

## Notes
- The platform stack reads the cluster from Google APIs.
- It waits for ingress and Secret Manager CSI surfaces before dependent resources.
- Terraform 1.14+ is required.

## Validation
```bash
terraform fmt -check -recursive terraform
terraform -chdir=terraform/bootstrap init -backend=false && terraform -chdir=terraform/bootstrap validate
terraform -chdir=terraform/environments/production/foundation init -backend=false && terraform -chdir=terraform/environments/production/foundation validate
terraform -chdir=terraform/environments/production/platform init -backend=false && terraform -chdir=terraform/environments/production/platform validate
```
