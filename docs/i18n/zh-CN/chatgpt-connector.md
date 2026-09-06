# ChatGPT Connector

*通过 MCP 与 OAuth 把网页里的 ChatGPT 接到本地工作站。*

ChatGPT 不能直接访问 `127.0.0.1`。herdr-mcp 因此把本机开发环境放在一个稳定的远程 MCP 入口后面：ChatGPT 连接 Cloudflare Edge，工作站再通过出站 WSS 主动连到 Edge。

```text
ChatGPT
  │ OAuth + MCP
  ▼
Cloudflare Edge
  │ authenticated WSS
  ▼
herdr-link / herdr-mcp
  │
  ├─ files / Git / shell
  └─ Herdr workspaces / panes / agents
```

如果本地 runtime 和 Edge 还没有部署，先看 [安装](install.md)。本页集中解释 ChatGPT 这一侧：怎样连接、为什么需要 OAuth、什么叫“连接成功”、工具快照为什么会陈旧，以及出现问题时应该查哪一层。

> ChatGPT 的 Developer mode、Apps / custom MCP UI 和套餐权限仍在持续演进。项目文档描述 herdr-mcp 需要满足的协议边界；具体菜单名称和账户可用范围以 OpenAI 当前官方文档为准。

## 连接前先理解三个不同的“成功”

### 1. OAuth 成功

ChatGPT 知道这个 MCP 服务是谁，也拿到了访问授权。

### 2. MCP 握手成功

ChatGPT 能完成 initialize / discovery / `tools/list`。

### 3. 工作站真的可达

工具调用能穿过 Edge 和 workstation link，最终看到本机 Herdr / Git / shell。

这三层可以独立失败。所以“设置页面显示已连接”并不等于“当前聊天已经能改代码”。

最可靠的验收不是看一个绿色连接状态，而是在**新会话**里执行一次真实 `herdr_inspect`。

## 创建 Connector / Custom MCP App

当前 ChatGPT Web 的开发者模式可以添加自定义 MCP 应用。界面和套餐权限可能变化，整体流程保持一致：

1. 在 ChatGPT Workspace / Apps 设置中启用 Developer mode；
2. 创建或添加自定义 MCP App / Connector；
3. MCP URL 填写：

   ```text
   https://<your-edge-origin>/mcp
   ```

4. 完成浏览器 OAuth。首次授权时，Herdr 不会静默放行，而是显示一个短期批准请求；在任意已登记到这个 Worker 的电脑上运行 `herdr-mcp connector approve <approval-request-id>` 完成批准。已批准的 WebChat Connector 仍只有普通 MCP 权限，不能继续批准另一个 Connector；
5. 保存后新建一个聊天进行验收。

**不要填写本机 `HERDR_MCP_TOKEN`。** ChatGPT 公网入口使用 OAuth；静态 bearer 只用于本机 curl / Cursor 和兼容路径。

如果组织对自定义 MCP 应用有管理员审批、Action control 或 RBAC，先满足 Workspace 侧策略。herdr-mcp 无法绕过 ChatGPT 自己的组织治理。

## 为什么 OAuth issuer 不能随便换

一个 Connector 不只是记住 `/mcp` 地址。OAuth metadata、issuer、resource audience 和 MCP origin 必须互相一致。

推荐配置：

```text
HERDR_MCP_BASE_URL=https://herdr-edge.example.workers.dev
MCP URL=https://herdr-edge.example.workers.dev/mcp
```

`HERDR_MCP_BASE_URL` 不带 `/mcp`。

Edge origin 应当稳定。本地 runtime 可以升级、A/B 切代甚至回滚；只要 contract 和 Edge identity 不变，ChatGPT Connector 不需要跟着重新配置。

这也是项目把 Edge 生命周期和本机 runtime 生命周期分开的原因。

## OAuth 链路

ChatGPT 会探索远程服务器的 OAuth / protected-resource metadata。Dynamic Client Registration（DCR）只负责登记 client metadata，**不等于授权**。从 v0.4.6 开始，新 Connector 必须由已登记 Device/operator 控制通道明确批准后才能取得可用 token：

```text
Connector
  │ metadata discovery + DCR
  │ authorize + PKCE
  ▼
Herdr pending approval 页面
  │ request id + 短期 6 位批准码
  └─ 任意已登记电脑：
       herdr-mcp connector approve <request-id>
  ▼
authorization code → token → /mcp
```

同一个 Worker 内的已登记设备在这条控制面上没有 owner/member 高下之分；Device/operator 负责 fleet 管理。已批准 Connector 只获得普通 MCP 访问，不能批准/撤销其它 Connector，也不能创建 pairing 或 revoke Device。v0.4.6 之前、在没有明确批准步骤时签发的 OAuth token 可以继续做普通 MCP 兼容访问，直到 operator 显式撤销其 client grant。v0.4.6 的当前 Connector 实例用 `herdr-mcp connector list` 取得不可变 `connector_id`，再用 `herdr-mcp connector revoke <connector-id> --confirm` 单独撤销；更早、还没有 Connector-instance 记录的 legacy client 仍可通过兼容 grant tombstone 被彻底封禁。

批准码是单用途、短时、限制错误次数的交互凭据，不接受普通 CLI argv 传入。若目的是取消 Herdr 授权，仅在 Web AI 的 UI 中 Disconnect 并不能替代 Worker 端 revoke。

实现兼容 Client ID Metadata Document、PKCE、JWT access token 和当前 ChatGPT MCP 客户端行为。

排查时重点关注：

- public origin 是否一致；
- `OAUTH_ISSUER` 是否与用户实际访问的 origin 一致；
- redirect/token 请求是否成功；
- token 的 audience 是否对应 MCP resource；
- Edge 是否能继续把工具调用路由到在线 workstation。

OAuth 成功以后 workstation 仍可能离线，所以不要把 OAuth 当作完整健康检查。

## 为什么新会话很重要：tool snapshot

ChatGPT 使用经过审核的 MCP action 冻结快照。只升级本地 runtime 或部署 Edge，并不会自动把新增 action 启用到已经批准的 Workspace App 中。

Herdr 0.4.3 明确区分两层 contract：

**ChatGPT public contract：epoch 3 / 19 tools；workstation runtime execution contract：epoch 2 / 18 tools。** 新增的第 19 个 action 是 Edge-local `herdr_devices`，不会转发到 workstation。

典型现象：

```text
服务器已经 public epoch 3 / 19 tools
        │
        ├─ 已刷新 action 集：可以看到 19 tools ✓
        │
        └─ 旧/冻结 action 集：可能仍只有 18 tools
```

升级 runtime 后：

1. 确认 Edge / runtime 暴露的是当前版本；
2. **不要**仅因为 workstation runtime 升级就断开、删除或重新添加 Connector；
3. Herdr public action catalog 发生变化时，通过当前账户可用的 Workspace App 管理入口刷新、审核并发布 actions；新增 action 如需显式启用则同时启用；
4. action snapshot 更新后，用新会话重新验证。

不要为了陈旧 tool snapshot 重装 workstation。v0.4.2 runtime 仍可继续执行 epoch-2 / 18-tool workstation contract；只有升级到 v0.4.3 后才获得新的多设备 runtime 能力。

工具 catalog 真正发生不兼容变化时，项目使用 contract epoch 管理；普通 runtime bugfix 不应该随意改变 catalog。

## 为什么工具少反而更适合 ChatGPT

Herdr 原生 Socket API 有大量方法。把每个方法都变成 MCP tool，会让 ChatGPT 每轮携带大量 schema，也让工具选择变得困难。

ChatGPT 常用的是：

```text
herdr_inspect
herdr_since
herdr_fs_*
herdr_git
herdr_exec*
herdr_prompt
```

低频 Herdr 原生能力再由 `herdr_methods` → `herdr_call` 动态发现。

所以 `tools/list` 很短，但能力并没有被裁掉。

## 一次真实会话应该怎样开始

推荐的第一条任务可以很简单：

```text
检查当前 Herdr 工作区和 Git 状态。只读，不要修改。
```

理想流程：

1. ChatGPT 调 `herdr_inspect`；
2. 读取一次 `herdr_skill` 获取当前操作策略；
3. 选定目标 managed Git root；
4. 用 `herdr_git status` / `herdr_fs_read` 获取事实；
5. 回答用户。

之后真正开发时再进入 patch / exec / agent delegation。

这比用“工具数量显示正确”作为唯一验收更可靠。

## 权限确认卡是谁控制的

ChatGPT 对写入或有风险的 App action 可能显示确认 UI。这个行为属于 ChatGPT 产品安全层，herdr-mcp 服务端无法保证“永远不询问”。

项目浏览器扩展可以在满足严格条件时自动处理**页面 DOM 内明确的 Allow/允许卡片**，但它不会绕过 Workspace 权限，也不会处理浏览器/系统原生权限对话框。

当前 ChatGPT Project 自动化只有在：

- Options 允许 Project 自动化；
- 当前 Project HUD 为 `自动 开`；
- 页面出现可识别、可见、可用的允许动作；

时才进行自动点击。`自动 关` 时只观察。

详见 [浏览器连续工作](browser-continuity.md) 与 [自动继续、恢复和接力](browser-continuity.md)。

## Streamable HTTP 与 ChatGPT 兼容层

下面属于维护者参考，普通用户不需要为了连接成功理解全部细节。

### ChatGPT 路径保持无状态

herdr-mcp 对 ChatGPT 的 MCP HTTP 路径不依赖长期 `Mcp-Session-Id`。实践中，runtime 重启后客户端继续复用 stale session id 会产生 `Session terminated` 一类假故障。

长命令状态由 `herdr_exec_start/read/kill` 独立管理，不需要绑定 MCP HTTP session 生命周期。

### initialize / tools/list

ChatGPT MCP 客户端对协议版本和响应形态有明确兼容需求。项目的兼容层负责：

- discovery / initialize；
- protocol-version negotiation；
- SSE / JSON 响应形态；
- CallToolResult 的结构化内容和 image 透传；
- OAuth 错误与 JSON-RPC 错误语义分离。

不要为了修一个客户端兼容问题把公共 tool schema 随意改掉。

### schema 是一个整体

一个不兼容的 `inputSchema` 可能导致 ChatGPT 拒绝整份 tool catalog。历史上需要特别谨慎的 JSON Schema 结构包括自由对象、某些 `additionalProperties` 形态以及客户端不接受的约束关键字。

因此改工具输入时必须跑真实 ChatGPT UAT，不能只验证本地 MCP inspector。

## 长任务为什么还需要浏览器扩展

MCP 是请求驱动的。ChatGPT 把任务成功交给一个 Herdr Agent 后，工具调用可能已经返回，而 Agent 仍在本机工作。

```text
ChatGPT: “已提交任务”
        ↓
本轮网页回合结束

Herdr Agent: working ... working ... done
```

没有额外通道，Agent done 并不会自动让 ChatGPT 开始下一轮。

浏览器扩展负责把本地 progress / settled 事件送回绑定会话，并处理回复卡住和长对话 handoff。因此：

- Connector 解决 **ChatGPT → workstation**；
- 浏览器 continuity 解决 **workstation → ChatGPT**。

两条方向合起来才适合持续数小时的网页开发工作。

## 从症状定位故障层

| 症状 | 优先检查 |
|---|---|
| Connector 根本添加不了 | public URL / OAuth metadata / Workspace developer permissions |
| OAuth 成功，聊天里没有工具 | tools/list / schema / App action refresh / 旧会话 snapshot |
| 能看到工具，但 `herdr_inspect` 报 workstation offline | herdr-link / workstation identity / runtime health |
| `herdr_inspect` 正常，文件工具失败 | managed Git root / readonly / path gate |
| shell/Agent 工作了但网页不继续 | browser extension binding / Auto / progress channel |
| 显示 Session terminated | MCP compatibility / stale session semantics，不要先怀疑 Git 仓库 |

完整排查顺序见 [故障排查](troubleshooting.md)。

## 最低验收标准

一次真正成功的 ChatGPT 接入至少满足：

- OAuth 完成；
- 新会话能看到当前 contract；
- `herdr_inspect` 返回真实 workstation；
- 能读一个 managed Git 项目；
- 能执行一条安全测试命令；
- 写操作的确认和权限行为符合预期；
- 长任务场景下，安装扩展后能收到 progress / settled 回推。

当这些都成立时，ChatGPT 才真正从“远程聊天窗口”变成连接到本机 Herdr 的开发 planner。
