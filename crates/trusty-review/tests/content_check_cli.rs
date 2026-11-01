//! #10588: `trusty-review content check` refuses content older than this
//! binary, naming the missing file, and passes content that serves it.
//!
//! Why: a fresh install pinned a content release without
//! `prompts/principles_addendum.md` and reported success; tctl now runs this
//! verb after `tm content update`, so its exit code and last stderr line are
//! the contract.
//! What: spawns the built binary with a scratch `$HOME`, stages each content
//! state with `support/content.rs`, and runs `content check`.
//! Test: this file IS the test.

use std::path::Path;
use std::process::{Command, Output, Stdio};

#[path = "support/content.rs"]
mod content;

use content::{PRINCIPLES_PATH, Staged, stage_bundle};

const BIN: &str = env!("CARGO_BIN_EXE_trusty-review");

/// Runs `content check` hermetically under `home`.
fn content_check(home: &Path) -> Output {
    let cwd = tempfile::tempdir().expect("scratch cwd");
    Command::new(BIN)
        .args(["content", "check"])
        .current_dir(cwd.path())
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("RUST_LOG", "off")
        .stdin(Stdio::null())
        .output()
        .expect("spawn trusty-review")
}

/// The last non-blank stderr line: what tctl reports.
fn last_stderr_line(out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    println!("exit {:?}\nstderr:\n{stderr}", out.status.code());
    stderr
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// A binary newer than the deployed content: the release lacks the
/// principles addendum, so the check exits 1 and its last line names it.
#[test]
fn content_check_names_the_file_older_content_lacks() {
    let home = tempfile::tempdir().expect("scratch home");
    stage_bundle(home.path(), Staged::Without(PRINCIPLES_PATH));
    let out = content_check(home.path());
    let last = last_stderr_line(&out);
    assert_eq!(out.status.code(), Some(1), "must refuse");
    assert!(last.contains(PRINCIPLES_PATH), "names the file: {last}");
    assert!(
        last.contains("tm content update"),
        "names the remedy: {last}"
    );
}

/// Content that holds every file this binary reads passes, exit 0.
#[test]
fn content_check_passes_on_content_that_serves_this_binary() {
    let home = tempfile::tempdir().expect("scratch home");
    stage_bundle(home.path(), Staged::Current);
    let out = content_check(home.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "passes: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("content-v"), "names the release: {stdout}");
}

/// No content at all fails too, naming the remedy on the last line.
#[test]
fn content_check_without_content_names_the_remedy() {
    let home = tempfile::tempdir().expect("scratch home");
    let out = content_check(home.path());
    let last = last_stderr_line(&out);
    assert!(!out.status.success(), "must refuse");
    assert!(
        last.contains("tm content update"),
        "names the remedy: {last}"
    );
}
