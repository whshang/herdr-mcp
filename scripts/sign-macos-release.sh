#!/usr/bin/env bash
# Sign a tagged macOS herdr-mcp release asset with a stable Developer ID identity.
set -euo pipefail

BINARY="${1:-}"
STABLE_IDENTIFIER="${HERDR_MACOS_SIGNING_IDENTIFIER:-dev.herdr.mcp}"
CERT_BASE64="${HERDR_MACOS_CERT_P12_BASE64:-}"
CERT_PASSWORD="${HERDR_MACOS_CERT_PASSWORD:-}"
SIGNING_IDENTITY="${HERDR_MACOS_SIGNING_IDENTITY:-}"
EXPECTED_TEAM_ID="${HERDR_MACOS_TEAM_ID:-}"

fail() { printf '[macos-sign] ERROR: %s\n' "$*" >&2; exit 2; }

[[ "$(uname -s)" == "Darwin" ]] || fail "macOS signing must run on Darwin"
[[ -n "$BINARY" && -f "$BINARY" ]] || fail "usage: scripts/sign-macos-release.sh <binary>"
[[ -n "$CERT_BASE64" ]] || fail "HERDR_MACOS_CERT_P12_BASE64 is required for tagged macOS releases"
[[ -n "$CERT_PASSWORD" ]] || fail "HERDR_MACOS_CERT_PASSWORD is required for tagged macOS releases"
[[ -n "$SIGNING_IDENTITY" ]] || fail "HERDR_MACOS_SIGNING_IDENTITY is required for tagged macOS releases"
[[ -n "$EXPECTED_TEAM_ID" ]] || fail "HERDR_MACOS_TEAM_ID is required for tagged macOS releases"

WORK_DIR="$(mktemp -d "${RUNNER_TEMP:-/tmp}/herdr-macos-sign.XXXXXX")"
KEYCHAIN="$WORK_DIR/herdr-mcp-signing.keychain-db"
CERT_P12="$WORK_DIR/developer-id.p12"
KEYCHAIN_PASSWORD="$(uuidgen | tr -d '-')"

cleanup() {
  local status=$?
  set +e
  /usr/bin/security delete-keychain "$KEYCHAIN" >/dev/null 2>&1 || true
  rm -rf "$WORK_DIR"
  return "$status"
}
trap cleanup EXIT INT TERM

printf '%s' "$CERT_BASE64" | /usr/bin/base64 -D >"$CERT_P12"
/usr/bin/security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
/usr/bin/security set-keychain-settings -lut 21600 "$KEYCHAIN"
/usr/bin/security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
/usr/bin/security import "$CERT_P12" \
  -k "$KEYCHAIN" \
  -P "$CERT_PASSWORD" \
  -T /usr/bin/codesign >/dev/null
/usr/bin/security set-key-partition-list \
  -S apple-tool:,apple:,codesign: \
  -s \
  -k "$KEYCHAIN_PASSWORD" \
  "$KEYCHAIN" >/dev/null

/usr/bin/codesign \
  --force \
  --options runtime \
  --timestamp \
  --identifier "$STABLE_IDENTIFIER" \
  --keychain "$KEYCHAIN" \
  --sign "$SIGNING_IDENTITY" \
  "$BINARY"

/usr/bin/codesign --verify --strict --verbose=4 "$BINARY"
DETAILS="$(/usr/bin/codesign -dvvv --requirements - "$BINARY" 2>&1)"
IDENTIFIER="$(printf '%s\n' "$DETAILS" | sed -n 's/^Identifier=//p' | head -n 1)"
TEAM="$(printf '%s\n' "$DETAILS" | sed -n 's/^TeamIdentifier=//p' | head -n 1)"
REQUIREMENT="$(printf '%s\n' "$DETAILS" | sed -n 's/^# designated => //p' | head -n 1)"

[[ "$IDENTIFIER" == "$STABLE_IDENTIFIER" ]] || fail "signed identifier is '$IDENTIFIER', expected '$STABLE_IDENTIFIER'"
[[ -n "$TEAM" && "$TEAM" != "not set" ]] || fail "Developer ID TeamIdentifier is missing"
[[ "$TEAM" == "$EXPECTED_TEAM_ID" ]] || fail "signed TeamIdentifier '$TEAM' does not match expected '$EXPECTED_TEAM_ID'"
[[ "$REQUIREMENT" == *"identifier \"$STABLE_IDENTIFIER\""* ]] || fail "designated requirement does not bind the stable identifier"
[[ "$REQUIREMENT" != cdhash* ]] || fail "designated requirement is still cdhash-bound"

printf '[macos-sign] PASS identifier=%s team=%s\n' "$IDENTIFIER" "$TEAM"
