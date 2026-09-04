# 多设备群控

*用一个 Herdr Worker 和一个 ChatGPT 连接管理多台已加入的电脑。*

Herdr 的多设备模型是：一个公网 Worker/Connector，后面连接多台拥有独立身份的电脑。ChatGPT 可以查看设备列表、为任务选择目标设备，并让后续操作继续绑定到同一台设备。新电脑通过短期配对加入现有 Worker，不会重新部署 Worker，也不会获得一份全局共享密钥。

> 安全配对目前使用 macOS Keychain 保存设备凭据，因此新设备配对流程当前仅支持 macOS。

## 在 ChatGPT 查看设备组

使用 `herdr_devices` 查看当前 Worker 中的设备。结果包含稳定设备身份，以及授权、连接、调度和健康状态。

推荐直接这样描述任务：

```text
列出我的 Herdr 设备和在线状态。后端任务使用 macbook-main，独立测试任务使用 macbook-lab；两边 working tree 保持隔离，完成后验证两台设备的结果再汇报。
```

路由规则保持保守：

- 明确指定设备时，操作只发往该设备；
- 后续引用和重试继续保持原设备身份；
- 只有一台设备可执行时，可以自动选择；
- 多台设备都可执行修改操作、但没有指定目标时，返回 `device_ambiguous`，不会自行猜测。

每台加入的电脑都有独立凭据和不可变 `device_id`。设备名称用于方便人阅读和选择，真实身份仍由 `device_id` 保持稳定。

## 把新电脑加入设备组

### 1. 从已登记设备创建短期配对

在任意一台已登记的电脑上运行：

```bash
herdr-mcp worker pair
```

`worker pair` 属于设备/操作员管理的 fleet 动作，需要凭据证明本机已经加入目标 Worker。它会在 Worker 控制面创建 pairing，不需要把动作路由到任何工作站。返回结果应把这些信息一起展示：

- 包含高熵 pairing id 的配对地址；
- 一次性 6 位验证码；
- 精确过期时间；
- 可直接复制到新电脑执行的 `herdr-mcp worker connect "<pairing-address>"` 命令。

正常最长有效期为 600 秒，应立即使用，不要把它当成长期邀请链接保存。绝不能在正在安装的全新电脑上运行 `worker pair` 来探测是否已有 fleet。如果这是第一套 Herdr Worker、还不存在任何已登记设备，先完成 Cloudflare Worker 初始化再配对。

### 2. 在新电脑上连接

新电脑上的 Agent 运行：

```bash
herdr-mcp worker connect "<pairing-address>"
```

随后 CLI 会要求输入 6 位验证码，输入的数字会正常显示，便于核对。验证码不会作为普通命令行参数传入，因此不会进入 shell history。

默认情况下，新加入电脑会自动使用 macOS 的**电脑名称（Computer Name）**作为 device display name。只有用户明确希望使用其他名字时，才传 `--name "<device-name>"`。如果创建配对时显式使用了 `worker pair --name ...`，它同样属于用户覆盖，并优先于新电脑自动读取的名称。

配对被消费后，`worker connect` 会自动安装/启动本机 `herdr-mcp` 服务，并确保当前设备对应的 Rust production Link 已创建并加载。只有本机 service 健康、`link-prod` 已由 managed runtime 持有且设备身份正确时命令才返回成功；启动失败会进入既有的远端 revoke、Keychain 清理和 config 恢复补偿流程。

使用 Agent 安装时，可以直接把这一句话发给新电脑上的 Coding Agent：

```text
把这台电脑加入我现有的 Herdr 设备组，请按照 https://github.com/whshang/herdr-mcp/blob/main/docs/i18n/zh-CN/existing-worker-connect.md 执行；配对地址是 <pairing-address>，等 CLI 提示时再让我输入 6 位验证码，完成后验证这台设备已经在同一个 Worker 中在线。
```

### 3. 验证新设备

连接成功后运行：

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
```

然后让 ChatGPT 调用 `herdr_devices`，确认新设备已经出现在同一个 Worker 中并处于在线状态。

以后只有用户明确要求改名时，才运行：

```bash
herdr-mcp worker rename "<new-device-name>"
```

`herdr-mcp device rename ...` 是等价别名。rename 只修改面向人的显示名称；不可变 `device_id`、workstation identity、设备凭据、授权和调度状态全部保持不变。Link 重连不会覆盖用户显式改过的名字。最初的 default/legacy workstation 在首次登记时也会自动记录本机 Computer Name。

如果要永久撤销另一台已登记设备的授权，在任意已登记工作站运行。先通过 `herdr_devices` 取得不可变的 `device_id`，然后：

```bash
herdr-mcp worker revoke "<device-id>" --confirm
```

设备/操作员负责 fleet 管理。这个动作绝不接受 display name，必须使用不可变 `device_id`。已批准的 WebChat Connector 只有普通 MCP 权限，不能 revoke Device。已登记设备之间不存在 owner/member 高下之分，都是同一操作员控制平面下的对等成员。

revoke 对该设备身份和凭据是永久操作：在线 Link 会立即断开，旧凭据以后不能再次连接；系统内部会保留最小 revoked tombstone 防止旧身份“复活”，但正常设备列表会隐藏这些 tombstone。以后若要重新加入这台电脑，需要重新生成配对并登记为新的设备身份。

## 配对实际做了什么

短期配对会换取新的单设备凭据。最终凭据写入 macOS Keychain，Worker 只保存验证该设备所需的 verifier；成功消费后，原配对立即失效。

新电脑不需要：

- Cloudflare 部署凭据；
- 新建 Worker 或 Durable Object；
- 新建 ChatGPT Connector/OAuth client；
- 复制旧的全局 `LINK_SHARED_SECRET`。

## 配对安全规则

- 6 位验证码单次使用且有效期很短；
- 连续输错 5 次会永久锁定本次配对，应重新创建配对；
- pairing id 具有高熵，并放在 URL fragment 中，避免进入普通 HTTP access log 路径；
- 最终单设备凭据不得打印或复制，应始终留在操作系统凭据存储中。

## 恢复与重试

修改操作如果返回交付状态不确定，应先读取当前状态，再决定是否重试。不要直接重复一个可能已经执行成功的操作。

如果服务器已经消费配对后连接失败，使用内置 compensation/revoke 机制并检查实际状态。只有确认旧配对已经无法继续后，再创建新的配对。
