# Variable Catalog

## Application Environment
- `APP_SERVER_BIND_ADDR` - Axum bind address
- `VITE_APP_URL` - public app URL used for same-origin bootstrap
- `DATABASE_URL` - Postgres connection string; required in production
- `ALLOWED_ORIGINS` - comma-separated origin allowlist
- `RUST_SESSION_CODE_PREFIX` - optional single-digit workshop code prefix
- `TRUST_X_FORWARDED_FOR` - trust forwarded client IPs only behind a trusted edge
- `CREATE_RATE_LIMIT_MAX` - workshop creation rate limit
- `JOIN_RATE_LIMIT_MAX` - join and reconnect rate limit
- `COMMAND_RATE_LIMIT_MAX` - workshop command rate limit
- `WEBSOCKET_RATE_LIMIT_MAX` - websocket upgrade and message rate limit
- `RECONNECT_TOKEN_TTL_SECONDS` - reconnect token inactivity TTL
- `DATABASE_POOL_SIZE` - Postgres connection pool size
- `LLM_JUDGE_PROVIDERS` - JSON array of judge provider pool entries
- `LLM_IMAGE_PROVIDERS` - JSON array of image provider pool entries
- `LLM_JUDGE_API_KEY_0`, `LLM_JUDGE_API_KEY_1`, … - API keys for judge providers (positional, injected from Kubernetes Secrets)
- `LLM_IMAGE_API_KEY_0`, `LLM_IMAGE_API_KEY_1`, … - API keys for image providers (positional, injected from Kubernetes Secrets)

## Helm Values
- `image.repository` - image repository
- `image.tag` - mutable image tag
- `image.digest` - immutable image digest
- `app.allowedOrigins` - runtime origin allowlist
- `app.viteAppUrl` - runtime base URL
- `app.googleCloudProject` - optional GCP project for server-side Google API calls
- `app.googleCloudLocation` - optional GCP region/location for model routing
- `app.judgeProviders` - ordered provider pool for the judge LLM; each entry has `type` (`vertex_ai` or `api_key`), `model`, and optional `apiKeySecretName`/`apiKeySecretKey`
- `app.imageProviders` - ordered provider pool for image generation; same entry schema as `judgeProviders`
- `app.extraEnv` - additional runtime env entries appended to the container
- `serviceAccount.create` - create a dedicated Kubernetes service account for the app pod
- `serviceAccount.automountServiceAccountToken` - enable projected tokens for Workload Identity / in-cluster auth
- `serviceAccount.annotations` - Kubernetes service account annotations including `iam.gke.io/gcp-service-account`
- `database.url` - inline database URL
- `database.existingSecretName` - Kubernetes secret name for `DATABASE_URL`

## Notes
- LLM provider pools are configured as ordered arrays in Helm values (`judgeProviders` / `imageProviders`). Failover happens left-to-right on 429 or provider failure.
- `vertex_ai` providers use Application Default Credentials (GCE metadata server) and require no API key secret.
- `api_key` providers read their key from a Kubernetes Secret referenced by `apiKeySecretName` / `apiKeySecretKey` in the provider entry.
- GKE Workload Identity additionally requires the corresponding Google IAM binding for the Kubernetes service account principal.
- Browser local/dev API routing can be overridden without code changes by setting the saved Advanced panel address or passing `?apiBaseUrl=https://...` in the page URL.
- `postgresql.enabled` - bundled Postgres toggle

## Terraform Foundation
- `project_id` - GCP project ID
- `region` - GCP region
- `cluster_name` - GKE cluster name
- `db_password` - Cloud SQL application password
- `db_password_version` - write-only password version bump
- `database_url_secret_version` - optional extra Secret Manager version bump
- `support_email` - support contact used by monitoring and ownership flows
- `master_authorized_networks` - allowed control-plane CIDRs

## Terraform Platform
- `hostname` - public production hostname
- `dns_zone_name` - Cloud DNS zone name
- `dns_zone_dns_name` - DNS zone DNS name
- `image_repository` - deployed image repository
- `image_digest` - deployed image digest
- `image_tag` - deployed image tag
- `notification_channel_id` - Monitoring notification channel ID
- `kubeconfig_path` - optional kubeconfig path for platform apply
- `kubeconfig_context` - optional kubeconfig context name

## GitHub Automation
- `GCP_PROJECT_ID` - repository variable for the production GCP project ID
- `GCP_REGION` - repository variable for the production GCP region
- `TF_SUPPORT_EMAIL` - repository variable for the monitoring/operator contact email
- `TF_STATE_BUCKET_NAME` - optional repository variable overriding the default Terraform state bucket name
- `TF_HOSTNAME_MODE` - repository variable for `managed_dns`, `external_dns`, or `nip_io`
- `TF_HOSTNAME` - repository variable for the public hostname when not using `nip_io`
- `TF_DNS_ZONE_NAME` - repository variable for managed Cloud DNS zone name
- `TF_DNS_ZONE_DNS_NAME` - repository variable for managed Cloud DNS zone suffix
- `TF_NIP_IO_LABEL` - repository variable for the `nip.io` hostname label
- `TF_ENABLE_CLOUD_ARMOR` - repository variable to disable Cloud Armor when quota is unavailable
- `TF_ENABLE_UPTIME_CHECKS` - repository variable to opt into Monitoring uptime checks
- `TF_NOTIFICATION_CHANNEL_ID` - repository variable required when `TF_ENABLE_UPTIME_CHECKS=true`
- `TF_EXTRA_MASTER_AUTHORIZED_CIDRS` - repository variable with extra operator IPv4 CIDRs, comma-separated
- `TF_VERIFY_PUBLIC_EDGE` - repository variable to require public HTTPS and browser smoke validation
- `GCP_WORKLOAD_IDENTITY_PROVIDER` - repository secret for the Google Workload Identity Provider resource name
- `GCP_SERVICE_ACCOUNT_EMAIL` - repository secret for the GitHub Actions Terraform service account email
- `TF_PRODUCTION_DB_PASSWORD` - repository secret for the Cloud SQL application password
