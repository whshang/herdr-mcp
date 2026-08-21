# herdr-mcp

把 [herdr](https://herdr.dev) 暴露成 MCP，让 ChatGPT / Claude / Cursor 等远程客户端指挥本机 pane 与 agent。

English: [README.md](README.md).

## 地址

| 用途 | URL |
|---|---|
| 公网 MCP（ChatGPT / Claude） | `https://xxxx.trycloudflare.com/mcp` |
| 本机 MCP | `http://127.0.0.1:8772/mcp` |
| 浏览器插件推送 | `http://127.0.0.1:8772/push/events` |

Connector 认证走 **OAuth（自动注册）**，不要填 API key。静态 Bearer 只给本机 curl / Cursor：`herdr-mcp token`。

## 接入

### ChatGPT / Claude

1. 添加 Connector，MCP URL 填：`https://xxxx.trycloudflare.com/mcp`
2. 选 OAuth，**不要**粘贴 Token
3. 配好后**开新对话**

```bash
herdr-mcp connector
```

### Cursor（本机）

`~/.cursor/mcp.json` 只挂本地（同一配置里不要再挂公网，Cursor 会对相同工具面去重）：

```json
{
  "mcpServers": {
    "herdr-mcp-local": {
      "url": "http://127.0.0.1:8772/mcp",
      "headers": {
        "Authorization": "Bearer <执行 herdr-mcp token 后粘贴>"
      }
    }
  }
}
```

别的机器或只走公网：用上面的 `/mcp` + Bearer，或客户端支持的 OAuth。

## 命令行

```bash
herdr-mcp              # 菜单
herdr-mcp status
herdr-mcp connector
herdr-mcp start | stop | restart
herdr-mcp logs [-f]
herdr-mcp token | url
```

## 默认工具

`herdr_methods` · `herdr_inspect` · `herdr_call` · `herdr_since` · `herdr_prompt` · `herdr_fs_read` · `herdr_fs_list` · `herdr_fs_grep` · `herdr_fs_write` · `herdr_fs_edit` · `herdr_exec`

写操作限制在 managed git root。可选：`HERDR_MCP_READONLY=1`、`HERDR_MCP_WRITE_ROOTS=/a,/b`。

## 浏览器插件

目录 `extension/`（MV3）。在 `chrome://extensions` 加载「未打包的扩展」。

herdr agent 结束后经 `/push/events` 唤醒已绑定的网页对话。在扩展选项里填本机 URL 与 token。支持站点：z.ai、deepseek、claude.ai、chatgpt.com。

## 运维

```bash
npx tsc && herdr-mcp restart
herdr-mcp logs -f
```

LaunchAgent：`dev.herdr-mcp.server`。会话文件：`~/.config/herdr-mcp/sessions/`。
