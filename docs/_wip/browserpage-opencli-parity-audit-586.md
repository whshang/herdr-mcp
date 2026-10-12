# BrowserPage general-ops audit and OpenCLI / opencli-mcp parity (#586)

Audited 2026-10-10 on the Air DEV runtime (extension 0.1.182) against the
OpenCLI browser command catalog (opencli 1.8.7 `browser --help`) and the
opencli-mcp object API. This is the implementation boundary, not a release
claim. One issue (#586) tracks everything; there is no issue per primitive.

Classification:

- **verified-real**: passed live Chrome UAT on a real signed-in browser.
- **unit-test-only**: implemented and covered by tests, no live UAT yet.
- **site-adapter-insufficient**: works generically, but the real site needs
  an adapter (the generic path returns a shell or partial data).
- **missing-needed-for-1.1**: absent, and a 1.1 workflow needs it.
- **later**: absent and deliberately deferred (security review or no 1.1
  workflow).

## A. Generic primitives (BrowserPage kernel, `browser_page.*`)

| Capability | OpenCLI / opencli-mcp equivalent | Herdr surface | Class | Evidence / note |
| --- | --- | --- | --- | --- |
| Open an owned tab | `open`, `tab new` | `lifecycle open` | verified-real | Chrome UAT; Phase 3 observation tabs |
| Claim an existing tab | `tab select` | `lifecycle claim` | verified-real | 0.1.179: worker tabs claimable across the ChatGPT Project slug (D1/D2/E1) |
| Release / finalize (close owned) | `tab close` | `lifecycle release/finalize` | verified-real | finalize closes owned tab, stale ref -> `browser_page_not_found` |
| Structured state / observe | `state`, `snapshot` | `action observe` | verified-real | refs, generation, bounded text, DOM hints (0.1.148) |
| Click | `click` | `action click` | verified-real | Phase 3 SPA navigation via same-origin link |
| Fill / type | `type`, `fill` | `action fill` | verified-real | text, select, checkbox/radio (0.1.151-0.1.153) |
| Select option | `select` | `action fill` (select) | verified-real | 0.1.151/0.1.152 |
| Wait text / URL / element | `wait` | `action expect` | verified-real | element_present/absent 0.1.154 |
| Viewport scroll | `scroll` | `action scroll` | verified-real | 0.1.157, 9/9 UAT |
| Targeted text extract | `get text`, `extract` | `action extract` | verified-real | 0.1.156, generation + ref bound |
| Screenshot | `screenshot` | `action screenshot` | verified-real | earlier UAT |
| Navigate / goto | `goto`, `navigate` | (in progress on `feat/v11-next-navigation-20261009`, another lane) | missing-needed-for-1.1 | needed for post-submit route recovery; must reuse the mutation ledger |
| Reload | `reload` | none | missing-needed-for-1.1 | the page-reload-mid-dispatch recovery test is still blocked; add next to `navigate` in the same lane |
| Back / forward | `back`, `forward` | none | later | SPA navigation is covered by click + reopen; no 1.1 workflow needs history yet |
| Tab list / select | `tab list` | `webchat resources`, claim by URL | unit-test-only | no generic tab enumeration action; claim needs an exact URL |
| Keyboard press / hotkeys | `press`, `keys` | none (DEV-only trusted typing for the `@app` mention) | missing-needed-for-1.1 | forms and lazy-loaded feeds; trusted input stays scoped to the DEV build |
| Hover / double-click / drag | `hover`, `dblclick`, `drag` | none | later | |
| Nested scroll containers | `scroll --selector` | none | site-adapter-insufficient | feeds such as X use their own containers; adapters handle it |
| HTML / attributes | `get html/attr` | none | later | redaction review first |
| Same-origin network read | network-assisted reads | builtin adapter request builders only | site-adapter-insufficient | never exposes cookies or tokens |
| Upload / download artifacts | `upload`, `download` | Doubao image artifact via adapter | later (generic) | generic download hand-off to the artifact subsystem is deferred |
| Cross-origin frames, CDP, `eval` | `eval`, `cdp` | none | later | explicit security review; no implicit grants |
| Radix / ARIA menu triggers | implicit in click | `pressMenuTrigger` (archive, project settings) | verified-real | 0.1.178 archive fix; generic `click` still sends a bare click (unify with click in the navigation lane) |

## B. WebChat operations (`webchat` / `browser_session.*`)

| Capability | Class | Evidence / note |
| --- | --- | --- |
| Create session with required App (`@herdr`) | verified-real | Phases 1-3; about 16s when applied |
| Late acceptance of an `uncertain` create (readback, no resend) | unit-test-only | 0.1.180-0.1.182; live proof pending (generation fence after reload; digest mention tolerance added in 0.1.182) |
| Work-chain binding + Work Memory writeback | verified-real | Phase 2, 6 dispatches |
| Fanout status by work chain | verified-real | `dispatch-status --work-chain-id`, `completed: 6` |
| Archive + archive-status, with refusal reason | verified-real | 13 sessions archived |
| Open session (stale target repair) | unit-test-only | 0.1.181 repairs once via exact recovery |
| Settlement after SPA navigation away and back | unit-test-only | 0.1.181 resets the result probe on route change; Phase 3 D2 failed before the fix |
| Settlement after extension reload | verified-real | Phase 2 C2 |
| Settlement after page reload | missing-needed-for-1.1 | blocked on the `reload` primitive |
| Continuity-resume handoff to a new sub-session | unit-test-only | `webchat handoff` exists; live handoff test pending |
| App-selection-failure draft cleanup | unit-test-only | cleanup is scoped to automation-typed text; live failure not reproduced |

## C. Site commands (adapters; tracked separately from primitives)

| Site command | OpenCLI equivalent | Class | Evidence / note |
| --- | --- | --- | --- |
| `bilibili.video.transcript` | `bilibili subtitle` | verified-real | C1, 2026-10-02 |
| `x.search.posts`, `x.thread.read` | `twitter search/thread` | verified-real | C2, 2026-10-03; generic observe returned only the search shell |
| `doubao.image.generate/status` | AI image flows | verified-real | C3, 2026-10-03 |
| ChatGPT / Grok / Claude / Gemini / DeepSeek / Z.ai WebChat | `claude`, `gemini` commands | ChatGPT verified-real; the others unit-test-only for dispatch/settlement | injectors exist; cross-provider dispatch UAT pending |
| X timeline, Reddit, Zhihu, Weixin, Xiaohongshu, Xiaoyuzhou | site commands | missing (later unless a 1.1 workflow asks) | generic path first, then promote |
| Write actions (post/reply/like/follow) | site write commands | later | separate write-policy review |
| Commerce / research (Amazon, 1688, HN, scholar) | site commands | later | read-first adapters on demand |

## Gaps that 1.1 needs

1. `navigate` + `reload` in the existing action path (coordinate with the
   navigation lane, which has uncommitted `navigate` work) -> then the
   page-reload-mid-dispatch recovery UAT.
2. Keyboard `press` for forms and feeds.
3. Live proofs: late-acceptance promotion, SPA-return settlement (0.1.181),
   handoff, App-selection-failure cleanup.
4. Cross-provider dispatch UAT (Grok, Claude) using the same envelope.
