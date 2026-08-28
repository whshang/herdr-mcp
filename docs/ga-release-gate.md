# herdr-mcp GA Release Gate

状态：alpha 进行中（截至 2026-08-28 **未达 GA**）
本文件是 **GA 判定的唯一事实源（SSOT）**。架构演进细节见 [`herdr-architecture-roadmap.md`](./herdr-architecture-roadmap.md)。

## GA 定义

**GA = General Availability**：正式可供普通用户使用的稳定版本。

判定标准不是「Rust 重构做完了」，而是：

> 一个全新用户只看正式文档，可以从零安装，并稳定完成真实使用闭环。

可执行边界（全部满足才可结束 alpha、打 stable tag）：

```text
Rust production ownership
+ stable install / update / rollback
+ reliable public MCP closed loop
+ long-task / streaming basics
+ mutation safety
+ browser distribution
+ product-grade doctor
+ docs == reality
```

**不是**等 roadmap 全部做完才 GA。下面「post-GA allowed」明确列出可在 GA 之后继续迭代的项。

---

## 25 道门禁（G1–G25）

每条对应产品标准 1–25。发布前以 checklist 勾选；任一门禁未 PASS 则不得 merge GA docs 分支并打 stable tag。

### G1 — 单一正式产品版本

- [ ] `herdr-mcp --version` 只呈现一个正式产品版本口径
- [ ] 用户不再看到 `0.3.x` / `0.4.0-alpha.x` / Node version 多套版本
- [ ] Git tag、GitHub Release、二进制、README/Docs 对应同一版本
- [ ] 正式版本不再带 `alpha`
- [ ] `package.json` 若继续用于网站/扩展构建，不得代表 runtime 产品版本

### G2 — 正式二进制发布链完整

- [ ] 用户不需要 clone repo
- [ ] 用户不需要 Node.js / npm / Python 才能运行 herdr-mcp
- [ ] GitHub Release 提供所有「正式支持平台」的 binary / artifact
- [ ] 每个 artifact 有完整性验证信息
- [ ] Release provenance / SHA / 签名验证闭环可用
- [ ] 安装文档里的下载方式在干净机器上真实跑通过
- [ ] 文档声明支持的平台必须都有真实 release artifact；没有就不能声称支持

### G3 — 顶层用户 CLI 冻结

正式用户至少应有稳定入口：

```text
herdr-mcp install
herdr-mcp status
herdr-mcp doctor

herdr-mcp update check
herdr-mcp update apply
herdr-mcp update status

herdr-mcp rollback
herdr-mcp uninstall
```

- [ ] 普通用户不需要知道 `service install`、generation、candidate 等内部概念
- [ ] `service ...` 可保留为高级/内部接口，但不能是 README 主路径
- [ ] CLI help、README、Docs 必须完全一致

### G4 — 安装生命周期完整

干净机器路径：

```text
下载 binary → herdr-mcp install → service 正常启动 → herdr-mcp status → herdr-mcp doctor
```

- [ ] 全部通过
- [ ] 安装失败不留下半损坏状态
- [ ] 重复 `install` 有明确、幂等或可解释行为
- [ ] 升级不覆盖用户配置
- [ ] 卸载只清理本产品拥有的服务/资源，不误删用户项目数据

### G5 — Rust 成为本机 production owner

- [ ] production runtime 是 Rust
- [ ] production Link 是 Rust
- [ ] Native Messaging production owner 是 Rust
- [ ] service lifecycle 由 Rust 管理
- [ ] update / rollback 由 Rust 管理
- [ ] 真实生产链路不再依赖 Node runtime
- [ ] 不再需要旧 Node watchdog / compatibility runtime 才能保持产品运行
- [ ] Node 可留在 repo（网站、Extension、Edge、测试），但不属于用户 runtime dependency

### G6 — 18-tool 公共 MCP contract 稳定

- [ ] epoch 2 / 18 tools 正式契约冻结
- [ ] ChatGPT 新会话真实 `tools/list` 返回预期工具
- [ ] 不为 GA 临时增加第 19 个 tool
- [ ] 全部 18 个工具走真实 Rust production path
- [ ] 文件、Git、exec、long exec、agent、Herdr native escape hatch 有真实 smoke
- [ ] mutation safety 不因 Rust 切换而退化

### G7 — 公网 Edge → 本机完整闭环

必须从真正公网客户端验收（不是只做 localhost）：

```text
ChatGPT → OAuth/MCP → Cloudflare Edge → workstation Link → Rust runtime → Herdr
```

至少覆盖：`/health`、OAuth discovery、authorize/token、新会话 tools、`inspect`、文件读取、grep、Git status、短/长 exec、至少一个安全 mutation、Agent prompt、有界可解释错误、断线重连恢复。

### G8 — `doctor` 成为产品级诊断入口

至少能明确报告哪一层坏了：

```text
Herdr / local runtime / service / local IPC / Native Messaging /
browser bridge / workstation Link / public Edge / OAuth·MCP reachability / update state
```

- [ ] 失败后给出下一步，不要求用户理解 generation / socket / plist

### G9 — 更新闭环真实通过

至少一次：`stable N → update check → update apply → stable N+1`，并验证 artifact、identity、candidate 健康检查、激活、Connector 不变、Herdr/MCP 可用、Native Messaging 仍正常、用户配置保留。

### G10 — 回退闭环真实通过

至少一次：`N → N+1 → rollback → N`，并确认 service 健康、Connector URL 不变、Herdr 未损坏、Link / Native Messaging 恢复、不出现重复 mutation、`doctor`/`status` 可解释 rollback。

### G11 — 重启 / 崩溃恢复通过

覆盖：runtime crash、service restart、workstation reboot、Link 断连重连、extension restart、页面刷新、Edge 短时不可用、update 中途异常、长任务期间网页断线。恢复后不得因不确定交付而盲目重放写操作。

### G12 — 长任务达到正式产品标准

```text
exec_start → progress → exec_read(next_offset) → progress → completed
```

- [ ] 不绑定单次网页回复生命周期
- [ ] 「继续」不会重复启动
- [ ] `phase=started/completed` 等语义稳定
- [ ] `next_offset` 增量正确
- [ ] 大输出有界
- [ ] 用户能看到有意义阶段反馈

### G13 — 性能：无明显产品退化

GA 不要求做完全部性能 roadmap，但高频主路径不能有明显已知瓶颈：

- [ ] 文件读取 / 列表 / grep / Git 的 Batch A 收益未回归
- [ ] Result Optimization 不丢失败诊断证据
- [ ] 输出有界；大 Git/grep/exec 不默认全量灌模型
- [ ] Streaming path 不因新抽象显著增加延迟
- [ ] 20–50 pane / 多 workspace 基本可用

### G14 — Reliability 覆盖所有正式 mutation

正式开放的 mutation 必须：明确 target、stale target fail closed、已知 operation identity / idempotency 边界、timeout ≠ 未执行、uncertain delivery 不盲重试、可重新观察真实状态、service/update mutation 有 single-writer / recovery protection。达不到的 Agent / terminal / browser mutation **不得**在 GA 对外声称正式支持。

### G15 — 浏览器扩展达到正式分发状态

若 README/Docs 将其列为 GA 能力，则必须：正式 artifact 或安装渠道、manifest / Native Messaging 流程稳定、`native-host install/status/uninstall/rollback` 可用、reload 无需手工复制 token、Project/conversation binding、自动继续可开关、刷新/超时恢复基本可用、不拦截 OpenAI/ChatGPT 网络、网页不拿本机 bearer、对应 smoke 全绿。

Browser Control Center：要么 GA 前做完并正式宣称，要么从正式 feature list 拿掉。不能写成正式能力但代码仍是 WIP。

### G16 — Browser Control Plane mutation 边界正确

若 GA 包含浏览器手动控制本地终端 / Agent，必须等：Rust local control owner、explicit pane target、stale target protection、mutation reliability、`agent.prompt`、terminal input、interrupt 正式完成。

Codex true steer 默认 **不是** 第一版 GA blocker（除非 README 把 mid-turn steer 当卖点）。

### G17 — 安全闭环

真实验证：Edge 不暴露本机 bearer、Native Messaging caller/origin 限制、Unix socket 权限、filesystem managed-root、sensitive path、write/mutation fail-closed、OAuth secret/token 不进日志、Cloudflare management token 与 runtime credential 分离、Edge 不能成为 Browser Control Plane 公网控制入口、unknown action fail closed。

### G18 — 干净机器安装 UAT

不使用开发仓库完成：

```text
install Herdr → download herdr-mcp release → install → doctor →
configure Edge → ChatGPT OAuth → read-only tool → real mutation →
long task → browser extension → update → rollback
```

任一步仍需「先跑仓库里某个 Node script」→ 未达 GA。

### G19 — 第二台环境 / 平台 UAT

每个正式支持平台至少一次真实安装验收。若第一版只支持 macOS Apple Silicon，文档必须写明；缩小范围优于虚假跨平台承诺。

### G20 — 文档与实际 CLI 100% 对账

- [ ] README / Docs 中所有 `herdr-mcp ...` 命令真实存在且参数名一致
- [ ] 正式用户文档不再出现 alpha / candidate / watchdog / cutover 等过程术语
- [ ] 链接有效；中英文含义一致；日文 README 不声明不存在的能力
- [ ] Edge README 不暴露开发阶段历史
- [ ] `assets/herdr-mcp-SKILL.md` 与当前 runtime semantics 一致

### G21 — 文档站最终构建通过

保持既有 gate：`en 21/21`、`zh-CN 21/21`、site / skill / links / migration-term scan / `diff --check` PASS。stable tag 的文档源与发布站点必须来自同一 commit。

### G22 — 真实用户路径不依赖开发仓库

普通用户不应需要知道：`crates/`、`src/`、`npm test`、`cargo test`、`wrangler.prod.toml`、launchd plist、generation directory、compatibility installer、Node epoch。这些可留在 contributor docs，不能出现在正常安装主路径。

### G23 — CI / Release Gate 全绿

GA merge/tag 前：Rust unit/integration、service guardian、Node compatibility（若仍保留）、Edge、extension、site、release artifact verification、platform build matrix、`fmt`、`clippy -D warnings`、`diff --check` 全绿。

### G24 — 没有未解决的 P0/P1 GA blocker

发布前 issue 三分：`GA blocker` / `post-GA enhancement` / `explicitly unsupported`。

典型 blocker：安装失败、update/rollback 不可靠、mutation 可能重复、credential 泄漏、Node 仍是生产必需、Link 不可靠、文档命令不存在、新用户无法自行完成闭环。

### G25 — 最后的 GA Definition of Done

见下一节八个一票否决问题；任一项仍是「要看情况 / 需要手工 hack / 先跑仓库脚本 / 文档写的是未来设计」→ 不得打 stable tag。

---

## 八个一票否决问题（veto）

```text
1. 新用户能否不看源码安装？
2. 安装后 doctor 能否告诉他系统是否真的可用？
3. ChatGPT 能否从公网真实调用工作站？
4. 长任务是否能跨网页回合持续执行？
5. 写操作异常时是否不会盲目重复？
6. 升级失败能否可靠回退？
7. 浏览器扩展是否有正式、可重复的安装路径？
8. README/Docs 里写的每一项正式能力是否都真实存在？
```

任一项否决 → 未达 GA。

---

## GA blockers vs 非 blockers

### GA blockers（未清则不能 GA）

| 类别 | 例子 |
| --- | --- |
| 安装与分发 | 仍要求 clone / Node；无顶层 `install`；文档命令与 CLI 不一致 |
| 生产所有权 | production Link 仍是 Node；用户 runtime 依赖 Node |
| 闭环验收 | 干净机器 UAT 未过；公网 ChatGPT 闭环未过；update/rollback 未真实过 |
| 诊断 | `doctor` 无法指出是哪一层坏了 |
| 安全 / 可靠 | credential 泄漏风险；mutation 可能重复执行 |
| 契约 / 文档 | 文档宣称的能力不存在；正式文档残留 alpha/cutover 过程术语 |
| 版本 | 仍对外发布 `alpha` 作为「正式」；多套版本口径并存 |

### 明确非 GA blocker / post-GA allowed

以下可在 GA **之后**继续迭代（除非 README 提前写成正式卖点）：

| 项 | 说明 |
| --- | --- |
| **IndexBackend** | Search Execution 其余架构 |
| **Batch B advanced** | 高级 batch API（仍等 Layer 3 / 基准证明） |
| **IngressProfile** | 多公网入口 / 自动 failover |
| **deeper Project Context Cache** | 更深缓存层 |
| **Codex true steer** | mid-turn steer（默认非第一版 GA） |
| **Wave runtime** | 完整 Tool Wave Scheduler runtime（skill 层指引可已存在） |
| 微小 allocation / 局部微优化 | 无产品级退化即可 |
| UI 美化 | 非闭环必需 |

---

## Current Scorecard（2026-08-28 · live alpha.19 · GA freeze）

评分：`PASS` / `PARTIAL` / `FAIL` / `DEFERRED` / `UNKNOWN`。本表对齐 live `0.4.0-alpha.19`（generation `rust-3d2f685c636c3f3e`；Release <https://github.com/whshang/herdr-mcp/releases/tag/v0.4.0-alpha.19>；tag commit `4690c13`）。产品仍处 **alpha**，**未达 GA**。**未打 stable `0.4.0` tag**。**alpha.19 = final GA candidate（禁止 alpha.20）**。候选快照：[`docs/ga-candidate-status.md`](./ga-candidate-status.md)。

本机 dogfood（默认实例）：`production_ready=true`；`dev.herdr-mcp.server` `:8772` healthy；production Link=Rust；epoch 2 / 18 tools；用户 CLI → `runtime/current`；`native-host status` `runtime_matches_current=true`（alpha.19 cut）。**G5 保持 PASS**。

**Dogfood G6/G7 public UAT（2026-08-28 · alpha.19 · PASS）：** 默认实例公网路径 OAuth DCR+PKCE → `tools/list` 18/18 → read-only smoke → bounded `fs_write`（无重复）→ `exec_start`/`exec_read` 完成。证据 [`_wip/g67-dogfood-public-uat-20260828.json`](./_wip/g67-dogfood-public-uat-20260828.json)。Connector：`https://herdr-edge-prod.whshang.workers.dev/mcp`（workers.dev）/ `https://herdr-mcp.agentforme.cc.cd/mcp`（custom domain）。

**Preview-channel update/rollback rehearsal（2026-08-28 · G9/G10 等价彩排 · PASS）：** `alpha.19` → `rollback` → `alpha.18` → `update check` → `update apply` → `alpha.19`；`doctor`/native-host/Link 全绿。证据 [`_wip/g910-rc1-rehearsal-20260828.json`](./_wip/g910-rc1-rehearsal-20260828.json)。**不等价** stable-channel G9/G10 PASS。

**第二台 Mac 默认实例 UAT（2026-08-28 · owner re-eval · G18 PASS）：** macOS 15.7.3 aarch64 干净机；Release `v0.4.0-alpha.17` 二进制 → `install` → 独立 Worker + Rust Link + native-host + extension；ChatGPT Connector OAuth/tools/list epoch2/18 + tools/call OK。详见 **G18 second-Mac evidence**。

| ID | 评分 | 一行证据 |
| --- | --- | --- |
| G1 | **FAIL** | 仍处 alpha：live `0.4.0-alpha.19`；`package.json` 不得冒充 runtime 产品版本 |
| G2 | **PASS** | alpha.19 tag-path publish 全绿（run `33152207094`）；manifest `source_commit=4690c13`；attestation + extension zip；fail-closed 已证 |
| G3 | **PARTIAL** | 顶层 CLI + symlink → `runtime/current`；stable 口径未统一 |
| G4 | **PARTIAL** | 第二台 Mac + dogfood install/doctor 已封；stable install 路径未封 |
| G5 | **PASS** | production owner=rust；link-prod Rust；`production_ready=true` |
| G6 | **PASS** | dogfood 公网 OAuth+tools/list 18/18+mutation+long-exec 已封（2026-08-28）；+ 第二台 Mac tools/call |
| G7 | **PASS** | dogfood 公网 Edge→Link→runtime 全矩阵已封；soak/failover 未做（非 veto） |
| G8 | **PARTIAL** | dogfood `doctor` 全层绿；WSS dial skipped |
| G9 | **PARTIAL** | preview `alpha.18↔alpha.19` update apply 已封；**无 stable-channel N→N+1** |
| G10 | **PARTIAL** | preview rollback+recovery 已封；**无 stable-channel rollback rehearsal** |
| G11 | **PARTIAL** | guardian / Link reconnect 有证据；完整矩阵未做 |
| G12 | **PASS** | dogfood 公网 `exec_start`→`exec_read` completed（2026-08-28） |
| G13 | **PASS** | Batch A + Result Optimization 已合入 |
| G14 | **PARTIAL** | fs/service mutation fencing 已有；browser mutation 不宣称 |
| G15 | **PARTIAL** | 第二台 Mac extension + native-host 已封；dogfood 同机 uat 禁止双注册 |
| G16 | **DEFERRED** | browser terminal / true-steer / mutation — post-GA |
| G17 | **PARTIAL** | 公网路径已走通（dogfood + 第二台 Mac）；完整安全矩阵未全封 |
| G18 | **PASS** | 第二台 Mac clean install + public MCP loop |
| G19 | **PARTIAL** | 第二台 Mac Apple Silicon UAT 已封；Windows/Linux 未宣称 |
| G20 | **PARTIAL** | 用户文档仍有 alpha 术语；stable 封板未做 |
| G21 | **PARTIAL** | 站点 CI 可绿；无 stable tag 封板 |
| G22 | **PARTIAL** | docs PASS（#104）；第二台 Mac 实证已封；stable 文档封板未做 |
| G23 | **PARTIAL** | main CI 可绿；alpha.19 tag-path Rust Release 全绿（`33152207094`） |
| G24 | **FAIL** | 剩余 blocker：G1 alpha、G9/G10 stable-channel、G20–G22 docs freeze |
| G25 | **FAIL** | 未达 GA：不得打 stable |

**合计（诚实快照）**：PASS 7 · PARTIAL 14 · FAIL 3 · DEFERRED 1 · UNKNOWN 0

### G6/G7 dogfood public evidence（2026-08-28 · alpha.19 · PASS）

| Step | Result | Notes |
| --- | --- | --- |
| Connector URL | PASS | `https://herdr-edge-prod.whshang.workers.dev/mcp` + `https://herdr-mcp.agentforme.cc.cd/mcp` |
| OAuth DCR+PKCE | PASS | Issuer `herdr-mcp.agentforme.cc.cd`; scope `mcp`; refresh issued |
| `tools/list` | PASS | 18 tools (epoch 2 catalog) |
| Read-only smoke | PASS | `herdr_inspect`, `herdr_fs_list`, `herdr_git status` |
| Bounded mutation | PASS | `.herdr-ga-uat-marker.txt`; duplicate blocked |
| Long exec | PASS | `es_107ef-1a04768766c-10` → `phase=completed`, 2 polls |

OAuth via programmatic DCR+PKCE (ChatGPT-equivalent endpoints). ChatGPT browser UI not re-run this session (second-Mac G18 sealed Connector OAuth).

### G9/G10 preview-channel rehearsal（2026-08-28 · rc.1-equivalent · PASS）

| Step | Result |
| --- | --- |
| `rollback` alpha.19→alpha.18 | PASS |
| `doctor` / native-host / Link | PASS after rollback |
| `update check` (preview) | PASS (`v0.4.0-alpha.19` available) |
| `update apply` | PASS (health gate passed) |
| Post-update `doctor` / Link | PASS on alpha.19 |

**Not equivalent to stable-channel G9/G10 PASS.** See [`ga-candidate-status.md`](./ga-candidate-status.md) rc.1 path.

### alpha.18 dogfood evidence（2026-08-28 · historical）

| Step | Result | Notes |
| --- | --- | --- |
| Machine | PASS | Second Mac aarch64, macOS 15.7.3, clean default instance |
| Release download + `--version` | PASS | `0.4.0-alpha.17` Release binary only; no repo checkout runtime |
| `install` | PASS | `dev.herdr-mcp.server` healthy; config `~/.config/herdr-mcp` |
| `doctor` / `status` | PASS | Herdr / runtime / service / native-messaging / Link layers green |
| Independent Worker + Rust Link | PASS | Dedicated Edge Worker; Rust `link-prod`; not dogfood coupling |
| Extension + `native-host install` | PASS | Managed native-host owned; Chrome binding smoke green |
| ChatGPT Connector OAuth | PASS | Fresh connector authorize on second-Mac Worker origin |
| `tools/list` | PASS | epoch 2 / 18 tools |
| `tools/call` | PASS | Representative read/call path OK |
| Custom domain removal | PASS | Temp custom domain removed; workers.dev origin still healthy |
| update/rollback on clean instance | **OPEN** | Deferred to GA closure C (stable-channel rehearsal; no stable tag yet) |

Score **PASS** for canonical second-Mac default-instance clean install + public MCP closed loop. Same-machine `--instance uat` evidence below remains historical progress only.

### alpha.19 tag-path release evidence（2026-08-28 · #133 + #134 · G2 PASS）

Tag `v0.4.0-alpha.19` @ `4690c13`（#134 bump）。**Rust Release** tag-path run [`33152207094`](https://github.com/whshang/herdr-mcp/actions/runs/33152207094)：verify → build (darwin + windows) → manifest → attest → **publish** 全 PASS。**非 recovery-only**。

| Check | Result |
| --- | --- |
| GitHub Release | <https://github.com/whshang/herdr-mcp/releases/tag/v0.4.0-alpha.19> |
| Assets (5) | darwin aarch64 + windows exe + extension `0.1.66` zip + sha256 + `release-manifest.json` |
| Manifest `source_commit` | `4690c13d9e7304852f9f762cb783d1326f7a5e12` |
| Manifest `source_ref` | `refs/tags/v0.4.0-alpha.19` |
| Darwin SHA256 | `3d2f685c636c3f3e2c4720c9a63c703a818307af0507a68ac953268f8a009a60` |
| `state_schema` | 4 |
| Identity verify (#133) | extension zip excluded from manifest asset compare |
| Fail-closed (prior) | alpha.18 tag run `33150060112` failed identity verify; publish refuses clobber on existing release |

### alpha.19 dogfood evidence（2026-08-28 · default instance）

| Step | Before (alpha.18) | After (alpha.19) |
| --- | --- | --- |
| `--version` | `0.4.0-alpha.18` / `rust-50dc9a2550aefd2a` | `0.4.0-alpha.19` / `rust-3d2f685c636c3f3e` |
| `update apply` | — | `upd-1787903368810-67219-3d2f685c` succeeded |
| `native-host` / `doctor` | `runtime_matches_current=true` | `runtime_matches_current=true`, `version_consistent=true` |
| `link seal status` | `production_ready=true` | `production_ready=true` |

### G6 dogfood local MCP smoke（2026-08-28 · not public ChatGPT UAT）

| Tool / flow | Result |
| --- | --- |
| `herdr_inspect` | PASS — epoch 2 / 18 tools |
| `herdr_fs_grep` | PASS — docs `G2` hits |
| `herdr_git status` | PASS |
| `herdr_fs_write` | PASS — bounded create under `docs/_wip/` |
| `herdr_exec_start` → `herdr_exec_read` | PASS — single session completed |

Public Connector OAuth / cross-turn matrix：仍 **owner-only**（见 Owner ChatGPT UAT pack）。

### alpha.18 dogfood evidence（2026-08-28 · #130 + #131 cut · historical）

Release `v0.4.0-alpha.18`（tag `5ad301fc`）via recovery workflow from attested tag run `33150060112`（publish identity verify fail-closed；recovery `33150646912` 发布）。Edge `#130` 已在 main push 部署（run `33142199660` / `9bb0df8`）；`link-prod` 未动。

| Step | Before (alpha.17) | After (alpha.18) |
| --- | --- | --- |
| `--version` | `0.4.0-alpha.17` | `0.4.0-alpha.18` / `rust-50dc9a2550aefd2a` |
| `update check` | `available=false` | `available=true` → apply → `succeeded` |
| `native-host status` | `runtime_matches_current=false` | `runtime_matches_current=true`, `stale_runtime=false`, `version_consistent=true` |
| `doctor` native-messaging | no `runtime_matches_current` field | `runtime_matches_current=true version_consistent=true`（无 stale-runtime） |
| bare `herdr-mcp update` | N/A | equals `update check`; `available=false` at head |
| `link seal status` | `production_ready=true` | `production_ready=true`（prod Link Rust 未回归） |
| Release assets | — | darwin aarch64 + windows exe + extension `0.1.66` zip + manifest |

**未封：** G9/G10 stable-channel update/rollback；G1 stable version unification。

### G18 same-machine named-instance evidence（2026-08-28 · alpha.16 · historical）

Re-checked live after PR #113/#114 merge. Commands used Release binary only (`v0.4.0-alpha.16`); dogfood untouched.

| Step | Result | Notes |
| --- | --- | --- |
| Release download + `--version` | PASS | `0.4.0-alpha.16` / `rust-a3a09e936235c6b8` |
| `--instance uat install` | PASS | label `dev.herdr-mcp.uat.server`, port `:8885`, config `~/.config/herdr-mcp-uat` |
| `--instance uat doctor` | PASS (local) | Herdr / runtime / service / local-ipc green; `native-messaging absent` expected on dogfood Mac |
| `--instance uat status` | PASS | runtime `:8885` healthy |
| `--instance uat update check` | PASS | `available=false`, provenance verified |
| dogfood isolation | PASS | `dev.herdr-mcp.server` `:8772` still loaded; `~/.local/bin/herdr-mcp` still dogfood |
| extension zip → uat config | PASS | `herdr-mcp-extension-0.1.64.zip` sha256 OK; extracted to `~/.config/herdr-mcp-uat/extension` |
| extension static smoke | PASS | `extension_smoke.mjs`, `background_bind_test.mjs`, `queued-insert.test.mjs` all green |
| uat `native-host install` | **BLOCKED (by design)** | Chrome host name `dev.herdr.mcp` is singleton per profile; would overwrite dogfood manifest |
| public ChatGPT OAuth/tools | **OPEN (owner)** | not attempted on uat instance; see Owner ChatGPT UAT pack below |
| update/rollback on clean instance | **OPEN** | needs second Mac default instance or post-uat cleanup cycle |

Score as **PARTIAL**, not PASS: advances honest same-Mac runtime install path; canonical full G18 remains second-Mac default instance.

### Owner ChatGPT UAT pack（封 G6/G7/G18 公网段 · owner-only）

**Who:** product owner with ChatGPT Developer mode + org approval if required.

**Where (pick one — do not break dogfood):**

| Path | When to use | Risk to dogfood |
| --- | --- | --- |
| **A. Second Mac / VM, default instance** | Canonical G18 + G7 seal; deploy an independent Edge Worker first ([clean-machine-uat §Second Mac Worker](i18n/en/clean-machine-uat.md)); copy the internal [_wip Second Mac GA UAT Agent prompt](_wip/en/second-mac-ga-uat-agent-prompt.md) to the pi coding Agent (not the public install tutorial) | None |
| **B. Dogfood Mac, default instance, maintenance window** | Owner accepts brief prod Link/Edge coupling | OAuth/tools exercise uses live `link-prod`; follow rollback runbook if anything regresses |

**Do not** run ChatGPT OAuth UAT through `--instance uat` on the dogfood Mac: uat has no owned Link/Edge identity and must not cut over `link-prod`.

**Preflight (read-only):**

```bash
herdr-mcp --version
herdr-mcp doctor
herdr-mcp link seal status
# expect: production_ready=true, edge-reachable, oauth-metadata, mcp-endpoint (401 auth=not-sent)
```

**Owner steps (new ChatGPT conversation each validation):**

1. **Connectors → Add MCP App** with public MCP URL from install docs (`https://<edge-origin>/mcp`). Never paste `HERDR_MCP_TOKEN`.
2. **Complete OAuth** in browser; confirm connector shows authorized (not merely "added").
3. **Start a new chat** (fresh `tools/list`).
4. **Verify epoch 2 / 18 tools** in developer tooling or first tool call metadata.
5. **Read-only smoke:** `herdr_inspect` (or `herdr_fs_list` on a safe path).
6. **Bounded mutation:** one explicit fs/git mutation the owner chooses; confirm single delivery (no duplicate).
7. **Long exec smoke:** `herdr_exec_start` → poll `herdr_exec_read` with `next_offset` until `completed`; confirm no duplicate start on "continue".
8. **Record evidence** (non-secret): tool names count, mutation target, exec task id, `doctor` layer summary. If blocked, record exact step (connector add / authorize / tools/list / tool call / long exec).

**Pass criteria for G6/G7 public segment:** steps 1–7 complete on a **default-instance clean or owner-approved dogfood** machine without credential leakage and without duplicate mutation.

**Fail / defer honestly:** partial OAuth, stale tools list, or owner unavailable → leave G6/G7/G18 public rows **PARTIAL**; do not mark PASS.

### UAT instance cleanup（optional, after tests）

Does not touch dogfood:

```bash
export HERDR_MCP_INSTANCE=uat
herdr-mcp --instance uat uninstall
# removes dev.herdr-mcp.uat.* LaunchAgents, ~/.config/herdr-mcp-uat (including extension/)
```

---

## Current P0 work queue

按 2026-08-28 GA freeze（G6/G7 dogfood PASS；preview-channel G9/G10 rehearsal PASS；G2 tag-path PASS）排序：

1. **rc.1 tag workflow** — `0.4.0-rc.1` bump PR（**禁止 alpha.20**）；见 [`ga-candidate-status.md`](./ga-candidate-status.md)。
2. **G9/G10 — stable-channel rehearsal**（rc.1/stable tag 后：`update check`/`apply`/`rollback` on **stable** channel）。
3. **G1 — 退出 alpha**（stable-channel G9/G10 PASS 后评估 `v0.4.0`；**现在不要打 stable tag**）。
4. **G20–G22 — stable docs freeze**（用户路径去除 alpha/candidate 术语）。
5. **G16 — 保持 DEFERRED**（browser terminal / true-steer 不进第一 GA）。

**已完成：** G2 tag-path PASS（alpha.19）、G18 second-Mac PASS、G6/G7 dogfood public PASS、preview-channel update/rollback rehearsal。

### GA closure C — stable update/rollback rehearsal（blocker until stable tag）

**Blocker:** 无 `v0.4.0` stable tag；`update.channel = stable` 不能代替 GA 证据。

**Rehearsal path（alpha 期间可执行，不等价 GA PASS）：**

```bash
# Read-only preflight (independent shell)
herdr-mcp --version
herdr-mcp service status
herdr-mcp native-host status
herdr-mcp update status

# Preview channel cutover rehearsal (dogfood or second Mac)
herdr-mcp update check --manifest <pinned-alpha-manifest-url>
herdr-mcp update apply --manifest <pinned-alpha-manifest-url>
herdr-mcp doctor
herdr-mcp native-host status   # expect runtime_matches_current=true after GA closure B
herdr-mcp rollback
herdr-mcp doctor
herdr-mcp native-host status
```

**GA PASS criteria for G9/G10:** same commands on **default instance** using **stable-channel** `update check` (no `--manifest` override) immediately before/after `v0.4.0` tag exists.

---

## 使用方式

- 日常产品优先级以本文件 G1–G25 与上方 P0 queue 为准。
- roadmap 记录架构切片与历史；**是否 GA 只看本文件 scorecard 与 veto**。
- `DEFERRED` = 明确 post-GA，**不是**当前 veto。
- 更新 scorecard 时改日期、逐行证据，并同步刷新 P0 queue。
