//! Process-level contract for `herdr-mcp git-filter strip-handoff`, the clean
//! filter that keeps the managed HANDOFF block in the working tree but out of
//! every blob Git writes (#839).
//!
//! The filter runs inside `git add`/`git commit`, so it must be silent, always
//! exit 0, and never alter content that carries no managed block.

use std::io::Write;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_herdr-mcp");
const START: &str = "<!-- herdr:handoff:start -->";
const END: &str = "<!-- herdr:handoff:end -->";

fn run_filter(input: &[u8]) -> Output {
    let mut child = Command::new(BIN)
        .args(["git-filter", "strip-handoff"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn herdr-mcp git-filter strip-handoff");
    child
        .stdin
        .as_mut()
        .expect("stdin pipe")
        .write_all(input)
        .expect("write filter input");
    child.wait_with_output().expect("wait for filter")
}

#[test]
fn user_commits_agent_doc_content_without_the_managed_block() {
    let input = format!("## Rules\n\n- [r1] keep tests green\n\n{START}\nread HANDOFF.md\n{END}\n");
    let output = run_filter(input.as_bytes());
    assert!(
        output.status.success(),
        "the clean filter must exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "the clean filter must not write to stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "## Rules\n\n- [r1] keep tests green\n"
    );
}

#[test]
fn user_gets_byte_identical_content_when_no_managed_block_is_present() {
    for input in [
        &b""[..],
        b"plain\n",
        b"no trailing newline",
        b"crlf\r\nlines\r\n",
        b"\xff\xfe non-utf8 bytes\n",
    ] {
        let output = run_filter(input);
        assert!(output.status.success());
        assert!(
            output.stderr.is_empty(),
            "unexpected stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, input.to_vec());
    }
}

#[test]
fn user_keeps_status_clean_because_git_runs_the_real_filter_on_a_tracked_agent_doc() {
    if !git_available() {
        return;
    }
    let root = temp_repo("filter-roundtrip");
    write(&root, "AGENTS.md", "## Rules\n\n- [r1] keep tests green\n");
    git_ok(&root, &["add", "-A"]);
    git_ok(&root, &["commit", "-qm", "seed"]);

    // The block lives in the working tree only.
    write(
        &root,
        "AGENTS.md",
        &format!("## Rules\n\n- [r1] keep tests green\n\n{START}\nread HANDOFF.md\n{END}\n"),
    );
    std::fs::create_dir_all(root.join(".git/info")).expect("git info dir");
    write(
        &root,
        ".git/info/attributes",
        "/AGENTS.md filter=herdr-handoff\n",
    );
    git_ok(
        &root,
        &[
            "config",
            "--local",
            "filter.herdr-handoff.clean",
            &format!("'{}' git-filter strip-handoff", BIN),
        ],
    );
    git_ok(
        &root,
        &["config", "--local", "filter.herdr-handoff.smudge", "cat"],
    );
    // `required` stays unset: a missing binary must fall back, not fail commits.
    assert!(
        !Command::new("git")
            .args([
                "config",
                "--local",
                "--get",
                "filter.herdr-handoff.required"
            ])
            .current_dir(&root)
            .output()
            .expect("git config")
            .status
            .success()
    );

    git_ok(&root, &["add", "-A"]);
    assert_eq!(stdout(&root, &["status", "--porcelain"]), "");
    // Staging the block-only change produces no new commit: the filtered blob
    // is byte-identical to what is already committed.
    let empty_commit = Command::new("git")
        .args(["commit", "-qm", "agent doc"])
        .current_dir(&root)
        .output()
        .expect("run git commit");
    assert!(!empty_commit.status.success());
    assert_eq!(stdout(&root, &["status", "--porcelain"]), "");

    // A real edit is committed, the block is not.
    write(
        &root,
        "AGENTS.md",
        &format!(
            "## Rules\n\n- [r1] keep tests green\n- [r2] real user rule\n\n{START}\nread HANDOFF.md\n{END}\n"
        ),
    );
    git_ok(&root, &["add", "-A"]);
    git_ok(&root, &["commit", "-qm", "agent doc"]);
    let committed = stdout(&root, &["show", "HEAD:AGENTS.md"]);
    assert!(!committed.contains("herdr:handoff"), "{committed}");
    assert!(committed.contains("- [r1] keep tests green"), "{committed}");
    assert!(committed.contains("- [r2] real user rule"), "{committed}");
    let worktree = std::fs::read_to_string(root.join("AGENTS.md")).expect("read AGENTS.md");
    assert!(worktree.contains(START), "{worktree}");
    assert_eq!(stdout(&root, &["status", "--porcelain"]), "");

    std::fs::remove_dir_all(root).expect("remove temp repo");
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn temp_repo(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "herdr-handoff-filter-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&root).expect("temp dir");
    git_ok(&root, &["init", "-q"]);
    git_ok(&root, &["config", "user.email", "test@example.com"]);
    git_ok(&root, &["config", "user.name", "Test"]);
    git_ok(&root, &["config", "commit.gpgsign", "false"]);
    root
}

fn write(root: &std::path::Path, name: &str, content: &str) {
    std::fs::write(root.join(name), content).expect("write file");
}

fn git_ok(root: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout(root: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_owned()
}
