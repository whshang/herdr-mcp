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

pub fn help(section: HelpSection, locale: Locale) -> String {
    if locale == Locale::En {
        return match section {
            HelpSection::General => crate::cli::help(),
            HelpSection::Worker => crate::cli::worker_help(),
            HelpSection::Connector => crate::cli::connector_help(),
            HelpSection::Automation => crate::cli::automation_help(),
            HelpSection::Instance => crate::cli::instance_help(),
            HelpSection::Qualification => crate::cli::qualification_help(),
            HelpSection::Continuity => crate::cli::continuity_help(),
            HelpSection::Memory => crate::cli::memory_help(),
            HelpSection::WebChat => crate::cli::webchat_help(),
        }
        .to_owned();
    }

    match section {
        HelpSection::General => general_help(locale),
        HelpSection::Worker => worker_help(locale),
        HelpSection::Connector => connector_help(locale),
        HelpSection::Automation => automation_help(locale),
        HelpSection::Instance => instance_help(locale),
        HelpSection::Qualification => qualification_help(locale),
        HelpSection::Continuity => continuity_help(locale),
        HelpSection::Memory => memory_help(locale),
        HelpSection::WebChat => webchat_help(locale),
    }
}

fn general_help(l: Locale) -> String {
    let mut out = String::new();
    out.push_str(l.text(
        "",
        "Herdr MCP 本机运行时\n\n",
        "Herdr MCP ネイティブランタイム\n\n",
    ));
    out.push_str(l.text("", "用户常用命令：\n", "ユーザー向けコマンド：\n"));
    out.push_str(
        "  herdr-mcp install\n  herdr-mcp status\n  herdr-mcp doctor\n  herdr-mcp permissions <status|setup [--upgrade-broker]|verify>\n  herdr-mcp scan [--json] [--refresh] [--probe]\n  herdr-mcp agent-skill <status|sync>\n  herdr-mcp continuity <search|resume> ...\n  herdr-mcp memory <resume|search> ...\n  herdr-mcp webchat <endpoints|resources|create|send|dispatch-status|archive|handoff> ...\n  herdr-mcp profile check --file <profile.json>\n  herdr-mcp instance list\n  herdr-mcp instance reap <name> --confirm\n  herdr-mcp qualification <lock|unlock|status>\n  herdr-mcp worker bootstrap\n  herdr-mcp worker pair [--ttl-seconds 600] [--name NAME] [--recover-device DEVICE_ID]\n  herdr-mcp worker connect <pairing-address> [--name NAME]\n  herdr-mcp worker update\n  herdr-mcp device list\n  herdr-mcp connector list\n  herdr-mcp connector approve <approval-request-id>\n  herdr-mcp connector revoke <connector-id> --confirm\n  herdr-mcp automation create --name NAME --device <device-id-or-unique-name>\n  herdr-mcp automation list\n  herdr-mcp update [check [--manifest URL]|apply [--manifest URL]|major-apply|major-rollback|auto|status]\n  herdr-mcp extension standalone <install [--ref REF] [--path PATH]|status>\n  herdr-mcp rollback\n  herdr-mcp reinstall\n  herdr-mcp uninstall\n  herdr-mcp lang [auto|en|zh|ja]\n\n",
    );
    out.push_str(l.text(
        "",
        "可在任意命令前使用 --lang <en|zh|ja> 临时覆盖语言；lang 命令保存偏好，auto 恢复系统语言自动探测。命令名、JSON 字段、状态码和错误码始终保持英文。\n\n",
        "任意のコマンドで --lang <en|zh|ja> を指定すると一時的に言語を上書きできます。lang コマンドは設定を保存し、auto はシステム言語の自動検出に戻します。コマンド名、JSON キー、状態コード、エラーコードは常に英語です。\n\n",
    ));
    out.push_str(l.text("", "同机 UAT 隔离：\n", "同一マシンでの UAT 分離：\n"));
    out.push_str(
        "  herdr-mcp --instance uat install\n  HERDR_MCP_INSTANCE=uat herdr-mcp doctor\n\n",
    );
    out.push_str(l.text("", "高级 / 内部命令：\n", "高度な操作 / 内部コマンド：\n"));
    out.push_str(
        "  herdr-mcp version\n  herdr-mcp config [path|show|init [--edge-origin https://host]|set-edge-origin https://host]\n  herdr-mcp service <install [--adopt-node]|status|start|stop|restart|rollback|uninstall>\n  herdr-mcp herdr-supervisor <install|status|enable|disable|start|stop|uninstall>\n  herdr-mcp link <status|run|install|uninstall>\n  herdr-mcp link cutover [--dry-run|--execute|--rollback]\n  herdr-mcp link seal [status|record --dual-uat|record --rollback-uat|adopt-existing-rust --ack --reason REASON|--dry-run|--execute]\n  herdr-mcp link migrate-runtime-control [--dry-run|--write-staging|--apply]\n  herdr-mcp tcc-broker <install [--force]|status|uninstall>\n  herdr-mcp native-host <install|status|uninstall|rollback>\n  herdr-mcp native-host dev <enable [PATH]|disable>\n  herdr-mcp native-host use <store|standalone|dev>\n  herdr-mcp extension-host [chrome-extension://.../]\n  herdr-mcp artifact import --url HTTPS_URL --path PROJECT_PATH [--sha256 HEX] [--capability-env NAME | --signed-url] [--overwrite] [--confirm-dirty] [--confirm-busy]\n  herdr-mcp dev [status]\n  herdr-mcp dev sync [--dry-run] [--allow-dirty]\n  herdr-mcp dev rollback\n  herdr-mcp candidate [--port 8873]\n",
    );
    out
}

fn worker_help(l: Locale) -> String {
    format!(
        "Herdr MCP — {}\n\n{}\n\n  herdr-mcp worker bootstrap\n  herdr-mcp worker update\n  herdr-mcp device list\n  herdr-mcp worker pair [--ttl-seconds 600] [--name NAME] [--recover-device DEVICE_ID]\n  herdr-mcp worker credential-repair prepare|apply|finalize\n  herdr-mcp worker connect <pairing-address> [--name NAME]\n  herdr-mcp device rename <name>\n  herdr-mcp device revoke <device-id> --confirm\n",
        l.text("", "设备 / Worker 管理", "デバイス / Worker 管理"),
        l.text(
            "",
            "bootstrap 仅用于创建首个 fleet；其他命令需要已注册设备。6 位配对码通过交互终端或 stdin 读取，绝不放进 argv。credential-repair 只传递校验值，设备 secret 不跨机器。",
            "bootstrap は最初の fleet の作成専用です。その他の操作には登録済みデバイスが必要です。6 桁コードは対話端末または stdin から読み取り、argv には含めません。credential-repair は検証値だけを渡し、デバイス secret を別マシンへ送信しません。",
        )
    )
}

fn connector_help(l: Locale) -> String {
    format!(
        "Herdr MCP — {}\n\n{}\n\n  herdr-mcp connector list [--all]\n  herdr-mcp connector approve <approval-request-id>\n  herdr-mcp connector cancel <approval-request-id>\n  herdr-mcp connector revoke <connector-id> --confirm\n  herdr-mcp connector revoke-client <client-id> --confirm\n  herdr-mcp connector planner-control list | <approve|revoke> <request-id> --confirm\n  herdr-mcp connector webchat-control <allow|deny> <connector-id> <device-id> <endpoint-ref> <provider> <account-ref> --confirm\n  herdr-mcp connector page-assist <allow|deny> <connector-id> <device-id> <endpoint-ref> --confirm\n",
        l.text("", "Connector 管理", "コネクター管理"),
        l.text(
            "",
            "这些命令需要已注册设备凭据。审批码从终端/stdin 读取。--all 包含 revoked 审计记录；revoke-client 会使该 OAuth client 的全部授权失效。",
            "登録済みデバイスの認証情報が必要です。承認コードは端末/stdin から読み取ります。--all は revoked の監査記録も含み、revoke-client はその OAuth client の全認可を無効化します。",
        )
    )
}

fn automation_help(l: Locale) -> String {
    format!(
        "Herdr MCP — {}\n\n{}\n\n  herdr-mcp automation create --name NAME --device <device-id-or-unique-name>\n  herdr-mcp automation list\n  herdr-mcp automation rotate <client-id> --confirm\n  herdr-mcp automation revoke <client-id> --confirm\n",
        l.text("", "自动化身份管理", "自動化プリンシパル管理"),
        l.text(
            "",
            "需要已注册设备。create 必须明确指定名称和目标设备，secret 只显示一次；rotate 立即使旧 secret 失效，revoke 立即生效。",
            "登録済みデバイスが必要です。create では名前と対象デバイスを明示し、secret は一度だけ表示されます。rotate は旧 secret を即時無効化し、revoke も即時に反映されます。",
        )
    )
}

fn instance_help(l: Locale) -> String {
    format!(
        "Herdr MCP — {}\n\n{}\n\n  herdr-mcp instance list\n  herdr-mcp instance reap <name> --confirm\n",
        l.text("", "验证实例", "検証インスタンス"),
        l.text(
            "",
            "默认生产实例只读；只有通过 ownership 检查的命名验证实例可以被 reap。",
            "既定の本番インスタンスは読み取り専用です。ownership 検証を通過した名前付き検証インスタンスだけを reap できます。",
        )
    )
}

fn qualification_help(l: Locale) -> String {
    format!(
        "Herdr MCP — {}\n\n{}\n\n  herdr-mcp qualification lock\n  herdr-mcp qualification status\n  herdr-mcp qualification unlock\n",
        l.text("", "发布验收 generation lock", "リリース検証 generation lock"),
        l.text(
            "",
            "lock 在验收期间阻止自动更新和 runtime generation 替换；status 只读；unlock 恢复正常 generation 变化。",
            "lock は検証中の自動更新と runtime generation の置換を防ぎます。status は読み取り専用、unlock で通常の generation 変更に戻ります。",
        )
    )
}

fn continuity_help(l: Locale) -> String {
    format!(
        "Herdr-MCP — {}\n\n  herdr-mcp continuity search <query> [--project-id ID] [--project-path PATH] [--workspace-id ID] [--limit N]\n  herdr-mcp continuity resume <continuity_id>\n\n{}\n",
        l.text("", "持久 Continuity", "永続 Continuity"),
        l.text(
            "",
            "search 只返回有界候选证据，不读取完整 journal。仅文本唯一仍是 confirmation_required；优先使用 project-path/repo identity 缩小范围。resume 按稳定 continuity_id 恢复权威 journal，随后仍需重新检查实时 workspace/Git/runtime。",
            "search は境界付きの候補証拠だけを返し、journal 全体は読みません。テキストだけの一意性は confirmation_required のままです。project-path/repo identity で範囲を絞ってください。resume は安定した continuity_id から権威ある journal を復元しますが、その後に現在の workspace/Git/runtime を再確認します。",
        )
    )
}

fn memory_help(l: Locale) -> String {
    format!(
        "Herdr-MCP — {}\n\n  herdr-mcp memory resume <project_ref> <repo_id> <work_chain_id> [--max-turns N]\n  herdr-mcp memory search <project_ref> <repo_id> <work_chain_id> <query> [--limit N]\n\n{}\n",
        l.text("", "有界 Work Memory", "境界付き Work Memory"),
        l.text(
            "",
            "Work Memory 必须使用精确 partition tuple。优先使用已安全选择的 Continuity candidate 返回的 work_memory locator，不要猜测 partition id。",
            "Work Memory には正確な partition tuple が必要です。安全に選択した Continuity candidate が返す work_memory locator を使い、partition id を推測しないでください。",
        )
    )
}

fn webchat_help(l: Locale) -> String {
    format!(
        "Herdr-MCP — {}\n\n  herdr-mcp webchat endpoints [--limit N]\n  herdr-mcp webchat resources [--endpoint-ref REF] [--provider PROVIDER] [--kind account|space|session] [--parent-ref REF] [--limit N]\n  herdr-mcp webchat inspect <resource_ref>\n  herdr-mcp webchat create ...\n  herdr-mcp webchat send ...\n  herdr-mcp webchat dispatch-status <dispatch_id>\n  herdr-mcp webchat archive ...\n  herdr-mcp webchat handoff --continuity-id HC --source-url URL ...\n\n{}\n",
        l.text("", "受支持的本机 WebChat 控制", "サポート対象のローカル WebChat 操作"),
        l.text(
            "",
            "先按 endpoints → resources → inspect 发现能力。Ref 是 opaque，不得自行构造；mutation 必须使用观察到的 generation 和稳定 idempotency key。handoff 复用同一 continuity_id；prepared packet 不等于已发送，只有 delivery_state=applied 才表示浏览器交付已发生。不确定交付禁止盲目重试。",
            "まず endpoints → resources → inspect で能力を確認します。Ref は opaque であり自作せず、mutation には観測済み generation と安定した idempotency key を使います。handoff は同じ continuity_id を再利用します。prepared packet は送信完了を意味せず、delivery_state=applied の場合だけブラウザーへの配信済みです。不確実な配信を盲目的に再試行しないでください。",
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_and_environment_language_tags_are_normalized() {
        assert_eq!(Locale::explicit("en"), Some(Locale::En));
        assert_eq!(Locale::explicit("zh"), Some(Locale::ZhCn));
        assert_eq!(Locale::explicit("zh-CN"), Some(Locale::ZhCn));
        assert_eq!(Locale::explicit("ja"), Some(Locale::Ja));
        assert_eq!(Locale::explicit("de"), None);
        assert_eq!(Locale::environment("zh_CN.UTF-8"), Some(Locale::ZhCn));
        assert_eq!(Locale::environment("ja_JP.UTF-8"), Some(Locale::Ja));
        assert_eq!(Locale::environment("en_US.UTF-8"), Some(Locale::En));
        assert_eq!(Locale::environment("C"), Some(Locale::En));
        assert_eq!(Locale::environment("fr_FR.UTF-8"), None);
    }

    #[test]
    fn language_precedence_is_explicit_env_saved_system_preferred_then_english() {
        assert_eq!(
            choose(
                Some(Locale::Ja),
                Some("zh"),
                Some("en"),
                &[Some("zh_CN.UTF-8")],
                || Some(Locale::En),
            ),
            Locale::Ja,
        );
        assert_eq!(
            choose(None, Some("zh"), Some("ja"), &[Some("en_US.UTF-8")], || {
                Some(Locale::En)
            }),
            Locale::ZhCn,
        );
        assert_eq!(
            choose(None, None, Some("ja"), &[Some("zh_CN.UTF-8")], || {
                Some(Locale::En)
            }),
            Locale::Ja,
        );
        assert_eq!(
            choose(None, None, None, &[Some("zh_CN.UTF-8")], || Some(
                Locale::Ja
            )),
            Locale::ZhCn,
        );
        assert_eq!(
            choose(None, None, None, &[], || Some(Locale::Ja)),
            Locale::Ja
        );
        assert_eq!(choose(None, None, None, &[], || None), Locale::En);
        assert_eq!(
            choose(None, None, None, &[Some("fr_FR.UTF-8")], || {
                Some(Locale::Ja)
            }),
            Locale::En,
        );
    }

    #[test]
    fn saved_preference_preserves_other_ui_keys_and_auto_removes_only_language() {
        let path = std::env::temp_dir().join(format!(
            "herdr-mcp-locale-test-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        fs::write(&path, r#"{"theme":"dark","lang":"en"}"#).unwrap();
        save_preference(&path, "ja").unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["lang"], "ja");

        save_preference(&path, "zh-CN").unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["lang"], "zh");

        save_preference(&path, "auto").unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["theme"], "dark");
        assert!(value.get("lang").is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn help_localizes_human_copy_without_translating_machine_syntax() {
        assert_eq!(help(HelpSection::General, Locale::En), crate::cli::help());
        let zh = help(HelpSection::General, Locale::ZhCn);
        let ja = help(HelpSection::General, Locale::Ja);
        assert!(zh.contains("用户常用命令"));
        assert!(ja.contains("ユーザー向けコマンド"));
        for text in [&zh, &ja] {
            assert!(text.contains("herdr-mcp continuity <search|resume>"));
            assert!(text.contains("herdr-mcp lang [auto|en|zh|ja]"));
            assert!(text.contains("HERDR_MCP_INSTANCE=uat"));
        }
        let ja_webchat = help(HelpSection::WebChat, Locale::Ja);
        assert!(ja_webchat.contains("delivery_state=applied"));
        assert!(ja_webchat.contains("idempotency key"));
        assert!(ja_webchat.contains("herdr-mcp webchat dispatch-status"));
    }

    #[test]
    fn apple_language_parser_accepts_supported_tags_only() {
        assert_eq!(
            parse_apple_languages("(\"ja-JP\", \"en-US\")"),
            Some(Locale::Ja)
        );
        assert_eq!(
            parse_apple_languages("(zh-Hans, en-US)"),
            Some(Locale::ZhCn)
        );
        assert_eq!(parse_apple_languages("(fr-FR, de-DE)"), None);
        assert_eq!(parse_apple_languages("missing domain AppleLanguages"), None);
    }
}
