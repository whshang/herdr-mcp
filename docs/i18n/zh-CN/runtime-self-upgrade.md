# Runtime A/B 自升级

Herdr MCP 在本机 MCP runtime generation 切换时，保持 ChatGPT 面向的 Edge 与工作站 `herdr-link` 稳定。Runtime A/B 是**同一 contract epoch 内**的机制；公开 contract epoch 变化使用独立的受控迁移流程。

## 边界

```text
ChatGPT
  -> stable Worker / OAuth / frozen public MCP contract
  -> persistent herdr-link
  -> active runtime generation
       A: http://127.0.0.1:8772/mcp
       B: http://127.0.0.1:8773/mcp
```

`herdr-link` 持有 active-generation pointer。单个 runtime 进程**不拥有**公网连接，因此切换或停止某个 runtime generation 不会终止 ChatGPT transport。

generation manager 不负责启动任意进程。candidate process 的创建属于部署/升级步骤；candidate 在 loopback endpoint 监听后，再由 manager 校验并切流。这样把 process execution 与 traffic activation 分开。

## 冻结 contract profile

Production 0.3.32 使用：

```bash
HERDR_MCP_CONTRACT_PROFILE=epoch2
```

Contract epoch 2：

- 精确暴露 **18 tools**；
- 包含只读 `herdr_skill`；
- 冻结哈希为 `sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8`；
- 与 `HERDR_MCP_ALL_TOOLS=1` 同时出现时 fail closed；
- candidate 激活前从真实 `tools/list` 计算并验证。

`epoch1` 只保留为历史 **17-tool 回滚/旧会话兼容 profile**，不再是当前 production 目标。

## 本机控制文件

Production 使用独立的本机文件：

```text
~/.config/herdr-mcp/runtime-control-prod.json
~/.config/herdr-mcp/runtime-status-prod.json
```

权限为 `0600`。Runtime bearer credential **不会**写入这两个文件；persistent link 继续持有已有的本机 MCP credential。

control document 包含：

- 单调递增的 `revision`；
- `desired_active` generation；
- 最多 8 个 loopback generation spec；
- 可选的 activation observation checks。

generation boundary 只接受 `http://127.0.0.0/8`、`localhost` 或 `::1` candidate。

## CLI

```bash
herdr-runtime-generation status
herdr-runtime-generation register --generation candidate-0.3.33-abc123 \
  --endpoint http://127.0.0.1:8773/mcp \
  --runtime-version 0.3.33
herdr-runtime-generation activate --generation candidate-0.3.33-abc123
herdr-runtime-generation rollback
herdr-runtime-generation remove --generation candidate-0.3.33-abc123
```

操作 production/canary 等非默认环境时，通过 `HERDR_RUNTIME_CONTROL_PATH` 和 `HERDR_RUNTIME_STATUS_PATH` 指定对应文件。

## Activation gate

active pointer 移动前，`herdr-link` 使用已有 runtime credential 验证 candidate：

1. loopback endpoint 可达；
2. health / discovery 成功；
3. 真实 `tools/list` 成功；
4. tool count 与 canonical SHA-256 contract hash 匹配**当前冻结 epoch**；
5. 可选的 expected runtime version 匹配；
6. 重复 observation check 持续健康。

失败的 candidate 永远不会成为 active。`bin/herdr-self-update` 使用同样原则：它要求现有 server plist 已经使用当前 public contract profile，并且**拒绝执行跨 epoch 迁移**。

## 切换与 drain 语义

Activation 在 generation pointer 上原子完成：

- 已经派发到 generation A 的请求继续固定在 A 上 drain；
- 新请求立即使用 generation B；
- stable WSS link 保持连接；
- Edge/OAuth/MCP URL 与 ChatGPT Connector 不变化。

Rollback 用相反方向的同一 pointer 机制。

## Edge runtime 状态

Heartbeat frame 携带 active runtime identity。Production Edge 消费该 identity，并在 version/generation 变化时持久化，transition 期间绕过普通 heartbeat 写入节流。

因此 `/status/<workstation>` 会在下一次 heartbeat 收敛到新 runtime，不需要重连 workstation link。正常 heartbeat 间隔为 15 秒。

## Contract epoch 迁移

公开 tool surface 的变化**不是** A/B runtime update。epoch 1 → epoch 2 的安全顺序是：

```text
1. 安装/重启本机 MCP server：0.3.32 + HERDR_MCP_CONTRACT_PROFILE=epoch2
   -> direct loopback tools/list 必须是 18 tools + epoch-2 hash
2. 发布 public Edge epoch 2
   -> public tools/list 变成 18 tools，并包含 herdr_skill
   -> 为保持回滚连续性，Edge 临时接受紧邻的 epoch-1 workstation link identity
3. 更新/重启 herdr-link：HERDR_CONTRACT_EPOCH=2 + epoch-2 hash
   -> /status/<workstation> 必须收敛到 epoch 2 / runtime 0.3.32
4. 用新的 ChatGPT conversation 验证拿到 18-tool snapshot
```

旧 epoch-1 catalog 继续以冻结源码和测试保留，作为 compatibility evidence；绝不能把它静默修改成 epoch 2 的样子。

## 安全规则

- 同一 contract epoch 下，绝不激活 contract hash 不同的 candidate。
- 绝不用 `herdr-self-update` 跨 contract epoch。
- 除非重新设计 transport boundary，否则 generation 只能绑定 loopback endpoint。
- old generation 的 in-flight 请求没有 drain 完前不能 kill。
- runtime bearer credential 不得进入 control/status file 或命令输出。
- 公开 tool-surface 变化属于 contract-epoch operation，不属于普通 runtime upgrade。
- 尽量把 Edge/Link upgrade 与 runtime-generation activation 分开执行。
