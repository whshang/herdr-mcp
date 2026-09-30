//! WebChat CLI parsing and help ownership.
//!
//! Extracted verbatim from `cli.rs`. The public `Command`, `HelpSection`, and
//! `WebChatCommand` enums and the shared numeric flag helper stay in the parent
//! module; this module owns WebChat argv acceptance, error strings, bounds,
//! defaults, and help text for `herdr-mcp webchat`.

use super::{Command, HelpSection, WebChatCommand, parse_bounded_usize};

pub(super) fn parse_webchat(args: &[String]) -> Result<Command, String> {
    match args.first().map(String::as_str) {
        Some("help" | "--help" | "-h") if args.len() == 1 => Ok(Command::Help {
            section: HelpSection::WebChat,
        }),
        Some("endpoints") => {
            let limit = parse_optional_limit_flag(&args[1..], 32)?;
            Ok(Command::WebChat(WebChatCommand::Endpoints { limit }))
        }
        Some("resources") => parse_webchat_resources(&args[1..]),
        Some("inspect") => {
            if args.len() != 2 || args[1].is_empty() {
                return Err("webchat inspect requires <resource_ref>".to_owned());
            }
            Ok(Command::WebChat(WebChatCommand::Inspect {
                resource_ref: args[1].clone(),
            }))
        }
        Some("create") => parse_webchat_create(&args[1..]),
        Some("send") => parse_webchat_send(&args[1..]),
        Some("dispatch-status") => {
            if args.len() != 2 || args[1].is_empty() {
                return Err("webchat dispatch-status requires <dispatch_id>".to_owned());
            }
            Ok(Command::WebChat(WebChatCommand::DispatchStatus {
                dispatch_id: args[1].clone(),
            }))
        }
        Some("open") => parse_webchat_open(&args[1..]),
        Some("archive") => parse_webchat_archive(&args[1..]),
        Some("archive-status") => parse_webchat_archive_status(&args[1..]),
        Some("handoff") => parse_webchat_handoff(&args[1..]),
        Some(value) => Err(format!(
            "unknown webchat command '{value}' (expected endpoints, resources, inspect, create, send, dispatch-status, open, archive, archive-status, or handoff)"
        )),
        None => Err(
            "webchat requires endpoints, resources, inspect, create, send, dispatch-status, open, archive, archive-status, or handoff"
                .to_owned(),
        ),
    }
}

fn parse_webchat_resources(args: &[String]) -> Result<Command, String> {
    let mut endpoint_ref = None;
    let mut provider = None;
    let mut kind = None;
    let mut parent_ref = None;
    let mut limit = 32usize;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag {
            "--endpoint-ref" => endpoint_ref = Some(value.clone()),
            "--provider" => provider = Some(value.clone()),
            "--kind" => {
                if !matches!(value.as_str(), "account" | "space" | "session") {
                    return Err("--kind must be account, space, or session".to_owned());
                }
                kind = Some(value.clone());
            }
            "--parent-ref" => parent_ref = Some(value.clone()),
            "--limit" => limit = parse_bounded_usize(value, "--limit", 1, 64)?,
            _ => return Err(format!("unknown webchat resources flag '{flag}'")),
        }
        index += 2;
    }
    Ok(Command::WebChat(WebChatCommand::Resources {
        endpoint_ref,
        provider,
        kind,
        parent_ref,
        limit,
    }))
}

fn parse_webchat_create(args: &[String]) -> Result<Command, String> {
    let mut endpoint_ref = None;
    let mut provider = None;
    let mut account_ref = None;
    let mut space_ref = None;
    let mut source_url = None;
    let mut display_label = None;
    let mut message = None;
    let mut expected_generation = None;
    let mut idempotency_key = None;
    let mut work_chain_id = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag {
            "--endpoint-ref" => endpoint_ref = Some(value.clone()),
            "--provider" => provider = Some(value.clone()),
            "--account-ref" => account_ref = Some(value.clone()),
            "--space-ref" => space_ref = Some(value.clone()),
            "--source-url" => source_url = Some(value.clone()),
            "--display-label" => display_label = Some(value.clone()),
            "--message" => message = Some(value.clone()),
            "--expected-generation" => {
                expected_generation = Some(parse_positive_i64(value, "--expected-generation")?)
            }
            "--idempotency-key" => idempotency_key = Some(value.clone()),
            "--work-chain-id" => work_chain_id = Some(value.clone()),
            _ => return Err(format!("unknown webchat create flag '{flag}'")),
        }
        index += 2;
    }
    if source_url.is_none() {
        if endpoint_ref.is_none() {
            return Err("webchat create requires --endpoint-ref without --source-url".to_owned());
        }
        if provider.is_none() {
            return Err("webchat create requires --provider without --source-url".to_owned());
        }
        if account_ref.is_none() {
            return Err("webchat create requires --account-ref without --source-url".to_owned());
        }
        if display_label.is_none() {
            return Err("webchat create requires --display-label without --source-url".to_owned());
        }
        if expected_generation.is_none() {
            return Err(
                "webchat create requires --expected-generation without --source-url".to_owned(),
            );
        }
    }
    Ok(Command::WebChat(WebChatCommand::Create {
        endpoint_ref,
        provider,
        account_ref,
        space_ref,
        source_url,
        display_label,
        message: required_flag(message, "--message")?,
        expected_generation,
        idempotency_key: required_flag(idempotency_key, "--idempotency-key")?,
        work_chain_id,
    }))
}

fn parse_webchat_send(args: &[String]) -> Result<Command, String> {
    let mut session_ref = None;
    let mut message = None;
    let mut expected_generation = None;
    let mut idempotency_key = None;
    let mut work_chain_id = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag {
            "--session-ref" => session_ref = Some(value.clone()),
            "--message" => message = Some(value.clone()),
            "--expected-generation" => {
                expected_generation = Some(parse_positive_i64(value, "--expected-generation")?)
            }
            "--idempotency-key" => idempotency_key = Some(value.clone()),
            "--work-chain-id" => work_chain_id = Some(value.clone()),
            _ => return Err(format!("unknown webchat send flag '{flag}'")),
        }
        index += 2;
    }
    Ok(Command::WebChat(WebChatCommand::Send {
        session_ref: required_flag(session_ref, "--session-ref")?,
        message: required_flag(message, "--message")?,
        expected_generation: expected_generation
            .ok_or_else(|| "webchat send requires --expected-generation".to_owned())?,
        idempotency_key: required_flag(idempotency_key, "--idempotency-key")?,
        work_chain_id,
    }))
}

fn parse_webchat_open(args: &[String]) -> Result<Command, String> {
    let mut session_ref = None;
    let mut expected_generation = None;
    let mut idempotency_key = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag {
            "--session-ref" => session_ref = Some(value.clone()),
            "--expected-generation" => {
                expected_generation = Some(parse_positive_i64(value, "--expected-generation")?)
            }
            "--idempotency-key" => idempotency_key = Some(value.clone()),
            _ => return Err(format!("unknown webchat open flag '{flag}'")),
        }
        index += 2;
    }
    Ok(Command::WebChat(WebChatCommand::Open {
        session_ref: required_flag(session_ref, "--session-ref")?,
        expected_generation: expected_generation
            .ok_or_else(|| "webchat open requires --expected-generation".to_owned())?,
        idempotency_key: required_flag(idempotency_key, "--idempotency-key")?,
    }))
}

fn parse_webchat_archive(args: &[String]) -> Result<Command, String> {
    let mut session_ref = None;
    let mut expected_generation = None;
    let mut idempotency_key = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag {
            "--session-ref" => session_ref = Some(value.clone()),
            "--expected-generation" => {
                expected_generation = Some(parse_positive_i64(value, "--expected-generation")?)
            }
            "--idempotency-key" => idempotency_key = Some(value.clone()),
            _ => return Err(format!("unknown webchat archive flag '{flag}'")),
        }
        index += 2;
    }
    Ok(Command::WebChat(WebChatCommand::Archive {
        session_ref: required_flag(session_ref, "--session-ref")?,
        expected_generation: expected_generation
            .ok_or_else(|| "webchat archive requires --expected-generation".to_owned())?,
        idempotency_key: required_flag(idempotency_key, "--idempotency-key")?,
    }))
}

fn parse_webchat_archive_status(args: &[String]) -> Result<Command, String> {
    let mut session_ref = None;
    let mut expected_generation = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag {
            "--session-ref" => session_ref = Some(value.clone()),
            "--expected-generation" => {
                expected_generation = Some(parse_positive_i64(value, "--expected-generation")?)
            }
            _ => return Err(format!("unknown webchat archive-status flag '{flag}'")),
        }
        index += 2;
    }
    Ok(Command::WebChat(WebChatCommand::ArchiveStatus {
        session_ref: required_flag(session_ref, "--session-ref")?,
        expected_generation: expected_generation
            .ok_or_else(|| "webchat archive-status requires --expected-generation".to_owned())?,
    }))
}

fn parse_webchat_handoff(args: &[String]) -> Result<Command, String> {
    let mut continuity_id = None;
    let mut source_url = None;
    let mut objective = None;
    let mut work_chain_id = None;
    let mut handoff_id = None;
    let mut idempotency_key = None;
    let mut prepare_only = false;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--help" | "-h" => {
                return Ok(Command::Help {
                    section: HelpSection::WebChat,
                });
            }
            "--prepare-only" => {
                prepare_only = true;
                index += 1;
                continue;
            }
            _ => {}
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag {
            "--continuity-id" => continuity_id = Some(value.clone()),
            "--source-url" => source_url = Some(value.clone()),
            "--objective" => objective = Some(value.clone()),
            "--work-chain-id" => work_chain_id = Some(value.clone()),
            "--handoff-id" => handoff_id = Some(value.clone()),
            "--idempotency-key" => idempotency_key = Some(value.clone()),
            _ => return Err(format!("unknown webchat handoff flag '{flag}'")),
        }
        index += 2;
    }
    if prepare_only && idempotency_key.is_some() {
        return Err(
            "webchat handoff --prepare-only does not deliver, so --idempotency-key is unused"
                .to_owned(),
        );
    }
    Ok(Command::WebChat(WebChatCommand::Handoff {
        continuity_id: required_flag(continuity_id, "--continuity-id")?,
        source_url: required_flag(source_url, "--source-url")?,
        objective,
        work_chain_id,
        handoff_id,
        idempotency_key,
        prepare_only,
    }))
}

fn parse_optional_limit_flag(args: &[String], default: usize) -> Result<usize, String> {
    if args.is_empty() {
        return Ok(default);
    }
    if args.len() != 2 || args[0] != "--limit" {
        return Err("only --limit N is accepted here".to_owned());
    }
    parse_bounded_usize(&args[1], "--limit", 1, 64)
}

fn parse_positive_i64(value: &str, flag: &str) -> Result<i64, String> {
    value
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{flag} must be a positive integer"))
}

fn required_flag(value: Option<String>, flag: &str) -> Result<String, String> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing required {flag}"))
}

pub fn webchat_help() -> &'static str {
    "Herdr-MCP supported local WebChat control\n\n\
Usage:\n\
  herdr-mcp webchat endpoints [--limit N]\n\
  herdr-mcp webchat resources [--endpoint-ref REF] [--provider PROVIDER] [--kind account|space|session] [--parent-ref REF] [--limit N]\n\
  herdr-mcp webchat inspect <resource_ref>\n\
  herdr-mcp webchat create --endpoint-ref REF --provider PROVIDER --account-ref REF --display-label LABEL --message MESSAGE --expected-generation N --idempotency-key KEY [--space-ref REF] [--work-chain-id ID]\n\
  herdr-mcp webchat create --source-url URL --message MESSAGE --idempotency-key KEY [--work-chain-id ID]\n\
  herdr-mcp webchat send --session-ref REF --message MESSAGE --expected-generation N --idempotency-key KEY [--work-chain-id ID]\n\
  herdr-mcp webchat dispatch-status <dispatch_id>\n\
  herdr-mcp webchat open --session-ref REF --expected-generation N --idempotency-key KEY\n\
  herdr-mcp webchat archive --session-ref REF --expected-generation N --idempotency-key KEY\n\
  herdr-mcp webchat archive-status --session-ref REF --expected-generation N\n\
  herdr-mcp webchat handoff --continuity-id HC --source-url URL [--objective TEXT] [--work-chain-id ID] [--handoff-id ID] [--idempotency-key KEY] [--prepare-only]\n\n\
Discover capability first: endpoints -> resources -> inspect. Refs are opaque; never\n\
synthesize them, and pass the observed observation_generation as --expected-generation.\n\n\
webchat handoff is the canonical continuation path for one existing Continuity chain.\n\
It prepares the canonical handoff packet, then attempts automatic delivery into a new\n\
WebChat conversation and reports the delivery evidence.\n\
  - It reuses the existing continuity_id; it never creates a second chain, a second\n\
    handoff message generator, or a second state model.\n\
  - automatic_delivery.params.message and manual_delivery.copy_prompt are the same\n\
    canonical string; the CLI never rewrites or re-encodes it.\n\
  - --source-url is the audit anchor and resolves the existing WebChat route\n\
    (endpoint/account/Project) from the registered conversation, so no routing ids are\n\
    passed on the command line.\n\
  - When browser control is unavailable the packet is still returned: deliver it manually\n\
    with manual_delivery.copy_prompt. Only automatic_delivery.delivery_state=applied\n\
    means a browser delivery happened; a prepared packet alone is not a completed handoff.\n\
  - One logical handoff keeps one idempotency key. Without --idempotency-key the CLI reuses\n\
    the canonical handoff_id. Do not rotate the key to probe delivery state; use the returned\n\
    automatic_delivery evidence and dispatch-status when a dispatch exists.\n\
  - The CLI never retries an uncertain delivery; read automatic_delivery and\n\
    webchat dispatch-status first.\n"
}
