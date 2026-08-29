#!/usr/bin/env bash
# Authoritative maintainer release/test gate.
#
# Usage:
#   scripts/release-gate.sh           # full local/CI gate
#   scripts/release-gate.sh rust      # Rust fmt/lint/tests
#   scripts/release-gate.sh node      # Node/docs/Edge/extension tests
#   scripts/release-gate.sh hygiene   # shell/package/diff checks
#
# Every phase starts from the same isolated test environment profile. Runtime
# semantics are unchanged; only this test process has production-scoped Herdr
# overrides removed.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHASE="${1:-full}"
GATE_TMP=""
RUNTIME_STARTED=0

log() { printf '[release-gate] %s\n' "$*" >&2; }
fail() { printf '[release-gate] ERROR: %s\n' "$*" >&2; exit 2; }

scrub_test_environment() {
  # These values can point ordinary commands at a developer/production runtime,
  # config directory, Native Host origin, or alternate contract surface. Tests
  # must opt into their fixtures explicitly instead of inheriting them.
  unset \
    HERDR_MCP_PORT \
    HERDR_MCP_CONFIG_DIR \
    HERDR_MCP_INSTANCE \
    HERDR_MCP_ROOT \
    HERDR_MCP_STATE_DIR \
    HERDR_MCP_DEV_STATE_DIR \
    HERDR_MCP_BASE_URL \
    HERDR_MCP_CONTRACT_PROFILE \
    HERDR_MCP_ALL_TOOLS \
    HERDR_MCP_TOKEN \
    HERDR_EXTENSION_PATH \
    HERDR_EXTENSION_ORIGIN \
    HERDR_EXTENSION_IPC_SOCKET \
    HERDR_CONFIG_PATH \
    HERDR_BIN \
    HERDR_CLIENT_SOCKET_PATH \
    HERDR_SOCKET_PATH \
    HERDR_SOCKET \
    HERDR_STATE_DIR \
    HERDR_INSTALL_DIR \
    XDG_CONFIG_HOME
}

cleanup() {
  local status=$?
  set +e
  if [ "${RUNTIME_STARTED}" -eq 1 ]; then
    scripts/ci-herdr-runtime.sh stop
    RUNTIME_STARTED=0
  fi
  if [ -n "${GATE_TMP}" ] && [ -d "${GATE_TMP}" ]; then
    rm -rf "${GATE_TMP}"
  fi
  return "${status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

setup_isolated_herdr() {
  # Keep the Unix-domain socket short enough for macOS sockaddr_un and isolate
  # Herdr's real persisted state through XDG_CONFIG_HOME, not just its socket.
  GATE_TMP="$(mktemp -d /tmp/herdr-gate.XXXXXX)"
  export XDG_CONFIG_HOME="${GATE_TMP}/xdg"
  export HERDR_STATE_DIR="${XDG_CONFIG_HOME}/herdr"
  export HERDR_SOCKET="${HERDR_STATE_DIR}/herdr.sock"
  export HERDR_SOCKET_PATH="${HERDR_SOCKET}"
  export HERDR_INSTALL_DIR="${GATE_TMP}/bin"
  export HERDR_BIN="${HERDR_INSTALL_DIR}/herdr"
  # ci-herdr-runtime.sh runs in a child shell, so its PATH export cannot update
  # this release-gate process. Export the isolated install dir here so the Node
  # transport tests resolve the exact pinned Herdr binary in the same step.
  export PATH="${HERDR_INSTALL_DIR}:${PATH}"
  export GITHUB_WORKSPACE="${GITHUB_WORKSPACE:-${ROOT}}"
  export HERDR_CI_WORKSPACE="${GATE_TMP}/workspace"
  # Mark cleanup ownership before start so a partial spawn/readiness failure is
  # still handled by the isolated exact-PID stop path.
  RUNTIME_STARTED=1
  scripts/ci-herdr-runtime.sh start
}

run_rust() {
  log 'Rust format'
  cargo fmt --check
  log 'Rust lint'
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  log 'Rust tests'
  cargo test --workspace
}

run_node() {
  log 'Install Node dependencies'
  npm ci
  log 'Build Node runtime'
  npm run build
  log 'Build documentation site'
  npm run build:site

  log 'Start isolated pinned Herdr runtime'
  setup_isolated_herdr
  log 'Node tests'
  npm test
  log 'Edge tests'
  npm run test:edge
  log 'Browser extension smoke'
  node tests/manual/extension_smoke.mjs

  log 'Stop isolated pinned Herdr runtime'
  scripts/ci-herdr-runtime.sh stop
  RUNTIME_STARTED=0
}

run_git_diff_check_bounded() {
  local out err pid i status
  out="$(mktemp /tmp/herdr-git-diff.XXXXXX)"
  err="${out}.err"
  GIT_PAGER=cat PAGER=cat git -c diff.external= diff --no-ext-diff --check </dev/null >"${out}" 2>"${err}" &
  pid=$!
  for i in $(seq 1 40); do
    if ! kill -0 "${pid}" 2>/dev/null; then
      if wait "${pid}"; then
        cat "${out}"
        rm -f "${out}" "${err}"
        return 0
      else
        status=$?
        cat "${out}"
        cat "${err}" >&2
        rm -f "${out}" "${err}"
        return "${status}"
      fi
    fi
    sleep 0.25
  done
  log "git diff --check exceeded 10s; terminating exact pid=${pid}"
  kill -TERM "${pid}" 2>/dev/null || true
  for i in $(seq 1 10); do
    kill -0 "${pid}" 2>/dev/null || break
    sleep 0.1
  done
  if kill -0 "${pid}" 2>/dev/null; then
    kill -KILL "${pid}" 2>/dev/null || true
  fi
  wait "${pid}" 2>/dev/null || true
  cat "${out}"
  cat "${err}" >&2
  rm -f "${out}" "${err}"
  fail 'git diff --check timed out after 10s'
}

run_hygiene() {
  log 'Shell syntax'
  bash -n \
    bin/herdr-mcp \
    bin/watchdog.sh \
    bin/lib/i18n.sh \
    scripts/ci-herdr-runtime.sh \
    scripts/release-gate.sh \
    scripts/sign-macos-release.sh
  log 'Package surface'
  npm pack --dry-run
  log 'Diff hygiene'
  run_git_diff_check_bounded
}

cd "${ROOT}"
scrub_test_environment

case "${PHASE}" in
  rust)
    run_rust
    ;;
  node)
    run_node
    ;;
  hygiene)
    run_hygiene
    ;;
  full)
    run_rust
    run_node
    run_hygiene
    ;;
  *)
    fail 'usage: scripts/release-gate.sh [full|rust|node|hygiene]'
    ;;
esac

log "PASS phase=${PHASE}"
