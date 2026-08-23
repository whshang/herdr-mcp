#!/usr/bin/env bash
# ci-herdr-runtime.sh — bootstrap a real, pinned Herdr runtime on a GitHub-hosted
# Ubuntu runner for the non-hermetic transport/regression tests.
#
# The transport tests (tests/transport.test.mjs) connect to a live Herdr over
# HERDR_SOCKET_PATH (default ~/.config/herdr/herdr.sock) and assert herdr_inspect
# returns a focused_workspace. On a fresh runner there is no pre-existing session,
# so the CI server listens on that default socket and tests find it automatically.
#
# We pin the exact release (v0.8.2) instead of the rolling `latest` installer for
# reproducibility, and fail fast if the installed version differs.
#
# Usage:
#   scripts/ci-herdr-runtime.sh start   # install + start headless server + focused workspace
#   scripts/ci-herdr-runtime.sh stop    # stop the server we started (idempotent)
#
# Env overrides (only for isolated testing on a dev machine; CI uses defaults):
#   HERDR_VERSION, HERDR_INSTALL_DIR, HERDR_STATE_DIR, HERDR_SOCKET
# This script never starts a TUI and never touches a developer's default session
# unless HERDR_STATE_DIR/HERDR_SOCKET are explicitly pointed at it.
set -euo pipefail

: "${HERDR_VERSION:=0.8.2}"
HERDR_ASSET="herdr-linux-x86_64"
HERDR_URL="https://github.com/herdrdev/herdr/releases/download/v${HERDR_VERSION}/${HERDR_ASSET}"
INSTALL_DIR="${HERDR_INSTALL_DIR:-$HOME/.local/bin}"
HERDR_BIN="${INSTALL_DIR}/herdr"
# CI uses the default Herdr state/socket dir so the tests (which default to the
# same path) connect automatically. Overridable for isolated local testing.
STATE_DIR="${HERDR_STATE_DIR:-$HOME/.config/herdr}"
SOCKET="${HERDR_SOCKET:-${STATE_DIR}/herdr.sock}"

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

start_server() {
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
  mkdir -p "${STATE_DIR}"
  # Detached headless server with interleaved output captured to a log.
  HERDR_SOCKET_PATH="${SOCKET}" "${HERDR_BIN}" server >"${STATE_DIR}/ci-server.log" 2>&1 || true
  # Wait for the server to report running.
  local i ok=0
  for i in $(seq 1 60); do
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
  ws="$(HERDR_SOCKET_PATH="${SOCKET}" "${HERDR_BIN}" workspace create \
        --cwd "${GITHUB_WORKSPACE:-$PWD}" --label "ci" --focus 2>/dev/null || true)"
  log "workspace create/focus: ${ws:-<idempotent/focused>}"
  # Final reachability confirmation.
  HERDR_SOCKET_PATH="${SOCKET}" "${HERDR_BIN}" status server --json >/dev/null 2>&1 \
    || err "server not reachable after workspace setup"
  log "herdr runtime ready"
}

stop_server() {
  if [ -x "${HERDR_BIN}" ]; then
    log "stopping herdr server (socket=${SOCKET})"
    HERDR_SOCKET_PATH="${SOCKET}" "${HERDR_BIN}" server stop >/dev/null 2>&1 || true
  fi
  log "herdr server stopped"
}

case "${1:-}" in
  start) start_server ;;
  stop)  stop_server ;;
  *) err "usage: $0 start|stop" ;;
esac