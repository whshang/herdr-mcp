#!/usr/bin/env bash
# herdr-mcp Rust-era health watchdog.
#
# The managed server LaunchAgent owns process-crash supervision with
# RunAtLoad=true + KeepAlive=true. This periodic sidecar handles the separate
# failure mode where that job remains loaded but loopback health is repeatedly
# unavailable. It never bootstraps an explicitly stopped server.
set -euo pipefail

CFG_DIR="${HERDR_MCP_CONFIG_DIR:-$HOME/.config/herdr-mcp}"
STATE_FILE="$CFG_DIR/health-watchdog.state.json"
LOG_FILE="$CFG_DIR/health-watchdog.log"
PLIST_WATCH="$HOME/Library/LaunchAgents/dev.herdr-mcp.health-watchdog.plist"
LABEL_SERVER="dev.herdr-mcp.server"
LABEL_WATCH="dev.herdr-mcp.health-watchdog"
LABEL_LINK_PROD="dev.herdr-mcp.link-prod"
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
# Do not turn a persistently unhealthy server into an endless kill/restart loop.
# After a small burst of unsuccessful recovery attempts, keep probing read-only
# and suppress further kickstarts for a longer backoff. A real HTTP 200 (or a
# legitimate lifecycle/explicit-stop transition) resets the recovery circuit.
RESTART_BURST_LIMIT="${HERDR_MCP_WATCHDOG_RESTART_BURST_LIMIT:-3}"
RESTART_STORM_BACKOFF_SEC="${HERDR_MCP_WATCHDOG_RESTART_BACKOFF_SEC:-300}"
# Bounded Link-health recovery uses its own persistent-mismatch threshold and a
# longer cooldown so a generation switch can never trigger a WSS interruption
# storm: at most one kickstart per cooldown window, only after the mismatch
# persists across LINK_FAIL_THRESHOLD healthy-server observations.
LINK_FAIL_THRESHOLD="${HERDR_MCP_LINK_WATCHDOG_FAIL_THRESHOLD:-2}"
LINK_RESTART_COOLDOWN_SEC="${HERDR_MCP_LINK_WATCHDOG_COOLDOWN_SEC:-120}"
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
  RESTART_BURST_LIMIT="$RESTART_BURST_LIMIT" \
  RESTART_STORM_BACKOFF_SEC="$RESTART_STORM_BACKOFF_SEC" \
  STATE_FILE="$STATE_FILE" python3 - <<'PY'
import json, os, tempfile, time
def atomic_write(path, state):
    directory = os.path.dirname(path) or "."
    fd, tmp = tempfile.mkstemp(prefix=".health-watchdog.", dir=directory)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w") as fh:
            fd = -1
            json.dump(state, fh, indent=2); fh.write("\n")
            fh.flush(); os.fsync(fh.fileno())
        os.replace(tmp, path)
    finally:
        if fd >= 0: os.close(fd)
        try: os.unlink(tmp)
        except FileNotFoundError: pass
path = os.environ["STATE_FILE"]
loaded = os.environ["LOADED"] == "1"
health = os.environ["HEALTH"]
suppression = os.environ["SUPPRESSION"]
threshold = max(1, int(os.environ["FAIL_THRESHOLD"]))
cooldown = max(0, int(os.environ["RESTART_COOLDOWN_SEC"]))
burst_limit = max(1, int(os.environ["RESTART_BURST_LIMIT"]))
storm_backoff = max(1, int(os.environ["RESTART_STORM_BACKOFF_SEC"]))
now = int(time.time())
state = {}
state_corrupt = False
if os.path.exists(path):
    try:
        with open(path) as fh: state = json.load(fh)
    except Exception:
        state = {}
        state_corrupt = True
state["updated_at"] = now
state["server_loaded"] = loaded
state["last_health_code"] = health
if not loaded:
    state["consecutive_fail"] = 0
    state["recovery_attempts_without_health"] = 0
    state["restart_suppressed_until"] = 0
    state["last_action"] = "stopped"
    decision = "none"
elif suppression != "none":
    state["consecutive_fail"] = 0
    state["recovery_attempts_without_health"] = 0
    state["restart_suppressed_until"] = 0
    state["last_action"] = "suppressed_" + suppression
    decision = "none"
elif health == "200":
    state["consecutive_fail"] = 0
    state["recovery_attempts_without_health"] = 0
    state["restart_suppressed_until"] = 0
    state["last_action"] = "healthy"
    decision = "none"
elif state_corrupt:
    state["consecutive_fail"] = 0
    state["recovery_attempts_without_health"] = burst_limit
    state["restart_suppressed_until"] = now + storm_backoff
    state["last_action"] = "state_corrupt_suppressed"
    decision = "none"
else:
    state["consecutive_fail"] = int(state.get("consecutive_fail") or 0) + 1
    attempts = int(state.get("recovery_attempts_without_health") or 0)
    suppressed_until = int(state.get("restart_suppressed_until") or 0)
    if suppressed_until and now >= suppressed_until:
        attempts = 0
        suppressed_until = 0
        state["recovery_attempts_without_health"] = 0
        state["restart_suppressed_until"] = 0
    last_restart = int(state.get("last_restart_at") or 0)
    if suppressed_until > now:
        state["last_action"] = "restart_storm_suppressed"
        decision = "none"
    elif attempts >= burst_limit:
        state["restart_suppressed_until"] = now + storm_backoff
        state["last_action"] = "restart_storm_suppressed"
        decision = "none"
    elif state["consecutive_fail"] >= threshold and now - last_restart >= cooldown:
        state["last_action"] = "restart_pending"
        decision = "kickstart"
    elif state["consecutive_fail"] >= threshold:
        state["last_action"] = "cooldown"
        decision = "none"
    else:
        state["last_action"] = "check"
        decision = "none"
atomic_write(path, state)
print(decision)
PY
}

record_kickstart_result() {
  RESULT="$1" STATE_FILE="$STATE_FILE" python3 - <<'PY'
import json, os, tempfile, time
def atomic_write(path, state):
    directory = os.path.dirname(path) or "."
    fd, tmp = tempfile.mkstemp(prefix=".health-watchdog.", dir=directory)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w") as fh:
            fd = -1
            json.dump(state, fh, indent=2); fh.write("\n")
            fh.flush(); os.fsync(fh.fileno())
        os.replace(tmp, path)
    finally:
        if fd >= 0: os.close(fd)
        try: os.unlink(tmp)
        except FileNotFoundError: pass
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
state["recovery_attempts_without_health"] = int(state.get("recovery_attempts_without_health") or 0) + 1
if result == "ok":
    state["restarts_total"] = int(state.get("restarts_total") or 0) + 1
    state["consecutive_fail"] = 0
    state["last_action"] = "kickstart"
else:
    state["consecutive_fail"] = 0
    state["last_action"] = "kickstart_failed"
atomic_write(path, state)
PY
}

link_target() { printf 'gui/%s/%s' "$(id -u)" "$LABEL_LINK_PROD"; }

# Collect read-only `herdr-mcp link status` JSON (local runtime binary). The
# production Link ownership/alignment semantics (runtime-status-prod.json
# preference, loaded launchd generation) live in the Rust binary, so this
# script never names or reads runtime-control/status files itself; a stale
# non-prod file cannot affect recovery. Missing/malformed output is
# link_unobservable (fail-closed, no mutation).
link_status_json() {
  [[ -x "$RUNTIME_BIN" ]] || return 1
  "$RUNTIME_BIN" link status 2>/dev/null || true
}

# Parse bounded launchd evidence (runs + last exit code) for the production
# Link. Absent fields mean evidence is unavailable; the watchdog then relies
# on generation-mismatch detection only.
link_launchd_evidence() {
  "$LAUNCHCTL_BIN" print "$(link_target)" 2>/dev/null || true
}

update_link_state_and_decide() {
  LINK_STATUS_JSON="$1" LINK_EVIDENCE="$2" SUPPRESSION="$3" \
  LINK_FAIL_THRESHOLD="$LINK_FAIL_THRESHOLD" LINK_RESTART_COOLDOWN_SEC="$LINK_RESTART_COOLDOWN_SEC" \
  STATE_FILE="$STATE_FILE" python3 - <<'PY'
import json, os, re, tempfile, time
def atomic_write(path, state):
    directory = os.path.dirname(path) or "."
    fd, tmp = tempfile.mkstemp(prefix=".health-watchdog.", dir=directory)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w") as fh:
            fd = -1
            json.dump(state, fh, indent=2); fh.write("\n")
            fh.flush(); os.fsync(fh.fileno())
        os.replace(tmp, path)
    finally:
        if fd >= 0: os.close(fd)
        try: os.unlink(tmp)
        except FileNotFoundError: pass
path = os.environ["STATE_FILE"]
status_json = os.environ["LINK_STATUS_JSON"]
launchd_evidence = os.environ["LINK_EVIDENCE"]
suppression = os.environ["SUPPRESSION"]
threshold = max(1, int(os.environ["LINK_FAIL_THRESHOLD"]))
cooldown = max(0, int(os.environ["LINK_RESTART_COOLDOWN_SEC"]))
now = int(time.time())
state = {}
if os.path.exists(path):
    try:
        with open(path) as fh: state = json.load(fh)
    except Exception: state = {}

observable = False
loaded = False
owner = "absent"
impl = "absent"
points_repo = False
points_managed = False
active_matches = False
loaded_stale = False
current_gen = None
active_gen = None
loaded_gen = None
try:
    doc = json.loads(status_json) if status_json else {}
    owner = str(doc.get("production_owner") or "absent")
    for agent in (doc.get("agents") or []):
        if agent.get("label") == "dev.herdr-mcp.link-prod":
            loaded = bool(agent.get("loaded"))
            impl = str(agent.get("implementation") or "absent")
            points_repo = bool(agent.get("points_at_repo_checkout"))
            points_managed = bool(agent.get("points_at_managed_runtime"))
    align = doc.get("production_runtime_alignment") or {}
    active_matches = bool(align.get("runtime_control_active_matches_current"))
    loaded_stale = bool(align.get("loaded_environment_stale"))
    current_gen = align.get("current_generation")
    active_gen = align.get("active_generation")
    loaded_gen = align.get("loaded_launchd_generation")
    observable = bool(doc.get("ok")) and bool(doc.get("production_owner")) \
        and bool(doc.get("production_runtime_alignment"))
except Exception:
    observable = False

# Generation evidence must be present and non-empty before active!=current is
# actionable. Missing current or active generation is unobservable/fail-closed:
# no failure counter, no restart.
current_ok = isinstance(current_gen, str) and bool(current_gen.strip())
active_ok = isinstance(active_gen, str) and bool(active_gen.strip())
if not current_ok or not active_ok:
    observable = False

# Bounded launchd restart-storm evidence. A storm is only declared after a
# growth streak: the runs counter must increase across >=2 consecutive
# observations while the last exit code is non-zero (crash-loop). A single
# runs increment with a historical non-zero exit is not a storm. Storm is
# report/fail-closed only; never kickstart.
last_exit = None
runs = None
for line in launchd_evidence.splitlines():
    m = re.search(r"last exit code\s*=\s*(-?\d+)", line)
    if m:
        last_exit = int(m.group(1))
    m = re.search(r"runs\s*=\s*(\d+)", line)
    if m:
        runs = int(m.group(1))

prev_runs = state.get("link_last_launchd_runs")
storm_streak = int(state.get("link_storm_streak") or 0)
storm = False
if last_exit is not None and runs is not None:
    if last_exit != 0 and isinstance(prev_runs, int) and runs > prev_runs:
        storm_streak += 1
    else:
        storm_streak = 0
    if storm_streak >= 2:
        storm = True
    state["link_storm_streak"] = storm_streak
    state["link_last_launchd_runs"] = runs
    state["link_last_exit_code"] = last_exit
else:
    state["link_storm_streak"] = 0
    state["link_last_launchd_runs"] = None
    state["link_last_exit_code"] = None

state["link_observable"] = observable
state["link_loaded"] = loaded
state["link_production_owner"] = owner
state["link_implementation"] = impl
state["link_points_at_repo_checkout"] = points_repo
state["link_points_at_managed_runtime"] = points_managed
state["link_active_matches_current"] = active_matches
state["link_loaded_environment_stale"] = loaded_stale
state["link_current_generation"] = current_gen
state["link_active_generation"] = active_gen
state["link_loaded_launchd_generation"] = loaded_gen
state["link_restart_storm"] = storm

if not observable:
    state["link_consecutive_fail"] = 0
    state["link_last_action"] = "link_unobservable"
    decision = "none"
elif not loaded:
    state["link_consecutive_fail"] = 0
    state["link_last_action"] = "link_unloaded"
    decision = "none"
elif owner != "rust" or impl != "rust" or points_repo or not points_managed:
    state["link_consecutive_fail"] = 0
    state["link_last_action"] = "link_unowned"
    decision = "none"
elif suppression != "none":
    state["link_consecutive_fail"] = 0
    state["link_last_action"] = "link_suppressed_" + suppression
    decision = "none"
elif storm:
    state["link_consecutive_fail"] = 0
    state["link_last_action"] = "link_restart_storm"
    decision = "none"
elif not active_matches:
    state["link_consecutive_fail"] = int(state.get("link_consecutive_fail") or 0) + 1
    last_restart = int(state.get("link_last_restart_at") or 0)
    if state["link_consecutive_fail"] >= threshold and now - last_restart >= cooldown:
        state["link_last_action"] = "link_restart_pending"
        decision = "kickstart"
    elif state["link_consecutive_fail"] >= threshold:
        state["link_last_action"] = "link_cooldown"
        decision = "none"
    else:
        state["link_last_action"] = "link_check"
        decision = "none"
elif loaded_stale and active_matches:
    state["link_consecutive_fail"] = 0
    state["link_last_action"] = "link_degraded_startup_metadata"
    decision = "none"
else:
    state["link_consecutive_fail"] = 0
    state["link_last_action"] = "link_healthy"
    decision = "none"

state["updated_at"] = now
atomic_write(path, state)
print(decision)
PY
}

record_link_kickstart_result() {
  RESULT="$1" STATE_FILE="$STATE_FILE" python3 - <<'PY'
import json, os, tempfile, time
def atomic_write(path, state):
    directory = os.path.dirname(path) or "."
    fd, tmp = tempfile.mkstemp(prefix=".health-watchdog.", dir=directory)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w") as fh:
            fd = -1
            json.dump(state, fh, indent=2); fh.write("\n")
            fh.flush(); os.fsync(fh.fileno())
        os.replace(tmp, path)
    finally:
        if fd >= 0: os.close(fd)
        try: os.unlink(tmp)
        except FileNotFoundError: pass
path = os.environ["STATE_FILE"]
result = os.environ["RESULT"]
state = {}
if os.path.exists(path):
    try:
        with open(path) as fh: state = json.load(fh)
    except Exception: state = {}
now = int(time.time())
state["updated_at"] = now
state["link_last_restart_at"] = now
if result == "ok":
    state["link_restarts_total"] = int(state.get("link_restarts_total") or 0) + 1
    state["link_consecutive_fail"] = 0
    state["link_last_action"] = "link_kickstart"
else:
    state["link_consecutive_fail"] = 0
    state["link_last_action"] = "link_kickstart_failed"
atomic_write(path, state)
PY
}

# Local-only Link health observation. Runs only on the healthy-server path so
# the incident class "server healthy + Link unhealthy/offline" is covered while
# the existing server-only recovery branches stay untouched. It never touches
# dev.herdr-mcp.server, never bootstraps, and mutates exactly one owned label
# (dev.herdr-mcp.link-prod) only after threshold, cooldown, lifecycle
# suppression, and an immediate re-check.
run_link_check() {
  local status_json evidence suppression decision
  status_json="$(link_status_json || true)"
  if [[ -z "$status_json" ]]; then
    update_link_state_and_decide "" "" "none" >/dev/null
    log_line "link status unobservable (runtime binary missing); fail-closed, no mutation"
    return 0
  fi
  evidence="$(link_launchd_evidence)"
  # Lifecycle suppression is probed on every Link observation. It is local-only
  # (lsof lock, runtime update status, guardian transaction files) and keeps the
  # healthy 15s path free of any network call while preventing a legitimate
  # update/rollback/service mutation from contributing to the failure counter.
  suppression="$(lifecycle_suppression_reason)"
  decision="$(update_link_state_and_decide "$status_json" "$evidence" "$suppression")"
  log_line "link check decision=$decision"
  [[ "$decision" == "kickstart" ]] || return 0

  # A persistent active!=current mismatch exists. Re-check explicit unload,
  # ownership/alignment, and suppression immediately before the only Link
  # mutation so a concurrent operator always wins. Never bootstrap; never touch
  # the server label.
  if ! "$LAUNCHCTL_BIN" print "$(link_target)" >/dev/null 2>&1; then
    update_link_state_and_decide "$status_json" "$evidence" "none" >/dev/null
    log_line "link restart cancelled: link-prod was explicitly unloaded before kickstart"
    return 0
  fi
  status_json="$(link_status_json || true)"
  [[ -n "$status_json" ]] || return 0
  suppression="$(lifecycle_suppression_reason)"
  if [[ "$suppression" != "none" ]]; then
    update_link_state_and_decide "$status_json" "$evidence" "$suppression" >/dev/null
    log_line "link restart cancelled: lifecycle became active ($suppression)"
    return 0
  fi
  decision="$(update_link_state_and_decide "$status_json" "$evidence" "$suppression")"
  [[ "$decision" == "kickstart" ]] || return 0

  if "$LAUNCHCTL_BIN" kickstart -k "$(link_target)" >>"$LOG_FILE" 2>&1; then
    record_link_kickstart_result ok
    log_line "link kickstart requested after ${LINK_FAIL_THRESHOLD} consecutive active!=current observations"
  else
    record_link_kickstart_result error
    log_line "link kickstart failed"
    return 1
  fi
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
    run_link_check
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
  if "$LAUNCHCTL_BIN" print "$(link_target)" >/dev/null 2>&1; then
    echo "link-prod: loaded ($LABEL_LINK_PROD)"
  else
    echo "link-prod: not loaded"
  fi
  if [[ -f "$LOG_FILE" ]]; then
    echo "log: $LOG_FILE (last 8)"; tail -8 "$LOG_FILE"
  fi
}

cmd_install() {
  local source_bin runtime_bin
  source_bin="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
  runtime_bin="$CFG_DIR/health-watchdog.sh"
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
  <key>StandardOutPath</key><string>${CFG_DIR}/health-watchdog.launchd.out.log</string>
  <key>StandardErrorPath</key><string>${CFG_DIR}/health-watchdog.launchd.err.log</string>
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
  rm -f "$PLIST_WATCH" "$CFG_DIR/health-watchdog.sh" "$STATE_FILE"
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
