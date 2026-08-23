# Runtime A/B 自升级

Herdr MCP 让 ChatGPT 侧的 Edge 与工作站 `herdr-link` 保持稳定，同时在本机 MCP runtime 世代在 link 背后被替换。

## 边界

```text
ChatGPT
  -> stable Worker / OAuth / MCP contract
  -> persistent herdr-link
  -> active runtime generation
       A: http://127.0.0.1:8772/mcp
       B: http://127.0.0.1:8773/mcp
```

`herdr-link` 持有 active-generation 指针。runtime 进程**不**拥有公网连接，所以切换或停止一个 runtime 世代不会终止 ChatGPT transport。

世代管理器不会启动任意进程。候选进程的创建是部署/升级步骤；候选在 loopback endpoint 上监听后，管理器才验证并切换它。这让进程执行与流量激活分离。

## 冻结 contract profile

可以用更旧的 ChatGPT contract 在背后测试更新的 runtime，启动方式：

```bash
HERDR_MCP_CONTRACT_PROFILE=epoch1
```

对 contract epoch 1，该 profile：

- 只暴露冻结的 17 个工具；
- 隐藏 `herdr_skill`，直到将来刻意进入新的 contract epoch；
- 恢复 0.3.23 之后变化的模型可见元数据；
- 保留更新 runtime 的实现与行为；
- 与高级 all-tools 表面组合时 fail closed。

除非候选的真实 `tools/list` 哈希到精确的冻结 epoch-1 哈希，否则激活被阻止。

## 本机控制文件

生产使用独立的本地文件：

```text
~/.config/herdr-mcp/runtime-control-prod.json
~/.config/herdr-mcp/runtime-status-prod.json
```

权限为 `0600`。Runtime bearer 凭据**不**存在这两个文件里；持久 link 保留现有本地 MCP 凭据。

控制文档包含：

- 单调递增的 `revision`；
- `desired_active` 世代；
- 最多八个 loopback 世代规格；
- 可选的激活观察检查。

世代边界只接受 `http://127.0.0.0/8`、`localhost` 或 `::1` 候选。

## CLI

```bash
herdr-runtime-generation status
herdr-runtime-generation register --generation candidate-026 \
  --endpoint http://127.0.0.1:8773/mcp \
  --runtime-version 0.3.26
herdr-runtime-generation activate --generation candidate-026
herdr-runtime-generation rollback
herdr-runtime-generation remove --generation candidate-026
```

在操作非默认环境（如 production 或 canary）时设置 `HERDR_RUNTIME_CONTROL_PATH` 与 `HERDR_RUNTIME_STATUS_PATH`。

## 激活闸门

active 指针移动之前，`herdr-link` 通过它现有的 runtime 凭据验证候选：

1. loopback endpoint 可达；
2. health / discovery 成功；
3. 真实 `tools/list` 成功；
4. 工具数与规范 SHA-256 contract 哈希匹配冻结 contract；
5. 可选的期望 runtime 版本匹配；
6. 重复观察检查保持健康。

失败的候选永远不会成为 active。

## 切换与 drain 语义

激活在世代指针上是原子的：

- 已经派发给世代 A 的请求仍钉在 A 并在那里 drain；
- 新请求立刻使用世代 B；
- 稳定 WSS link 保持连接；
- Edge/OAuth/MCP URL 与 ChatGPT Connector 不变。

Rollback 用同一个指针机制反向进行。

## Edge runtime 状态

心跳帧已经携带 active runtime identity。生产 Edge 消费该 identity，并在 version/generation 变化时持久化它，绕过该 transition 的普通心跳写节流。

因此 `/status/<workstation>` 在下一个心跳时收敛到新 runtime，无需重连工作站 link。普通心跳间隔为 15 秒。

## 生产证据 — 2026-08-23

现有 `prod-real-runtime` link 首先完成了一次真实往返，没有改动公网 Connector：

```text
0.3.23 / stable-023
  -> 0.3.26 / candidate-026
  -> 0.3.23 / stable-023
```

那次证明之后，生产被永久提升到更新的实现，公网 ABI 不变：

```text
0.3.23 / stable-023 @ 8772
  -> temporary 0.3.26 / candidate-026 @ 8773
  -> restart 8772 with 0.3.26 + HERDR_MCP_CONTRACT_PROFILE=epoch1
  -> 0.3.26 / stable-026 @ 8772
```

生产现在停在 `stable-026`、端口 8772、runtime 0.3.26。临时 8773 进程与旧世代条目被移除。transition 候选与最终 stable runtime 都恰好暴露 17 个工具，contract epoch 1，哈希：

```text
sha256:3f23083ae31b977dad21b1ec9d6919c49e1067a27f7b7eea7bdd021b54770c0d
```

从同一个 ChatGPT 会话发起的 `herdr_inspect` 观察到版本变化，无需重配 Connector。生产 Edge 心跳状态与 active generation 收敛，浏览器扩展冒烟套件在 `127.0.0.1:8772` 的最终 0.3.26 server 上通过。

## 安全规则

- 同一 contract epoch 下，绝不激活 contract 哈希不同的候选。
- 除非有意重新设计 transport 边界，绝不把世代绑到非 loopback endpoint。
- 旧世代的在飞请求计数 drain 完之前不要杀掉它。
- 不要把 runtime bearer 凭据写进 control/status 文件或命令输出。
- 公网工具面变化是 contract-epoch 操作，不是 runtime 升级。
- 尽量把 Edge 与 Link 升级和 runtime 世代激活分开。