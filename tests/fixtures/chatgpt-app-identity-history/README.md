# ChatGPT App identity history fixture

`history.json.gz` contains the exact historical extension sources consumed by
`tests/chatgpt-app-identity-history.test.mjs` for versions `0.1.121` through
`0.1.129`. The test decompresses the archive in memory and runs the same
broken/fixed probes against those sources, so CI does not require Git history.

`provenance.json` records the full Git SHA and original source paths for each
version. The compressed archive is a storage format only; it must remain
byte-for-byte reproducible from those commits. If a historical scenario is
intentionally replaced, regenerate the affected sources from the recorded full
SHA and update provenance in the same change.
