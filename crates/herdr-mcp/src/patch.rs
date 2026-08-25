use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PatchOp {
    Add {
        path: String,
        content: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        hunks: Vec<Vec<String>>,
        move_to: Option<String>,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PatchError {
    pub code: &'static str,
    pub message: String,
    pub details: Map<String, Value>,
}

impl PatchError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: Map::new(),
        }
    }

    fn with_detail(mut self, key: &str, value: Value) -> Self {
        self.details.insert(key.to_owned(), value);
        self
    }

    pub fn to_value(&self) -> Value {
        let mut output = self.details.clone();
        output.insert("ok".to_owned(), json!(false));
        output.insert("code".to_owned(), json!(self.code));
        output.insert("message".to_owned(), json!(self.message));
        Value::Object(output)
    }
}

pub fn parse_patch(patch: &str) -> Result<Vec<PatchOp>, PatchError> {
    let lines = patch
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect::<Vec<_>>();
    if lines
        .first()
        .is_none_or(|line| line.trim() != "*** Begin Patch")
        || lines
            .last()
            .is_none_or(|line| line.trim() != "*** End Patch")
    {
        return Err(PatchError::new(
            "PATCH_FAILED",
            "Patch must use *** Begin Patch / *** End Patch envelope.",
        ));
    }

    let mut operations = Vec::new();
    let mut index = 1usize;
    while index < lines.len().saturating_sub(1) {
        let line = &lines[index];
        if line.is_empty() {
            index += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let path = path.trim().to_owned();
            index += 1;
            let mut content = Vec::new();
            while index < lines.len().saturating_sub(1) && !lines[index].starts_with("*** ") {
                let Some(value) = lines[index].strip_prefix('+') else {
                    return Err(PatchError::new(
                        "PATCH_FAILED",
                        "Add file lines must start with '+'.",
                    ));
                };
                content.push(value.to_owned());
                index += 1;
            }
            operations.push(PatchOp::Add {
                path,
                content: format!("{}\n", content.join("\n")),
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            operations.push(PatchOp::Delete {
                path: path.trim().to_owned(),
            });
            index += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            let path = path.trim().to_owned();
            index += 1;
            let mut move_to = None;
            if index < lines.len().saturating_sub(1)
                && let Some(destination) = lines[index].strip_prefix("*** Move to: ")
            {
                move_to = Some(destination.trim().to_owned());
                index += 1;
            }
            let mut hunks = Vec::new();
            let mut current = Vec::new();
            while index < lines.len().saturating_sub(1) && !lines[index].starts_with("*** ") {
                if lines[index].starts_with("@@") {
                    if !current.is_empty() {
                        hunks.push(std::mem::take(&mut current));
                    }
                } else {
                    current.push(lines[index].clone());
                }
                index += 1;
            }
            if !current.is_empty() {
                hunks.push(current);
            }
            operations.push(PatchOp::Update {
                path,
                hunks,
                move_to,
            });
            continue;
        }
        return Err(PatchError::new(
            "PATCH_FAILED",
            format!("Unrecognized patch line: {line}"),
        ));
    }
    Ok(operations)
}

pub fn apply_update_hunks(
    content: &str,
    hunks: &[Vec<String>],
    file_path: &str,
) -> Result<String, PatchError> {
    if hunks.is_empty() {
        return Ok(content.to_owned());
    }
    let (bom, text) = content
        .strip_prefix('\u{feff}')
        .map_or(("", content), |value| ("\u{feff}", value));
    let crlf = text.contains("\r\n");
    let normalized = text.replace("\r\n", "\n");
    let had_trailing = normalized.ends_with('\n');
    let mut lines = normalized
        .split('\n')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if had_trailing && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }

    #[derive(Debug)]
    struct Matched {
        start: usize,
        end: usize,
        next: Vec<String>,
    }

    let mut matched = Vec::new();
    for (index, hunk) in hunks.iter().enumerate() {
        let (old, next) = parse_update_hunk(hunk)?;
        let matches = if old.is_empty() {
            vec![0]
        } else {
            find_subsequence_all(&lines, &old)
        };
        if matches.is_empty() {
            return Err(PatchError::new(
                "PATCH_CONTEXT_NOT_FOUND",
                format!("Patch context did not match in {file_path}."),
            )
            .with_detail("path", json!(file_path))
            .with_detail("hunk_index", json!(index))
            .with_detail(
                "retry_hint",
                json!("Read the current file and regenerate this hunk."),
            ));
        }
        if matches.len() > 1 {
            return Err(PatchError::new(
                "PATCH_CONTEXT_AMBIGUOUS",
                format!(
                    "Patch context matched {} locations in {file_path}.",
                    matches.len()
                ),
            )
            .with_detail("path", json!(file_path))
            .with_detail("hunk_index", json!(index))
            .with_detail("match_count", json!(matches.len()))
            .with_detail(
                "retry_hint",
                json!("Include additional unchanged context lines."),
            ));
        }
        let start = matches[0];
        matched.push(Matched {
            start,
            end: start + old.len(),
            next,
        });
    }

    matched.sort_by_key(|item| item.start);
    if matched.windows(2).any(|pair| pair[0].end > pair[1].start) {
        return Err(PatchError::new(
            "PATCH_HUNKS_OVERLAP",
            format!("Patch hunks overlap in {file_path}."),
        ));
    }
    for item in matched.into_iter().rev() {
        lines.splice(item.start..item.end, item.next);
    }

    let mut output = lines.join("\n");
    if (had_trailing && (!lines.is_empty() || output.is_empty()))
        || (text.is_empty() && !lines.is_empty())
    {
        output.push('\n');
    }
    if crlf {
        output = output.replace('\n', "\r\n");
    }
    Ok(format!("{bom}{output}"))
}

fn parse_update_hunk(hunk: &[String]) -> Result<(Vec<String>, Vec<String>), PatchError> {
    let mut old = Vec::new();
    let mut next = Vec::new();
    for raw in hunk {
        if raw == "*** End of File" {
            continue;
        }
        let Some(marker) = raw.chars().next() else {
            return Err(PatchError::new("PATCH_FAILED", "Invalid empty patch line."));
        };
        let value = &raw[marker.len_utf8()..];
        match marker {
            ' ' => {
                old.push(value.to_owned());
                next.push(value.to_owned());
            }
            '-' => old.push(value.to_owned()),
            '+' => next.push(value.to_owned()),
            _ => {
                return Err(PatchError::new(
                    "PATCH_FAILED",
                    "Update lines must start with space, '-' or '+'.",
                ));
            }
        }
    }
    Ok((old, next))
}

fn find_subsequence_all(lines: &[String], needle: &[String]) -> Vec<usize> {
    if needle.is_empty() {
        return vec![0];
    }
    if needle.len() > lines.len() {
        return vec![];
    }
    (0..=lines.len() - needle.len())
        .filter(|start| lines[*start..*start + needle.len()] == *needle)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_add_update_delete_and_move() {
        let operations = parse_patch(
            "*** Begin Patch\n*** Add File: a.txt\n+hello\n*** Update File: b.txt\n*** Move to: moved.txt\n@@\n-old\n+new\n*** Delete File: c.txt\n*** End Patch",
        )
        .unwrap();
        assert_eq!(operations.len(), 3);
        assert!(matches!(operations[0], PatchOp::Add { .. }));
        assert!(matches!(operations[1], PatchOp::Update { .. }));
        assert!(matches!(operations[2], PatchOp::Delete { .. }));
        let PatchOp::Update { move_to, .. } = &operations[1] else {
            unreachable!();
        };
        assert_eq!(move_to.as_deref(), Some("moved.txt"));
    }

    #[test]
    fn applies_unique_context_and_preserves_crlf_bom() {
        let output = apply_update_hunks(
            "\u{feff}one\r\nold\r\ntwo\r\n",
            &[vec![
                " one".to_owned(),
                "-old".to_owned(),
                "+new".to_owned(),
                " two".to_owned(),
            ]],
            "b.txt",
        )
        .unwrap();
        assert_eq!(output, "\u{feff}one\r\nnew\r\ntwo\r\n");
    }

    #[test]
    fn ambiguous_context_is_rejected() {
        let error = apply_update_hunks("x\nx\n", &[vec!["-x".to_owned(), "+y".to_owned()]], "f")
            .unwrap_err();
        assert_eq!(error.code, "PATCH_CONTEXT_AMBIGUOUS");
        assert_eq!(error.details["match_count"], 2);
    }

    #[test]
    fn envelope_is_strict() {
        assert_eq!(
            parse_patch("*** Begin Patch\n*** End Patch\n")
                .unwrap_err()
                .code,
            "PATCH_FAILED"
        );
    }
}
