#!/usr/bin/env bash
# herdr-mcp Rust-era health watchdog.
#
# The managed server LaunchAgent owns process-crash supervision with
# RunAtLoad=true + KeepAlive=true. This periodic sidecar handles the separate
# failure mode where that job remains loaded but loopback health is repeatedly
# unavailable. It never bootstraps an explicitly stopped server.
set -euo pipefail

CFG_DIR="${HERDR_MCP_CONFIG_DIR:-$HOME/.config/herdr-mcp}"
STATE_FILE="$CFG_DIR/watchdog.state.json"
LOG_FILE="$CFG_DIR/watchdog.log"
PLIST_WATCH="$HOME/Library/LaunchAgents/dev.herdr-mcp.health-watchdog.plist"
LABEL_SERVER="dev.herdr-mcp.server"
LABEL_WATCH="dev.herdr-mcp.health-watchdog"
RUNTIME_BIN="${HERDR_MCP_RUNTIME_BIN:-$HOME/.config/herdr-mcp/runtime/current/herdr-mcp}"
HEALTH_URL="http://127.0.0.1:8772/health"
LAUNCHCTL_BIN="${HERDR_MCP_LAUNCHCTL_BIN:-/bin/launchctl}"
CURL_BIN="${HERDR_MCP_CURL_BIN:-/usr/bin/curl}"
LSOF_BIN="${HERDR_MCP_LSOF_BIN:-/usr/sbin/lsof}"

# Two failed probes at 15-second cadence fit inside the remote planner's
# bounded ~35s reconnect window while ensuring one transient probe can never
# trigger a restart.
FAIL_THRESHOLD="${HERDR_MCP_WATCHDOG_FAIL_THRESHOLD:-2}"
RESTART_COOLDOWN_SEC="${HERDR_MCP_WATCHDOG_COOLDOWN_SEC:-60}"
INTERVAL_SEC="${HERDR_MCP_WATCHDOG_INTERVAL_SEC:-15}"
HEALTH_TIMEOUT_SEC="${HERDR_MCP_WATCHDOG_HEALTH_TIMEOUT_SEC:-2}"

mkdir -p "$CFG_DIR"

log_line() {
  local ts
  ts="$(date '+%Y-%m-%d %H:%M:%S')"
  echo "[$ts] $*" | tee -a "$LOG_FILE"
}

server_target() { printf 'gui/%s/%s' "$(id -u)" "$LABEL_SERVER"; }
watchdog_target() { printf 'gui/%s/%s' "$(id -u)" "$LABEL_WATCH"; }

server_loaded() {
  "$LAUNCHCTL_BIN" print "$(server_target)" >/dev/null 2>&1
}

health_code() {
  local code="000"
  if code="$("$CURL_BIN" -s -o /dev/null -w "%{http_code}" --connect-timeout 1 -m "$HEALTH_TIMEOUT_SEC" "$HEALTH_URL" 2>/dev/null)"; then
    :
  else
    code="000"
  fi
  [[ "$code" =~ ^[0-9]{3}$ ]] || code="000"
  printf '%s\n' "$code"
}

service_mutation_active() {
  local lock="$CFG_DIR/service-mutation.lock"
  [[ -e "$lock" ]] || return 1
  # The lock file is persistent. Normally lsof distinguishes an active holder
  # from an idle file; if the probe itself is unavailable, suppress recovery
  # rather than risking a kickstart during a legitimate service mutation.
  [[ -x "$LSOF_BIN" ]] || return 0
  "$LSOF_BIN" -t "$lock" >/dev/null 2>&1
}

update_active() {
  [[ -x "$RUNTIME_BIN" ]] || return 1
  local json state
  json="$("$RUNTIME_BIN" update status 2>/dev/null || true)"
  [[ -n "$json" ]] || return 1
  state="$(printf '%s' "$json" | python3 -c 'import json,sys
try:
 d=json.load(sys.stdin); print(((d.get("job") or {}).get("state") or "").lower())
except Exception: print("")' 2>/dev/null)"
  case "$state" in
    queued|installing) return 0 ;;
    *) return 1 ;;
  esac
}

guardian_active() {
  local tx state
  while IFS= read -r tx; do
    state="$(python3 - "$tx" <<'PY' 2>/dev/null || true
import json, sys
try:
    with open(sys.argv[1]) as fh:
        value = json.load(fh)
    print(str(value.get("state") or "").lower())
except Exception:
    pass
PY
)"
    case "$state" in
      armed|watching|recovering) return 0 ;;
    esac
  done < <(find "$CFG_DIR/guardians" -mindepth 2 -maxdepth 2 -name transaction.json -type f 2>/dev/null || true)
  return 1
}

lifecycle_suppression_reason() {
  if service_mutation_active; then
    echo mutation_active
  elif update_active; then
    echo update_active
  elif guardian_active; then
    echo guardian_active
  else
    echo none
  fi
}

update_state_and_decide() {
  LOADED="$1" HEALTH="$2" SUPPRESSION="$3" \
  FAIL_THRESHOLD="$FAIL_THRESHOLD" RESTART_COOLDOWN_SEC="$RESTART_COOLDOWN_SEC" \
  STATE_FILE="$STATE_FILE" python3 - <<'PY'
import json, os, time
path = os.environ["STATE_FILE"]
loaded = os.environ["LOADED"] == "1"
health = os.environ["HEALTH"]
suppression = os.environ["SUPPRESSION"]
threshold = max(1, int(os.environ["FAIL_THRESHOLD"]))
cooldown = max(0, int(os.environ["RESTART_COOLDOWN_SEC"]))
now = int(time.time())
state = {}
if os.path.exists(path):
    try:
        with open(path) as fh: state = json.load(fh)
    except Exception: state = {}
state["updated_at"] = now
state["server_loaded"] = loaded
state["last_health_code"] = health
if not loaded:
    state["consecutive_fail"] = 0
    state["last_action"] = "stopped"
    decision = "none"
elif suppression != "none":
    state["consecutive_fail"] = 0
    state["last_action"] = "suppressed_" + suppression
    decision = "none"
elif health == "200":
    state["consecutive_fail"] = 0
    state["last_action"] = "healthy"
    decision = "none"
else:
    state["consecutive_fail"] = int(state.get("consecutive_fail") or 0) + 1
    last_restart = int(state.get("last_restart_at") or 0)
    if state["consecutive_fail"] >= threshold and now - last_restart >= cooldown:
        state["last_action"] = "restart_pending"
        decision = "kickstart"
    elif state["consecutive_fail"] >= threshold:
        state["last_action"] = "cooldown"
        decision = "none"
    else:
        state["last_action"] = "check"
        decision = "none"
with open(path, "w") as fh:
    json.dump(state, fh, indent=2); fh.write("\n")
print(decision)
PY
}

record_kickstart_result() {
  RESULT="$1" STATE_FILE="$STATE_FILE" python3 - <<'PY'
import json, os, time
path = os.environ["STATE_FILE"]
result = os.environ["RESULT"]
state = {}
if os.path.exists(path):
    try:
        with open(path) as fh: state = json.load(fh)
    except Exception: state = {}
now = int(time.time())
state["updated_at"] = now
state["last_restart_at"] = now
if result == "ok":
    state["restarts_total"] = int(state.get("restarts_total") or 0) + 1
    state["consecutive_fail"] = 0
    state["last_action"] = "kickstart"
else:
    state["consecutive_fail"] = 0
    state["last_action"] = "kickstart_failed"
with open(path, "w") as fh:
    json.dump(state, fh, indent=2); fh.write("\n")
PY
}

run_once() {
  local loaded=0 code="000" suppression="none" decision
  if server_loaded; then
    loaded=1
    code="$(health_code)"
  fi

  if [[ "$loaded" != "1" ]]; then
    update_state_and_decide "0" "000" "none" >/dev/null
    log_line "server stopped/unloaded; watchdog will not start it"
    return 0
  fi

  if [[ "$code" == "200" ]]; then
    update_state_and_decide "1" "$code" "none" >/dev/null
    log_line "check server=loaded health=200"
    return 0
  fi

  # Only an unhealthy observation needs lifecycle suppression probes. This
  # keeps the healthy 15-second path cheap and prevents a legitimate update,
  # rollback, or service mutation from contributing to the failure counter.
  suppression="$(lifecycle_suppression_reason)"
  decision="$(update_state_and_decide "$loaded" "$code" "$suppression")"

  if [[ "$suppression" != "none" ]]; then
    log_line "health recovery suppressed: $suppression"
    return 0
  fi

  log_line "check server=loaded health=$code"
  [[ "$decision" == "kickstart" ]] || return 0

  # Re-check both explicit stop and lifecycle activity immediately before the
  # only server mutation so a concurrent operator/update always wins.
  if ! server_loaded; then
    update_state_and_decide "0" "000" "none" >/dev/null
    log_line "restart cancelled: server was explicitly unloaded before kickstart"
    return 0
  fi
  suppression="$(lifecycle_suppression_reason)"
  if [[ "$suppression" != "none" ]]; then
    update_state_and_decide "1" "$code" "$suppression" >/dev/null
    log_line "restart cancelled: lifecycle became active ($suppression)"
    return 0
  fi

  if "$LAUNCHCTL_BIN" kickstart -k "$(server_target)" >>"$LOG_FILE" 2>&1; then
    record_kickstart_result ok
    log_line "server kickstart requested after ${FAIL_THRESHOLD} consecutive failed health checks"
  else
    record_kickstart_result error
    log_line "server kickstart failed"
    return 1
  fi
}

cmd_status() {
  if [[ -f "$STATE_FILE" ]]; then
    echo "state: $STATE_FILE"; cat "$STATE_FILE"
  else
    echo "no state yet (run: watchdog.sh once)"
  fi
  echo
  if "$LAUNCHCTL_BIN" print "$(watchdog_target)" >/dev/null 2>&1; then
    echo "launchd: loaded ($LABEL_WATCH)"
  else
    echo "launchd: not loaded"
  fi
  if [[ -f "$LOG_FILE" ]]; then
    echo "log: $LOG_FILE (last 8)"; tail -8 "$LOG_FILE"
  fi
}

cmd_install() {
  local source_bin runtime_bin
  source_bin="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
  runtime_bin="$CFG_DIR/watchdog.sh"
  mkdir -p "$CFG_DIR"
  mkdir -p "$HOME/Library/LaunchAgents"
  if [[ "$source_bin" != "$runtime_bin" ]]; then
    cp "$source_bin" "$runtime_bin"
  fi
  chmod 700 "$runtime_bin"
  cat >"$PLIST_WATCH" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>${LABEL_WATCH}</string>
  <key>ProgramArguments</key>
  <array><string>/bin/bash</string><string>${runtime_bin}</string><string>once</string></array>
  <key>StartInterval</key><integer>${INTERVAL_SEC}</integer>
  <key>RunAtLoad</key><true/>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>${HOME}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
    <key>HOME</key><string>${HOME}</string>
  </dict>
  <key>StandardOutPath</key><string>${CFG_DIR}/watchdog.launchd.out.log</string>
  <key>StandardErrorPath</key><string>${CFG_DIR}/watchdog.launchd.err.log</string>
</dict>
</plist>
EOF
  "$LAUNCHCTL_BIN" bootout "$(watchdog_target)" >/dev/null 2>&1 || true
  "$LAUNCHCTL_BIN" enable "$(watchdog_target)"
  "$LAUNCHCTL_BIN" bootstrap "gui/$(id -u)" "$PLIST_WATCH"
  log_line "watchdog installed: every ${INTERVAL_SEC}s -> $PLIST_WATCH"
  echo "installed: $PLIST_WATCH (interval ${INTERVAL_SEC}s)"
}

cmd_uninstall() {
  "$LAUNCHCTL_BIN" bootout "$(watchdog_target)" >/dev/null 2>&1 || true
  "$LAUNCHCTL_BIN" disable "$(watchdog_target)" >/dev/null 2>&1 || true
  rm -f "$PLIST_WATCH" "$CFG_DIR/watchdog.sh" "$STATE_FILE"
  log_line "watchdog uninstalled"
  echo "uninstalled"
}

case "${1:-once}" in
  once) run_once ;;
  status) cmd_status ;;
  install) cmd_install ;;
  uninstall) cmd_uninstall ;;
  -h|--help) echo "Usage: watchdog.sh once|status|install|uninstall" ;;
  *) echo "unknown: $1" >&2; exit 1 ;;
esac
