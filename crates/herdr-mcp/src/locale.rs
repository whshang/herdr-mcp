//! Native CLI language policy and human text. Machine codes remain English.
use crate::cli::HelpSection;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    ZhCn,
    Ja,
}

impl Locale {
    pub fn explicit(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "en" => Some(Self::En),
            "zh" | "zh-cn" => Some(Self::ZhCn),
            "ja" => Some(Self::Ja),
            _ => None,
        }
    }
    fn environment(raw: &str) -> Option<Self> {
        let normalized = raw.trim().to_ascii_lowercase().replace('_', "-");
        let base = normalized.split(['.', '@']).next()?;
        match base.split('-').next()? {
            "en" | "c" | "posix" => Some(Self::En),
            "zh" => Some(Self::ZhCn),
            "ja" => Some(Self::Ja),
            _ => None,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
            Self::Ja => "ja",
        }
    }
    pub fn text<'a>(self, en: &'a str, zh: &'a str, ja: &'a str) -> &'a str {
        match self {
            Self::En => en,
            Self::ZhCn => zh,
            Self::Ja => ja,
        }
    }
}

pub fn strip_lang_flag(args: &[String]) -> Result<(Option<Locale>, Vec<String>), String> {
    let mut locale = None;
    let mut rest = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let value = if arg == "--lang" {
            Some(
                iter.next()
                    .ok_or("--lang requires en, zh-CN or ja")?
                    .as_str(),
            )
        } else {
            arg.strip_prefix("--lang=")
        };
        if let Some(value) = value {
            if locale.is_some() {
                return Err("duplicate --lang flag".into());
            }
            locale =
                Some(Locale::explicit(value).ok_or("unsupported --lang; use en, zh-CN or ja")?);
        } else {
            rest.push(arg.clone());
        }
    }
    Ok((locale, rest))
}

pub fn preference_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("HERDR_MCP_UI_CFG").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".config/herdr-mcp/ui.json"))
        .ok_or_else(|| "HOME is not set".into())
}

fn stored_preference(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value["lang"].as_str().map(str::to_owned)
}

fn choose(
    explicit: Option<Locale>,
    env: Option<&str>,
    saved: Option<&str>,
    system: &[Option<&str>],
    preferred: impl FnOnce() -> Option<Locale>,
) -> Locale {
    explicit
        .or_else(|| {
            env.filter(|value| !value.trim().is_empty())
                .map(|value| Locale::explicit(value).unwrap_or(Locale::En))
        })
        .or_else(|| saved.and_then(Locale::explicit))
        .or_else(|| {
            // A non-empty POSIX locale is authoritative, including C and unsupported languages.
            system
                .iter()
                .flatten()
                .find(|s| !s.trim().is_empty())
                .map(|s| Locale::environment(s).unwrap_or(Locale::En))
        })
        .or_else(preferred)
        .unwrap_or(Locale::En)
}

#[cfg(any(target_os = "macos", test))]
fn parse_apple_languages(output: &str) -> Option<Locale> {
    let list = output.trim().strip_prefix('(')?.strip_suffix(')')?;
    list.split(',')
        .find_map(|item| Locale::environment(item.trim().trim_matches('"')))
}

#[cfg(any(target_os = "macos", test))]
fn read_preferred_language(command: &mut std::process::Command) -> Option<Locale> {
    use std::io::Read;
    use std::process::Stdio;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::child_process::configure_process_group(command);
    let mut child = command.spawn().ok()?;
    let status =
        match crate::child_process::wait_bounded(&mut child, std::time::Duration::from_millis(300))
        {
            Ok(Some(status)) => status,
            Ok(None) => return None,
            Err(_) => {
                crate::child_process::terminate_and_reap(&mut child);
                return None;
            }
        };
    if !status.success() {
        return None;
    }
    let mut output = String::new();
    child
        .stdout
        .take()?
        .take(8192)
        .read_to_string(&mut output)
        .ok()?;
    parse_apple_languages(&output)
}

fn preferred_language() -> Option<Locale> {
    #[cfg(target_os = "macos")]
    {
        read_preferred_language(std::process::Command::new("/usr/bin/defaults").args([
            "read",
            "-g",
            "AppleLanguages",
        ]))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

pub fn resolve(explicit: Option<Locale>) -> Locale {
    let env = std::env::var("HERDR_MCP_LANG").ok();
    let saved = preference_path()
        .ok()
        .and_then(|path| stored_preference(&path));
    let system = ["LC_ALL", "LC_MESSAGES", "LANG"].map(|key| std::env::var(key).ok());
    choose(
        explicit,
        env.as_deref(),
        saved.as_deref(),
        &system.each_ref().map(|v| v.as_deref()),
        preferred_language,
    )
}

pub fn save_preference(path: &Path, raw: &str) -> Result<(), String> {
    let mut value = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
            .map_err(|e| format!("cannot read UI preference: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("cannot read UI preference: {e}")),
    };
    let object = value
        .as_object_mut()
        .ok_or("UI preference must be a JSON object")?;
    if raw == "auto" {
        object.remove("lang");
    } else {
        let locale = Locale::explicit(raw).ok_or("unsupported language")?;
        // Legacy Bash accepts zh, not zh-CN.
        object.insert("lang".into(), locale.text("en", "zh", "ja").into());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(path, format!("{value}\n")).map_err(|e| format!("cannot write UI preference: {e}"))
}

pub fn label(locale: Locale, code: &str) -> &str {
    match code {
        "pass" | "healthy" => locale.text("OK", "正常", "正常"),
        "fail" => locale.text("Needs attention", "需要处理", "対処が必要"),
        "not_probed" => locale.text("Not verified", "未验证", "未検証"),
        "not_proven" => locale.text(
            "No known failure; remote access not verified",
            "未发现已知故障；远程访问尚未验证",
            "既知の障害なし；リモートアクセスは未検証",
        ),
        "configured" => locale.text("Configured", "已配置", "設定済み"),
        "unconfigured" => locale.text("Not configured", "未配置", "未設定"),
        "enabled" => locale.text("Enabled", "已启用", "有効"),
        "disabled" => locale.text("Disabled", "已关闭", "無効"),
        "not_applicable" => locale.text("Not applicable", "不适用", "対象外"),
        "not_installed" => locale.text("Not installed", "未安装", "未インストール"),
        "not_loaded" => locale.text("Not running", "未运行", "停止中"),
        "stable" => locale.text("Stable", "稳定版", "安定版"),
        "preview" => locale.text("Preview", "预览版", "プレビュー版"),
        "running" => locale.text("Running", "运行中", "稼働中"),
        "unknown" => locale.text("Unknown", "未知", "不明"),
        "overall" => locale.text("Overall", "整体状态", "全体の状態"),
        "service_health" => locale.text(
            "Service / runtime",
            "服务 / 运行时",
            "サービス / ランタイム",
        ),
        "herdr" => locale.text("Herdr connection", "Herdr 连接", "Herdr 接続"),
        "permissions" => locale.text("Platform permissions", "系统权限", "システム権限"),
        "browser_integration" | "browser" => {
            locale.text("Browser integration", "浏览器集成", "ブラウザー連携")
        }
        "local_ipc" => locale.text(
            "Local browser connection",
            "浏览器本地连接",
            "ブラウザーのローカル接続",
        ),
        "edge_reachable" => locale.text("Cloud reachability", "云端连接", "クラウド接続"),
        "oauth_metadata" => locale.text(
            "Cloud authorization setup",
            "云端授权配置",
            "クラウド認証設定",
        ),
        "mcp_surface" => locale.text(
            "Public MCP endpoint",
            "公共 MCP 端点",
            "公開 MCP エンドポイント",
        ),
        "authenticated_local_mcp" => locale.text(
            "Authenticated local MCP",
            "本地 MCP 认证",
            "ローカル MCP 認証",
        ),
        "authenticated_remote_mcp" => locale.text(
            "Authenticated remote MCP",
            "远程 MCP 认证",
            "リモート MCP 認証",
        ),
        "cloud_health" | "cloud" => locale.text("Cloud", "云端", "クラウド"),
        "link" => locale.text("Link", "Link", "Link"),
        "update_channel" => locale.text("Update channel", "更新通道", "更新チャネル"),
        "update_checks" => locale.text("Update checks", "更新检查", "更新確認"),
        "scheduler" => locale.text(
            "Automatic update scheduler",
            "自动更新调度",
            "自動更新スケジュール",
        ),
        _ => code,
    }
}

pub fn help(section: HelpSection, l: Locale) -> String {
    let mut out = format!(
        "Herdr MCP — {}\n\n",
        l.text("Command guide", "命令指南", "コマンドガイド")
    );
    let mut heading = |en, zh, ja| {
        out.push_str(l.text(en, zh, ja));
        out.push('\n');
    };
    match section {
        HelpSection::All => {
            return format!(
                "{}\n{}\n{}",
                help(HelpSection::General, l),
                help(HelpSection::Agent, l),
                help(HelpSection::Advanced, l)
            );
        }
        HelpSection::General => {
            heading("Setup & health", "安装与健康检查", "セットアップと診断");
            out.push_str("  herdr-mcp install\n  herdr-mcp status\n  herdr-mcp doctor\n  herdr-mcp permissions <status|setup|verify>\n");
            out.push_str(l.text("\nDevices\n", "\n设备\n", "\nデバイス\n"));
            out.push_str("  herdr-mcp worker bootstrap\n  herdr-mcp worker pair [--name NAME]\n  herdr-mcp worker connect <pairing-address> [--name NAME]\n  herdr-mcp device list\n");
            out.push_str(l.text("\nConnectors\n", "\n连接器\n", "\nコネクター\n"));
            out.push_str("  herdr-mcp connector list\n  herdr-mcp connector --help\n");
            out.push_str(l.text("\nAutomation\n", "\n自动化\n", "\n自動化\n"));
            out.push_str("  herdr-mcp automation --help\n");
            out.push_str(l.text("\nUpdates & repair\n", "\n更新与修复\n", "\n更新と修復\n"));
            out.push_str("  herdr-mcp update [check|apply|auto|status]\n  herdr-mcp extension standalone <install|status>\n  herdr-mcp rollback\n  herdr-mcp reinstall\n  herdr-mcp uninstall\n");
            out.push_str(l.text(
                "\nLanguage & more help\n",
                "\n语言与更多帮助\n",
                "\n言語とその他のヘルプ\n",
            ));
            out.push_str("  herdr-mcp lang [auto|en|zh-CN|ja]\n  herdr-mcp --lang <en|zh-CN|ja> <command>\n  herdr-mcp help agent\n  herdr-mcp help advanced\n  herdr-mcp --help-all\n");
            out.push_str(l.text("\nUse worker/connector/automation --help for arguments. On Linux, repair with install; remove with service uninstall. Product rollback/reinstall/uninstall are macOS-only.\n", "\n参数详见 worker/connector/automation --help。Linux 使用 install 修复、service uninstall 移除。产品级 rollback/reinstall/uninstall 仅支持 macOS。\n", "\n引数は worker/connector/automation --help を参照。Linux の修復は install、削除は service uninstall。製品の rollback/reinstall/uninstall は macOS 専用です。\n"));
        }
        HelpSection::Agent => {
            heading(
                "Agent & automation output",
                "Agent 与自动化输出",
                "Agent と自動化向け出力",
            );
            out.push_str(l.text(
                "Use compact JSON for machine decisions. Keys, status codes and error codes stay English. Agents must not parse localized human text. --details is for developer investigation.\n",
                "机器判断使用紧凑 JSON，字段名、状态码和错误码保持英文。Agent 不得解析本地化人类文本。--details 用于开发者排查。\n",
                "機械による判断には簡潔な JSON を使用します。キー、状態コード、エラーコードは英語で固定です。Agent は翻訳済みの人向けテキストを解析してはいけません。--details は開発者の調査用です。\n"));
            out.push_str("  herdr-mcp status --json\n  herdr-mcp doctor --json\n  herdr-mcp scan --json [--refresh] [--probe]\n  herdr-mcp device list\n");
            out.push_str(l.text(
                "Use device list for enrolled-device inventory. Doctor exit 0 means no known failure in probed layers; authenticated_remote_mcp=not_probed is not remote readiness. Verify with an authenticated MCP client.\n",
                "已注册设备清单使用 device list。doctor 退出码 0 表示已探测层无已知故障；authenticated_remote_mcp=not_probed 不代表远程就绪。请用已认证的 MCP 客户端验证。\n",
                "登録済みデバイスの一覧には device list を使います。doctor の終了コード 0 は検査済みレイヤーに既知の障害がないことを示します。authenticated_remote_mcp=not_probed はリモートの準備完了を意味しません。認証済み MCP クライアントで検証してください。\n"));
        }
        HelpSection::Worker => {
            heading("Devices / Worker", "设备 / Worker", "デバイス / Worker");
            out.push_str(l.text("Bootstrap creates the first fleet. Other commands require an enrolled device. Pairing codes are read from terminal/stdin, never argv.\n", "bootstrap 创建首个设备集群。其他命令需要已注册设备。配对码通过终端或标准输入读取，不传入命令参数。\n", "bootstrap は最初のフリートを作成します。他の操作には登録済みデバイスが必要です。ペアリングコードは端末または標準入力から読み取ります。\n"));
            out.push_str("  herdr-mcp worker bootstrap\n  herdr-mcp device list\n  herdr-mcp worker pair [--ttl-seconds 600] [--name NAME]\n  herdr-mcp worker connect <pairing-address> [--name NAME]\n  herdr-mcp device rename <name>\n  herdr-mcp device revoke <device-id> --confirm\n");
        }
        HelpSection::Connector => {
            heading("Connectors", "连接器", "コネクター");
            out.push_str(l.text("Requires an enrolled device. Approval codes are read from terminal/stdin. --all includes revoked records. cancel refuses requests with issued credentials; revoke-client invalidates every grant for the client.\n", "需要已注册设备。审批码通过终端或标准输入读取。--all 包含已撤销记录。cancel 拒绝已发放凭据的请求；revoke-client 撤销该客户端的全部授权。\n", "登録済みデバイスが必要です。承認コードは端末または標準入力から読み取ります。--all は失効済み記録も表示します。cancel は認証情報発行済みの要求を拒否し、revoke-client はクライアントの全認可を無効化します。\n"));
            out.push_str("  herdr-mcp connector list [--all]\n  herdr-mcp connector approve <approval-request-id>\n  herdr-mcp connector cancel <approval-request-id>\n  herdr-mcp connector revoke <connector-id> --confirm\n  herdr-mcp connector revoke-client <client-id> --confirm\n  herdr-mcp connector webchat-control <allow|deny> <connector-id> <device-id> <endpoint-ref> <provider> <account-ref> --confirm\n  herdr-mcp connector page-assist <allow|deny> <connector-id> <device-id> <endpoint-ref> --confirm\n");
        }
        HelpSection::Automation => {
            heading("Automation", "自动化", "自動化");
            out.push_str(l.text("Requires an enrolled device. create requires both name and target device. The secret is shown once. rotate invalidates the old secret immediately; revoke is immediate.\n", "需要已注册设备。create 必须指定名称和目标设备。密钥仅显示一次。rotate 立即使旧密钥失效；revoke 立即撤销。\n", "登録済みデバイスが必要です。create には名前と対象デバイスの両方が必要です。シークレットは一度だけ表示されます。rotate は旧シークレットを即時無効化し、revoke も即時に適用されます。\n"));
            out.push_str("  herdr-mcp automation create --name NAME --device <device-id-or-unique-name>\n  herdr-mcp automation list\n  herdr-mcp automation rotate <client-id> --confirm\n  herdr-mcp automation revoke <client-id> --confirm\n");
        }
        HelpSection::Instance => {
            heading("Validation instances", "验证实例", "検証インスタンス");
            out.push_str(l.text("The default production instance is read-only. Only named instances can be reaped after ownership checks.\n", "默认生产实例只读。仅允许清理通过归属检查的命名实例。\n", "既定の本番インスタンスは読み取り専用です。所有権確認後、名前付きインスタンスのみ削除できます。\n"));
            out.push_str("  herdr-mcp instance list\n  herdr-mcp instance reap <name> --confirm\n");
        }
        HelpSection::Qualification => {
            heading(
                "Qualification generation lock",
                "验收期间的版本锁",
                "検証期間のバージョンロック",
            );
            out.push_str(l.text("lock prevents automatic updates and generation replacement until unlock. status is read-only.\n", "lock 阻止自动更新及版本替换，unlock 后恢复。status 只读。\n", "lock は自動更新と世代の置換を防ぎ、unlock で解除します。status は読み取り専用です。\n"));
            out.push_str("  herdr-mcp qualification <lock|status|unlock>\n");
        }
        HelpSection::Advanced => {
            heading(
                "Advanced maintenance / UAT",
                "高级维护 / UAT",
                "高度な保守 / UAT",
            );
            out.push_str("  herdr-mcp --instance uat install\n  herdr-mcp --instance uat doctor [--details|--json]\n  herdr-mcp instance --help\n  herdr-mcp qualification --help\n  herdr-mcp config [path|show|init [--edge-origin https://host]|set-edge-origin https://host]\n  herdr-mcp service <install [--adopt-node]|status|start|stop|restart|rollback|uninstall>\n  herdr-mcp herdr-supervisor <install|status|enable|disable|start|stop|uninstall>\n  herdr-mcp link <status|run|install|uninstall>\n  herdr-mcp link cutover [--dry-run|--execute|--rollback]\n  herdr-mcp link seal [status|record --dual-uat|record --rollback-uat|--dry-run|--execute]\n  herdr-mcp link migrate-runtime-control [--dry-run|--write-staging|--apply]\n  herdr-mcp tcc-broker <install [--force]|status|uninstall>\n  herdr-mcp native-host <install|status|uninstall|rollback>\n  herdr-mcp native-host dev <enable [PATH]|disable>\n  herdr-mcp native-host use <store|standalone|dev>\n  herdr-mcp extension-host [chrome-extension://.../]\n  herdr-mcp artifact import --url HTTPS_URL --path PROJECT_PATH [--sha256 HEX] [--capability-env NAME|--signed-url] [--overwrite] [--confirm-dirty] [--confirm-busy]\n  herdr-mcp dev [status|sync [--dry-run] [--allow-dirty]|rollback]\n  herdr-mcp candidate [--port 8873]\n");
            out.push_str(l.text("\nNamed macOS instances isolate labels, ports and state; they never rewrite the user CLI. Run lifecycle mutations from an independent shell. Managed services execute runtime/current; installed generations are immutable. macOS Link run/install/uninstall manage the candidate only; Linux uses the production service manager. Cutover defaults to dry-run and requires HERDR_LINK_CUTOVER_I_UNDERSTAND=1 to execute/rollback. Seal execution requires HERDR_LINK_SEAL_I_UNDERSTAND=1; migration apply requires HERDR_LINK_MIGRATE_RUNTIME_CONTROL=1 and never mutates service ownership. Secrets must never appear in argv.\n",
"\nmacOS 命名实例隔离服务标签、端口和状态，不改写用户 CLI。生命周期操作须在独立 shell 执行。托管服务使用 runtime/current，已安装版本不可变。macOS Link run/install/uninstall 仅管理候选服务；Linux 使用生产服务管理器。cutover 默认 dry-run，执行或回滚须设置 HERDR_LINK_CUTOVER_I_UNDERSTAND=1。seal 执行须设置 HERDR_LINK_SEAL_I_UNDERSTAND=1；迁移 apply 须设置 HERDR_LINK_MIGRATE_RUNTIME_CONTROL=1，不改变服务归属。密钥不得出现在命令参数中。\n",
"\nmacOS の名前付きインスタンスはラベル、ポート、状態を分離し、ユーザー CLI を書き換えません。ライフサイクル操作は独立したシェルで実行します。管理サービスは runtime/current を使用し、インストール済み世代は不変です。macOS の Link run/install/uninstall は候補サービスのみを管理し、Linux は本番サービス管理機構を使用します。cutover は既定で dry-run、実行とロールバックには HERDR_LINK_CUTOVER_I_UNDERSTAND=1 が必要です。seal の実行には HERDR_LINK_SEAL_I_UNDERSTAND=1、移行 apply には HERDR_LINK_MIGRATE_RUNTIME_CONTROL=1 が必要で、サービスの所有権は変更しません。シークレットを引数に含めないでください。\n"));
        }
    }
    out
}

pub fn render_report(
    facts: &serde_json::Value,
    details: &[String],
    mode: crate::cli::OutputMode,
    language: Locale,
    doctor: bool,
) -> String {
    use crate::cli::OutputMode;
    if mode == OutputMode::Json {
        return format!("{facts}\n");
    }
    let mut output = format!(
        "Herdr MCP {}{}\n",
        facts["version"].as_str().unwrap_or("?"),
        if doctor {
            language.text(" — Health check", " — 健康检查", " — ヘルスチェック")
        } else {
            ""
        }
    );
    let keys: &[&str] = if doctor {
        &[
            "overall",
            "service_health",
            "herdr",
            "permissions",
            "browser_integration",
            "link",
            "cloud_health",
            "authenticated_local_mcp",
            "authenticated_remote_mcp",
        ]
    } else {
        &[
            "overall",
            "service_health",
            "herdr",
            "link",
            "cloud",
            "update_channel",
            "update_checks",
            "scheduler",
        ]
    };
    for key in keys {
        if let Some(code) = facts[*key].as_str() {
            if matches!(*key, "permissions" | "browser_integration") && code == "not_applicable" {
                continue;
            }
            output.push_str(&format!(
                "{}: {}\n",
                label(language, key),
                label(language, code)
            ));
        }
    }
    if doctor {
        let step = facts["next_step"].as_str().unwrap_or("verify_remote_mcp");
        output.push_str(language.text("Next: ", "下一步：", "次の操作: "));
        output.push_str(match step {
            "repair_local" => language.text(
                "Run herdr-mcp install, then herdr-mcp doctor --details.",
                "运行 herdr-mcp install，再运行 herdr-mcp doctor --details。",
                "herdr-mcp install の後に herdr-mcp doctor --details を実行してください。",
            ),
            "connect_herdr" => language.text(
                "Start Herdr, then run herdr-mcp doctor again.",
                "启动 Herdr，再运行 herdr-mcp doctor。",
                "Herdr を起動し、herdr-mcp doctor を再実行してください。",
            ),
            "permissions_setup" => language.text(
                "Run herdr-mcp permissions setup.",
                "运行 herdr-mcp permissions setup。",
                "herdr-mcp permissions setup を実行してください。",
            ),
            "inspect_details" => language.text(
                "Run herdr-mcp doctor --details to inspect the failed check.",
                "运行 herdr-mcp doctor --details 查看失败检查。",
                "herdr-mcp doctor --details で失敗した検査を確認してください。",
            ),
            _ => language.text(
                "Connect your MCP client and perform an authenticated remote tool call.",
                "连接 MCP 客户端并执行一次已认证的远程工具调用。",
                "MCP クライアントを接続し、認証付きリモートツール呼び出しを実行してください。",
            ),
        });
        output.push('\n');
    }
    if mode == OutputMode::Details {
        output.push('\n');
        output.push_str(&details.join("\n"));
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_precedence_normalization_and_legacy_preference() {
        assert_eq!(Locale::explicit("zh"), Some(Locale::ZhCn));
        assert_eq!(Locale::explicit("zh-CN"), Some(Locale::ZhCn));
        assert_eq!(Locale::environment("ja_JP.UTF-8"), Some(Locale::Ja));
        assert_eq!(Locale::environment("zh_CN.UTF-8"), Some(Locale::ZhCn));
        assert_eq!(
            choose(
                Some(Locale::Ja),
                Some("zh"),
                Some("en"),
                &[Some("en")],
                || panic!("unexpected OS lookup")
            ),
            Locale::Ja
        );
        assert_eq!(
            choose(None, Some("zh"), Some("ja"), &[Some("en")], || panic!(
                "unexpected OS lookup"
            )),
            Locale::ZhCn
        );
        assert_eq!(
            choose(None, Some("de"), Some("ja"), &[Some("ja_JP")], || panic!(
                "unexpected OS lookup"
            )),
            Locale::En
        );
        assert_eq!(
            choose(None, None, Some("ja"), &[Some("zh_CN")], || panic!(
                "unexpected OS lookup"
            )),
            Locale::Ja
        );
        assert_eq!(
            choose(
                None,
                None,
                None,
                &[Some(""), Some("ja_JP"), Some("en_US")],
                || panic!("unexpected OS lookup")
            ),
            Locale::Ja
        );
        assert_eq!(
            choose(
                None,
                None,
                None,
                &[Some("de_DE"), Some("ja_JP")],
                || panic!("unexpected OS lookup")
            ),
            Locale::En
        );
        assert_eq!(
            choose(None, None, None, &[Some("C")], || panic!(
                "unexpected OS lookup"
            )),
            Locale::En
        );
        assert_eq!(
            choose(None, None, None, &[None, Some(" ")], || Some(Locale::Ja)),
            Locale::Ja
        );
        assert_eq!(choose(None, None, None, &[], || None), Locale::En);
        assert_eq!(
            parse_apple_languages("(\n \"zh-Hans-CN\", en-US\n)"),
            Some(Locale::ZhCn)
        );
        assert_eq!(parse_apple_languages("(zh-Hant-TW)"), Some(Locale::ZhCn));
        assert_eq!(
            parse_apple_languages("(de-DE, ja-JP, en-US)"),
            Some(Locale::Ja)
        );
        assert_eq!(parse_apple_languages("(en-GB)"), Some(Locale::En));
        assert_eq!(parse_apple_languages("missing domain AppleLanguages"), None);
        assert_eq!(parse_apple_languages("()"), None);
        let path = std::env::temp_dir().join(format!("herdr-cli-ui-{}.json", std::process::id()));
        fs::write(&path, r#"{"theme":"dark","lang":"ja"}"#).unwrap();
        save_preference(&path, "zh-CN").unwrap();
        assert_eq!(stored_preference(&path).as_deref(), Some("zh"));
        save_preference(&path, "auto").unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(value.get("lang").is_none());
        assert_eq!(value["theme"], "dark");
        fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn preferred_language_probe_is_bounded_and_reaps_timeout() {
        use std::process::Command;
        assert_eq!(
            read_preferred_language(Command::new("/bin/sh").args(["-c", "printf '(ja-JP)'"])),
            Some(Locale::Ja)
        );
        let start = std::time::Instant::now();
        assert_eq!(
            read_preferred_language(Command::new("/bin/sh").args(["-c", "sleep 5"])),
            None
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn help_localizes_all_sections_and_keeps_maintenance_advanced() {
        for (l, heading) in [
            (Locale::En, "Setup & health"),
            (Locale::ZhCn, "安装与健康检查"),
            (Locale::Ja, "セットアップと診断"),
        ] {
            let text = help(HelpSection::General, l);
            assert!(text.contains(heading));
            for hidden in [
                "herdr-mcp scan",
                "--json",
                "--details",
                "Same-machine",
                "qualification",
                "instance reap",
                "link cutover",
                "link seal",
                "migrate-runtime-control",
                "tcc-broker",
                "native-host dev",
                "candidate",
            ] {
                assert!(!text.contains(hidden), "{hidden}");
            }
            for section in [
                HelpSection::Agent,
                HelpSection::Worker,
                HelpSection::Connector,
                HelpSection::Automation,
                HelpSection::Instance,
                HelpSection::Qualification,
                HelpSection::Advanced,
            ] {
                let text = help(section, l);
                assert!(text.contains("herdr-mcp "));
                if l != Locale::En {
                    assert_ne!(text, help(section, Locale::En));
                }
            }
            assert!(text.contains("help agent"));
            assert!(text.contains("help advanced"));
            let agent = help(HelpSection::Agent, l);
            for command in [
                "status --json",
                "doctor --json",
                "scan --json [--refresh] [--probe]",
                "device list",
            ] {
                assert!(agent.contains(command));
            }
            let all = help(HelpSection::All, l);
            assert!(all.contains(&text) && all.contains(&agent));
            let advanced = help(HelpSection::Advanced, l);
            assert!(advanced.contains("link cutover"));
            assert!(advanced.contains("HERDR_LINK_CUTOVER_I_UNDERSTAND=1"));
        }
    }
}
