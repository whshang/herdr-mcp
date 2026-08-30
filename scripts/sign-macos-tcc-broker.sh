#!/usr/bin/env bash
# Sign a TCC broker candidate with the long-lived broker signing identifier.
#
# Release/CI use delegates to sign-macos-release.sh when a P12 is supplied.
# Local development may instead reference an already-installed persistent code
# signing identity in the user's Keychain. This script never creates or rotates
# an identity automatically because doing so would defeat TCC continuity.
set -euo pipefail

BINARY="${1:-}"
STABLE_IDENTIFIER="cc.agentforme.herdr.tcc-broker"
SIGNING_IDENTITY="${HERDR_MACOS_SIGNING_IDENTITY:-}"

fail() { printf '[tcc-broker-sign] ERROR: %s\n' "$*" >&2; exit 2; }

[[ "$(uname -s)" == "Darwin" ]] || fail "macOS signing must run on Darwin"
[[ -n "$BINARY" && -f "$BINARY" ]] || fail "usage: scripts/sign-macos-tcc-broker.sh <broker-candidate>"

if [[ -n "${HERDR_MACOS_CERT_P12_BASE64:-}" ]]; then
  HERDR_MACOS_SIGNING_IDENTIFIER="$STABLE_IDENTIFIER" \
    "$(dirname "$0")/sign-macos-release.sh" "$BINARY"
  exit $?
fi

[[ -n "$SIGNING_IDENTITY" ]] || fail "set HERDR_MACOS_SIGNING_IDENTITY to a persistent local code-signing identity, or provide the release P12 variables"

/usr/bin/codesign \
  --force \
  --identifier "$STABLE_IDENTIFIER" \
  --sign "$SIGNING_IDENTITY" \
  "$BINARY"

/usr/bin/codesign --verify --strict --verbose=4 "$BINARY"
DETAILS="$(/usr/bin/codesign -dvvv --requirements - "$BINARY" 2>&1)"
IDENTIFIER="$(printf '%s\n' "$DETAILS" | sed -n 's/^Identifier=//p' | head -n 1)"
REQUIREMENT="$(printf '%s\n' "$DETAILS" | sed -n 's/^# designated => //p' | head -n 1)"

[[ "$IDENTIFIER" == "$STABLE_IDENTIFIER" ]] || fail "signed identifier is '$IDENTIFIER', expected '$STABLE_IDENTIFIER'"
[[ -n "$REQUIREMENT" ]] || fail "designated requirement is missing"
[[ "$REQUIREMENT" != cdhash* ]] || fail "designated requirement is cdhash-bound; use a persistent certificate-based signing identity"

printf '[tcc-broker-sign] PASS identifier=%s\n' "$IDENTIFIER"
