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
#   -t TOOL       tool name (default herdr_inspect); must be a valid JSON
#                 string payload — the request body is built with jq
#   -a ARGS_JSON  tool arguments JSON (default {}); must parse as JSON
#   -o OUT_CSV    also write raw samples to OUT_CSV (columns: path,seq,ms)
#
# Env knobs:
#   HERDR_SAMPLE_MAX_TIME   per-sample curl --max-time seconds (default 30)
#   HERDR_CONNECT_TIMEOUT   per-sample curl --connect-timeout (default 10)
#
# A sample counts toward percentiles only when the HTTP status is 2xx AND
# the body is a valid JSON-RPC response containing "result" and no "error".
# Everything else is recorded as FAIL (kept in CSV) and never folded into
# percentiles. The script exits non-zero when a configured path produced
# zero valid samples.
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

# Build the request body with jq so tool names/args are escaped per JSON
# rules; reject an ARGS_JSON that does not parse.
BODY="$(jq -cn --arg name "$TOOL" --argjson args "$ARGS_JSON" \
  '{jsonrpc:"2.0",id:1,method:"tools/call",params:{name:$name,arguments:$args}}' 2>/dev/null)" \
  || { echo "-t/-a does not form a valid tools/call body" >&2; exit 2; }

MAX_TIME="${HERDR_SAMPLE_MAX_TIME:-30}"
CONNECT_TIMEOUT="${HERDR_CONNECT_TIMEOUT:-10}"

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
sample() { # $1=url $2=bearer $3=bodyfile
  local url="$1" bearer="$2" bodyfile="$3" http_code time_total
  local meta
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
      -d "$BODY" 2>/dev/null
  )" || { echo "FAIL"; return; }
  http_code="${meta%% *}"
  time_total="${meta##* }"
  case "$http_code" in
    2*) ;;
    *) echo "FAIL"; return ;;
  esac
  jq -e 'type == "object" and .error == null and .result != null' \
    "$bodyfile" >/dev/null 2>&1 || { echo "FAIL"; return; }
  printf '%s\n' "$(awk -v t="$time_total" 'BEGIN{printf "%d", t*1000}')"
}

percentiles() { # stdin: one ms (or FAIL) per line
  awk '
    $1 == "FAIL" { fails++; next }
    { a[++n]=$1 }
    END {
      if (n == 0) { printf "n=0 fails=%d\n", fails+0; exit }
      n2 = asort(a)
      p50 = a[int(n2*0.50)+1]; p95 = a[int(n2*0.95)+1]
      printf "n=%d fails=%d p50_ms=%d p95_ms=%d\n", n2, fails+0, p50, p95
    }'
}

# Interleave paths per-iteration to cancel drift when both are measured.
run_samples() { # $1=label $2=url $3=bearer $4=outfile
  local label="$1" url="$2" bearer="$3" outfile="$4" i ms
  for i in $(seq 1 "$SAMPLES"); do
    ms="$(sample "$url" "$bearer" "$RESULTS_DIR/body-$label-$i.json")"
    printf '%s\n' "$ms" >>"$outfile"
  done
}

if [ -n "$LOOPBACK_URL" ] && [ -n "$EDGE_URL" ]; then
  : >"$RESULTS_DIR/loopback.txt"; : >"$RESULTS_DIR/edge.txt"
  for i in $(seq 1 "$SAMPLES"); do
    sample "$LOOPBACK_URL" "$HERDR_MCP_TOKEN" "$RESULTS_DIR/body-loopback-$i.json" \
      >>"$RESULTS_DIR/loopback.txt"
    sample "$EDGE_URL" "$HERDR_EDGE_TOKEN" "$RESULTS_DIR/body-edge-$i.json" \
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
unset HERDR_MCP_TOKEN HERDR_EDGE_TOKEN BODY

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
