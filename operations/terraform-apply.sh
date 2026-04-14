#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

PROJECT_ID="${GCP_PROJECT_ID:?GCP_PROJECT_ID is required}"
REGION="${GCP_REGION:-europe-west4}"
SUPPORT_EMAIL="${TF_SUPPORT_EMAIL:?TF_SUPPORT_EMAIL is required}"
DB_PASSWORD="${TF_PRODUCTION_DB_PASSWORD:?TF_PRODUCTION_DB_PASSWORD is required}"
IMAGE_REPOSITORY="${IMAGE_REPOSITORY:-ghcr.io/agredyaev/dragon-shift}"
IMAGE_DIGEST="${IMAGE_DIGEST:-}"
IMAGE_TAG="${IMAGE_TAG:-main}"
HOSTNAME_MODE="${TF_HOSTNAME_MODE:-nip_io}"
HOSTNAME="${TF_HOSTNAME:-}"
DNS_ZONE_NAME="${TF_DNS_ZONE_NAME:-}"
DNS_ZONE_DNS_NAME="${TF_DNS_ZONE_DNS_NAME:-}"
NIP_IO_LABEL="${TF_NIP_IO_LABEL:-dragon-shift}"
ENABLE_CLOUD_ARMOR="${TF_ENABLE_CLOUD_ARMOR:-true}"
ENABLE_UPTIME_CHECKS="${TF_ENABLE_UPTIME_CHECKS:-false}"
NOTIFICATION_CHANNEL_ID="${TF_NOTIFICATION_CHANNEL_ID:-}"
CLUSTER_NAME="${TF_CLUSTER_NAME:-dragon-shift-prod}"
NETWORK_NAME="${TF_NETWORK_NAME:-dragon-shift-prod}"
STATE_BUCKET_NAME="${TF_STATE_BUCKET_NAME:-${PROJECT_ID}-tfstate}"
STATE_PREFIX_BASE="${TF_STATE_PREFIX_BASE:-production}"
NAMESPACE="${TF_NAMESPACE:-dragon-shift}"
DB_PASSWORD_VERSION="${TF_DB_PASSWORD_VERSION:-1}"
DATABASE_URL_SECRET_VERSION="${TF_DATABASE_URL_SECRET_VERSION:-1}"
EXTRA_MASTER_AUTHORIZED_CIDRS="${TF_EXTRA_MASTER_AUTHORIZED_CIDRS:-}"
RUN_DEPLOYED_SMOKE="${RUN_DEPLOYED_SMOKE:-true}"
RUNNER_PUBLIC_IPV4="${RUNNER_PUBLIC_IPV4:-}"
VERIFY_PUBLIC_EDGE="${TF_VERIFY_PUBLIC_EDGE:-}"
GOOGLE_CLOUD_PROJECT="${TF_GOOGLE_CLOUD_PROJECT:-}"
GOOGLE_CLOUD_LOCATION="${TF_GOOGLE_CLOUD_LOCATION:-}"
LLM_PROVIDER_TYPE="${TF_LLM_PROVIDER_TYPE:-vertex_ai}"
LLM_JUDGE_MODEL="${TF_LLM_JUDGE_MODEL:-gemini-2.5-flash}"
LLM_IMAGE_MODEL="${TF_LLM_IMAGE_MODEL:-gemini-2.5-flash-preview-04-17}"
RUST_LOG="${TF_RUST_LOG:-info,tower_http=debug}"
GEMINI_API_KEY="${TF_GEMINI_API_KEY:-}"

if [[ -z "${IMAGE_DIGEST}" && -z "${IMAGE_TAG}" ]]; then
  printf 'Either IMAGE_DIGEST or IMAGE_TAG must be set.\n' >&2
  exit 1
fi

require_boolean() {
  local name="$1"
  local value="$2"

  case "${value}" in
    true|false) ;;
    *)
      printf '%s must be true or false.\n' "${name}" >&2
      exit 1
      ;;
  esac
}

json_escape() {
  local value="$1"

  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//$'\n'/\\n}"
  value="${value//$'\r'/\\r}"
  value="${value//$'\t'/\\t}"
  printf '%s' "${value}"
}

if [[ -z "${RUNNER_PUBLIC_IPV4}" ]]; then
  RUNNER_PUBLIC_IPV4="$(curl -4 -fsSL https://ifconfig.me 2>/dev/null || curl -4 -fsSL https://icanhazip.com 2>/dev/null)"
  RUNNER_PUBLIC_IPV4="$(printf '%s' "${RUNNER_PUBLIC_IPV4}" | tr -d '\n')"
fi

if [[ -z "${RUNNER_PUBLIC_IPV4}" ]]; then
  printf 'Failed to resolve the current public IPv4 address. Set RUNNER_PUBLIC_IPV4 explicitly.\n' >&2
  exit 1
fi

if [[ "${HOSTNAME_MODE}" != "nip_io" && -z "${HOSTNAME}" ]]; then
  printf 'TF_HOSTNAME is required when TF_HOSTNAME_MODE is not nip_io.\n' >&2
  exit 1
fi

if [[ "${HOSTNAME_MODE}" == "managed_dns" && ( -z "${DNS_ZONE_NAME}" || -z "${DNS_ZONE_DNS_NAME}" ) ]]; then
  printf 'TF_DNS_ZONE_NAME and TF_DNS_ZONE_DNS_NAME are required when TF_HOSTNAME_MODE=managed_dns.\n' >&2
  exit 1
fi

if [[ -z "${VERIFY_PUBLIC_EDGE}" ]]; then
  VERIFY_PUBLIC_EDGE="true"
fi

if [[ -z "${GOOGLE_CLOUD_PROJECT}" ]]; then
  GOOGLE_CLOUD_PROJECT="${PROJECT_ID}"
fi

if [[ -z "${GOOGLE_CLOUD_LOCATION}" ]]; then
  GOOGLE_CLOUD_LOCATION="${REGION}"
fi

require_boolean "TF_ENABLE_CLOUD_ARMOR" "${ENABLE_CLOUD_ARMOR}"
require_boolean "TF_ENABLE_UPTIME_CHECKS" "${ENABLE_UPTIME_CHECKS}"
require_boolean "TF_VERIFY_PUBLIC_EDGE" "${VERIFY_PUBLIC_EDGE}"

FOUNDATION_VARS_FILE="${TMP_DIR}/foundation.auto.tfvars.json"
PLATFORM_VARS_FILE="${TMP_DIR}/platform.auto.tfvars.json"
BOOTSTRAP_STATE="${TMP_DIR}/bootstrap.tfstate"
KUBECONFIG_PATH="${TMP_DIR}/kubeconfig"
PORT_FORWARD_LOG="${TMP_DIR}/port-forward.log"

authorized_network_entries=()
authorized_network_entries+=("{\"cidr_block\":\"$(json_escape "${RUNNER_PUBLIC_IPV4}/32")\",\"display_name\":\"automation-runner\"}")

if [[ -n "${EXTRA_MASTER_AUTHORIZED_CIDRS}" ]]; then
  IFS=',' read -r -a extra_cidrs <<< "${EXTRA_MASTER_AUTHORIZED_CIDRS}"
  index=1
  for cidr in "${extra_cidrs[@]}"; do
    cidr="$(printf '%s' "${cidr}" | xargs)"
    if [[ -z "${cidr}" || "${cidr}" == "${RUNNER_PUBLIC_IPV4}/32" ]]; then
      continue
    fi
    authorized_network_entries+=("{\"cidr_block\":\"$(json_escape "${cidr}")\",\"display_name\":\"operator-${index}\"}")
    index=$((index + 1))
  done
fi

authorized_networks_json=""
for entry in "${authorized_network_entries[@]}"; do
  if [[ -n "${authorized_networks_json}" ]]; then
    authorized_networks_json+=", "
  fi
  authorized_networks_json+="${entry}"
done

cat >"${FOUNDATION_VARS_FILE}" <<EOF
{
  "project_id": "$(json_escape "${PROJECT_ID}")",
  "region": "$(json_escape "${REGION}")",
  "support_email": "$(json_escape "${SUPPORT_EMAIL}")",
  "cluster_name": "$(json_escape "${CLUSTER_NAME}")",
  "network_name": "$(json_escape "${NETWORK_NAME}")",
  "db_password": "$(json_escape "${DB_PASSWORD}")",
  "db_password_version": ${DB_PASSWORD_VERSION},
  "database_url_secret_version": ${DATABASE_URL_SECRET_VERSION},
  "master_authorized_networks": [${authorized_networks_json}],
  "labels": {
    "owner": "platform"
  }
}
EOF

notification_channel_json=""
if [[ -n "${NOTIFICATION_CHANNEL_ID}" ]]; then
  notification_channel_json=$',\n  "notification_channel_id": "'"$(json_escape "${NOTIFICATION_CHANNEL_ID}")"'"'
fi

gemini_api_key_json=""
if [[ -n "${GEMINI_API_KEY}" ]]; then
  gemini_api_key_json=$',\n  "gemini_api_key": "'"$(json_escape "${GEMINI_API_KEY}")"'"'
fi

cat >"${PLATFORM_VARS_FILE}" <<EOF
{
  "project_id": "$(json_escape "${PROJECT_ID}")",
  "region": "$(json_escape "${REGION}")",
  "cluster_name": "$(json_escape "${CLUSTER_NAME}")",
  "namespace": "$(json_escape "${NAMESPACE}")",
  "hostname_mode": "$(json_escape "${HOSTNAME_MODE}")",
  "hostname": "$(json_escape "${HOSTNAME}")",
  "dns_zone_name": "$(json_escape "${DNS_ZONE_NAME}")",
  "dns_zone_dns_name": "$(json_escape "${DNS_ZONE_DNS_NAME}")",
  "nip_io_label": "$(json_escape "${NIP_IO_LABEL}")",
  "image_repository": "$(json_escape "${IMAGE_REPOSITORY}")",
  "image_digest": "$(json_escape "${IMAGE_DIGEST}")",
  "image_tag": "$(json_escape "${IMAGE_TAG}")",
  "enable_cloud_armor": ${ENABLE_CLOUD_ARMOR},
  "enable_uptime_checks": ${ENABLE_UPTIME_CHECKS},
  "google_cloud_project": "$(json_escape "${GOOGLE_CLOUD_PROJECT}")",
  "google_cloud_location": "$(json_escape "${GOOGLE_CLOUD_LOCATION}")",
  "llm_provider_type": "$(json_escape "${LLM_PROVIDER_TYPE}")",
  "llm_judge_model": "$(json_escape "${LLM_JUDGE_MODEL}")",
  "llm_image_model": "$(json_escape "${LLM_IMAGE_MODEL}")",
  "rust_log": "$(json_escape "${RUST_LOG}")",
  "kubeconfig_path": "$(json_escape "${KUBECONFIG_PATH}")",
  "labels": {
    "owner": "platform"
  }${notification_channel_json}${gemini_api_key_json}
}
EOF

terraform_init_gcs() {
  local workdir="$1"
  local prefix="$2"
  terraform -chdir="${workdir}" init -reconfigure \
    -backend-config="bucket=${STATE_BUCKET_NAME}" \
    -backend-config="prefix=${prefix}"
}

wait_for_https_health() {
  local verify_url="$1"
  local max_attempts="${2:-120}"

  for ((attempt=1; attempt<=max_attempts; attempt++)); do
    if curl --fail --silent --show-error --max-time 10 "${verify_url}/api/live" >/dev/null \
      && curl --fail --silent --show-error --max-time 10 "${verify_url}/api/ready" >/dev/null; then
      return 0
    fi
    sleep 15
  done

  return 1
}

wait_for_managed_certificate() {
  local namespace="$1"

  for ((attempt=1; attempt<=120; attempt++)); do
    status="$(kubectl --kubeconfig "${KUBECONFIG_PATH}" -n "${namespace}" get managedcertificate dragon-shift-managed-cert -o jsonpath='{.status.certificateStatus}' 2>/dev/null || true)"
    if [[ "${status}" == "Active" ]]; then
      return 0
    fi
    sleep 15
  done

  return 1
}

with_port_forward_health() {
  local namespace="$1"
  local service_name="$2"
  local local_port="${3:-32000}"
  local pid=""
  local service_port=""

  cleanup_port_forward() {
    if [[ -n "${pid}" ]]; then
      kill "${pid}" >/dev/null 2>&1 || true
      wait "${pid}" 2>/dev/null || true
    fi
  }

  service_port="$(kubectl --kubeconfig "${KUBECONFIG_PATH}" -n "${namespace}" get service "${service_name}" -o jsonpath='{.spec.ports[0].port}')"
  if [[ -z "${service_port}" ]]; then
    printf 'Failed to resolve a service port for %s in namespace %s.\n' "${service_name}" "${namespace}" >&2
    return 1
  fi

  trap cleanup_port_forward RETURN
  kubectl --kubeconfig "${KUBECONFIG_PATH}" -n "${namespace}" port-forward "svc/${service_name}" "${local_port}:${service_port}" >"${PORT_FORWARD_LOG}" 2>&1 &
  pid="$!"

  for ((attempt=1; attempt<=30; attempt++)); do
    if curl --fail --silent --show-error "http://127.0.0.1:${local_port}/api/live" >/dev/null \
      && curl --fail --silent --show-error "http://127.0.0.1:${local_port}/api/ready" >/dev/null; then
      return 0
    fi
    sleep 2
  done

  printf 'Port-forward health check failed. See %s\n' "${PORT_FORWARD_LOG}" >&2
  return 1
}

printf 'Ensuring Terraform state bucket exists in %s...\n' "${PROJECT_ID}"
if gcloud storage buckets describe "gs://${STATE_BUCKET_NAME}" --project "${PROJECT_ID}" >/dev/null 2>&1; then
  printf 'Terraform state bucket gs://%s already exists; skipping bootstrap apply.\n' "${STATE_BUCKET_NAME}"
else
  terraform -chdir="${ROOT_DIR}/terraform/bootstrap" init -reconfigure -backend-config="path=${BOOTSTRAP_STATE}"
  terraform -chdir="${ROOT_DIR}/terraform/bootstrap" apply -input=false -auto-approve \
    -var="project_id=${PROJECT_ID}" \
    -var="region=${REGION}" \
    -var="state_bucket_name=${STATE_BUCKET_NAME}"
fi

printf 'Applying foundation stack...\n'
terraform_init_gcs "${ROOT_DIR}/terraform/environments/production/foundation" "${STATE_PREFIX_BASE}/foundation"
terraform -chdir="${ROOT_DIR}/terraform/environments/production/foundation" apply -input=false -auto-approve -var-file="${FOUNDATION_VARS_FILE}"

if [[ -z "${NOTIFICATION_CHANNEL_ID}" ]]; then
  NOTIFICATION_CHANNEL_ID="$(terraform -chdir="${ROOT_DIR}/terraform/environments/production/foundation" output -raw notification_channel_id)"
  python3 - <<'PY' "${PLATFORM_VARS_FILE}" "${NOTIFICATION_CHANNEL_ID}"
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
value = sys.argv[2].strip()
data = json.loads(path.read_text())
if value:
    data["notification_channel_id"] = value
path.write_text(json.dumps(data, indent=2) + "\n")
PY
fi

printf 'Fetching cluster credentials...\n'
KUBECONFIG="${KUBECONFIG_PATH}" gcloud container clusters get-credentials "${CLUSTER_NAME}" --region "${REGION}" --project "${PROJECT_ID}"

printf 'Applying platform stack...\n'
terraform_init_gcs "${ROOT_DIR}/terraform/environments/production/platform" "${STATE_PREFIX_BASE}/platform"
terraform -chdir="${ROOT_DIR}/terraform/environments/production/platform" apply -input=false -auto-approve -var-file="${PLATFORM_VARS_FILE}"

VERIFY_URL="$(terraform -chdir="${ROOT_DIR}/terraform/environments/production/platform" output -raw verify_url)"
VERIFY_NAMESPACE="$(terraform -chdir="${ROOT_DIR}/terraform/environments/production/platform" output -raw namespace)"
HELM_RELEASE_NAME="$(terraform -chdir="${ROOT_DIR}/terraform/environments/production/platform" output -raw helm_release_name)"
PORT_FORWARD_SERVICE="${PORT_FORWARD_SERVICE:-${HELM_RELEASE_NAME}-dragon-shift}"

kubectl --kubeconfig "${KUBECONFIG_PATH}" rollout status deployment/"${HELM_RELEASE_NAME}-dragon-shift" -n "${VERIFY_NAMESPACE}" --timeout=10m
with_port_forward_health "${VERIFY_NAMESPACE}" "${PORT_FORWARD_SERVICE}"

if [[ "${VERIFY_PUBLIC_EDGE}" == "true" ]]; then
  printf 'Waiting for managed certificate...\n'
  wait_for_managed_certificate "${VERIFY_NAMESPACE}"

  printf 'Waiting for HTTPS health checks at %s...\n' "${VERIFY_URL}"
  wait_for_https_health "${VERIFY_URL}"

  live_payload="$(curl --fail --silent --show-error "${VERIFY_URL}/api/live")"
  ready_payload="$(curl --fail --silent --show-error "${VERIFY_URL}/api/ready")"
  [[ "${live_payload}" == *'"ok":true'* ]]
  [[ "${live_payload}" == *'"service":"app-server"'* ]]
  [[ "${live_payload}" == *'"status":"live"'* ]]
  [[ "${ready_payload}" == *'"service":"app-server"'* ]]
  [[ "${ready_payload}" == *'"status":"ready"'* ]]

  if [[ "${RUN_DEPLOYED_SMOKE}" == "true" ]]; then
    printf 'Running deployed browser smoke against %s...\n' "${VERIFY_URL}"
    E2E_BASE_URL="${VERIFY_URL}" npm --prefix "${ROOT_DIR}/e2e" run test:deployed -- --project=chromium
  fi
else
  printf 'Skipping public HTTPS verification because TF_VERIFY_PUBLIC_EDGE=false. Internal rollout and health checks passed.\n'
fi

printf 'Deployment verified at %s\n' "${VERIFY_URL}"
