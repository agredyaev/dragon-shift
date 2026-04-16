#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

PROJECT_ID="${GCP_PROJECT_ID:?GCP_PROJECT_ID is required}"
REGION="${GCP_REGION:-europe-west4}"
SUPPORT_EMAIL="${TF_SUPPORT_EMAIL:?TF_SUPPORT_EMAIL is required}"
DB_PASSWORD="${TF_PRODUCTION_DB_PASSWORD:?TF_PRODUCTION_DB_PASSWORD is required}"
DB_TIER="${TF_DB_TIER:-}"
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
LLM_IMAGE_MODEL="${TF_LLM_IMAGE_MODEL:-gemini-2.5-flash-image}"
RUST_LOG="${TF_RUST_LOG:-info,tower_http=debug}"
GEMINI_API_KEY="${TF_GEMINI_API_KEY:-}"
GEMINI_API_KEY_1="${TF_GEMINI_API_KEY_1:-}"
GEMINI_API_KEY_2="${TF_GEMINI_API_KEY_2:-}"
GEMINI_API_KEY_3="${TF_GEMINI_API_KEY_3:-}"
GEMINI_API_KEY_4="${TF_GEMINI_API_KEY_4:-}"
GEMINI_API_KEY_5="${TF_GEMINI_API_KEY_5:-}"
GEMINI_API_KEY_6="${TF_GEMINI_API_KEY_6:-}"
GEMINI_API_KEY_7="${TF_GEMINI_API_KEY_7:-}"
GEMINI_API_KEY_8="${TF_GEMINI_API_KEY_8:-}"
GEMINI_API_KEY_9="${TF_GEMINI_API_KEY_9:-}"
GEMINI_API_KEY_10="${TF_GEMINI_API_KEY_10:-}"
GEMINI_API_KEY_11="${TF_GEMINI_API_KEY_11:-}"
GEMINI_API_KEY_12="${TF_GEMINI_API_KEY_12:-}"
GEMINI_API_KEY_13="${TF_GEMINI_API_KEY_13:-}"
GEMINI_API_KEY_14="${TF_GEMINI_API_KEY_14:-}"
GEMINI_API_KEY_15="${TF_GEMINI_API_KEY_15:-}"
DATABASE_POOL_SIZE="${TF_DATABASE_POOL_SIZE:-}"
APP_CPU_REQUEST="${TF_APP_CPU_REQUEST:-}"
APP_CPU_LIMIT="${TF_APP_CPU_LIMIT:-}"
APP_MEMORY_REQUEST="${TF_APP_MEMORY_REQUEST:-}"
APP_MEMORY_LIMIT="${TF_APP_MEMORY_LIMIT:-}"
CREATE_RATE_LIMIT_MAX="${TF_CREATE_RATE_LIMIT_MAX:-}"
JOIN_RATE_LIMIT_MAX="${TF_JOIN_RATE_LIMIT_MAX:-}"
COMMAND_RATE_LIMIT_MAX="${TF_COMMAND_RATE_LIMIT_MAX:-}"
WEBSOCKET_RATE_LIMIT_MAX="${TF_WEBSOCKET_RATE_LIMIT_MAX:-}"

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

require_integer() {
  local name="$1"
  local value="$2"

  case "${value}" in
    ''|*[!0-9]*)
      printf '%s must be a non-negative integer.\n' "${name}" >&2
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

if [[ -n "${DATABASE_POOL_SIZE}" ]]; then
  require_integer "TF_DATABASE_POOL_SIZE" "${DATABASE_POOL_SIZE}"
fi
if [[ -n "${CREATE_RATE_LIMIT_MAX}" ]]; then
  require_integer "TF_CREATE_RATE_LIMIT_MAX" "${CREATE_RATE_LIMIT_MAX}"
fi
if [[ -n "${JOIN_RATE_LIMIT_MAX}" ]]; then
  require_integer "TF_JOIN_RATE_LIMIT_MAX" "${JOIN_RATE_LIMIT_MAX}"
fi
if [[ -n "${COMMAND_RATE_LIMIT_MAX}" ]]; then
  require_integer "TF_COMMAND_RATE_LIMIT_MAX" "${COMMAND_RATE_LIMIT_MAX}"
fi
if [[ -n "${WEBSOCKET_RATE_LIMIT_MAX}" ]]; then
  require_integer "TF_WEBSOCKET_RATE_LIMIT_MAX" "${WEBSOCKET_RATE_LIMIT_MAX}"
fi

FOUNDATION_VARS_FILE="${TMP_DIR}/foundation.auto.tfvars.json"
PLATFORM_VARS_FILE="${TMP_DIR}/platform.auto.tfvars.json"
BOOTSTRAP_STATE="${TMP_DIR}/bootstrap.tfstate"
KUBECONFIG_PATH="${TMP_DIR}/kubeconfig"
PORT_FORWARD_LOG="${TMP_DIR}/port-forward.log"

foundation_db_tier_json=""
if [[ -n "${DB_TIER}" ]]; then
  foundation_db_tier_json=$',\n  "db_tier": "'"$(json_escape "${DB_TIER}")"'"'
fi

platform_database_pool_json=""
if [[ -n "${DATABASE_POOL_SIZE}" ]]; then
  platform_database_pool_json=$',\n  "database_pool_size": '"${DATABASE_POOL_SIZE}"
fi

platform_app_cpu_request_json=""
if [[ -n "${APP_CPU_REQUEST}" ]]; then
  platform_app_cpu_request_json=$',\n  "app_cpu_request": "'"$(json_escape "${APP_CPU_REQUEST}")"'"'
fi

platform_app_cpu_limit_json=""
if [[ -n "${APP_CPU_LIMIT}" ]]; then
  platform_app_cpu_limit_json=$',\n  "app_cpu_limit": "'"$(json_escape "${APP_CPU_LIMIT}")"'"'
fi

platform_app_memory_request_json=""
if [[ -n "${APP_MEMORY_REQUEST}" ]]; then
  platform_app_memory_request_json=$',\n  "app_memory_request": "'"$(json_escape "${APP_MEMORY_REQUEST}")"'"'
fi

platform_app_memory_limit_json=""
if [[ -n "${APP_MEMORY_LIMIT}" ]]; then
  platform_app_memory_limit_json=$',\n  "app_memory_limit": "'"$(json_escape "${APP_MEMORY_LIMIT}")"'"'
fi

platform_create_rate_limit_json=""
if [[ -n "${CREATE_RATE_LIMIT_MAX}" ]]; then
  platform_create_rate_limit_json=$',\n  "create_rate_limit_max": '"${CREATE_RATE_LIMIT_MAX}"
fi

platform_join_rate_limit_json=""
if [[ -n "${JOIN_RATE_LIMIT_MAX}" ]]; then
  platform_join_rate_limit_json=$',\n  "join_rate_limit_max": '"${JOIN_RATE_LIMIT_MAX}"
fi

platform_command_rate_limit_json=""
if [[ -n "${COMMAND_RATE_LIMIT_MAX}" ]]; then
  platform_command_rate_limit_json=$',\n  "command_rate_limit_max": '"${COMMAND_RATE_LIMIT_MAX}"
fi

platform_websocket_rate_limit_json=""
if [[ -n "${WEBSOCKET_RATE_LIMIT_MAX}" ]]; then
  platform_websocket_rate_limit_json=$',\n  "websocket_rate_limit_max": '"${WEBSOCKET_RATE_LIMIT_MAX}"
fi

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
  }${foundation_db_tier_json}
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

gemini_api_keys_json=""
gemini_api_keys=()
for key in \
  "${GEMINI_API_KEY_1}" "${GEMINI_API_KEY_2}" "${GEMINI_API_KEY_3}" "${GEMINI_API_KEY_4}" "${GEMINI_API_KEY_5}" \
  "${GEMINI_API_KEY_6}" "${GEMINI_API_KEY_7}" "${GEMINI_API_KEY_8}" "${GEMINI_API_KEY_9}" "${GEMINI_API_KEY_10}" \
  "${GEMINI_API_KEY_11}" "${GEMINI_API_KEY_12}" "${GEMINI_API_KEY_13}" "${GEMINI_API_KEY_14}" "${GEMINI_API_KEY_15}"; do
  if [[ -n "${key}" ]]; then
    gemini_api_keys+=("\"$(json_escape "${key}")\"")
  fi
done
if (( ${#gemini_api_keys[@]} > 0 )); then
  gemini_api_keys_json=$',\n  "gemini_api_keys": ['
  for ((i=0; i<${#gemini_api_keys[@]}; i++)); do
    if (( i > 0 )); then
      gemini_api_keys_json+=', '
    fi
    gemini_api_keys_json+="${gemini_api_keys[$i]}"
  done
  gemini_api_keys_json+=']'
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
  }${notification_channel_json}${gemini_api_key_json}${gemini_api_keys_json}${platform_database_pool_json}${platform_app_cpu_request_json}${platform_app_cpu_limit_json}${platform_app_memory_request_json}${platform_app_memory_limit_json}${platform_create_rate_limit_json}${platform_join_rate_limit_json}${platform_command_rate_limit_json}${platform_websocket_rate_limit_json}
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
