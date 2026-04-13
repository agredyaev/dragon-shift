# Deployment Inputs

## GitHub Environment
- `KUBECONFIG_B64` - base64 kubeconfig for the target environment
- `IMAGE_REPOSITORY` - image repository to deploy
- `KUBE_NAMESPACE` - target namespace
- `APP_ALLOWED_ORIGINS` - public origin allowlist for the app
- `APP_VITE_APP_URL` - public base URL used by the frontend
- `VERIFY_URL` - URL used for post-deploy checks
- `HELM_VALUES_FILE` - optional in-repo values file
- `DATABASE_SECRET_NAME` - optional Kubernetes secret name for `DATABASE_URL`
- `DATABASE_SECRET_KEY` - optional secret key name, default `DATABASE_URL`
- `PORT_FORWARD_SERVICE` - optional service override for deployed smoke checks

## Helm Release
- `image.repository` - container image repository
- `image.digest` - immutable image reference preferred for production
- `image.tag` - mutable image reference when explicitly requested
- `app.allowedOrigins` - runtime origin allowlist
- `app.viteAppUrl` - runtime frontend URL
- `app.googleCloudProject` - optional runtime GCP project id for server-side model access
- `app.googleCloudLocation` - optional runtime GCP region/location for server-side model access
- `app.judgeProviders` - ordered provider pool for the judge LLM; each entry has `type` (`vertex_ai` or `api_key`), `model`, and optional `apiKeySecretName`/`apiKeySecretKey`
- `app.imageProviders` - ordered provider pool for image generation; same entry schema as `judgeProviders`
- `app.extraEnv` - extra container env entries
- `serviceAccount.create` - create a Kubernetes service account for the app pod
- `serviceAccount.automountServiceAccountToken` - enable projected service account tokens for Workload Identity / in-cluster auth
- `serviceAccount.annotations` - Kubernetes service account annotations such as `iam.gke.io/gcp-service-account`
- `database.url` - inline database URL
- `database.existingSecretName` - Kubernetes secret name for `DATABASE_URL`
- `database.existingSecretKey` - Kubernetes secret key name

## Notes
- LLM provider pools are configured as ordered arrays. Failover happens left-to-right on 429 or provider failure.
- `vertex_ai` providers use Application Default Credentials (GCE metadata server) and need no API key.
- `api_key` providers read their key from a Kubernetes Secret referenced in the provider entry.
- GKE Workload Identity also requires the matching IAM binding (`roles/iam.workloadIdentityUser`) from the Kubernetes service account to the target Google service account.
- Browser local/dev API routing can be overridden without rebuilding by using the Advanced panel or `?apiBaseUrl=https://...` in the page URL.
