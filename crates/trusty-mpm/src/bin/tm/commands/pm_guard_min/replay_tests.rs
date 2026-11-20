//! Replay of the owner's must-catch corpus through the shadow guard (#11431).
//!
//! Why: the brief's rollout gate, narrowed by the owner (Bob, 15:54Z) to
//! actual risks: every recorded true positive caught by the right rule, and
//! ordinary work silent.
//! What: rows are `{id, tool, command, path, new_rule, expect?}`.
//! `new_rule` R1/R2/R3/R5 → asserted: a would-refuse from that rule.
//! `expect: "no_refuse"` → asserted: no would-refuse. R4 → expected miss in
//! PR1 (secret values, PR2). `none` → reported as uncovered. A null command,
//! or one whose literal text was not recorded (`<hand edit ...>`), is
//! skipped and reported. `{HOME}` becomes a scratch home; the cwd and
//! `CLAUDE_PROJECT_DIR` are a scratch main checkout. `--nocapture` prints
//! the table.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use super::*;

const FIXTURES: &[&str] = &[
    "tests/fixtures/pm_guard_min/must_catch.jsonl",
    "tests/fixtures/pm_guard_min/synthetic.jsonl",
];

/// `"R2"`, `"2"`, `"rule 2"` → `"R2"`.
fn rule_id(v: &Value) -> Option<String> {
    let digits: String = v.as_str()?.chars().filter(char::is_ascii_digit).collect();
    (!digits.is_empty()).then(|| format!("R{digits}"))
}

/// A recorded placeholder such as `<LaunchAgent plist>` instead of a literal command.
fn is_placeholder(cmd: &str) -> bool {
    cmd.match_indices('<').any(|(i, _)| {
        let rest = &cmd[i + 1..];
        rest.starts_with(|c: char| c.is_ascii_alphabetic())
            && rest.find('>').is_some_and(|j| !rest[..j].contains('\n'))
    })
}

fn session_repo(root: &Path) -> PathBuf {
    let repo = root.join("session");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init", "-q"][..],
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:owner/session.git",
        ],
    ] {
        let ok = Command::new("git").arg("-C").arg(&repo).args(args).status();
        assert!(ok.is_ok_and(|s| s.success()));
    }
    repo
}

/// One row's verdict: (`ok`/`FAIL ...`/`reported ...`, rule got).
fn replay_row(
    row: &Value,
    home: &Path,
    env: &dyn Fn(&Value, &Path) -> GuardEnv,
) -> (String, Option<String>) {
    let Some(command) = row["command"].as_str() else {
        return ("reported: skipped, command not recorded".into(), None);
    };
    if is_placeholder(command) {
        return ("reported: skipped, placeholder text".into(), None);
    }
    let payload = json!({ "session_id": "replay", "tool_name": "Bash", "tool_input": { "command": command } });
    let got = judge(payload.to_string().as_bytes(), home, env).and_then(|l| rule_id(&l["rule"]));
    let want = rule_id(&row["new_rule"]);
    let verdict = if row["expect"] == "no_refuse" {
        if got.is_none() {
            "ok"
        } else {
            "FAIL would-refuse ordinary work"
        }
    } else {
        match want.as_deref() {
            Some("R1" | "R2" | "R3" | "R5") if got == want => "ok",
            Some("R1" | "R2" | "R3" | "R5") => "FAIL missed or wrong rule",
            Some("R4") => "reported: expected miss, R4 is PR2",
            _ if command.contains("get-secret-value") => {
                "reported: expected miss, egress with secret — R4/R6, PR2"
            }
            _ => "reported: no rule covers it",
        }
    };
    (verdict.to_owned(), got)
}

#[test]
fn replay_must_catch_corpus() {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = session_repo(scratch.path());
    let env = |_: &Value, home: &Path| GuardEnv {
        home: home.to_path_buf(),
        project_dir: Some(repo.clone()),
        cwd: repo.clone(),
        r5_allow: Vec::new(),
        architect: false,
    };
    let (mut failures, mut report) = (Vec::new(), Vec::new());
    for fixture in FIXTURES {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(fixture);
        let text = std::fs::read_to_string(&path).expect("fixture");
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let line = line.replace("{HOME}", &home.display().to_string());
            let row: Value = serde_json::from_str(&line).expect("row is JSON");
            let (verdict, got) = replay_row(&row, &home, &env);
            let entry = format!(
                "{verdict:<42} {:<28} want {:<5} got {got:?}",
                row["id"].as_str().unwrap_or("?"),
                row["new_rule"].as_str().unwrap_or("-"),
            );
            if verdict.starts_with("FAIL") {
                failures.push(entry.clone());
            }
            report.push(entry);
        }
    }
    println!("replay:\n{}", report.join("\n"));
    assert!(
        failures.is_empty(),
        "replay failures:\n{}",
        failures.join("\n")
    );
}
