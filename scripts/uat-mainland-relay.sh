#!/bin/zsh
set -euo pipefail
umask 077

MODE="${1:-run}"
if [[ "$MODE" != "run" && "$MODE" != "--preflight" ]]; then
  echo "usage: scripts/uat-mainland-relay.sh [--preflight]" >&2
  exit 2
fi

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
BIN="$HOME/.config/herdr-mcp/runtime/current/herdr-mcp"
PLIST="$HOME/Library/LaunchAgents/dev.herdr-mcp.link-prod.plist"
LABEL="dev.herdr-mcp.link-prod"
DOMAIN="gui/$UID"
CACHE="$HOME/.config/herdr-mcp/relay-pool/last-known-good.json"
UAT_DIR="$HOME/.config/herdr-mcp/relay-uat"
R3="$UAT_DIR/r3-deno.json"
R4="$UAT_DIR/r4-supabase.json"
R5="$UAT_DIR/r5-combined.json"
WORKERS_ORIGIN="https://herdr-edge-prod.whshang.workers.dev"
WORKERS_WS="wss://herdr-edge-prod.whshang.workers.dev/ws"
CUSTOM_HEALTH="https://herdr-mcp.agentforme.cc.cd/health"
WORKERS_HEALTH="$WORKERS_ORIGIN/health"
DENO_HEALTH="https://relay.herdr-mcp.deno.net/health"
SUPABASE_HEALTH="https://sppeaueojvcxifimozqx.supabase.co/functions/v1/herdr-relay/health"

for path in "$BIN" "$PLIST" "$R3" "$R4" "$R5"; do
  [[ -e "$path" ]] || { echo "ERROR: required path missing: $path" >&2; exit 10; }
done
[[ -x "$BIN" ]] || { echo "ERROR: active herdr-mcp binary is not executable" >&2; exit 11; }
[[ -z "$(git status --porcelain)" ]] || { echo "ERROR: UAT checkout is dirty" >&2; git status --short; exit 12; }

HEAD="$(git rev-parse HEAD)"
DEV_STATUS="$($BIN dev status)"
RUNTIME_COMMIT="$(printf '%s\n' "$DEV_STATUS" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin)["source_commit"])')"
GEN="$(printf '%s\n' "$DEV_STATUS" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin)["active_generation"])')"
VERSION="$(printf '%s\n' "$DEV_STATUS" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin)["version"])')"
[[ "$HEAD" == "$RUNTIME_COMMIT" ]] || {
  echo "ERROR: Git HEAD and active DEV runtime differ" >&2
  echo "HEAD=$HEAD runtime=$RUNTIME_COMMIT" >&2
  exit 13
}

LINK_STATUS="$($BIN link status)"
read_link_field() {
  printf '%s\n' "$LINK_STATUS" | FIELD="$1" /usr/bin/python3 -c 'import json,os,sys; d=json.load(sys.stdin); a=next(x for x in d["agents"] if x["label"]=="dev.herdr-mcp.link-prod"); v=a[os.environ["FIELD"]]; print("" if v is None else v)'
}
CONTROL="$(read_link_field control_path)"
STATUS_PATH="$(read_link_field status_path)"
WSID="$(read_link_field workstation_id)"
KEYCHAIN_SERVICE="$(/usr/libexec/PlistBuddy -c 'Print :EnvironmentVariables:HERDR_LINK_KEYCHAIN_SERVICE' "$PLIST")"
LINK_WAS_LOADED="$(printf '%s\n' "$LINK_STATUS" | /usr/bin/python3 -c 'import json,sys; d=json.load(sys.stdin); a=next(x for x in d["agents"] if x["label"]=="dev.herdr-mcp.link-prod"); print("yes" if a["loaded"] else "no")')"
LOADED_STALE="$(printf '%s\n' "$LINK_STATUS" | /usr/bin/python3 -c 'import json,sys; print("yes" if json.load(sys.stdin)["production_runtime_alignment"]["loaded_environment_stale"] else "no")')"
[[ "$LOADED_STALE" == "no" ]] || { echo "ERROR: production Link launchd environment is stale before UAT" >&2; exit 14; }

IFACE="$(route -n get default 2>/dev/null | awk '/interface:/{print $2; exit}')"
[[ -n "$IFACE" ]] || { echo "ERROR: cannot resolve default interface" >&2; exit 15; }
SERVICE="$(networksetup -listnetworkserviceorder | awk -v iface="$IFACE" '
  /^\([0-9]+\) / { name=$0; sub(/^\([0-9]+\) /,"",name); next }
  /Device:/ && index($0,"Device: " iface ")") { print name; exit }
')"
[[ -n "$SERVICE" ]] || { echo "ERROR: cannot map interface $IFACE to a network service" >&2; exit 16; }

proxy_enabled() {
  networksetup "$1" "$SERVICE" | awk -F': ' '/^Enabled:/{print $2; exit}'
}
WEB_ORIG="$(proxy_enabled -getwebproxy)"
SECURE_ORIG="$(proxy_enabled -getsecurewebproxy)"
SOCKS_ORIG="$(proxy_enabled -getsocksfirewallproxy)"
AUTO_ORIG="$(proxy_enabled -getautoproxyurl)"
DISC_ORIG="$(networksetup -getproxyautodiscovery "$SERVICE" | awk -F': ' '/Auto Proxy Discovery:/{print $2; exit}')"

to_onoff() {
  case "$1" in
    Yes|On|on) echo on ;;
    *) echo off ;;
  esac
}

check_vpn_and_route() {
  if scutil --nc list 2>/dev/null | grep -q '(Connected)'; then
    echo "ERROR: an active VPN/Network Extension is still connected:" >&2
    scutil --nc list 2>/dev/null | grep '(Connected)' >&2 || true
    return 1
  fi
  local active_iface
  active_iface="$(route -n get default 2>/dev/null | awk '/interface:/{print $2; exit}')"
  [[ -n "$active_iface" && "$active_iface" != utun* ]] || {
    echo "ERROR: default route is missing or still uses a tunnel: ${active_iface:-missing}" >&2
    return 1
  }
  echo "NAKED_ROUTE_INTERFACE=$active_iface"
}

check_system_proxy_off() {
  local dump
  dump="$(scutil --proxy)"
  for key in HTTPEnable HTTPSEnable SOCKSEnable ProxyAutoConfigEnable ProxyAutoDiscoveryEnable; do
    if printf '%s\n' "$dump" | grep -Eq "^[[:space:]]*$key[[:space:]]*:[[:space:]]*1[[:space:]]*$"; then
      echo "ERROR: $key is still enabled" >&2
      return 1
    fi
  done
  echo "NAKED_SYSTEM_PROXY=off"
}

if [[ "$MODE" == "--preflight" ]]; then
  echo "HERDR_MAINLAND_RELAY_UAT_PREFLIGHT=PASS"
  echo "HEAD=$HEAD"
  echo "GENERATION=$GEN"
  echo "DEVICE=$WSID"
  echo "NETWORK_SERVICE=$SERVICE"
  echo "DEFAULT_INTERFACE=$IFACE"
  echo "PROXY_ORIGINAL web=$WEB_ORIG secure=$SECURE_ORIG socks=$SOCKS_ORIG auto=$AUTO_ORIG discovery=$DISC_ORIG"
  echo "MANIFEST_R3_SHA256=$(shasum -a 256 "$R3" | awk '{print $1}')"
  echo "MANIFEST_R4_SHA256=$(shasum -a 256 "$R4" | awk '{print $1}')"
  echo "MANIFEST_R5_SHA256=$(shasum -a 256 "$R5" | awk '{print $1}')"
  exit 0
fi

sudo -v
mkdir -p "$UAT_DIR"
chmod 700 "$UAT_DIR"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
LOG="$UAT_DIR/mainland-relay-$STAMP.log"
RESULT_FILE="$UAT_DIR/mainland-relay-$STAMP.result"
ORIG_CACHE="$UAT_DIR/mainland-relay-$STAMP.original-lkg.json"
CACHE_EXISTED=no
if [[ -f "$CACHE" ]]; then
  cp "$CACHE" "$ORIG_CACHE"
  chmod 600 "$ORIG_CACHE"
  CACHE_EXISTED=yes
fi

exec > >(tee -a "$LOG") 2>&1

RESULT=FAIL
CURRENT_LINK_PID=""
WORKERS_DIRECT=UNKNOWN
DENO_RESULT=FAIL
SUPABASE_RESULT=FAIL
COMBINED_RESULT=FAIL

restore_manifest() {
  if [[ "$RESULT" == PASS ]]; then
    install_manifest 5 combined "$R5" || true
  elif [[ "$CACHE_EXISTED" == yes && -f "$ORIG_CACHE" ]]; then
    local tmp="$CACHE.restore.$$"
    cp "$ORIG_CACHE" "$tmp"
    chmod 600 "$tmp"
    mv -f "$tmp" "$CACHE"
  else
    rm -f "$CACHE"
  fi
}

restore_network() {
  sudo networksetup -setwebproxystate "$SERVICE" "$(to_onoff "$WEB_ORIG")" || true
  sudo networksetup -setsecurewebproxystate "$SERVICE" "$(to_onoff "$SECURE_ORIG")" || true
  sudo networksetup -setsocksfirewallproxystate "$SERVICE" "$(to_onoff "$SOCKS_ORIG")" || true
  sudo networksetup -setautoproxystate "$SERVICE" "$(to_onoff "$AUTO_ORIG")" || true
  sudo networksetup -setproxyautodiscovery "$SERVICE" "$(to_onoff "$DISC_ORIG")" || true
}

restore_direct_link() {
  if [[ "$LINK_WAS_LOADED" == yes ]]; then
    if ! launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1; then
      launchctl bootstrap "$DOMAIN" "$PLIST" || true
    fi
    launchctl kickstart "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
  fi
}

cleanup() {
  local rc=$?
  trap - EXIT INT TERM HUP
  if [[ -n "${CURRENT_LINK_PID:-}" ]] && kill -0 "$CURRENT_LINK_PID" 2>/dev/null; then
    kill -TERM "$CURRENT_LINK_PID" 2>/dev/null || true
    wait "$CURRENT_LINK_PID" 2>/dev/null || true
  fi
  CURRENT_LINK_PID=""
  restore_manifest
  restore_network
  sleep 2
  restore_direct_link
  sleep 4
  {
    echo "HERDR_MAINLAND_RELAY_UAT_RESULT=$RESULT"
    echo "WORKERS_DEV_DIRECT=$WORKERS_DIRECT"
    echo "DENO_RELAY=$DENO_RESULT"
    echo "SUPABASE_RELAY=$SUPABASE_RESULT"
    echo "COMBINED_AUTO=$COMBINED_RESULT"
    echo "LOG=$LOG"
    echo "HEAD=$HEAD"
    echo "GENERATION=$GEN"
    echo "FINAL_POOL=$($BIN status 2>/dev/null | grep '^relay pool: ' || true)"
    echo "FINAL_LINK_ALIGNMENT=$($BIN link status 2>/dev/null | /usr/bin/python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("production_runtime_alignment",{}))' || true)"
    echo "FINAL_PROXY=$(scutil --proxy | tr '\n' ' ' | sed 's/[[:space:]][[:space:]]*/ /g')"
  } | tee "$RESULT_FILE"
  [[ "$RESULT" == PASS ]] && rm -f "$ORIG_CACHE"
  exit "$rc"
}
trap cleanup EXIT INT TERM HUP

install_manifest() {
  local revision="$1" name="$2" source="$3"
  local tmp="$CACHE.tmp.$$"
  cp "$source" "$tmp"
  chmod 600 "$tmp"
  mv -f "$tmp" "$CACHE"
  local line
  line="$($BIN status | grep '^relay pool: ')"
  echo "MANIFEST_${revision}_${name}=$line"
  [[ "$line" == *"source=cached-remote"*"revision=$revision"*"key_id=relay-prod-2026-09"*"freshness=fresh"* ]]
}

probe_url() {
  local name="$1" url="$2" body="$UAT_DIR/.probe-${STAMP}-${name}.body" meta rc
  set +e
  meta="$(curl --noproxy '*' -fsS --connect-timeout 8 --max-time 15 -o "$body" -w 'http=%{http_code} remote=%{remote_ip} connect=%{time_connect} total=%{time_total}' "$url" 2>&1)"
  rc=$?
  set -e
  local preview=""
  [[ -f "$body" ]] && preview="$(head -c 240 "$body" | tr '\n' ' ')"
  rm -f "$body"
  if [[ $rc -eq 0 ]]; then
    echo "PROBE_$name=PASS $meta body=$preview"
    return 0
  fi
  echo "PROBE_$name=FAIL rc=$rc $meta body=$preview"
  return "$rc"
}

established_443() {
  local pid="$1"
  lsof -nP -a -p "$pid" -iTCP -sTCP:ESTABLISHED 2>/dev/null | awk 'NR>1 && $9 ~ /->.*:443$/ {print}'
}

run_link_test() {
  local label="$1" revision="$2" manifest="$3" duration="$4" forced="$5"
  local link_log="$UAT_DIR/mainland-relay-$STAMP-$label.link.log"
  local route_args=()
  install_manifest "$revision" "$label" "$manifest" || return 1
  [[ "$forced" == yes ]] && route_args=(HERDR_LINK_ROUTE=relay)
  : > "$link_log"
  chmod 600 "$link_log"
  echo "LINK_TEST_${label}=START duration=${duration}s forced_relay=$forced"
  env -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY -u NO_PROXY -u http_proxy -u https_proxy -u all_proxy -u no_proxy -u HERDR_LINK_PROXY \
    HERDR_EDGE_URL="$WORKERS_WS" \
    HERDR_PUBLIC_ORIGIN="$WORKERS_ORIGIN" \
    HERDR_LINK_UPSTREAM_ORIGIN="$WORKERS_ORIGIN" \
    HERDR_WORKSTATION_ID="$WSID" \
    HERDR_RUNTIME_GENERATION="$GEN" \
    HERDR_RUNTIME_VERSION="$VERSION" \
    HERDR_RUNTIME_CONTROL_PATH="$CONTROL" \
    HERDR_RUNTIME_STATUS_PATH="$STATUS_PATH" \
    HERDR_LINK_KEYCHAIN_SERVICE="$KEYCHAIN_SERVICE" \
    "${route_args[@]}" \
    "$BIN" link run >>"$link_log" 2>&1 &
  CURRENT_LINK_PID=$!

  local initial_ok=no
  for n in {1..12}; do
    sleep 5
    if ! kill -0 "$CURRENT_LINK_PID" 2>/dev/null; then
      echo "LINK_TEST_${label}=EARLY_EXIT"
      tail -n 80 "$link_log" || true
      wait "$CURRENT_LINK_PID" 2>/dev/null || true
      CURRENT_LINK_PID=""
      return 1
    fi
    local conns="$(established_443 "$CURRENT_LINK_PID")"
    if [[ -n "$conns" ]]; then
      echo "LINK_TEST_${label}=CONNECTED initial_sample=$n"
      echo "$conns" | sed 's/^/  /'
      initial_ok=yes
      break
    fi
  done
  if [[ "$initial_ok" != yes ]]; then
    echo "LINK_TEST_${label}=NO_ESTABLISHED_443"
    tail -n 80 "$link_log" || true
    kill -TERM "$CURRENT_LINK_PID" 2>/dev/null || true
    wait "$CURRENT_LINK_PID" 2>/dev/null || true
    CURRENT_LINK_PID=""
    return 1
  fi

  local elapsed=0 good=0 total=0
  while (( elapsed < duration )); do
    sleep 5
    elapsed=$((elapsed + 5))
    total=$((total + 1))
    if ! kill -0 "$CURRENT_LINK_PID" 2>/dev/null; then
      echo "LINK_TEST_${label}=DIED elapsed=$elapsed"
      tail -n 100 "$link_log" || true
      wait "$CURRENT_LINK_PID" 2>/dev/null || true
      CURRENT_LINK_PID=""
      return 1
    fi
    if [[ -n "$(established_443 "$CURRENT_LINK_PID")" ]]; then
      good=$((good + 1))
    fi
    if (( elapsed % 30 == 0 )); then
      echo "LINK_TEST_${label}_CHECKPOINT elapsed=$elapsed established_samples=$good/$total"
    fi
  done

  local required=$(( (total * 2 + 2) / 3 ))
  if (( good < required )); then
    echo "LINK_TEST_${label}=INSUFFICIENT_CONNECTIVITY samples=$good/$total required=$required"
    tail -n 100 "$link_log" || true
    kill -TERM "$CURRENT_LINK_PID" 2>/dev/null || true
    wait "$CURRENT_LINK_PID" 2>/dev/null || true
    CURRENT_LINK_PID=""
    return 1
  fi
  if grep -Eqi 'hello_ack refused|auth rejected|runtime contract .*incompatible|runner error|io error' "$link_log"; then
    echo "LINK_TEST_${label}=PROTOCOL_ERROR"
    tail -n 100 "$link_log" || true
    kill -TERM "$CURRENT_LINK_PID" 2>/dev/null || true
    wait "$CURRENT_LINK_PID" 2>/dev/null || true
    CURRENT_LINK_PID=""
    return 1
  fi

  echo "LINK_TEST_${label}=PASS samples=$good/$total"
  kill -TERM "$CURRENT_LINK_PID" 2>/dev/null || true
  wait "$CURRENT_LINK_PID" 2>/dev/null || true
  CURRENT_LINK_PID=""
  return 0
}

echo "=== HERDR MAINLAND RELAY UAT ==="
echo "HEAD=$HEAD"
echo "GENERATION=$GEN"
echo "DEVICE=$WSID"
echo "NETWORK_SERVICE=$SERVICE"
echo "DEFAULT_INTERFACE=$IFACE"
echo "PROXY_ORIGINAL web=$WEB_ORIG secure=$SECURE_ORIG socks=$SOCKS_ORIG auto=$AUTO_ORIG discovery=$DISC_ORIG"
echo "NOTE=ChatGPT/browser connectivity may disappear while proxies are disabled; this script restores network and Direct Link automatically."

# Stop the formal Link before network manipulation so all test sockets are owned
# by the foreground UAT Link processes below.
if [[ "$LINK_WAS_LOADED" == yes ]]; then
  launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
fi

sudo networksetup -setwebproxystate "$SERVICE" off
sudo networksetup -setsecurewebproxystate "$SERVICE" off
sudo networksetup -setsocksfirewallproxystate "$SERVICE" off
sudo networksetup -setautoproxystate "$SERVICE" off
sudo networksetup -setproxyautodiscovery "$SERVICE" off
unset HTTP_PROXY HTTPS_PROXY ALL_PROXY NO_PROXY http_proxy https_proxy all_proxy no_proxy HERDR_LINK_PROXY
sleep 2
check_system_proxy_off
check_vpn_and_route

echo "=== NAKED NETWORK PROBES ==="
probe_url CUSTOM_DOMAIN "$CUSTOM_HEALTH" || true
if probe_url WORKERS_DEV "$WORKERS_HEALTH"; then WORKERS_DIRECT=PASS; else WORKERS_DIRECT=FAIL; fi
probe_url DENO "$DENO_HEALTH" || true
probe_url SUPABASE "$SUPABASE_HEALTH" || true

echo "=== DENO RELAY ==="
if run_link_test deno 3 "$R3" 60 yes; then DENO_RESULT=PASS; fi

echo "=== SUPABASE RELAY ==="
if run_link_test supabase 4 "$R4" 180 yes; then SUPABASE_RESULT=PASS; fi

echo "=== COMBINED AUTO LADDER ==="
if run_link_test combined 5 "$R5" 60 no; then COMBINED_RESULT=PASS; fi

if [[ "$DENO_RESULT" == PASS && "$SUPABASE_RESULT" == PASS && "$COMBINED_RESULT" == PASS ]]; then
  RESULT=PASS
  echo "HERDR_MAINLAND_RELAY_UAT_CORE=PASS"
else
  echo "HERDR_MAINLAND_RELAY_UAT_CORE=FAIL"
  exit 40
fi
