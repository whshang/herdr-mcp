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

## Current Scorecard（2026-08-28 · live alpha.14 / G5 sealed · docs G22 refresh）

评分：`PASS` / `PARTIAL` / `FAIL` / `UNKNOWN`。本表对齐 **2026-08-27 live `0.4.0-alpha.14`** runtime（generation `rust-7c0ac0b73060aae0`，merge `7efd30c` / Release [v0.4.0-alpha.14](https://github.com/whshang/herdr-mcp/releases/tag/v0.4.0-alpha.14)）+ G5 seal 证据。产品仍处 **alpha**，**未达 GA**；干净机 / 多机 / 退出 alpha 仍未完成。

本机 update 证据：managed update `0.4.0-alpha.13` / `rust-5c7799b56a426855` → `0.4.0-alpha.14` / `rust-7c0ac0b73060aae0`；service healthy；epoch 2 / 18 tools；用户 CLI → `runtime/current`。

本机 G5：`link-prod` = Rust `runtime/current link run`；candidate 留在 edge-prod；canary Node 未动。Rollback UAT PASS 后 `HERDR_LINK_SEAL_I_UNDERSTAND=1 link seal --execute` → active seal；`/health` `runtime=rust`，`native_migration.production_ready=true` / `link_cutover.production_ready=true`；`doctor` `production_ready_eligible=true`；`link status` 全 gate true。干净机仍属 G3/G18，不挡本机 G5 PASS。

| ID | 评分 | 一行证据 |
| --- | --- | --- |
| G1 | **FAIL** | 仍处 alpha：live `0.4.0-alpha.14`；`package.json` = `0.3.32` |
| G2 | **PARTIAL** | Rust Release + attestation/manifest 链已存在；正式文档主路径已改为 Release binary → `herdr-mcp install`；干净机下载 UAT 仍属 G18 |
| G3 | **PARTIAL** | 顶层 CLI + entrypoint 已合入；**本机 live symlink 已迁到 `runtime/current`**。仍 PARTIAL：缺干净机 / 正式多机 seal |
| G4 | **PARTIAL** | 正式文档已给出 binary → `herdr-mcp install` → doctor/status/update；干净机 UAT 仍属 G18 |
| G5 | **PASS** | 本机 production owner=rust：link-prod Rust；alpha.14 seal execute；health/`doctor`/`link status` 一致 `production_ready`；rollback UAT 证据在 `seals/evidence/`（干净机另计 G18） |
| G6 | **PARTIAL** | 嵌入契约 epoch 2 / 18 tools（hash 冻结）；真实 ChatGPT 与全路径 production smoke 仍属 alpha 验收 |
| G7 | **PARTIAL** | Edge→Link→runtime 在本机已走 sealed Rust `link-prod`；canary 仍 Node；缺多机 / 公网完整 GA UAT |
<<<<<<< HEAD
| G8 | **PARTIAL** | doctor 区分 Herdr/runtime/service/NM/Link/Edge configured + 有界 HTTPS Edge reachable / OAuth metadata / MCP endpoint（不发 token）；WSS dial 仍 skipped；干净机/公网完整 UAT 未封 |
=======
| G8 | **PARTIAL** | Live alpha.14 `doctor` 本地 LAYER 完整；远端 HTTPS probe 另跟 P1-1 PR；WSS dial 仍不在 doctor 内 |
>>>>>>> 4c88413 (docs(ga): make Release binary the primary install path)
| G9 | **PARTIAL** | 又一次 alpha N→N+1 通过（alpha.13→alpha.14）；**无** stable N→N+1 / 干净机用户向 UAT |
| G10 | **PARTIAL** | alpha.8↔alpha.6 真实受控回退已通过；非 stable、非干净机 |
| G11 | **PARTIAL** | service mutation guardian、Link reconnect 有现场证据；完整崩溃/重启矩阵未做 GA UAT |
| G12 | **PARTIAL** | Streaming basics 已落地；跨网页回合正式 GA UAT 未封板 |
| G13 | **PASS** | Batch A + Result Optimization 已合入 |
| G14 | **PARTIAL** | fs mutation policy / service mutation fencing 已有；Agent/terminal/browser mutation 未达「全部正式开放」 |
| G15 | **PARTIAL** | **决策 A（草图）**：第一 GA 走 versioned package 路径（Release/docs 给出可重复安装包 + managed native-host），不绑 browser-control worktree 实现；未封板前不作为正式卖点 |
| G16 | **FAIL** | **post-GA boundary**：browser terminal / true-steer / Browser Control Plane mutation **不**进入第一 GA 宣称；完成前保持 FAIL，不实现本轮 |
| G17 | **PARTIAL** | bearer/socket/managed-root/fail-closed 等边界多已实现；缺干净机 + 公网完整安全验收 |
| G18 | **FAIL** | 干净机 install UAT 未做 |
| G19 | **FAIL** | 多平台 / 第二台环境干净安装 UAT 未做；**第一 GA 平台收敛为 macOS Apple Silicon**；Windows 仅 preview artifact；不宣称 Linux lifecycle |
| G20 | **PARTIAL** | 文档与 CLI 合同在收敛中 |
| G21 | **PARTIAL** | 站点 CI gate 在 alpha 主线上维护；尚无 stable tag 封板 |
| G22 | **PARTIAL** | 正式 README/install/agent-install 主路径已改为 Release binary → `install`/`doctor`/`status`/`update`；clone/npm 降为 appendix。**docs PASS ≠ clean-machine UAT**（后者仍 G18） |
| G23 | **PARTIAL** | main CI 在 alpha 迭代中可绿；GA tag 专用全绿验收未跑 |
| G24 | **FAIL** | P0/P1 blocker 仍在：G1 alpha、G18/G19 干净机、G16 post-GA boundary、G15 封板、G22 干净机验证；本机 G5 seal + docs G22 仍不足 GA |
| G25 | **FAIL** | 未达 GA：不得打 stable |

**合计（诚实快照）**：PASS 2 · PARTIAL 16 · FAIL 7 · UNKNOWN 0

---

## Current P0 work queue

按 2026-08-28 live alpha.14 / G5 sealed / docs G22 refresh 证据排序：

1. **G18 — 干净机 install UAT**（命令清单见 `docs/i18n/en/clean-machine-uat.md`；本机开发环境不能代替）。
2. **G1 — 单一正式产品版本（退出 alpha）**。
3. **G3 formal seal — 干净机 / 多机确认**。
4. **G22 seal — 干净机验证文档主路径**（文档已 PARTIAL；需 G18 证据才能升 PASS）。
5. **G15 — 决策 A：versioned package 路径落地**（不绑 browser-control worktree）。
6. **G16 — 保持 post-GA boundary**（browser terminal / true-steer 不进第一 GA 宣称）。
7. **G5 follow-ups（非 blocker）**：candidate edge-prod soak 加深；长心跳 / cancel / long-request 矩阵。

---

## 使用方式

- 日常产品优先级以本文件 G1–G25 与上方 P0 queue 为准。
- roadmap 记录架构切片与历史；**是否 GA 只看本文件 scorecard 与 veto**。
- 更新 scorecard 时改日期、逐行证据，并同步刷新 P0 queue。
