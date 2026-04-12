# Variable Catalog

## Application Environment
- `APP_SERVER_BIND_ADDR` - Axum bind address
- `VITE_APP_URL` - public app URL used for same-origin bootstrap
- `DATABASE_URL` - Postgres connection string; required unless `DATABASE_URL_FILE` is set
- `DATABASE_URL_FILE` - path to a file containing the database URL
- `ALLOWED_ORIGINS` - comma-separated origin allowlist
- `RUST_SESSION_CODE_PREFIX` - optional single-digit workshop code prefix
- `TRUST_X_FORWARDED_FOR` - trust forwarded client IPs only behind a trusted edge
- `CREATE_RATE_LIMIT_MAX` - workshop creation rate limit
- `JOIN_RATE_LIMIT_MAX` - join and reconnect rate limit
- `COMMAND_RATE_LIMIT_MAX` - workshop command rate limit
- `WEBSOCKET_RATE_LIMIT_MAX` - websocket upgrade and message rate limit
- `RECONNECT_TOKEN_TTL_SECONDS` - reconnect token inactivity TTL
- `DATABASE_POOL_SIZE` - Postgres connection pool size
- `VITE_GEMINI_API_KEY` - browser-side Gemini key for sprite generation

## Helm Values
- `image.repository` - image repository
- `image.tag` - mutable image tag
- `image.digest` - immutable image digest
- `app.allowedOrigins` - runtime origin allowlist
- `app.viteAppUrl` - runtime base URL
- `database.url` - inline database URL
- `database.existingSecretName` - Kubernetes secret name for `DATABASE_URL`
- `database.existingSecretFile` - file path for mounted secret workflows
- `secretManager.enabled` - enable Secret Manager CSI mount
- `secretManager.secretProviderClassName` - CSI provider class name
- `secretManager.mountPath` - secret mount path
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
