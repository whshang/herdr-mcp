# ChatGPT App identity history fixtures

Frozen extension sources for `tests/chatgpt-app-identity-history.test.mjs`.

Each directory is an extension package version (`0.1.121` … `0.1.129`) with:

- `background.js` ← `extension/background.js`
- `wake.js` ← `extension/content/wake.js`
- `chatgpt.js` ← `extension/content/injector/chatgpt.js`
- `manifest.json` ← `extension/manifest.json`

`provenance.json` records the git SHAs these were extracted from so CI no longer needs `git show` / full history for this gate.

Treat these files as immutable regression inputs. Ordinary tests must read the
fixtures directly and must not require the provenance commits to exist locally.
If a historical scenario is intentionally replaced, refresh the affected files
from the full SHA in `provenance.json` with `git show <sha>:<source-path>` and
update the provenance entry in the same change.
