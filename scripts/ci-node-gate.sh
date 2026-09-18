#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-full}"

case "${MODE}" in
  full)
    exec scripts/release-gate.sh node
    ;;
  docs)
    npm ci
    npm run build:site
    node --test       tests/agent-install-doc.test.mjs       tests/docs-command-contract.test.mjs       tests/platform-support-matrix.test.mjs       tests/release-notes.test.mjs       tests/site-build.test.mjs
    ;;
  extension)
    npm ci
    npm run build
    node --test \
      tests/browser-actuation-recovery.test.mjs \
      tests/browser-control-plane.test.mjs \
      tests/browser-extension-store-contract.test.mjs \
      tests/browser-extension-store-listing.test.mjs \
      tests/chatgpt-artifact-capture.test.mjs \
      tests/continuity-journal.test.mjs \
      tests/extension-auth.test.mjs \
      tests/extension-i18n.test.mjs \
      tests/extension-local-auth.test.mjs \
      tests/extension-native-host.test.mjs \
      tests/extension-recovery.test.mjs \
      tests/options-i18n.test.mjs \
      tests/pack-extension.test.mjs \
      tests/page-assist-content.test.mjs \
      tests/page-assist-core.test.mjs \
      tests/queued-insert.test.mjs
    node tests/manual/extension_smoke.mjs
    node tests/manual/background_bind_test.mjs
    ;;
  edge)
    npm ci
    npm run build
    npm run test:edge:built
    ;;
  *)
    printf 'unknown CI test mode: %s\n' "${MODE}" >&2
    exit 2
    ;;
esac
