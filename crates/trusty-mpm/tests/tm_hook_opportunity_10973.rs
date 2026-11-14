//! `tm hook` records a compression-opportunity row per tool result (#10973).
//!
//! Why: a subagent's result and an MCP tool's output reach the model with no
//! row on the savings ledger, so nobody can see what compression they leave on
//! the table. This drives the real binary with the `PostToolUse` payloads
//! Claude Code sends for both and reads the ledger the scratch `$HOME` holds.
//! What: two `tm --url <unreachable> hook` runs under one scratch home, then
//! asserts two rows in the `usage/opportunity/` day file, keyed by the
//! payload's session, typed
//! `subagent` and `mcp`, and that the hook printed nothing to stdout — the
//! observer never changes a tool's output.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_opportunity_10973::`.

use crate::common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Run `tm hook` under `home` with `payload` on stdin; returns stdout.
fn run_hook_in(home: &Path, payload: &serde_json::Value) -> String {
    let mut child = common::tm_command_in(home)
        .args(["--url", "http://127.0.0.1:1", "hook"])
        .current_dir(home)
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook`");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait for `tm hook`");
    assert!(
        output.status.success(),
        "tm hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8 stdout")
}

/// Every opportunity day file under `dir`, wherever the framework root resolved.
fn ledgers_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(ledgers_under(&path));
        } else if path
            .parent()
            .is_some_and(|p| p.ends_with("usage/opportunity"))
            && path.extension().is_some_and(|e| e == "jsonl")
        {
            found.push(path);
        }
    }
    found
}

#[test]
fn a_subagent_result_and_an_mcp_result_each_append_an_opportunity_row() {
    let home = tempfile::tempdir().expect("scratch home");
    let report = "Changed 3 files.\n".repeat(200);
    let agent = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "sess-10973",
        "tool_name": "Agent",
        "tool_input": {"prompt": "do the thing", "subagent_type": "rust-engineer"},
        "tool_response": {"content": [{"type": "text", "text": report}]},
    });
    let mcp = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "sess-10973",
        "tool_name": "mcp__trusty-search__search",
        "tool_input": {"query": "x"},
        "tool_response": [{"type": "text", "text": "hit\n".repeat(500)}],
    });
    for payload in [&agent, &mcp] {
        assert_eq!(
            run_hook_in(home.path(), payload).trim(),
            "",
            "no output change"
        );
    }

    let rows: Vec<serde_json::Value> = ledgers_under(home.path())
        .iter()
        .filter_map(|l| std::fs::read_to_string(l).ok())
        .flat_map(|text| {
            text.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect::<Vec<serde_json::Value>>()
        })
        .collect();
    let types: Vec<&str> = rows
        .iter()
        .filter_map(|r| r["tool_type"].as_str())
        .collect();
    assert_eq!(types, ["subagent", "mcp"], "{rows:?}");
    for row in &rows {
        assert_eq!(row["session_id"], "sess-10973", "{row}");
        assert!(row["bytes_returned"].as_u64().unwrap_or(0) > 0, "{row}");
        assert!(row["would_compress_bytes"].is_u64(), "{row}");
    }
}

/// #10981 round 2: once a day's file is full, each hook process counts its
/// dropped row in `<day>.dropped`, and only the first writes the `day_full`
/// sentinel — the count survives across processes.
#[test]
fn a_full_day_counts_drops_across_hook_processes() {
    let home = tempfile::tempdir().expect("scratch home");
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "sess-10981",
        "tool_name": "Read",
        "tool_response": "line\n".repeat(200),
    });
    // The first run creates the day file wherever the framework root resolved.
    run_hook_in(home.path(), &payload);
    let day_files = ledgers_under(home.path());
    assert_eq!(day_files.len(), 1, "{day_files:?}");
    let day_file = &day_files[0];
    assert!(day_file.ends_with(format!("{day}.jsonl")), "{day_file:?}");
    // Fill the day to its 8 MiB cap.
    let filler = format!("{}\n", "x".repeat(1023));
    std::fs::write(day_file, filler.repeat(8 * 1024)).expect("fill the day");

    for _ in 0..3 {
        run_hook_in(home.path(), &payload);
    }
    let text = std::fs::read_to_string(day_file).expect("day file");
    assert_eq!(text.matches("\"day_full\"").count(), 1, "one sentinel");
    let dropped = day_file.with_extension("dropped");
    assert_eq!(std::fs::metadata(&dropped).expect("counter").len(), 3);
}

/// Every opportunity row under `home`, in file order.
fn rows_under(home: &Path) -> Vec<serde_json::Value> {
    ledgers_under(home)
        .iter()
        .filter_map(|l| std::fs::read_to_string(l).ok())
        .flat_map(|text| {
            text.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect::<Vec<serde_json::Value>>()
        })
        .collect()
}

/// The one row a `PostToolUse` Bash payload for `command`, run from `cwd`,
/// appends under a fresh scratch home.
fn bash_row(command: &str, cwd: &Path, stdout: &str) -> serde_json::Value {
    let home = tempfile::tempdir().expect("scratch home");
    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "sess-10973-reason",
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "tool_response": {"stdout": stdout, "stderr": "", "interrupted": false},
    });
    assert_eq!(run_hook_in(home.path(), &payload).trim(), "", "no output");
    let rows = rows_under(home.path());
    assert_eq!(rows.len(), 1, "{rows:?}");
    rows.into_iter().next().expect("one row")
}

/// A long grep match list: the shape the grep filter shrinks.
fn grep_matches() -> String {
    (0..300)
        .map(|i| format!("src/mod{i}.rs:{i}:    let x = foo(y);\n"))
        .collect()
}

/// #10973: a chained command is never rewritten (`has_unsafe_pipe_composition`),
/// so its row says so rather than leaving the reader to guess. D60's one
/// leading `cd <dir> && ` is the exception, and its row reads `compressed`.
#[test]
fn a_bash_opportunity_row_records_why_no_rewrite_applied() {
    let cwd = tempfile::tempdir().expect("cwd");
    for (command, reason) in [
        ("cd /x && grep -n foo big.txt", "compressed"),
        ("cd /x && cd /y && grep -n foo big.txt", "chained"),
        ("grep -n foo big.txt | head", "chained"),
    ] {
        let row = bash_row(command, cwd.path(), &grep_matches());
        assert_eq!(row["skip_reason"], reason, "{command}: {row}");
    }
}

/// #10973: each `PreToolUse` guard decision names itself on the row.
#[test]
fn a_bash_row_names_each_guard_refusal() {
    let cwd = tempfile::tempdir().expect("cwd");
    let source: String = (0..300).map(|i| format!("let v{i} = {i};\n")).collect();
    for (command, stdout, reason) in [
        ("sed -n 1,400p notes.txt", grep_matches(), "no_filter"),
        ("cat -n src/lib.rs", source, "source_read"),
        ("git diff", "+a\n".to_string(), "compressed"),
    ] {
        let row = bash_row(command, cwd.path(), &stdout);
        assert_eq!(row["skip_reason"], reason, "{command}: {row}");
    }
}

/// #10973: inside an isolation worktree the one-command rewrite also refuses a
/// command whose first word is not the program (`env git diff`).
#[test]
fn an_isolated_worktree_refusal_is_named_on_the_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tree = dir.path().join("repo/.claude/worktrees/agent-a");
    std::fs::create_dir_all(&tree).expect("mkdir");
    let row = bash_row("env git diff", &tree, &"+line\n".repeat(400));
    assert_eq!(row["skip_reason"], "wrapper_prefix", "{row}");
}

/// #10973: a non-Bash result is `non_bash` when compression would shrink it,
/// and `no_gain` when it would not.
#[test]
fn a_non_bash_row_is_non_bash_or_no_gain() {
    let home = tempfile::tempdir().expect("scratch home");
    let long_read: String = (0..400)
        .map(|i| format!("// comment {i}\n\nlet v{i} = {i};\n"))
        .collect();
    for text in [long_read, "short".to_string()] {
        let payload = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-10973-reason",
            "tool_name": "Read",
            "tool_response": text,
        });
        run_hook_in(home.path(), &payload);
    }
    let reasons: Vec<serde_json::Value> = rows_under(home.path())
        .into_iter()
        .map(|r| r["skip_reason"].clone())
        .collect();
    assert_eq!(reasons, ["non_bash", "no_gain"]);
}
