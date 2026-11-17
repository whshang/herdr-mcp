# Herdr 如何工作

*一页：谁负责什么、一次调用怎么走完、事实存放在哪里。*

深入设计见 [架构](architecture.md)。本页是你最先该看的地图。

## 组成部分

```text
ChatGPT / Claude / Grok 会话                  本机 coding agent(pi、claude、codex 等)
        │  MCP + OAuth(Connector)                        │  herdr-mcp CLI(受信本地 IPC)
        ▼                                                ▼
                  Cloudflare Edge ──────── 你自己的 Worker
                  OAuth、工具目录、                        (与 runtime 版本匹配)
                  投递证据、路由
                                │
              经认证的出站 WSS(Link)
                                │
        ┌───────────────────────┼────────────────────────────┐
        │              工作站(不开放入站端口)                  │
        │   herdr-mcp runtime(Rust,A/B generation)           │
        │     ├── 治理:受管根目录、投递、幂等                  │
        │     ├── 受信本地 IPC socket(扩展、CLI)              │
        │     └── 回环 HTTP MCP(127.0.0.1:8772)               │
        │   Herdr(TUI):workspace / tab / pane / agent        │
        │     └── 实际的 PTY、git checkout 与运行中的测试      │
        └────────────────────────────────────────────────────┘

        Chrome 扩展(可选回传通道)
          Native Messaging → mode-0600 socket → 同一 runtime
          Side Panel:实时 workspace / pane / 绑定状态
          页面侧:绑定会话唤醒 + handoff
```

## 谁负责什么

| 组件 | 负责 | 不负责 |
| --- | --- | --- |
| Web AI(ChatGPT/Claude/Grok) | 目标、规划、结果验收 | 本机状态;它只能拿到 runtime 返回的内容 |
| Cloudflare Edge | 公共 OAuth/MCP 身份、按 `device_id` 路由、每次请求的投递证据 | 工作站状态;它是转发层,不是第二事实源 |
| Link | 工作站的出站 WSS;重连并上报投递结果 | 任何状态;它只是传输层 |
| herdr-mcp runtime | 契约、治理(受管根目录、幂等、投递状态)、Continuity、Work Memory、agent 委派记录 | 实际 PTY/workspace 生命周期 |
| Herdr(TUI) | workspace、tab、pane、agent、PTY、会话 | 任何远端通信;它从不直接连 Edge |
| 浏览器扩展 | 浏览器侧执行、会话绑定、唤醒、观察 | 文件传输、agent runtime、通用浏览器自动化 |
| 本机 coding agent | 自己的任务;像其他本地客户端一样调 `herdr-mcp` CLI | 任务状态的第二个权威 |

## 一次调用的完整路径

```text
ChatGPT 工具调用(herdr_inspect,device=macbook-main)
  → Edge 校验 OAuth,选定 device_id
  → Link 已持有到该 Worker 的常驻 WSS
  → runtime 校验契约 + 受管根目录,调用 Herdr socket
  → Herdr 返回实时状态
  → runtime 返回结果与投递证据
```

两条规则保证它诚实地工作:

- **投递证据,不靠乐观假设。** 每个变更操作都报告 `delivery_state`(`applied`、`not_delivered`、`uncertain` 等)。超时或断连意味着"未知",绝不是"立即重试"。只有 `not_delivered` 可以重放,并且必须复用同一个 idempotency key。
- **引用只来自观察,绝不编造。** `device_id`、`session_ref`、`endpoint_ref`、`continuity_id`、pane/workspace id 都来自之前的调用或 CLI 返回。绝不从标签或主机名猜测。

## 设备身份与路由

每台已注册工作站是 fleet 中的一个 **device**,拥有不可变的 `dev_*` `device_id` 和独立凭据。`herdr_devices`(Edge 本地 action)列出 fleet;`herdr-mcp device list` 在本机显示同一份非秘密清单。

```text
第一台电脑     :herdr-mcp worker bootstrap(部署 Worker,注册 device #1)
后续电脑       :已注册设备 → herdr-mcp worker pair
               新电脑     → herdr-mcp worker connect "<pairing-address>"
ChatGPT       :显式指定 device,或让 Edge 以 device_ambiguous 拒绝猜测
```

裸的 `w1:p1` 只在一个 Herdr server/session 内唯一——跨机器引用 pane/workspace 时必须带上 `device_id`(或 `herdr_ref_*` 亲和)。见 [多设备控制](existing-worker-connect.md)与 [双通道控制](multi-machine-control.md)。

## WebChat 生命周期(本机 agent → 浏览器)

本机 agent 通过同一 runtime 驱动受支持的 Web 会话——扩展、受信 IPC、身份与投递规则与 MCP 路径一致:

```text
发现      herdr-mcp webchat endpoints → resources → inspect(拿到观察到的 ref 与 observation_generation)
创建      herdr-mcp webchat create --expected-generation N --idempotency-key KEY
发送      herdr-mcp webchat send   → dispatch_id + delivery_state
观察      herdr-mcp webchat dispatch-status <dispatch_id>
打开/归档 herdr-mcp webchat open | archive | archive-status
handoff  herdr-mcp webchat handoff --continuity-id hc:... --source-url URL
         (canonical packet;只有 automatic_delivery.completed=true 才算已投递)
```

## 事实存放在哪里

| 层 | 内容 | 生命周期 |
| --- | --- | --- |
| 实时状态 | workspace、pane、agent、Git、PTY | Herdr 持有;按需查询,不向上缓存 |
| Continuity | 一个 `continuity_id` 的持久任务日志 | 跨会话、浏览器刷新、runtime 重启存活 |
| Work Memory | 历史的精确分区(`project_ref` + `repo_id` + `work_chain_id`) | 按 work chain |
| registry/state db | device、grant、dispatch、generation | runtime 本地 |
| 页面/会话 | 仅渲染 | 可替换;永远不是状态权威 |

## 哪里出了问题时

按请求实际经过的顺序诊断——不要重启一个健康层去修上面的坏层:

```bash
herdr-mcp status            # runtime、Herdr、TCC,一行健康结论
herdr-mcp doctor            # 分层证据;默认输出是简洁摘要
herdr-mcp doctor --local    # 只读本机快照:白名单进程、
                            # 回环端口 8772、配置文件元数据
herdr-mcp link status       # Link 传输与 Edge 路由状态
```

`doctor --local` 绝不读取文件内容、环境变量或 `state.db`;使用非默认端口的命名实例在 8772 上显示 `listening:false` 是设计行为。完整故障词汇表与恢复路径见 [故障排查](troubleshooting.md)。
