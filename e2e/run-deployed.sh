#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASE_URL="${E2E_BASE_URL:-http://127.0.0.1:32000}"
BASE_URL="${BASE_URL%/}"
PORT_FORWARD_LOG="${PORT_FORWARD_LOG:-${ROOT_DIR}/e2e/.tmp/port-forward.log}"
PORT_FORWARD_NAMESPACE="${PORT_FORWARD_NAMESPACE:-default}"
PORT_FORWARD_SERVICE="${PORT_FORWARD_SERVICE:-dragon-shift-dragon-shift}"
PORT_FORWARD_REMOTE_PORT="${PORT_FORWARD_REMOTE_PORT:-3000}"
PORT_FORWARD_PID=""

BASE_URL_NO_SCHEME="${BASE_URL#*://}"
BASE_PATH=""
if [[ "${BASE_URL_NO_SCHEME}" == */* ]]; then
  BASE_PATH="/${BASE_URL_NO_SCHEME#*/}"
fi
BASE_HOST_PORT="${BASE_URL_NO_SCHEME%%/*}"
BASE_HOST="${BASE_HOST_PORT%%:*}"
BASE_PORT="${BASE_HOST_PORT##*:}"
if [[ "${BASE_HOST}" == "${BASE_PORT}" ]]; then
  BASE_PORT="32000"
fi

FORWARDED_HOST="${PORT_FORWARD_HOST:-127.0.0.1}"
FORWARDED_PORT="${PORT_FORWARD_LOCAL_PORT:-${BASE_PORT}}"
FORWARDED_BASE_URL="http://${FORWARDED_HOST}:${FORWARDED_PORT}${BASE_PATH}"

is_live() {
  local url="$1"
  local response
  response="$(curl --fail --silent "${url}/api/live" 2>/dev/null || true)"
  [[ "${response}" == *'"ok":true'* ]] \
    && [[ "${response}" == *'"service":"app-server"'* ]] \
    && [[ "${response}" == *'"status":"live"'* ]]
}

origin_is_allowed() {
  local url="$1"
  local http_code
  http_code="$({
    curl \
      --silent \
      --output /dev/null \
      --write-out '%{http_code}' \
      --header 'content-type: application/json' \
      --header "origin: ${url}" \
      --data '{"sessionCode":"000000","reconnectToken":"probe","command":"startPhase1"}' \
      "${url}/api/workshops/command"
  } || true)"
  [[ "${http_code}" != "403" && "${http_code}" != "000" ]]
}

cleanup() {
  if [[ -n "${PORT_FORWARD_PID}" ]]; then
    kill "${PORT_FORWARD_PID}" >/dev/null 2>&1 || true
    wait "${PORT_FORWARD_PID}" 2>/dev/null || true
  fi
}

wait_for_live() {
  for _ in {1..30}; do
    if is_live "${FORWARDED_BASE_URL}"; then
      return 0
    fi
    sleep 1
  done

  return 1
}

run_playwright() {
  local target_base_url="$1"
  shift
  cd "${ROOT_DIR}/e2e"
  E2E_BASE_URL="${target_base_url}" npx playwright test "$@"
}

ensure_origin_allowed() {
  local target_base_url="$1"
  if origin_is_allowed "${target_base_url}"; then
    return 0
  fi

  printf 'Target is live, but browser origin %s is not allowed by the deployed app. Update app.allowedOrigins/app.viteAppUrl or use a supported origin.\n' "${target_base_url}" >&2
  return 1
}

if is_live "${BASE_URL}"; then
  ensure_origin_allowed "${BASE_URL}"
  run_playwright "${BASE_URL}" "$@"
  exit 0
fi

trap cleanup EXIT
mkdir -p "$(dirname "${PORT_FORWARD_LOG}")"
kubectl port-forward -n "${PORT_FORWARD_NAMESPACE}" "svc/${PORT_FORWARD_SERVICE}" "${FORWARDED_PORT}:${PORT_FORWARD_REMOTE_PORT}" >"${PORT_FORWARD_LOG}" 2>&1 &
PORT_FORWARD_PID="$!"

if ! wait_for_live; then
  printf 'Port-forward did not become ready. See %s\n' "${PORT_FORWARD_LOG}" >&2
  exit 1
fi

ensure_origin_allowed "${FORWARDED_BASE_URL}"
run_playwright "${FORWARDED_BASE_URL}" "$@"
