# Best practices

Workflows that keep the system fast, safe and observable. These are the operating rules the project follows; treat them as defaults, not as the only way to work.

## Web plans, local stays cheap

Orchestration happens in the web chat; heavy lifting stays on cheap local workers.

- Prefer `herdr_fs_*` / `herdr_git` / `herdr_exec` — no local-agent API involved.
- When reasoning is actually required, prefer `herdr_prompt` to a cheap/fast worker (`pi`, `flash`, `cline`, `opencode`, `anti`) or an auditor (`droid`, `grok`). Do not route planning or delegation through local Claude/OMP/main.
- If Pi/Herdr workers are unavailable, `dsh --profile headless "job"` is a tested CLI fallback — run it through a long `herdr_exec_start` session, not a 60‑second synchronous shell. See [worker-fallbacks](worker-fallbacks.md).

## The per-session ritual

1. `herdr_inspect` — connection health, workspaces, panes, agents.
2. `herdr_skill` — once per session, load the project policy plus release-matched upstream Herdr guidance.
3. Then work. Resume later with `herdr_since <cursor>` instead of re-dumping full state.

Details live in [architecture](architecture.md) (tool surface, epoch 2).

## Mutation discipline

- Prefer fire-and-forget `herdr_prompt` with an `idempotency_key`; track delivery via `herdr_since` / `herdr_inspect`.
- After any transport failure, check state before retrying — never blind-retry a non-idempotent mutation.
- `herdr_exec`: if control-plane failure happens before delivery, a local fallback may run; if already delivered, return a structured error — never double-run.
- Failed `commitAtomic` cleans up files added by that attempt; writes stay gated by managed-root / dirty / busy / readonly gates.

## Keep the Edge as the only public surface

- The workstation only makes outbound authenticated WSS (`herdr-link`). There is no public inbound port; do not re-expose the local MCP server directly except as a legacy migration path.
- Keep the MCP URL and `OAUTH_ISSUER` on the **same origin**; do not put a `/mcp` suffix in `OAUTH_ISSUER`. Mismatched origins are the most common connector failure.
- Use a least-privilege Cloudflare token (Workers Routes Write + Workers Scripts Write) — see [cloudflare-edge-token](cloudflare-edge-token.md) — and never commit `~/.config/herdr-mcp/*.env`.

## Upgrade with A/B, not big-bang

Runtime releases switch behind the persistent Edge/Link: validate the new generation against the frozen tool contract, activate it atomically, drain and roll back if needed — the ChatGPT Connector never changes. Never use `herdr-self-update` to cross a contract epoch. See [runtime-self-upgrade](runtime-self-upgrade.md).

## Example end-to-end flow

1. ChatGPT connects to the Edge MCP endpoint and completes OAuth (see [install](install.md)).
2. A new conversation starts; the model calls `herdr_inspect` to see the workstation, then `herdr_skill` once.
3. You ask for a change in a git-managed project: the model reads with `herdr_fs_read` / `herdr_git status`, edits with `herdr_fs_patch`, runs tests with `herdr_exec`, and commits through atomic Git helpers under the managed root.
4. Bind the web chat to the working workspace in the MV3 extension. Options chooses **Manual globally / Per-Project automation**. In Per-Project mode, explicitly enable the current ChatGPT Project before automatic progress/settled wakes, LLM continuation, timeout recovery or safe rollover can run; z.ai / DeepSeek expose a narrower conversation-scoped switch for automatic progress/settled push-back. When you want an explicit **Manual handoff**, first switch `Auto off`; it is supported for bound ChatGPT Projects and persisted z.ai `/c/<chat_id>` conversations. All extension traffic stays localhost-only; the static token never crosses Cloudflare or page JavaScript. See [extension-wake](extension-wake.md).
5. When a worker is down, the model falls back to `dsh --profile headless` through a long `herdr_exec_start` session instead of stalling.

Result: one stable public contract, cheap local compute, and a reverse channel that keeps the web conversation live.