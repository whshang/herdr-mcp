# 添加电脑

*把一台新电脑接入已有的 Herdr Worker。*

这是**把一台新电脑接入已有的 herdr-mcp Worker/Connector** 的权威 Agent 执行契约。它**不是**全新 Worker 部署。

> **v0.4.3 中仅限 macOS。** 安全的新设备配对依赖 macOS Keychain 凭据后端。在 Linux/Windows 上，`worker pair` / `worker connect` 路径**不可用且 fail closed**；runtime 本身在这些平台上仍受支持。

## 开始之前

- 本路径要求 **v0.4.3+**。先检查最新 stable Release 的版本/能力。如果 stable 仍是 `<0.4.3`，或已安装 CLI 不提供 `herdr-mcp worker pair` / `herdr-mcp worker connect`，就**停止并报告版本/能力 blocker**。除非用户明确要求测试 preview/source，否则不要安装 prerelease/source build。
- 从 **GitHub Release** 安装最新 stable PROD herdr-mcp，不要从 repo checkout 安装。不要把 source/dev build 当作正常安装。
- 这**不是**全新 Worker 部署。**不要**新建 Cloudflare Worker、Durable Object namespace、OAuth app/client、Connector，也不要复制旧的全局 `LINK_SHARED_SECRET`。你加入的是用户已有的那个 Worker。

## 配对如何工作

1. 在**已授权的既有 macOS 电脑**上，属主运行：

   ```bash
   herdr-mcp worker pair
   ```

   这会创建一个短生命周期配对会话（默认 **10 分钟**，一次性使用），并打印：
   - 一个**配对地址**（Worker origin 加上 URL fragment 里的高熵配对 id），以及
   - 一个 **6 位验证码**（格式 `123 456`）。

2. 在**新电脑**上，Agent 运行：

   ```bash
   herdr-mcp worker connect "<pairing-address>" --name "<device-name>"
   ```

   然后 CLI 会**提示输入 6 位验证码**（无回显 TTY，或非交互时单行无回显 stdin）。验证码**绝不**作为命令行参数，也**绝不**回显或记录。

3. 成功后，临时配对会兑换成已有的高熵每设备密钥。最终设备密钥**只**存入 macOS Keychain；配对码/会话立即失效。加入设备不使用任何 Cloudflare 部署凭据，也不使用旧的 `LINK_SHARED_SECRET`。

## 安全规则

- 6 位验证码就是预期的短生命周期配对凭据。它一次性使用、10 分钟过期，且最多 **5 次错误尝试**后会话被永久锁定。
- 配对 id 是高熵且不可猜测的；它放在配对地址（URL fragment）里，不在 HTTP access-log 路径中。最终设备密钥绝不在配对地址里。
- 验证码**绝不**出现在 argv、shell history、日志或 transcript 中。**不要**用 `echo 123456 | ...` 或任何会把验证码写进 shell history 的 shell 字面量。
- 最终设备凭据属于 macOS Keychain。绝不打印或记录它。

## 验证

成功 connect 后，验证：

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
```

确认最终的不可变 `device_id`、Link online/healthy 以及本地绑定成功。

## 不确定投递 / 恢复

- 如果任何 mutation 报告投递不确定，**不要盲目重试**；先检查当前状态。
- 如果 connect 在服务端消费后失败，依赖内置的补偿/revoke 行为（精确的远端 revoke-self + 本地 Keychain 清理 + 恢复先前 config）并报告证据。不要自行发明手动密钥处理。
- 如果验证码连续输错 5 次，会话被永久锁定；用 `herdr-mcp worker pair` 重新创建一个配对。

## 双设备 UAT

正式的双设备 GA/UAT 尚未通过。这是预期的 v0.4.3 行为，待发布/UAT 确认。
