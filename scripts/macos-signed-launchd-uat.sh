#!/usr/bin/env bash
set -euo pipefail

fail() { printf '[macos-signed-launchd-uat] FAIL %s\n' "$*" >&2; exit 1; }
pass() { printf '[macos-signed-launchd-uat] PASS %s\n' "$*"; }

[[ "$(uname -s)" == Darwin ]] || fail 'macOS is required'
BINARY="${HERDR_MCP_UAT_BINARY:-$HOME/.config/herdr-mcp/runtime/current/herdr-mcp}"
ROOT="${HERDR_MCP_UAT_ROOT:-}"
EXPECTED_TEAM="${HERDR_MACOS_TEAM_ID:-}"
[[ -x "$BINARY" ]] || fail "runtime binary is not executable: $BINARY"
[[ -n "$ROOT" && -d "$ROOT" ]] || fail 'HERDR_MCP_UAT_ROOT must name the managed linked worktree used for the protected-Documents probe'
[[ -n "$EXPECTED_TEAM" ]] || fail 'HERDR_MACOS_TEAM_ID is required'

DETAILS="$(/usr/bin/codesign -dvvv --requirements - "$BINARY" 2>&1)" || fail 'codesign inspection failed'
IDENTIFIER="$(printf '%s\n' "$DETAILS" | sed -n 's/^Identifier=//p' | head -n 1)"
TEAM="$(printf '%s\n' "$DETAILS" | sed -n 's/^TeamIdentifier=//p' | head -n 1)"
REQ="$(printf '%s\n' "$DETAILS" | sed -n 's/^# designated => //p' | head -n 1)"
[[ "$IDENTIFIER" == dev.herdr.mcp.runtime ]] || fail "unexpected identifier: $IDENTIFIER"
[[ "$TEAM" == "$EXPECTED_TEAM" ]] || fail "unexpected TeamIdentifier: $TEAM"
[[ "$REQ" == *'identifier "dev.herdr.mcp.runtime"'* ]] || fail 'designated requirement does not bind stable identifier'
[[ "$REQ" != cdhash* ]] || fail 'designated requirement is cdhash-bound'
pass "stable identity identifier=$IDENTIFIER team=$TEAM"

STATUS="$($BINARY service status)" || fail 'service status failed'
printf '%s\n' "$STATUS" | grep -q '"healthy": true' || fail 'launchd runtime is not healthy'
pass 'launchd runtime healthy'

GITFILE="$ROOT/.git"
[[ -f "$GITFILE" ]] || fail 'UAT root must be a linked Git worktree with a .git file'
COMMON_HEAD="$(sed -n 's/^gitdir: //p' "$GITFILE")/HEAD"
[[ "$COMMON_HEAD" == "$HOME/Documents/"* ]] || fail "common gitdir is not under protected Documents: $COMMON_HEAD"

python3 - "$COMMON_HEAD" <<'PY2'
import os, sys, time
path=sys.argv[1]
start=time.monotonic()
with open(path, 'rb') as f:
    data=f.read(256)
elapsed=(time.monotonic()-start)*1000
if not data:
    raise SystemExit('empty common gitdir HEAD')
if elapsed > 2000:
    raise SystemExit(f'protected Documents read exceeded 2000ms: {elapsed:.2f}ms')
print(f'[macos-signed-launchd-uat] PASS protected Documents read elapsed_ms={elapsed:.2f}')
PY2

start_ms=$(python3 -c 'import time; print(int(time.time()*1000))')
git -C "$ROOT" status --porcelain -b >/dev/null
end_ms=$(python3 -c 'import time; print(int(time.time()*1000))')
elapsed=$((end_ms-start_ms))
(( elapsed <= 2000 )) || fail "git status exceeded 2000ms: ${elapsed}ms"
pass "git status elapsed_ms=$elapsed"

$BINARY doctor >/dev/null || fail 'doctor failed after protected-path probes'
pass 'doctor completed after protected-path probes'
pass 'signed launchd protected-Documents qualification complete'
