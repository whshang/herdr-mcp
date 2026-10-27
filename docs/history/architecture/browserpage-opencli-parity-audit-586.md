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

## #586 checklist correction (2026-10-13)

The #586 'Generic form actions' item's checkbox/radio set/unset part is done
(real Chrome UAT passed on extension 0.1.153 for checked on/off, radio set,
no-op and unsupported-state rejection, see
`v1.1-browser-adapter-platform.md`); native select also passed
(0.1.151/0.1.152); keyboard press is unit-test-only and hover/drag/double-click
remain open, so the parent item stays open.

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
| Table extract | `extract` (table reads) | `action extract_table` (0.1.192) | unit-test-only | observed table refs, bounded headers/rows (max_rows 1..100, 32 cols, 200 chars/cell), visible-only cell text, hidden rows dropped, rowspan/colspan flattened per occurrence; Chrome UAT script ready in `v1.1-browser-adapter-platform.md` pending section |
| Screenshot | `screenshot` | `action screenshot` | verified-real | earlier UAT |
| Navigate / goto | `goto`, `navigate` | `action navigate` (0.1.223) | unit-test-only | owned pages only; explicit same-origin `url` + `page_generation` + idempotency key; verified destination adopted, mismatch/off-origin stays delivery-unknown and stale; Chrome UAT pending |
| Reload | `reload` | `action reload` (0.1.223) | unit-test-only | owned pages only; same-URL reload verified through a loading/complete cycle, generation advanced; page-reload-mid-dispatch recovery UAT still pending |
| Back / forward | `back`, `forward` | `action back/forward` (0.1.223) | unit-test-only | owned pages only; caller states the exact same-origin `expected_url`; CDP `Page.getNavigationHistory` preflight proves the current and exact adjacent entry before `Page.navigateToHistoryEntry` (never blind `goBack`), so a cross-origin adjacent entry fails closed; Chrome UAT pending |
| Tab list / select | `tab list` | `webchat resources`, claim by URL | unit-test-only | no generic tab enumeration action; claim needs an exact URL |
| Keyboard press / hotkeys | `press`, `keys` | `action press` (single-key vocabulary, 0.1.192) | unit-test-only | fixed 14-key vocabulary, optional observed ref to focus; no text/modifier combos; Chrome UAT script ready in `v1.1-browser-adapter-platform.md` pending section |
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

1. Navigation is implemented in the existing action path as 0.1.223
   `navigate`/`reload`/`back`/`forward` (unit-tested); the remaining work is
   real Chrome UAT, including page-reload-mid-dispatch recovery and
   history-destination verification.
2. Keyboard `press` for forms and feeds: implemented as a unit-test-only
   bounded single-key action (0.1.192); the live Chrome proof script (form
   control, page-owned key handler, vocabulary and unsafe-target negatives)
   is in `v1.1-browser-adapter-platform.md`, and any hotkey/modifier
   decision remains open.
3. Live proofs: late-acceptance promotion, SPA-return settlement (0.1.181),
   handoff, App-selection-failure cleanup.
4. Cross-provider dispatch UAT (Grok, Claude) using the same envelope.
5. Bounded `extract_table` (0.1.192) is unit-test-only; live proof on a
   real table-bearing page (including hidden-cell exclusion and the
   rowspan/colspan per-occurrence flattening behavior) still needs Chrome
   UAT. The exact pages, steps and expected results are written down in
   `v1.1-browser-adapter-platform.md` under the pending-UAT section.
