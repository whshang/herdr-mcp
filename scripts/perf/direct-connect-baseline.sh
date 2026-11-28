#!/usr/bin/env bash
# direct-connect-baseline.sh — reproducible p50/p95 latency comparison for
# the #1094 direct-connect baseline: same machine, same window, Edge vs
# loopback (and later tailnet) MCP paths.
#
# SECURITY: credentials arrive ONLY via environment variables and are passed
# to curl through a stdin config (-K -) — never in argv (argv is visible to
# any same-user process via /proc/*/cmdline), never in stdout, stderr, or
# output files. `set +x` is forced and the credential variables are unset
# before any output is produced.
#
# Usage:
#   HERDR_LOOPBACK_URL="http://127.0.0.1:<port>/mcp" \
#   HERDR_MCP_TOKEN="<runtime bearer>" \
#   HERDR_EDGE_URL="https://<edge-origin>/mcp" \
#   HERDR_EDGE_TOKEN="<edge bearer>" \
#   scripts/perf/direct-connect-baseline.sh [-n SAMPLES] [-t TOOL] [-a ARGS_JSON] [-o OUT_CSV]
#
#   -n SAMPLES    samples per path (default 100, minimum 10)
#   -t TOOL       tool name (default herdr_inspect); restricted to the
#                 read-only allowlist below — this script replays the tool
#                 SAMPLES times with NO idempotency key, so any tool with a
#                 mutating surface would repeat side effects
#   -a ARGS_JSON  tool arguments JSON (default {}); must parse as JSON
#   -o OUT_CSV    also write raw samples to OUT_CSV (columns: path,seq,ms)
#
# Env knobs:
#   HERDR_SAMPLE_MAX_TIME   per-sample curl --max-time seconds (default 30)
#   HERDR_CONNECT_TIMEOUT   per-sample curl --connect-timeout (default 10)
#
# A sample counts toward percentiles only when the HTTP status is 2xx AND
# the body is a complete JSON-RPC response: jsonrpc == "2.0", the response
# id echoes the request id, "result" present and no "error". Everything
# else is recorded as FAIL (kept in CSV) and never folded into percentiles.
# The script exits non-zero when a configured path produced zero valid
# samples.
#
# Every sample sends a unique `sample_nonce` argument plus a unique
# JSON-RPC id. Edge's short-window read dedupe (readDedupeKey, 3s replay)
# keys on the tool arguments, so without the nonce identical requests
# would measure an in-process cached replay instead of real latency. The
# nonce is ignored by every allowlisted tool (they read named fields only).
#
# At least one of HERDR_LOOPBACK_URL or HERDR_EDGE_URL is required; a missing
# URL skips that path. P1 note: run a third pass with HERDR_LOOPBACK_URL set
# to "http://<tailscale-ip>:<direct-port>/mcp" after `tailscale ping`
# establishes whether the link is direct or via DERP.
set -u
set +x  # hard guarantee: never echo commands (tokens would leak)

SAMPLES=100
TOOL="herdr_inspect"
ARGS_JSON='{}'
OUT_CSV=""
while getopts ":n:t:a:o:" opt; do
  case "$opt" in
    n) SAMPLES="$OPTARG" ;;
    t) TOOL="$OPTARG" ;;
    a) ARGS_JSON="$OPTARG" ;;
    o) OUT_CSV="$OPTARG" ;;
    *) echo "usage: $0 [-n samples] [-t tool] [-a args_json] [-o out.csv]" >&2; exit 2 ;;
  esac
done

command -v curl >/dev/null || { echo "curl is required" >&2; exit 2; }
command -v jq   >/dev/null || { echo "jq is required"   >&2; exit 2; }
command -v awk  >/dev/null || { echo "awk is required"  >&2; exit 2; }
[ "$SAMPLES" -ge 10 ] || { echo "-n must be >= 10" >&2; exit 2; }

# -t is restricted to read-only tools: each is dispatched SAMPLES times
# with no idempotency key, so anything with a mutating surface (herdr_git
# has write ops, herdr_exec*/herdr_prompt/herdr_fs_write|edit|patch,
# herdr_call) would repeat side effects. All listed tools read named
# argument fields and ignore extras, which the per-sample nonce relies on.
case "$TOOL" in
  herdr_inspect|herdr_methods|herdr_since|herdr_fs_read|herdr_fs_list|herdr_fs_grep|herdr_tasks|herdr_exec_read) ;;
  *) echo "-t must be one of: herdr_inspect herdr_methods herdr_since herdr_fs_read herdr_fs_list herdr_fs_grep herdr_tasks herdr_exec_read" >&2; exit 2 ;;
esac

# Build the request body with jq so tool names/args are escaped per JSON
# rules; reject an ARGS_JSON that does not parse. Built per sample so the
# nonce/id vary — see the dedupe note in the header. RUN_ID is unique per
# invocation: back-to-back runs inside Edge's 3s read-dedupe window must
# not replay the previous run's samples (the JSON-RPC id is not part of
# the dedupe key).
RUN_ID="run-$(date +%s)-$$"
build_body() { # $1=seq
  jq -cn --arg name "$TOOL" --argjson args "$ARGS_JSON" \
    --argjson id "$1" --arg nonce "sample-$RUN_ID-$1" \
    '{jsonrpc:"2.0",id:$id,method:"tools/call",params:{name:$name,arguments:($args + {sample_nonce:$nonce})}}'
}
jq -cn --argjson args "$ARGS_JSON" 'true' 2>/dev/null \
  || { echo "-a is not valid JSON" >&2; exit 2; }

MAX_TIME="${HERDR_SAMPLE_MAX_TIME:-30}"
CONNECT_TIMEOUT="${HERDR_CONNECT_TIMEOUT:-10}"
# curl --max-time 0 means "no total limit" — reject non-positive and
# unbounded values so a wedged connection cannot stall the whole run.
validate_seconds() { # $1=value $2=name
  awk -v v="$1" 'BEGIN{ exit !(v+0 > 0 && v+0 <= 600) }' \
    || { echo "$2 must be a positive number of seconds, <= 600" >&2; exit 2; }
}
validate_seconds "$MAX_TIME" HERDR_SAMPLE_MAX_TIME
validate_seconds "$CONNECT_TIMEOUT" HERDR_CONNECT_TIMEOUT

LOOPBACK_URL="${HERDR_LOOPBACK_URL:-}"
EDGE_URL="${HERDR_EDGE_URL:-}"
[ -n "$LOOPBACK_URL" ] || [ -n "$EDGE_URL" ] || {
  echo "set at least one of HERDR_LOOPBACK_URL / HERDR_EDGE_URL" >&2; exit 2;
}
[ -z "$LOOPBACK_URL" ] || [ -n "${HERDR_MCP_TOKEN:-}" ] || {
  echo "HERDR_MCP_TOKEN is required for the loopback path" >&2; exit 2;
}
[ -z "$EDGE_URL" ] || [ -n "${HERDR_EDGE_TOKEN:-}" ] || {
  echo "HERDR_EDGE_TOKEN is required for the edge path" >&2; exit 2;
}

RESULTS_DIR="$(mktemp -d)"
trap 'rm -rf "$RESULTS_DIR"' EXIT

# Run one sample; prints "<milliseconds>" or "FAIL". The bearer reaches curl
# through its stdin config (-K -) — argv stays free of the credential. The
# response body must be a valid JSON-RPC success to count.
sample() { # $1=url $2=bearer $3=bodyfile $4=seq
  local url="$1" bearer="$2" bodyfile="$3" seq="$4" http_code time_total
  local meta body
  body="$(build_body "$seq")" || { echo "FAIL"; return; }
  meta="$(
    printf 'header = "Authorization: Bearer %s"\n' "$bearer" | curl -sS \
      --config - \
      --output "$bodyfile" \
      --write-out '%{http_code} %{time_total}' \
      --connect-timeout "$CONNECT_TIMEOUT" \
      --max-time "$MAX_TIME" \
      -X POST "$url" \
      -H 'content-type: application/json' \
      -H 'accept: application/json, text/event-stream' \
      -H 'user-agent: herdr-mcp/direct-connect-baseline' \
      -d "$body" 2>/dev/null
  )" || { echo "FAIL"; return; }
  http_code="${meta%% *}"
  time_total="${meta##* }"
  case "$http_code" in
    2*) ;;
    *) echo "FAIL"; return ;;
  esac
  # Full envelope check: JSON-RPC 2.0, echoed request id, result present,
  # no transport-level error AND no tool-level failure. Failures arrive
  # two ways under the real contract: `result.isError: true`, AND the
  # `is_error=false` wrapper that serializes `{"ok":false,...}` into
  # `result.content[].text` — an arg/permission refusal measured as
  # latency would be bogus, so both count as FAIL. Legitimate results
  # without an `ok` field pass.
  jq -e --argjson id "$seq" \
    'type == "object" and .jsonrpc == "2.0" and .id == $id
     and .error == null and .result != null
     and (if (.result|type) == "object" then (.result.isError // false) else false end) == false
     and ([.result.content[]? | .text? | select(type == "string")
           | (fromjson? | select(type == "object" and .ok == false))] | length == 0)' \
    "$bodyfile" >/dev/null 2>&1 || { echo "FAIL"; return; }
  printf '%s\n' "$(awk -v t="$time_total" 'BEGIN{printf "%d", t*1000}')"
}

# Percentiles use the nearest-rank convention: rank = ceil(p * n). The
# insertion sort keeps this portable — asort() is a gawk extension and
# macOS ships BSD awk.
percentiles() { # stdin: one ms (or FAIL) per line
  awk '
    $1 == "FAIL" { fails++; next }
    { a[++n]=$1 }
    END {
      if (n == 0) { printf "n=0 fails=%d\n", fails+0; exit }
      for (i = 1; i <= n; i++)
        for (j = i + 1; j <= n; j++)
          if (a[j] < a[i]) { t = a[i]; a[i] = a[j]; a[j] = t }
      p50 = a[int((50 * n + 99) / 100)]
      p95 = a[int((95 * n + 99) / 100)]
      printf "n=%d fails=%d p50_ms=%d p95_ms=%d\n", n, fails+0, p50, p95
    }'
}

# Interleave paths per-iteration to cancel drift when both are measured.
run_samples() { # $1=label $2=url $3=bearer $4=outfile
  local label="$1" url="$2" bearer="$3" outfile="$4" i ms
  for i in $(seq 1 "$SAMPLES"); do
    ms="$(sample "$url" "$bearer" "$RESULTS_DIR/body-$label-$i.json" "$i")"
    printf '%s\n' "$ms" >>"$outfile"
  done
}

if [ -n "$LOOPBACK_URL" ] && [ -n "$EDGE_URL" ]; then
  : >"$RESULTS_DIR/loopback.txt"; : >"$RESULTS_DIR/edge.txt"
  for i in $(seq 1 "$SAMPLES"); do
    sample "$LOOPBACK_URL" "$HERDR_MCP_TOKEN" "$RESULTS_DIR/body-loopback-$i.json" "$i" \
      >>"$RESULTS_DIR/loopback.txt"
    sample "$EDGE_URL" "$HERDR_EDGE_TOKEN" "$RESULTS_DIR/body-edge-$i.json" "$i" \
      >>"$RESULTS_DIR/edge.txt"
  done
else
  if [ -n "$LOOPBACK_URL" ]; then
    run_samples loopback "$LOOPBACK_URL" "$HERDR_MCP_TOKEN" "$RESULTS_DIR/loopback.txt"
  else
    run_samples edge "$EDGE_URL" "$HERDR_EDGE_TOKEN" "$RESULTS_DIR/edge.txt"
  fi
fi

# ---- credential variables are done being used; drop before any output ----
unset HERDR_MCP_TOKEN HERDR_EDGE_TOKEN

echo "# direct-connect baseline @ $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "# tool=$TOOL samples_per_path=$SAMPLES"
[ -n "$OUT_CSV" ] && printf 'path,seq,ms\n' >"$OUT_CSV"

exit_code=0
for path in loopback edge; do
  f="$RESULTS_DIR/$path.txt"
  [ -f "$f" ] || continue
  summary="$(percentiles <"$f")"
  printf '%s  %s\n' "$path" "$summary"
  case "$summary" in n=0\ *) exit_code=1 ;; esac
  if [ -n "$OUT_CSV" ]; then
    seq_no=0
    while IFS= read -r line; do
      seq_no=$((seq_no + 1))
      printf '%s,%d,%s\n' "$path" "$seq_no" "$line" >>"$OUT_CSV" || exit_code=1
    done <"$f"
  fi
done
[ -z "$OUT_CSV" ] || echo "# raw samples written to $OUT_CSV"
exit "$exit_code"
