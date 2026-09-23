use crate::cli::{SemanticCommand, SemanticQuestionSpec, SemanticState};
use crate::config::{Config, SemanticRouteConfig};
use crate::locale::Locale;
use crate::paths::RuntimePaths;
use serde_json::{Map, Value, json};
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

const RECOMMENDED_NAME: &str = "typesafe";
const RECOMMENDED_PROTOCOL: &str = "decision";
const RECOMMENDED_URL: &str = "https://api.typesafe.ai/v1/systemone";
const RECOMMENDED_MODEL: &str = "jev-latest";

pub(crate) fn run(command: SemanticCommand, language: Locale) -> Result<ExitCode, String> {
    match command {
        SemanticCommand::Status { json } => run_status(language, json),
        SemanticCommand::Setup {
            name,
            protocol,
            url,
            model,
        } => run_setup(language, name, protocol, url, model),
        SemanticCommand::Remove { name } => run_remove(language, &name),
        SemanticCommand::Evaluate {
            state,
            question,
            spec,
            json,
        } => run_evaluate(language, state, question, spec, json),
    }
}

pub(crate) fn print_install_hint(paths: &RuntimePaths, language: Locale) {
    if paths.instance.is_named() {
        return;
    }
    let Ok(config) = Config::load_for_instance(&paths.config_file, &paths.instance) else {
        return;
    };
    if !config.semantic.routes.is_empty() {
        return;
    }

    eprintln!();
    eprintln!(
        "{}",
        language.text(
            "Optional: add a fast semantic decision model for planning and automation.",
            "可选：配置快速 decision 模型，可提升规划和自动化中的语义判断。",
            "任意：高速 decision モデルを追加すると、計画や自動化のセマンティック判断を改善できます。"
        )
    );
    eprintln!(
        "{}",
        language.text(
            "Recommended reference: TypeSafe.ai. Herdr also accepts other compatible decision providers.",
            "默认推荐参考 TypeSafe.ai；Herdr 同样支持其他兼容的 decision Provider。",
            "推奨例は TypeSafe.ai です。Herdr は他の互換 decision Provider も利用できます。"
        )
    );
    eprintln!("  herdr-mcp semantic setup");
    eprintln!(
        "{}",
        language.text(
            "Skip this safely if you do not need semantic acceleration.",
            "暂时不需要语义加速可以直接跳过，不影响 Herdr 核心功能。",
            "セマンティック高速化が不要なら安全にスキップできます。Herdr の基本機能には影響しません。"
        )
    );
}

fn run_status(language: Locale, as_json: bool) -> Result<ExitCode, String> {
    let paths = RuntimePaths::discover()?;
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let capability = crate::semantic::SemanticService::from_config().capability_json();
    let routes = config
        .semantic
        .routes
        .iter()
        .map(|route| {
            json!({
                "name": route.name,
                "protocol": route.protocol,
                "url": route.url,
                "model": route.model,
                "credential": if route.api_key.is_some() { "configured" } else { "missing" },
            })
        })
        .collect::<Vec<_>>();

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "config": paths.config_file,
                "routes": routes,
                "capability": capability,
                "policy": "advisory_only",
            }))
            .map_err(|error| format!("cannot render semantic status: {error}"))?
        );
        return Ok(ExitCode::SUCCESS);
    }

    let configured = capability["configured"].as_bool().unwrap_or(false);
    let degraded = capability["provider_state"].as_str() == Some("degraded");
    println!(
        "{}: {}",
        language.text("Semantic decisions", "语义判断", "セマンティック判断"),
        if !configured {
            language.text(
                "optional / not configured",
                "可选 / 未配置",
                "任意 / 未設定",
            )
        } else if degraded {
            language.text("degraded", "降级", "DEGRADED")
        } else {
            language.text("ready", "就绪", "READY")
        }
    );
    println!(
        "{}: {}",
        language.text("Policy", "策略", "ポリシー"),
        language.text(
            "advisory only; deterministic checks remain authoritative",
            "仅辅助判断；确定性检查仍保持权威",
            "補助判断のみ。決定論的チェックが引き続き権威"
        )
    );
    println!(
        "{}: {}",
        language.text("Typed decisions", "Typed decision", "Typed decision"),
        if capability["evaluate_available"].as_bool().unwrap_or(false) {
            language.text("available", "可用", "利用可能")
        } else {
            language.text("not configured", "未配置", "未設定")
        }
    );
    println!(
        "{}: {}",
        language.text("Chat fallback", "Chat fallback", "Chat fallback"),
        if capability["chat_available"].as_bool().unwrap_or(false) {
            language.text("available", "可用", "利用可能")
        } else {
            language.text("not configured", "未配置", "未設定")
        }
    );

    if config.semantic.routes.is_empty() {
        println!(
            "{}",
            language.text(
                "No local routes. Run herdr-mcp semantic setup for the recommended TypeSafe.ai reference, or pass a compatible protocol/URL/model.",
                "没有本机 route。运行 herdr-mcp semantic setup 可使用推荐的 TypeSafe.ai 参考配置，也可以传入兼容的 protocol / URL / model。",
                "ローカル route はありません。herdr-mcp semantic setup で推奨 TypeSafe.ai 例を使うか、互換 protocol / URL / model を指定できます。"
            )
        );
    } else {
        println!(
            "{}",
            language.text("Local routes:", "本机 routes：", "ローカル routes:")
        );
        for route in &config.semantic.routes {
            println!(
                "  {} · {} · {} · {}",
                route.name,
                route.protocol,
                route.model.as_deref().unwrap_or("model missing"),
                route.url.as_deref().unwrap_or("URL missing")
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_setup(
    language: Locale,
    name: Option<String>,
    protocol: Option<String>,
    url: Option<String>,
    model: Option<String>,
) -> Result<ExitCode, String> {
    let (name, protocol, url, model) = resolve_setup_route(name, protocol, url, model)?;

    eprintln!(
        "{}",
        language.text(
            "Semantic route setup. TypeSafe.ai is the recommended reference, not a required provider.",
            "配置 semantic route。TypeSafe.ai 是默认推荐参考，并非必选 Provider。",
            "Semantic route を設定します。TypeSafe.ai は推奨例であり、必須 Provider ではありません。"
        )
    );
    eprintln!("  name: {name}");
    eprintln!("  protocol: {protocol}");
    eprintln!("  url: {url}");
    eprintln!("  model: {model}");
    if name == RECOMMENDED_NAME
        && protocol == RECOMMENDED_PROTOCOL
        && url == RECOMMENDED_URL
        && model == RECOMMENDED_MODEL
    {
        eprintln!("  TypeSafe.ai: https://typesafe.ai/");
    }

    let api_key = read_secret(language.text(
        "API key (hidden): ",
        "API Key（隐藏输入）：",
        "API Key（非表示入力）：",
    ))?;
    if api_key.trim().is_empty() {
        return Err("API key cannot be empty".to_owned());
    }

    let route = SemanticRouteConfig {
        name: name.clone(),
        protocol: protocol.clone(),
        url: Some(url),
        model: Some(model),
        api_key: Some(api_key),
    };
    validate_route_shape(&route)?;

    let verified = if matches!(protocol.as_str(), "decision" | "decision-vercel") {
        let probe = probe_route(&route);
        if probe["ok"].as_bool() != Some(true) {
            let code = probe["code"].as_str().unwrap_or("provider_error");
            eprintln!(
                "{}",
                language.text(
                    "Provider verification failed; configuration was not changed.",
                    "Provider 验证失败；现有配置没有修改。",
                    "Provider 検証に失敗しました。既存設定は変更していません。"
                )
            );
            eprintln!("  code: {code}");
            return Ok(ExitCode::from(2));
        }
        true
    } else {
        false
    };

    let paths = RuntimePaths::discover()?;
    let mut config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    upsert_route(&mut config, route);
    config.save(&paths.config_file)?;

    println!(
        "{}",
        if verified {
            language.text(
                "Semantic route verified, saved, and ready.",
                "Semantic route 已验证、保存并可用。",
                "Semantic route を検証して保存しました。",
            )
        } else {
            language.text(
                "Semantic route saved.",
                "Semantic route 已保存。",
                "Semantic route を保存しました。",
            )
        }
    );
    println!("  {name} · {protocol}");
    println!("  herdr-mcp semantic status");
    Ok(ExitCode::SUCCESS)
}

fn resolve_setup_route(
    name: Option<String>,
    protocol: Option<String>,
    url: Option<String>,
    model: Option<String>,
) -> Result<(String, String, String, String), String> {
    match (name, protocol, url, model) {
        (None, None, None, None) => Ok((
            RECOMMENDED_NAME.to_owned(),
            RECOMMENDED_PROTOCOL.to_owned(),
            RECOMMENDED_URL.to_owned(),
            RECOMMENDED_MODEL.to_owned(),
        )),
        (Some(name), Some(protocol), Some(url), Some(model)) => {
            Ok((name, protocol, url, model))
        }
        _ => Err(
            "custom semantic setup requires --name, --protocol, --url, and --model together; use no route options for the recommended TypeSafe.ai reference"
                .to_owned(),
        ),
    }
}

fn run_remove(language: Locale, name: &str) -> Result<ExitCode, String> {
    let paths = RuntimePaths::discover()?;
    let mut config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    let before = config.semantic.routes.len();
    config.semantic.routes.retain(|route| route.name != name);
    if config.semantic.routes.len() == before {
        return Err(format!("semantic route '{name}' was not found"));
    }
    config.save(&paths.config_file)?;
    println!(
        "{}",
        language.text(
            "Semantic route removed.",
            "Semantic route 已移除。",
            "Semantic route を削除しました。"
        )
    );
    println!("  {name}");
    Ok(ExitCode::SUCCESS)
}

fn run_evaluate(
    language: Locale,
    state: SemanticState,
    question: String,
    spec: SemanticQuestionSpec,
    as_json: bool,
) -> Result<ExitCode, String> {
    let state = match state {
        SemanticState::Text(text) => json!({"text": text}),
        SemanticState::Json(raw) => serde_json::from_str::<Value>(&raw)
            .map_err(|error| format!("invalid --state-json: {error}"))?,
    };

    let question_value = match spec {
        SemanticQuestionSpec::Decide { yes, no } => json!({
            "type": "noul",
            "instructions": question,
            "criteria": {"true": yes, "false": no},
        }),
        SemanticQuestionSpec::Choose { options } => {
            let mut criteria = Map::new();
            for option in options {
                let (key, description) = option
                    .split_once('=')
                    .map(|(key, description)| (key.trim(), Some(description.trim())))
                    .unwrap_or((option.trim(), None));
                if key.is_empty() || criteria.contains_key(key) {
                    return Err("choice option keys must be non-empty and unique".to_owned());
                }
                criteria.insert(
                    key.to_owned(),
                    description.map_or(Value::Null, |value| json!(value)),
                );
            }
            json!({
                "type": "choice",
                "instructions": question,
                "criteria": criteria,
            })
        }
        SemanticQuestionSpec::Score { criteria } => json!({
            "type": "score",
            "instructions": question,
            "criteria": criteria,
        }),
    };
    let payload = json!({
        "state": state,
        "questions": {"decision": question_value},
    });
    let result = crate::semantic::extension_evaluate_json(&payload);

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result)
                .map_err(|error| format!("cannot render semantic result: {error}"))?
        );
    } else {
        print_human_result(language, &result);
    }

    Ok(if result["ok"].as_bool() == Some(true) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

fn print_human_result(language: Locale, result: &Value) {
    if result["ok"].as_bool() != Some(true) {
        println!(
            "{}: {}",
            language.text("Decision unavailable", "判断不可用", "判断を利用できません"),
            result["code"].as_str().unwrap_or("unknown")
        );
        println!(
            "{}",
            language.text(
                "Herdr keeps the deterministic path unchanged.",
                "Herdr 会继续使用原有确定性路径。",
                "Herdr は決定論的な経路をそのまま使用します。"
            )
        );
        return;
    }

    let answer = &result["answers"]["decision"];
    match answer["type"].as_str() {
        Some("noul") => {
            println!(
                "{}: {}",
                language.text("Decision", "判断", "判断"),
                answer["result"].as_str().unwrap_or("uncertain")
            );
            if let Some(probability) = answer["noul"].as_f64() {
                println!(
                    "{}: {:.1}%",
                    language.text("True probability", "True 概率", "True 確率"),
                    probability * 100.0
                );
            }
        }
        Some("choice") => {
            println!(
                "{}: {}",
                language.text("Choice", "选择", "選択"),
                answer["choice"].as_str().unwrap_or("unknown")
            );
            if let Some(confidence) = answer["confidence"].as_f64() {
                println!(
                    "{}: {:.1}%",
                    language.text("Confidence", "置信度", "信頼度"),
                    confidence * 100.0
                );
            }
        }
        Some("score") => {
            println!(
                "{}: {:.3}",
                language.text("Score", "评分", "スコア"),
                answer["score"].as_f64().unwrap_or_default()
            );
            if let Some(confidence) = answer["confidence"].as_f64() {
                println!(
                    "{}: {:.1}%",
                    language.text("Confidence", "置信度", "信頼度"),
                    confidence * 100.0
                );
            }
        }
        _ => println!(
            "{}",
            language.text(
                "No decision returned.",
                "没有返回判断结果。",
                "判断結果が返されませんでした。"
            )
        ),
    }
    println!(
        "{}: {}",
        language.text("Route", "Route", "Route"),
        result["provider"].as_str().unwrap_or("unknown")
    );
    println!(
        "{}: {}",
        language.text("Model", "模型", "モデル"),
        result["model"].as_str().unwrap_or("unknown")
    );
}

fn probe_route(route: &SemanticRouteConfig) -> Value {
    crate::semantic::evaluate_decision_route_json(
        route,
        &json!({
            "state": {"purpose": "herdr-mcp semantic setup verification"},
            "questions": {
                "decision": {
                    "type": "noul",
                    "instructions": "Is this a semantic provider connectivity verification request?",
                    "criteria": {
                        "true": "This is a provider connectivity verification request.",
                        "false": "This is not a provider connectivity verification request."
                    }
                }
            }
        }),
    )
}

fn validate_route_shape(route: &SemanticRouteConfig) -> Result<(), String> {
    if route.name.is_empty()
        || route.name.len() > 64
        || !route
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("semantic route name must use only letters, digits, '_' or '-'".to_owned());
    }
    if !matches!(
        route.protocol.as_str(),
        "decision" | "decision-vercel" | "openai-chat"
    ) {
        return Err(
            "semantic protocol must be decision, decision-vercel, or openai-chat".to_owned(),
        );
    }

    let url = route.url.as_deref().unwrap_or_default();
    let parsed = url::Url::parse(url).map_err(|error| format!("invalid semantic URL: {error}"))?;
    let secure = parsed.scheme() == "https";
    let loopback = parsed.scheme() == "http"
        && parsed
            .host_str()
            .is_some_and(|host| matches!(host, "127.0.0.1" | "::1" | "localhost"));
    if (!secure && !loopback)
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(
            "semantic URL must be HTTPS (or loopback HTTP) without credentials, query, or fragment"
                .to_owned(),
        );
    }
    if route
        .model
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        return Err("semantic model cannot be empty".to_owned());
    }
    Ok(())
}

fn upsert_route(config: &mut Config, route: SemanticRouteConfig) {
    if let Some(existing) = config
        .semantic
        .routes
        .iter_mut()
        .find(|existing| existing.name == route.name)
    {
        *existing = route;
    } else {
        config.semantic.routes.push(route);
    }
}

fn read_secret(prompt: &str) -> Result<String, String> {
    if !io::stdin().is_terminal() {
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .map_err(|error| format!("cannot read API key from stdin: {error}"))?;
        return Ok(line.trim_end_matches(&['\r', '\n'][..]).to_owned());
    }
    read_secret_terminal(prompt)
}

#[cfg(unix)]
fn read_secret_terminal(prompt: &str) -> Result<String, String> {
    use std::os::fd::AsRawFd;

    let stdin = io::stdin();
    let fd = stdin.as_raw_fd();
    let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
    if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
        return Err("cannot read terminal settings for hidden API key input".to_owned());
    }
    let mut hidden = original;
    hidden.c_lflag &= !libc::ECHO;
    eprint!("{prompt}");
    io::stderr().flush().map_err(|error| error.to_string())?;
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &hidden) } != 0 {
        return Err("cannot disable terminal echo for API key input".to_owned());
    }

    let mut line = String::new();
    let read_result = stdin
        .read_line(&mut line)
        .map_err(|error| format!("cannot read API key: {error}"));
    let restore_result = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) };
    eprintln!();
    if restore_result != 0 {
        return Err("cannot restore terminal echo after API key input".to_owned());
    }
    read_result?;
    Ok(line.trim_end_matches(&['\r', '\n'][..]).to_owned())
}

#[cfg(windows)]
fn read_secret_terminal(prompt: &str) -> Result<String, String> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
    };

    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err("cannot access console for hidden API key input".to_owned());
    }

    let mut original = 0u32;
    if unsafe { GetConsoleMode(handle, &mut original) } == 0 {
        return Err("cannot read console settings for hidden API key input".to_owned());
    }
    eprint!("{prompt}");
    io::stderr().flush().map_err(|error| error.to_string())?;
    if unsafe { SetConsoleMode(handle, original & !ENABLE_ECHO_INPUT) } == 0 {
        return Err("cannot disable console echo for API key input".to_owned());
    }

    let mut line = String::new();
    let read_result = io::stdin()
        .read_line(&mut line)
        .map_err(|error| format!("cannot read API key: {error}"));
    let restore_result = unsafe { SetConsoleMode(handle, original) };
    eprintln!();
    if restore_result == 0 {
        return Err("cannot restore console echo after API key input".to_owned());
    }
    read_result?;
    Ok(line.trim_end_matches(&['\r', '\n'][..]).to_owned())
}

#[cfg(not(any(unix, windows)))]
fn read_secret_terminal(_prompt: &str) -> Result<String, String> {
    Err(
        "hidden API key input is unsupported on this platform; provide one line on stdin"
            .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_route_upsert_and_remove_preserve_other_config() {
        let mut config = Config::default();
        let route = SemanticRouteConfig {
            name: "fast".to_owned(),
            protocol: "decision".to_owned(),
            url: Some("https://example.com/v1/decision".to_owned()),
            model: Some("model-a".to_owned()),
            api_key: Some("secret".to_owned()),
        };
        upsert_route(&mut config, route.clone());
        assert_eq!(config.semantic.routes, vec![route.clone()]);

        let replacement = SemanticRouteConfig {
            model: Some("model-b".to_owned()),
            ..route
        };
        upsert_route(&mut config, replacement.clone());
        assert_eq!(config.semantic.routes, vec![replacement]);

        config.semantic.routes.retain(|route| route.name != "fast");
        assert!(config.semantic.routes.is_empty());
        assert_eq!(config.runtime_port, crate::config::DEFAULT_RUNTIME_PORT);

        assert_eq!(
            resolve_setup_route(None, None, None, None).unwrap(),
            (
                RECOMMENDED_NAME.to_owned(),
                RECOMMENDED_PROTOCOL.to_owned(),
                RECOMMENDED_URL.to_owned(),
                RECOMMENDED_MODEL.to_owned(),
            )
        );
        assert!(resolve_setup_route(None, Some("openai-chat".to_owned()), None, None).is_err());
        assert_eq!(
            resolve_setup_route(
                Some("custom".to_owned()),
                Some("decision".to_owned()),
                Some("https://example.com/v1/decision".to_owned()),
                Some("model-c".to_owned()),
            )
            .unwrap(),
            (
                "custom".to_owned(),
                "decision".to_owned(),
                "https://example.com/v1/decision".to_owned(),
                "model-c".to_owned(),
            )
        );
    }
}
