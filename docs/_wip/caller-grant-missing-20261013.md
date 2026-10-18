# caller_grant_missing 排查与修复进度（2026-10-13）

范围：ChatGPT（@herdr 插件）→ Herdr MCP → qingxian-macbookair（dev_01M1BDHFE8TJ2PZGQAAG4T1A3V），Runtime 1.0.3-dev `9cd1e90b`，Chrome 154，扩展 0.1.202（从 `~/Documents/herdr-mcp-main/extension` 未打包加载）。跟进项见 #626。

## 已证实的根因

两个配置问题叠加，代码没有缺陷：

1. **native host 不认新扩展 ID。** 未打包的 0.1.202 没有 `key`，Chrome 给了新 ID `nfpapdlifleaejfjignibinjbahbcham`，而 native host 清单的 `allowed_origins` 只有旧 ID `iflileolljfdjhlaopdpgjnpdjcidlae`。新扩展连不上 native host，也就注册不了新端点。
2. **当前 ChatGPT 连接器没有 grant。** grant 绑定 {connector_id, grant_generation, device_id, endpoint_ref}。ChatGPT 重新授权后会生成新的 connector_id，旧 grant 静默失效；当前连接器 `conn_uGpFPdf84BEd1zeNfLRXO1` 对任何在线端点都没有 Page Assist grant。

拒绝位置：`mcp.rs:9849`（`9cd1e90b`；main 上在 `:9955`）的 caller grant 检查。

证据：同一 Runtime、同一段检查代码，签发 grant 前拒绝、签发后通过，中间没有改任何代码，唯一的变量是 allowlist 和 grant。

## 已修复（仅配置，未改代码）

- native host `allowed_origins` 改为 `nfpapdlifleaejfjignibinjbahbcham`，原配置备份在 Air 的 `~/.herdr/backup/20261013-nativehost/`。旧 ID 已移出 allowlist。扩展自动重连，没有重载扩展，也没有重启 Chrome。
- 新端点：`bep_ad8cf0cb9dc49ff4347df2a0ee9767741271e6c1380614d45fb84a388d6017ff`（0.1.202，18:59 UTC+8 注册）。
- 签发 grant：`herdr-mcp connector page-assist allow conn_uGpFPdf84BEd1zeNfLRXO1 qingxian-macbookair bep_ad8cf0cb… --confirm`。
- 所有者 CLI 本地实测：open 返回 page_ref，observe 返回真实快照，finalize 后标签页已关闭；全程未点击、填写或提交。

## 验收状态

| # | 项目 | 状态 |
|---|---|---|
| 1 | open 返回 page_ref | **通过**（asu 在 ChatGPT 中调用：`ok:true`, `ownership:owned`, `page_generation:1`, `reused:false`, `page_ref=bp_d384bc72…`） |
| 6 | 不再出现 caller_grant_missing | **通过**（同上，经 ChatGPT 调用链） |
| 2 | observe 返回真实快照 | 所有者 CLI 已通过；ChatGPT 侧待验 |
| 7 | 跨 turn 稳定 | 待验 |
| 3、4 | 观察并定位「不通过」按钮 | 无法用当前链接验证：候选人申请已终止，页面无该按钮。需要一个未评估候选人的链接 |
| 5、8 | SPA 变化检测、与 OpenCLI 对齐 | 待验 |

全程只读；没有提交评价，没有改候选人状态，没有动 OpenCLI 会话。

## 仍存在的授权限制

- ChatGPT 再次重新授权会生成新 connector_id，需要所有者重新 `page-assist allow`（#626 跟踪迁移）。
- 换扩展 ID（重新未打包加载到别的目录）需要重新修 allowlist；建议用 `HERDR_EXTENSION_PATH=… herdr-mcp native-host install` 重装。
- 可能存在另一个连接器 `conn_ZzwCy2…`，它没有 grant；若插件改用它会再次报错。
