#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PLATFORM_DIR="$ROOT_DIR/platform"
ADC_FILE="${HOME}/.config/gcloud/application_default_credentials.json"
GCP_PROJECT_ID="${GCP_PROJECT_ID:-rna-workshop2}"
LOCAL_GCP_SERVICE_ACCOUNT_ID="${LOCAL_GCP_SERVICE_ACCOUNT_ID:-dragon-shift-local-kube}"
LOCAL_GCP_SERVICE_ACCOUNT_EMAIL="${LOCAL_GCP_SERVICE_ACCOUNT_EMAIL:-${LOCAL_GCP_SERVICE_ACCOUNT_ID}@${GCP_PROJECT_ID}.iam.gserviceaccount.com}"

KIND_CLUSTER_NAME="dragon-shift-local"
KUBE_CONTEXT="kind-dragon-shift-local"
NAMESPACE="dragon-shift"
RELEASE_NAME="dragon-shift"
IMAGE_NAME="dragon-shift-rust:kind-local"
ADC_SECRET_NAME="dragon-shift-gcp-adc"
SESSION_COOKIE_SECRET_NAME="dragon-shift-session-cookie-key"

HOST_ARCH="$(uname -m)"
case "$HOST_ARCH" in
  arm64|aarch64)
    APP_SERVER_TARGET_TRIPLE="aarch64-unknown-linux-gnu"
    ;;
  x86_64|amd64)
    APP_SERVER_TARGET_TRIPLE="x86_64-unknown-linux-gnu"
    ;;
  *)
    printf 'Unsupported host architecture: %s\n' "$HOST_ARCH" >&2
    exit 1
    ;;
esac

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'Missing required command: %s\n' "$1" >&2
    exit 1
  fi
}

need_cmd cargo
need_cmd docker
need_cmd gcloud
need_cmd kind
need_cmd kubectl
need_cmd helm
need_cmd openssl

if ! command -v cargo-zigbuild >/dev/null 2>&1 && ! cargo zigbuild --help >/dev/null 2>&1; then
  printf 'cargo zigbuild is required\n' >&2
  exit 1
fi

if [ ! -f "$ADC_FILE" ]; then
  printf 'ADC file not found: %s\n' "$ADC_FILE" >&2
  printf 'Run: gcloud auth application-default login\n' >&2
  exit 1
fi

GCLOUD_ACCOUNT="$(gcloud config get-value account 2>/dev/null || true)"
if [ -z "$GCLOUD_ACCOUNT" ] || [ "$GCLOUD_ACCOUNT" = "(unset)" ]; then
  printf 'No active gcloud account found. Run: gcloud auth login\n' >&2
  exit 1
fi

printf 'Ensuring local GCP service account %s exists...\n' "$LOCAL_GCP_SERVICE_ACCOUNT_EMAIL"
if ! gcloud iam service-accounts describe "$LOCAL_GCP_SERVICE_ACCOUNT_EMAIL" \
  --project "$GCP_PROJECT_ID" >/dev/null 2>&1; then
  gcloud iam service-accounts create "$LOCAL_GCP_SERVICE_ACCOUNT_ID" \
    --project "$GCP_PROJECT_ID" \
    --display-name "Dragon Shift local kind" >/dev/null
fi

printf 'Ensuring local service account can call Vertex AI...\n'
gcloud projects add-iam-policy-binding "$GCP_PROJECT_ID" \
  --member="serviceAccount:${LOCAL_GCP_SERVICE_ACCOUNT_EMAIL}" \
  --role="roles/aiplatform.user" \
  --quiet >/dev/null

printf 'Ensuring %s can impersonate the local service account...\n' "$GCLOUD_ACCOUNT"
gcloud iam service-accounts add-iam-policy-binding "$LOCAL_GCP_SERVICE_ACCOUNT_EMAIL" \
  --project "$GCP_PROJECT_ID" \
  --member="user:${GCLOUD_ACCOUNT}" \
  --role="roles/iam.serviceAccountTokenCreator" \
  --quiet >/dev/null

printf 'Verifying local service account impersonation...\n'
for _ in 1 2 3 4 5 6; do
  if gcloud auth print-access-token \
    --impersonate-service-account="$LOCAL_GCP_SERVICE_ACCOUNT_EMAIL" \
    --quiet >/dev/null 2>&1; then
    break
  fi
  sleep 5
done
if ! gcloud auth print-access-token \
  --impersonate-service-account="$LOCAL_GCP_SERVICE_ACCOUNT_EMAIL" \
  --quiet >/dev/null; then
  printf 'Failed to impersonate %s after granting roles. Check IAM propagation and your gcloud account.\n' "$LOCAL_GCP_SERVICE_ACCOUNT_EMAIL" >&2
  exit 1
fi

printf 'Building web assets...\n'
cargo run --manifest-path "$PLATFORM_DIR/Cargo.toml" -p xtask -- build-web

printf 'Building Linux %s app-server...\n' "$APP_SERVER_TARGET_TRIPLE"
cargo zigbuild --manifest-path "$PLATFORM_DIR/Cargo.toml" -p app-server --release --target "$APP_SERVER_TARGET_TRIPLE"

printf 'Building local Docker image...\n'
docker build \
  --build-arg "APP_SERVER_TARGET_TRIPLE=$APP_SERVER_TARGET_TRIPLE" \
  -f "$ROOT_DIR/Dockerfile.local" \
  -t "$IMAGE_NAME" \
  "$ROOT_DIR"

printf 'Ensuring namespace exists...\n'
kubectl --context "$KUBE_CONTEXT" create namespace "$NAMESPACE" --dry-run=client -o yaml | kubectl --context "$KUBE_CONTEXT" apply -f -

printf 'Refreshing ADC secret...\n'
kubectl --context "$KUBE_CONTEXT" -n "$NAMESPACE" create secret generic "$ADC_SECRET_NAME" \
  --from-file=credentials.json="$ADC_FILE" \
  --dry-run=client -o yaml | kubectl --context "$KUBE_CONTEXT" apply -f -

printf 'Refreshing session cookie secret...\n'
SESSION_COOKIE_KEY_VALUE="$(openssl rand -base64 64)"
kubectl --context "$KUBE_CONTEXT" -n "$NAMESPACE" create secret generic "$SESSION_COOKIE_SECRET_NAME" \
  --from-literal=SESSION_COOKIE_KEY="$SESSION_COOKIE_KEY_VALUE" \
  --dry-run=client -o yaml | kubectl --context "$KUBE_CONTEXT" apply -f -

printf 'Loading image into kind...\n'
kind load docker-image "$IMAGE_NAME" --name "$KIND_CLUSTER_NAME"

printf 'Upgrading local Helm release...\n'
helm upgrade --install "$RELEASE_NAME" "$ROOT_DIR/helm/dragon-shift" \
  --kube-context "$KUBE_CONTEXT" \
  --namespace "$NAMESPACE" \
  --create-namespace \
  --reset-values \
  -f "$ROOT_DIR/helm/dragon-shift/values.kind-local.yaml" \
  --set "app.googleCloudProject=${GCP_PROJECT_ID}" \
  --set "app.extraEnv[0].name=GOOGLE_IMPERSONATE_SERVICE_ACCOUNT" \
  --set "app.extraEnv[0].value=${LOCAL_GCP_SERVICE_ACCOUNT_EMAIL}"

printf 'Restarting deployment...\n'
kubectl --context "$KUBE_CONTEXT" -n "$NAMESPACE" rollout restart deploy/dragon-shift-dragon-shift
kubectl --context "$KUBE_CONTEXT" -n "$NAMESPACE" rollout status deploy/dragon-shift-dragon-shift --timeout=300s

printf 'Verifying live endpoint on kind host port 4100...\n'
for _ in 1 2 3 4 5; do
  if curl -fsS "http://127.0.0.1:4100/api/live"; then
    printf '\nDone. Local app is available at http://127.0.0.1:4100\n'
    exit 0
  fi
  sleep 2
done

printf 'Live endpoint did not become ready at http://127.0.0.1:4100/api/live\n' >&2
exit 1
