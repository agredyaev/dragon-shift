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
- `database.url` - inline database URL
- `database.existingSecretName` - Kubernetes secret name for `DATABASE_URL`
- `database.existingSecretKey` - Kubernetes secret key name
- `database.existingSecretFile` - mounted secret file path
