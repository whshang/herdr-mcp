# 浏览器插件：页面 → 本地 MCP 桥（路线）

读者：要把 z.ai / DeepSeek 聊天页接到本机 herdr-mcp 的人。

相关：`extension/`、[extension-wake.md](./extension-wake.md)、[architecture.md](./architecture.md)。

## 目标

在 **没有** ChatGPT 式 MCP Connector 的站点上，让网页里的模型用约定 JSON 调用本机 `http://127.0.0.1:8772/mcp`，拿到 herdr 工作站能力。

首批页面：`chat.z.ai`、`chat.deepseek.com`。

## 现状（已有）

| 能力 | 状态 |
|---|---|
| herdr → 网页唤醒（写输入框并发送） | 已有 |
| SpeaksJSON：从助手回复抠 `{"tool":...}` | 已有种子，尚未打到 MCP |
| chatgpt.com 工具权限卡自动点「允许」 | 已有（内容脚本 ≥ 0.1.3 常驻） |
| 页面 JSON → `POST /mcp` → 结果回填 | **未做**（本路线要做的） |

## 三阶段（已定）

按「先协议后能力，再到完整 MCP 面」推进；默认安全，能力用开关打开。

### 阶段 A — 协议骨架

1. 约定助手输出形态（与现有 SpeaksJSON 对齐）：

```json
{"tool":"herdr_inspect","args":{}}
```

多工具可连续多段 JSON；未闭合的流式半截不解析。

2. 扩展在回复完成后提取 tool call → background 用已存 token 调本地 MCP（`tools/call`）。
3. 结果写回同一会话输入框并发送（或追加一条「工具结果」消息，站点允许哪种用哪种）。
4. 默认 **只读白名单**：`herdr_inspect`、`herdr_methods`、`herdr_since`、`herdr_fs_read`、`herdr_fs_list`、`herdr_fs_grep`；只读类 `herdr_call`（如 `ping`、`agent.list`）可另列。
5. Options 里：总开关、白名单、是否自动回填。

成功标准：在 z.ai 或 DeepSeek 让模型输出一次 `herdr_inspect` JSON，扩展调用本机并回填 `ok: true` 摘要。

### 阶段 B — 能力面

在白名单中按需打开：

- `herdr_exec`（命令通道）
- `herdr_fs_write` / `herdr_fs_edit`（写文件，保留 confirm_* 门控）
- `herdr_prompt`（向窗格 agent 派活）

仍禁止：任意未声明的 `herdr_call` 变更方法、关闭工作区、reap 等，除非用户在 Options 显式勾选「完整 MCP」。

### 阶段 C — 完整 MCP 面

与 ChatGPT Connector 默认 11 工具对齐（或 `HERDR_MCP_ALL_TOOLS` 全量），仍只走本地 `127.0.0.1`，不经公网隧道（扩展不依赖 Cloudflare）。

## 不做

- 假装扩展安装后 DeepSeek「自带」和 ChatGPT 一样的 OAuth connector
- 把公网 MCP URL 写进扩展默认配置
- 在协议未稳定前把写文件 / exec 设为默认开启

## 与唤醒的关系

唤醒：herdr 干完 → 推网页。  
桥：网页模型要查本机 → 拉 herdr-mcp。

同一扩展、同一 token；两条通路独立开关。

## 实现顺序（下一刀实现时）

1. background：`mcpCall(tool, args)` 封装（initialize 可省略：本地可走与 Cursor 相同的 stateful 或单次 call，以现网 `openai` 以外客户端行为为准）
2. wake/speaks：回复完成 → extractToolCalls → 串行执行 → 回填
3. Options UI + 文档截图级说明
4. `tests/manual/extension_smoke.mjs` 增加桥协议用例

本文是路线与验收，不是已实现功能说明。实现前若协议字段有变，先改本文再改代码。
