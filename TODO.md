# herdr → 网页 浏览器插件 (herdr-mcp 补充)

方向: herdr agent 干完活 → 插件向绑定的网页会话输入框写入消息并提交 → 唤醒网页 AI。

## 阶段进度

- [x] 阶段 0: 读懂 ctmc (docs/ctmc-recon.md)
- [x] 阶段 1: 宿主侧 SSE 推送端点 (src/push.ts + server.ts 挂载 + SnapshotCache onEvent 钩子)
- [x] 阶段 2: 适配器两层 (Injectable 4 站 + SpeaksJSON 2 站)
- [x] 阶段 3: herdr agent ↔ 网页 tab/会话 绑定与恢复
- [x] 阶段 4: collect 清单 + 验证说明 (extension/README.md)

## 阶段 3 修正 (advisor 评审)

- 绑定 ≠ 立即唤醒: 初始 hello 只记基线不唤醒; working 才 armed; settle 仅当工作确实
  发生过才唤醒 (decideWake 纯函数在 extension/binding-core.js)。
- 过期强制生效: loadBindings 剪枝过期绑定 + 中止对应流 + 持久化清理。
- 配置变更全量重建流 (configReady 保证 onStartup 不用默认空 token 连)。
- 内容脚本版本上报 (h2w_hello) 补齐, 与 H2W_SCRIPT_VERSION 同步。
- claude MAIN-world 选择器: 无 id 时返回命中链选择器 (background 取最后一个可见匹配)。
- SpeaksJSON 投递确认改为提交前快照对比 (文本变化/块数增加)。
- tests/extension_smoke.mjs: manifest 引用完整 / 语法 / 状态机 / 解析单测。

## 决策记录

- (阶段 1) **鉴权**: 复用 `HERDR_MCP_TOKEN`(bearerAuth), 不加新 token。理由: 威胁模型与 /mcp 相同(token 持有者本就经 herdr_inspect 可读全部 agent 状态); 零新密钥管理; /mcp 未配 token 时本地裸跑, /push 保持一致。扩展从 background fetch 本地端点, 可带 Authorization 头(EventSource 不能带头)。
- (阶段 1) **SSE 事件源**: PushHub 挂 SnapshotCache 新增的 `onEvent` 钩子, 复用既有 events.subscribe 长连接, 不另开 socket。给 state.ts 的 buildSubscriptions 修复: `pane.agent_status_changed` 是 pane 作用域事件(实测 pane_id 省略时零事件), 原来只对 anchor pane 订阅 → 改为对所有 pane 订阅, 否则其他 agent 的状态迁移事件到不了缓存。
- (阶段 1) **事件过滤**: `?agent=` 匹配 pane.agent 字段(= agent kind, 如 pi, 不是自定义启动名)或 `?pane=` 精确 pane_id。服务端无状态, 绑定在扩展侧。
- (阶段 1) **错峰恢复**: hello 带权威 agent 快照(含 state_change_seq); 扩展用自己的去重(绑定 lastSettle.seq)决定是否唤醒。PushHub 每 10s 与 cache.agentViews() 对账, 事件流缺口自愈。
- (阶段 2) **两层适配器**: Injectable(输入框/写入/提交)4 站全实现; SpeaksJSON(JSON tool call 解析 + 回复完成判定, ctmc 移植)仅 z.ai/deepseek。claude.ai/chatgpt.com 只做唤醒。
- (阶段 2) **实测**: chatgpt.com 选择器 + MAIN world 写入经 ego-browser 实测确认(#prompt-textarea / button[data-testid=send-button] / execCommand 提交进 ProseMirror)。claude.ai 本机未登录, 选择器为推断链 + 标注待校准。
- (阶段 3) **绑定键**: `herdrWakeBindings[convKey]` 存 chrome.storage.local; convKey = origin+pathname(会话身份, ctmc 模式); tabId 非权威, 页面加载由 h2w_register 刷新; 浏览器重启 onStartup 重建推送流; hello 快照 + lastSettle.seq 去重补唤醒。

## 验证清单

- [x] 推送端点: node tests/push_sse.mjs + --integration (真实 agent working→done 触发 agent_settled)
- [x] 插件: 4 站适配器 + 绑定/恢复逻辑 (JS 语法检查 + chatgpt.com DOM 实测)
- [ ] 插件真机加载: chrome://extensions 手动加载 (见 extension/README.md)
- [ ] z.ai / deepseek 注入实测 (需真实登录会话)
