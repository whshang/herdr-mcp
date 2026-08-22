# 浏览器扩展 — 页面 JSON → 本地 MCP（主线 B）

读者：DeepSeek / z.ai 网页没有 MCP Connector，要用约定 JSON 打本机 herdr-mcp 的人。

总览与双主线：[extension.md](./extension.md)。进度回推见 [extension-wake.md](./extension-wake.md)。

## 目标

与主线 A **对等**：在无 connector 的站点上，助手输出 tool-call JSON → 扩展 `POST http://127.0.0.1:8772/mcp` → 结果回填同一会话。

首批：`chat.deepseek.com`、`chat.z.ai`。

## 现状

| 能力 | 状态 |
|---|---|
| SpeaksJSON 抠 `{"tool":"...","args":{}}` | **有**（DeepSeek / z.ai 内容脚本） |
| background `tools/call` + 结果回填同一会话 | **未做** |
| Options 白名单 | **未做** |
| 权限卡自动允许（chatgpt） | 另一主线附属能力 |

## 三阶段

### A — 协议骨架

```json
{"tool":"herdr_inspect","args":{}}
```

流式未闭合不解析；完成后提取 → background 调 MCP → 回填。  
默认只读白名单：`herdr_inspect` / `herdr_methods` / `herdr_since` / `herdr_fs_read|list|grep`。

### B — 能力面

Options 打开 `herdr_exec`、写文件、`herdr_prompt` 等；默认仍关。

### C — 完整 MCP 面

对齐 ChatGPT 默认 18 工具（仅本地，不经公网）。

## 与主线 A 的配合

DeepSeek 可同时：B 调 MCP 派活 + A 绑定同一 pane 做进度/收工回推。  
两条开关独立。

## 实现顺序（闭环未开）

解析层已在 `extension/content/webmcp/speaks-json.js`。还缺：

1. background `mcpCall(tool, args)`（`POST http://127.0.0.1:8772/mcp`）
2. SpeaksJSON 完成后串行执行并回填
3. Options 白名单
4. `extension_smoke` 增桥用例
