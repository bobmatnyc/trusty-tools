//! #9015: with no reviewer prompts installed, or with content too old or
//! broken for this binary, `trusty-review` refuses on stderr, naming the fix.
//!
//! Why: the reviewer prompts left the binary for runtime content. A content
//! release older than #9015 lacks them; an empty file is unusable; with no
//! release installed there is nothing at all. Each must stop the review with a
//! non-zero exit and the remedy on stderr — visible even with tracing off — and
//! never review on a missing or empty system prompt.
//! What: spawns the built binary with a scratch `$HOME` and cwd (so no checkout
//! serves content), stages each content state with `support/content.rs`, and
//! runs `run --local-diff` with `RUST_LOG=off`; then `mcp` with stdin at EOF,
//! which must print the remedy and still start.
//! Test: this file IS the test.

use std::path::Path;
use std::process::{Command, Output, Stdio};

#[path = "support/content.rs"]
mod content;

use content::{STOCK_PATH, Staged, stage_bundle};

const BIN: &str = env!("CARGO_BIN_EXE_trusty-review");

/// Runs the binary with `args` in a scratch cwd under `home`, hermetically.
fn run_in(home: &Path, args: &[&str]) -> Output {
    let cwd = tempfile::tempdir().expect("scratch cwd");
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(cwd.path())
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("AWS_EC2_METADATA_DISABLED", "true")
        // Tracing off, as a REPL or a quiet wrapper would set it: the remedy
        // must not depend on a log line.
        .env("RUST_LOG", "off")
        .stdin(Stdio::null());
    cmd.output().expect("spawn trusty-review")
}

/// `run --local-diff` over a one-line diff.
fn run_review(home: &Path) -> Output {
    let diff = home.join("change.diff");
    std::fs::write(
        &diff,
        "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -0,0 +1 @@\n+fn a() {}\n",
    )
    .expect("diff");
    run_in(
        home,
        &["run", "--local-diff", diff.to_str().expect("utf8 path")],
    )
}

/// Each content state refuses `run`: non-zero exit, the remedy and the
/// case's own detail on stderr, nothing on stdout.
#[test]
fn stale_or_absent_content_refuses_run_naming_the_remedy() {
    let cases: [(&str, Option<Staged<'_>>, &str); 3] = [
        ("absent", None, "no instructional content is installed"),
        ("stale", Some(Staged::Without(STOCK_PATH)), STOCK_PATH),
        (
            "empty",
            Some(Staged::Emptied(STOCK_PATH)),
            "the file is empty",
        ),
    ];
    for (name, staged, detail) in cases {
        let home = tempfile::tempdir().expect("scratch home");
        if let Some(staged) = staged {
            stage_bundle(home.path(), staged);
        }
        let out = run_review(home.path());
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        println!("{name}: exit {:?}\nstderr:\n{stderr}", out.status.code());
        assert!(
            !out.status.success(),
            "{name}: run must refuse\nstderr: {stderr}"
        );
        assert!(
            stderr.contains("tm content update"),
            "{name}: remedy\nstderr: {stderr}"
        );
        assert!(
            stderr.contains(detail),
            "{name}: names `{detail}`\nstderr: {stderr}"
        );
        assert!(
            !stdout.contains("tm content update"),
            "{name}: stdout stays clean"
        );
    }
}

/// The MCP server, which reviews per request, prints the remedy at startup on
/// stderr and still starts; stdout carries no remedy text.
#[test]
fn mcp_without_content_prints_the_remedy_on_stderr() {
    let home = tempfile::tempdir().expect("scratch home");
    let out = run_in(home.path(), &["mcp"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    println!("mcp: exit {:?}\nstderr:\n{stderr}", out.status.code());
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("tm content update"))
        .collect();
    assert_eq!(lines.len(), 1, "one remedy line\nstderr: {stderr}");
    assert!(
        lines[0].starts_with("trusty-review: ERROR "),
        "{}",
        lines[0]
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("tm content update"),
        "stdout is the JSON-RPC transport"
    );
}
