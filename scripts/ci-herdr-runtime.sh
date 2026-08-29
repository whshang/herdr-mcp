#!/usr/bin/env bash
# ci-herdr-runtime.sh — bootstrap a real, pinned Herdr runtime for the
# non-hermetic transport/regression tests on CI Linux or an isolated local macOS run.
#
# The transport tests (tests/transport.test.mjs) connect to a live Herdr over
# HERDR_SOCKET_PATH (default ~/.config/herdr/herdr.sock) and assert herdr_inspect
# returns a focused_workspace. The bootstrap workspace is deliberately non-Git:
# Herdr 0.8.2 can block workspace creation on Git discovery, which must not make
# the transport test fixture depend on repository metadata.
#
# We pin the exact release (v0.8.2) instead of the rolling `latest` installer for
# reproducibility, and fail fast if the installed version differs.
#
# Usage:
#   scripts/ci-herdr-runtime.sh start   # install + start headless server + focused workspace
#   scripts/ci-herdr-runtime.sh stop    # stop the server we started (idempotent)
#
# Env overrides (only for isolated testing on a dev machine; CI uses defaults):
#   HERDR_VERSION, HERDR_INSTALL_DIR, HERDR_STATE_DIR, HERDR_SOCKET,
#   HERDR_CI_WORKSPACE, XDG_CONFIG_HOME
# Local callers must isolate Herdr's persisted state via XDG_CONFIG_HOME as well as
# its socket. This script never starts a TUI and refuses the developer default state.
set -euo pipefail

: "${HERDR_VERSION:=0.8.2}"

detect_asset() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "${os}/${arch}" in
    Linux/x86_64|Linux/amd64) printf '%s\n' 'herdr-linux-x86_64' ;;
    Linux/aarch64|Linux/arm64) printf '%s\n' 'herdr-linux-aarch64' ;;
    Darwin/arm64|Darwin/aarch64) printf '%s\n' 'herdr-macos-aarch64' ;;
    Darwin/x86_64|Darwin/amd64) printf '%s\n' 'herdr-macos-x86_64' ;;
    *)
      printf '[ci-herdr] ERROR: unsupported host for pinned Herdr runtime: %s/%s\n' "$os" "$arch" >&2
      return 1
      ;;
  esac
}

HERDR_ASSET="${HERDR_ASSET:-$(detect_asset)}"
HERDR_URL="https://github.com/herdrdev/herdr/releases/download/v${HERDR_VERSION}/${HERDR_ASSET}"
INSTALL_DIR="${HERDR_INSTALL_DIR:-$HOME/.local/bin}"
HERDR_BIN="${INSTALL_DIR}/herdr"
# CI uses the default Herdr state/socket dir so the tests (which default to the
# same path) connect automatically. Overridable for isolated local testing.
STATE_DIR="${HERDR_STATE_DIR:-$HOME/.config/herdr}"
SOCKET="${HERDR_SOCKET:-${STATE_DIR}/herdr.sock}"
PID_FILE="${STATE_DIR}/ci-server.pid"
CI_WORKSPACE="${HERDR_CI_WORKSPACE:-${STATE_DIR}/ci-workspace}"

log() { printf '[ci-herdr] %s\n' "$*" >&2; }
err() { printf '[ci-herdr] ERROR: %s\n' "$*" >&2; exit 1; }

install_herdr() {
  if [ -x "${HERDR_BIN}" ]; then
    local have
    have="$("${HERDR_BIN}" --version 2>/dev/null | awk '{print $2}')"
    if [ "${have}" = "${HERDR_VERSION}" ]; then
      log "herdr ${HERDR_VERSION} already present at ${HERDR_BIN}"
      return 0
    fi
    log "herdr present but version ${have:-unknown} != ${HERDR_VERSION}; reinstalling"
  fi
  log "downloading pinned herdr ${HERDR_VERSION} from ${HERDR_URL}"
  mkdir -p "${INSTALL_DIR}"
  curl -fsSL --retry 3 --connect-timeout 10 --max-time 120 "${HERDR_URL}" -o "${HERDR_BIN}.tmp"
  chmod +x "${HERDR_BIN}.tmp"
  mv "${HERDR_BIN}.tmp" "${HERDR_BIN}"
  local installed
  installed="$("${HERDR_BIN}" --version 2>/dev/null | awk '{print $2}')"
  if [ "${installed}" != "${HERDR_VERSION}" ]; then
    err "installed herdr version ${installed:-unknown} != expected ${HERDR_VERSION}"
  fi
  log "installed herdr ${installed} to ${HERDR_BIN}"
}

server_pid_identity_ok() {
  local pid="$1"
  local command=""
  command="$(ps -p "${pid}" -o command= 2>/dev/null || true)"
  case "${command}" in
    "${HERDR_BIN} server"|"${HERDR_BIN} server "*) return 0 ;;
    *)
      log "refusing process mutation: pid=${pid} command=${command:-<missing>} expected=${HERDR_BIN} server"
      return 1
      ;;
  esac
}

terminate_exact_server_pid() {
  local pid="$1"
  local i
  kill -0 "${pid}" 2>/dev/null || return 0
  server_pid_identity_ok "${pid}" || return 1
  log "pid ${pid} still alive; sending TERM"
  kill -TERM "${pid}" 2>/dev/null || true
  for i in $(seq 1 20); do
    kill -0 "${pid}" 2>/dev/null || return 0
    sleep 0.1
  done
  server_pid_identity_ok "${pid}" || return 1
  log "pid ${pid} still alive after TERM; sending KILL"
  kill -KILL "${pid}" 2>/dev/null || true
  for i in $(seq 1 20); do
    kill -0 "${pid}" 2>/dev/null || return 0
    sleep 0.1
  done
  log "pid ${pid} remained alive after KILL"
  return 1
}

create_workspace_bounded() {
  local stdout_file="${STATE_DIR}/ci-workspace-create.out"
  local stderr_file="${STATE_DIR}/ci-workspace-create.err"
  local workspace_pid status i
  rm -f "${stdout_file}" "${stderr_file}"
  HERDR_SOCKET_PATH="${SOCKET}" "${HERDR_BIN}" workspace create \
    --cwd "${CI_WORKSPACE}" --label "ci" --focus \
    >"${stdout_file}" 2>"${stderr_file}" &
  workspace_pid=$!
  for i in $(seq 1 40); do
    if ! kill -0 "${workspace_pid}" 2>/dev/null; then
      if wait "${workspace_pid}"; then
        cat "${stdout_file}"
        return 0
      else
        status=$?
        tail -n 25 "${stderr_file}" >&2 || true
        return "${status}"
      fi
    fi
    sleep 0.25
  done
  log "workspace create exceeded 10s; terminating exact pid=${workspace_pid}"
  kill -TERM "${workspace_pid}" 2>/dev/null || true
  for i in $(seq 1 10); do
    kill -0 "${workspace_pid}" 2>/dev/null || break
    sleep 0.1
  done
  if kill -0 "${workspace_pid}" 2>/dev/null; then
    kill -KILL "${workspace_pid}" 2>/dev/null || true
  fi
  wait "${workspace_pid}" 2>/dev/null || true
  tail -n 25 "${stderr_file}" >&2 || true
  return 124
}

start_server() {
  # GitHub-hosted CI starts from an empty machine. Local runs must isolate both
  # persisted Herdr state and the socket so workspace creation cannot observe or
  # mutate the developer's live session.
  if [ "${GITHUB_ACTIONS:-}" != "true" ]; then
    [ -n "${XDG_CONFIG_HOME:-}" ] || err "local start requires isolated XDG_CONFIG_HOME"
    [ "${STATE_DIR}" = "${XDG_CONFIG_HOME}/herdr" ] || err "local HERDR_STATE_DIR must equal XDG_CONFIG_HOME/herdr"
    [ "${SOCKET}" = "${STATE_DIR}/herdr.sock" ] || err "local HERDR_SOCKET must equal HERDR_STATE_DIR/herdr.sock"
  fi
  install_herdr
  # Export the install dir for this and all later runner steps (GITHUB_PATH
  # persists for the whole job).
  if [ -n "${GITHUB_PATH:-}" ]; then
    printf '%s\n' "${INSTALL_DIR}" >>"${GITHUB_PATH}"
  fi
  case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *) export PATH="${INSTALL_DIR}:${PATH}" ;;
  esac
  log "starting headless herdr server (socket=${SOCKET})"
  mkdir -p "${STATE_DIR}" "${CI_WORKSPACE}"
  rm -f "${PID_FILE}"
  # `herdr server` is intentionally a foreground headless server. Start it as a
  # real background child for CI; otherwise this script blocks here forever and
  # the readiness loop below never runs. stdin/stdout/stderr are fully detached
  # from the Actions step and the PID is persisted for bounded cleanup.
  HERDR_SOCKET_PATH="${SOCKET}" "${HERDR_BIN}" server \
    </dev/null >"${STATE_DIR}/ci-server.log" 2>&1 &
  local server_pid=$!
  printf '%s\n' "${server_pid}" >"${PID_FILE}"
  log "herdr server spawned pid=${server_pid}"
  # Wait for the server to report running.
  local i ok=0
  for i in $(seq 1 60); do
    if ! kill -0 "${server_pid}" 2>/dev/null; then
      log "server exited before readiness"
      tail -n 25 "${STATE_DIR}/ci-server.log" >&2 || true
      err "herdr server process exited before becoming ready"
    fi
    if HERDR_SOCKET_PATH="${SOCKET}" "${HERDR_BIN}" status server --json 2>/dev/null \
        | grep -q '"running":true'; then
      log "herdr server ready after ${i}s"
      ok=1
      break
    fi
    sleep 1
  done
  if [ "${ok}" -ne 1 ]; then
    log "server.log tail:"
    tail -n 25 "${STATE_DIR}/ci-server.log" >&2 || true
    err "herdr server did not become ready within 60s"
  fi
  # Create and focus a workspace rooted at the checkout so inspect/snapshot has a
  # focused_workspace on first run; on re-runs an existing one just gets focused.
  local ws
  if ! ws="$(create_workspace_bounded)"; then
    log "server.log tail after workspace create failure:"
    tail -n 25 "${STATE_DIR}/ci-server.log" >&2 || true
    err "Herdr workspace create did not complete successfully within its 10s budget"
  fi
  log "workspace create/focus: ${ws:-<focused>}"
  # Final reachability confirmation.
  HERDR_SOCKET_PATH="${SOCKET}" "${HERDR_BIN}" status server --json >/dev/null 2>&1 \
    || err "server not reachable after workspace setup"
  log "herdr runtime ready"
}

stop_server() {
  if [ "${GITHUB_ACTIONS:-}" != "true" ]; then
    [ -n "${XDG_CONFIG_HOME:-}" ] || err "local stop requires isolated XDG_CONFIG_HOME"
    [ "${STATE_DIR}" = "${XDG_CONFIG_HOME}/herdr" ] || err "local HERDR_STATE_DIR must equal XDG_CONFIG_HOME/herdr"
    [ "${SOCKET}" = "${STATE_DIR}/herdr.sock" ] || err "local HERDR_SOCKET must equal HERDR_STATE_DIR/herdr.sock"
  fi
  local server_pid=""
  if [ -f "${PID_FILE}" ]; then
    server_pid="$(cat "${PID_FILE}" 2>/dev/null || true)"
  fi
  # Never call generic `herdr server stop` here: an environment/socket mistake
  # could terminate the developer's real Herdr session. Only the exact PID
  # recorded immediately after this script's spawn is eligible for mutation.
  if printf '%s\n' "${server_pid}" | grep -Eq '^[0-9]+$' && [ "${server_pid}" -gt 0 ] \
      && kill -0 "${server_pid}" 2>/dev/null; then
    if ! terminate_exact_server_pid "${server_pid}"; then
      err "refusing to remove PID evidence for an unverified or unreaped Herdr process"
    fi
  fi
  rm -f "${PID_FILE}"
  log "isolated Herdr server stopped"
}

case "${1:-}" in
  start)
    start_cleanup() {
      local status=$?
      trap - EXIT
      if [ "${status}" -ne 0 ]; then
        stop_server || true
      fi
      exit "${status}"
    }
    trap start_cleanup EXIT
    start_server
    trap - EXIT
    ;;
  stop)  stop_server ;;
  *) err "usage: $0 start|stop" ;;
esac