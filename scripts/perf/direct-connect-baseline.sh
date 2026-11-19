#!/usr/bin/env bash
# direct-connect-baseline.sh — reproducible p50/p95 latency comparison for
# the #1094 direct-connect baseline: same machine, same window, Edge vs
# loopback (and later tailnet) MCP paths.
#
# SECURITY: credentials arrive ONLY via environment variables and are never
# printed, logged, or written to any output file. `set +x` is forced and the
# credential variables are unset before any output is produced.
#
# Usage:
#   HERDR_LOOPBACK_URL="http://127.0.0.1:<port>/mcp" \
#   HERDR_MCP_TOKEN="<runtime bearer>" \
#   HERDR_EDGE_URL="https://<edge-origin>/mcp" \
#   HERDR_EDGE_TOKEN="<edge bearer>" \
#   scripts/perf/direct-connect-baseline.sh [-n SAMPLES] [-t TOOL] [-a ARGS_JSON] [-o OUT_CSV]
#
#   -n SAMPLES    samples per path (default 100, minimum 10)
#   -t TOOL       tool name (default herdr_inspect)
#   -a ARGS_JSON  tool arguments JSON (default {})
#   -o OUT_CSV    also write raw samples to OUT_CSV (columns: path,seq,ms)
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
command -v awk  >/dev/null || { echo "awk is required"  >&2; exit 2; }
[ "$SAMPLES" -ge 10 ] || { echo "-n must be >= 10" >&2; exit 2; }

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

BODY="$(printf '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":%s,"arguments":%s}}' \
  "$(printf '%s' "$TOOL" | awk 'BEGIN{printf "\""} {printf "%s", $0} END{printf "\""}')" "$ARGS_JSON")"

# Run one sample; prints "<milliseconds>" or "FAIL". Token stays inside the
# function via positional header, never on stderr/logs.
sample() { # $1=url $2=bearer
  local url="$1" bearer="$2" out
  out="$(curl -sS -o /dev/null -w '%{http_code} %{time_total}' \
    -X POST "$url" \
    -H "Authorization: Bearer $bearer" \
    -H 'content-type: application/json' \
    -H 'accept: application/json, text/event-stream' \
    -H 'user-agent: herdr-mcp/direct-connect-baseline' \
    -d "$BODY" 2>/dev/null)" || { echo "FAIL"; return; }
  case "$out" in
    2*|'3'*) printf '%s\n' "$(awk '{printf "%d", $2*1000}' <<<"$out")" ;;
    *) echo "FAIL" ;;
  esac
}

run_path() { # $1=label $2=url $3=bearer -> prints raw ms lines to stdout
  local label="$1" url="$2" bearer="$3" i ms
  for i in $(seq 1 "$SAMPLES"); do
    ms="$(sample "$url" "$bearer")"
    printf '%s\n' "$ms"
  done
}

percentiles() { # stdin: one ms (or FAIL) per line
  sort -n | awk '
    $1 != "FAIL" { a[NR]=$1; n++ }
    $1 == "FAIL" { fails++ }
    END {
      if (n == 0) { print "n=0 fails=" fails+0; exit }
      p50 = a[int(n*0.50)+1]; p95 = a[int(n*0.95)+1]
      printf "n=%d fails=%d p50_ms=%d p95_ms=%d\n", n, fails+0, p50, p95
    }'
}

RESULTS_DIR="$(mktemp -d)"
trap 'rm -rf "$RESULTS_DIR"' EXIT

# Interleave paths per-iteration to cancel drift when both are measured.
if [ -n "$LOOPBACK_URL" ] && [ -n "$EDGE_URL" ]; then
  : >"$RESULTS_DIR/loopback.txt"; : >"$RESULTS_DIR/edge.txt"
  for i in $(seq 1 "$SAMPLES"); do
    sample "$LOOPBACK_URL" "$HERDR_MCP_TOKEN" >>"$RESULTS_DIR/loopback.txt"
    sample "$EDGE_URL" "$HERDR_EDGE_TOKEN" >>"$RESULTS_DIR/edge.txt"
  done
else
  if [ -n "$LOOPBACK_URL" ]; then
    run_path loopback "$LOOPBACK_URL" "$HERDR_MCP_TOKEN" >"$RESULTS_DIR/loopback.txt"
  else
    run_path edge "$EDGE_URL" "$HERDR_EDGE_TOKEN" >"$RESULTS_DIR/edge.txt"
  fi
fi

# ---- credential variables are done being used; drop before any output ----
unset HERDR_MCP_TOKEN HERDR_EDGE_TOKEN BODY

echo "# direct-connect baseline @ $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "# tool=$TOOL samples_per_path=$SAMPLES"
[ -n "$OUT_CSV" ] && printf 'path,seq,ms\n' >"$OUT_CSV"

for path in loopback edge; do
  f="$RESULTS_DIR/$path.txt"
  [ -f "$f" ] || continue
  printf '%s  ' "$path"
  percentiles <"$f"
  if [ -n "$OUT_CSV" ]; then
    seq_no=0
    while IFS= read -r line; do
      seq_no=$((seq_no + 1))
      printf '%s,%d,%s\n' "$path" "$seq_no" "$line" >>"$OUT_CSV"
    done <"$f"
  fi
done
[ -z "$OUT_CSV" ] || echo "# raw samples written to $OUT_CSV"
