#!/usr/bin/env bash
# herdr-mcp watchdog (A+B): keep MCP alive; soft-probe herdr control plane; never kill herdr daemon.
#
# Usage:
#   bin/watchdog.sh once          # one check (LaunchAgent)
#   bin/watchdog.sh status        # print last state
#   bin/watchdog.sh install       # install LaunchAgent (every 120s)
#   bin/watchdog.sh uninstall     # remove LaunchAgent
#
# Policy:
#   - MCP process missing / local /mcp not 200|401 -> consecutive fail; after N -> herdr-mcp restart
#   - herdr socket missing / agent.list TaskGroup -> log only (no daemon restart)
#   - never auto-retry herdr_prompt
set -euo pipefail

ROOT="${HERDR_MCP_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
CFG_DIR="${HERDR_MCP_CONFIG_DIR:-$HOME/.config/herdr-mcp}"
STATE_FILE="$CFG_DIR/watchdog.state.json"
LOG_FILE="$CFG_DIR/watchdog.log"
NEED_FILE="$CFG_DIR/.watchdog.need_restart"
PLIST_SERVER="$HOME/Library/LaunchAgents/dev.herdr-mcp.server.plist"
PLIST_WATCH="$HOME/Library/LaunchAgents/dev.herdr-mcp.watchdog.plist"
LABEL_WATCH="dev.herdr-mcp.watchdog"
LOCAL_URL="http://127.0.0.1:8772/mcp"
SOCK="${HERDR_SOCKET_PATH:-$HOME/.config/herdr/herdr.sock}"

FAIL_THRESHOLD="${HERDR_MCP_WATCHDOG_FAIL_THRESHOLD:-2}"
RESTART_COOLDOWN_SEC="${HERDR_MCP_WATCHDOG_COOLDOWN_SEC:-600}"
INTERVAL_SEC="${HERDR_MCP_WATCHDOG_INTERVAL_SEC:-120}"

mkdir -p "$CFG_DIR"

log_line() {
  local ts
  ts="$(date '+%Y-%m-%d %H:%M:%S')"
  echo "[$ts] $*" | tee -a "$LOG_FILE"
}

get_token() {
  /usr/libexec/PlistBuddy -c "Print :EnvironmentVariables:HERDR_MCP_TOKEN" "$PLIST_SERVER" 2>/dev/null || echo ""
}

is_mcp_running() {
  pgrep -f "dist/server.js" >/dev/null 2>&1
}

check_local_http() {
  local token code
  token="$(get_token)"
  code="$(curl -s -o /dev/null -w "%{http_code}" -m 3 \
    -X POST "$LOCAL_URL" \
    -H "Authorization: Bearer ${token}" \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"watchdog","version":"0"}}}' \
    2>/dev/null || echo "000")"
  echo "$code"
}

# Soft herdr control-plane probe via UNIX socket (no mutation).
# Prints: ok | missing_socket | taskgroup | error:<msg>
probe_herdr_soft() {
  HERDR_SOCKET_PATH="$SOCK" python3 - <<'PY'
import json, os, socket, sys
sock_path = os.environ.get("HERDR_SOCKET_PATH") or os.path.expanduser("~/.config/herdr/herdr.sock")
if not os.path.exists(sock_path):
    print("missing_socket")
    sys.exit(0)

def call(method, params=None, timeout=4.0):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(sock_path)
    req = {"id": "wd-" + method, "method": method, "params": params or {}}
    s.sendall((json.dumps(req) + "\n").encode())
    buf = b""
    while b"\n" not in buf:
        chunk = s.recv(65536)
        if not chunk:
            break
        buf += chunk
    s.close()
    return json.loads(buf.split(b"\n", 1)[0].decode())

def is_tg(msg):
    m = msg or ""
    return ("ExceptionGroup" in m) or ("unhandled errors in a TaskGroup" in m) or (
        "TaskGroup" in m and ("unhandled" in m or "sub-exception" in m)
    )

try:
    r = call("agent.list")
    if "error" in r:
        msg = str((r.get("error") or {}).get("message") or r.get("error"))
        print("taskgroup" if is_tg(msg) else ("error:" + msg[:120]))
        sys.exit(0)
    try:
        call("workspace.list")
    except Exception:
        pass
    print("ok")
except Exception as e:
    msg = str(e)
    print("taskgroup" if is_tg(msg) else ("error:" + msg[:120]))
PY
}

update_state_and_decide() {
  # env: MCP_OK HTTP_CODE CONTROL THRESHOLD STATE_FILE NEED_FILE
  MCP_OK="$1" HTTP_CODE="$2" CONTROL="$3" \
  FAIL_THRESHOLD="$FAIL_THRESHOLD" STATE_FILE="$STATE_FILE" NEED_FILE="$NEED_FILE" \
  python3 - <<'PY'
import json, os, time
path = os.environ["STATE_FILE"]
need_path = os.environ["NEED_FILE"]
threshold = int(os.environ["FAIL_THRESHOLD"])
mcp_ok = os.environ["MCP_OK"] == "1"
http_code = os.environ["HTTP_CODE"]
control = os.environ["CONTROL"]
now = int(time.time())
st = {}
if os.path.exists(path):
    try:
        st = json.load(open(path))
    except Exception:
        st = {}
st.pop("restarts_today", None)
st.pop("restarts_day", None)
st["updated_at"] = now
st["last_mcp"] = "ok" if mcp_ok else ("fail:" + http_code)
st["last_control"] = control
if mcp_ok:
    st["consecutive_mcp_fail"] = 0
else:
    st["consecutive_mcp_fail"] = int(st.get("consecutive_mcp_fail") or 0) + 1
if control == "ok":
    st["consecutive_control_fail"] = 0
else:
    st["consecutive_control_fail"] = int(st.get("consecutive_control_fail") or 0) + 1
need = (not mcp_ok) and st["consecutive_mcp_fail"] >= threshold
st["last_action"] = "restart_pending" if need else "check"
open(path, "w").write(json.dumps(st, indent=2) + "\n")
open(need_path, "w").write("1" if need else "0")
print(json.dumps({
    "mcp": st["last_mcp"],
    "control": control,
    "consecutive_mcp_fail": st["consecutive_mcp_fail"],
    "consecutive_control_fail": st["consecutive_control_fail"],
    "need_restart": need,
}, ensure_ascii=False))
PY
}

maybe_restart_mcp() {
  STATE_FILE="$STATE_FILE" LOG_FILE="$LOG_FILE" ROOT="$ROOT" \
  RESTART_COOLDOWN_SEC="$RESTART_COOLDOWN_SEC" \
  python3 - <<'PY'
import json, os, subprocess, sys, time
state_path = os.environ["STATE_FILE"]
log_path = os.environ["LOG_FILE"]
cooldown = int(os.environ["RESTART_COOLDOWN_SEC"])
cli = os.path.join(os.environ["ROOT"], "bin", "herdr-mcp")

def log(msg):
    ts = time.strftime("%Y-%m-%d %H:%M:%S")
    line = "[%s] %s" % (ts, msg)
    print(line)
    with open(log_path, "a") as f:
        f.write(line + "\n")

st = {}
if os.path.exists(state_path):
    try:
        st = json.load(open(state_path))
    except Exception:
        st = {}
now = int(time.time())
last_at = int(st.get("last_restart_at") or 0)
if last_at and now - last_at < cooldown:
    left = cooldown - (now - last_at)
    log("mcp restart skipped: cooldown %ss left" % left)
    st["last_action"] = "skip_cooldown"
    st["updated_at"] = now
    open(state_path, "w").write(json.dumps(st, indent=2) + "\n")
    sys.exit(0)
log("mcp restart: invoking herdr-mcp restart")
r = subprocess.run([cli, "restart"], capture_output=True, text=True)
out = ((r.stdout or "") + (r.stderr or "")).strip().replace("\n", " | ")
log("mcp restart exit=%s out=%s" % (r.returncode, out[:300]))
st["last_restart_at"] = now
st["restarts_total"] = int(st.get("restarts_total") or 0) + 1
st["consecutive_mcp_fail"] = 0
st["last_action"] = "restart_mcp"
st["updated_at"] = now
# Drop legacy daily-cap fields if present.
st.pop("restarts_today", None)
st.pop("restarts_day", None)
open(state_path, "w").write(json.dumps(st, indent=2) + "\n")
sys.exit(0 if r.returncode == 0 else 1)
PY
}

run_once() {
  local http_code="000" mcp_ok=0 control need
  if is_mcp_running; then
    http_code="$(check_local_http)"
    if [[ "$http_code" == "200" || "$http_code" == "401" ]]; then
      mcp_ok=1
    fi
  else
    http_code="proc_down"
  fi
  control="$(probe_herdr_soft || echo error:probe_failed)"

  update_state_and_decide "$mcp_ok" "$http_code" "$control" >/dev/null

  log_line "check mcp=$(python3 -c "import json;print(json.load(open('$STATE_FILE'))['last_mcp'])") control=$(python3 -c "import json;print(json.load(open('$STATE_FILE'))['last_control'])") mcp_fail=$(python3 -c "import json;print(json.load(open('$STATE_FILE'))['consecutive_mcp_fail'])") ctrl_fail=$(python3 -c "import json;print(json.load(open('$STATE_FILE'))['consecutive_control_fail'])")"

  if [[ "$control" != "ok" ]]; then
    log_line "control-plane soft probe: $control (log only; will not restart herdr daemon)"
  fi

  need="$(cat "$NEED_FILE" 2>/dev/null || echo 0)"
  if [[ "$need" == "1" ]]; then
    maybe_restart_mcp
  fi
}

cmd_status() {
  if [[ -f "$STATE_FILE" ]]; then
    echo "state: $STATE_FILE"
    cat "$STATE_FILE"
  else
    echo "no state yet (run: herdr-mcp watchdog once)"
  fi
  echo ""
  if launchctl list 2>/dev/null | grep -q "$LABEL_WATCH"; then
    echo "launchd: loaded ($LABEL_WATCH)"
  else
    echo "launchd: not loaded"
  fi
  if [[ -f "$LOG_FILE" ]]; then
    echo "log: $LOG_FILE (last 8)"
    tail -8 "$LOG_FILE"
  fi
}

cmd_install() {
  local source_bin runtime_bin
  source_bin="$ROOT/bin/watchdog.sh"
  runtime_bin="$CFG_DIR/watchdog.sh"
  mkdir -p "$CFG_DIR"
  cp "$source_bin" "$runtime_bin"
  chmod 700 "$runtime_bin"
  cat >"$PLIST_WATCH" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>${LABEL_WATCH}</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>${runtime_bin}</string>
    <string>once</string>
  </array>
  <key>StartInterval</key>
  <integer>${INTERVAL_SEC}</integer>
  <key>RunAtLoad</key>
  <true/>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>${HOME}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
    <key>HOME</key>
    <string>${HOME}</string>
    <key>HERDR_SOCKET_PATH</key>
    <string>${SOCK}</string>
    <key>HERDR_MCP_ROOT</key>
    <string>${ROOT}</string>
  </dict>
  <key>StandardOutPath</key>
  <string>${CFG_DIR}/watchdog.launchd.out.log</string>
  <key>StandardErrorPath</key>
  <string>${CFG_DIR}/watchdog.launchd.err.log</string>
</dict>
</plist>
EOF
  launchctl unload "$PLIST_WATCH" 2>/dev/null || true
  launchctl load "$PLIST_WATCH"
  log_line "watchdog installed: every ${INTERVAL_SEC}s -> $PLIST_WATCH"
  echo "installed: $PLIST_WATCH (interval ${INTERVAL_SEC}s)"
}

cmd_uninstall() {
  launchctl unload "$PLIST_WATCH" 2>/dev/null || true
  rm -f "$PLIST_WATCH" "$CFG_DIR/watchdog.sh"
  log_line "watchdog uninstalled"
  echo "uninstalled"
}

case "${1:-once}" in
  once) run_once ;;
  status) cmd_status ;;
  install) cmd_install ;;
  uninstall) cmd_uninstall ;;
  -h|--help)
    echo "Usage: watchdog.sh once|status|install|uninstall"
    ;;
  *)
    echo "unknown: $1" >&2
    exit 1
    ;;
esac
