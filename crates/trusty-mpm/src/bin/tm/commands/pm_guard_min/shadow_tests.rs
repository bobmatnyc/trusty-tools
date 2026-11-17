//! Fail-safe and log-shape tests for [`super`] (#11431).

use std::path::Path;

use serde_json::{Value, json};

use super::*;

fn test_env(_: &Value, home: &Path) -> GuardEnv {
    GuardEnv {
        home: home.to_path_buf(),
        project_dir: None,
        cwd: home.to_path_buf(),
        r5_allow: Vec::new(),
        architect: false,
    }
}

#[test]
fn a_would_refuse_logs_one_line_without_the_command_text() {
    let home = tempfile::tempdir().unwrap();
    let raw = json!({
        "session_id": "s-1",
        "tool_name": "Bash",
        "tool_input": { "command": "rm -rf ~ # sentinel-prose-xyz" },
    });
    let line = judge(raw.to_string().as_bytes(), home.path(), &test_env).expect("would-refuse");
    assert_eq!(line["rule"], "R2");
    assert_eq!(line["session_id"], "s-1");
    assert_eq!(line["tool"], "Bash");
    assert_eq!(line["fact"]["argv0"], "rm");
    assert_eq!(line["sha256"].as_str().map(str::len), Some(64));
    assert!(!line.to_string().contains("sentinel-prose-xyz"));
    append_log(home.path(), &line);
    append_log(home.path(), &line);
    let log = std::fs::read_to_string(home.path().join(LOG_PATH)).unwrap();
    assert_eq!(log.lines().count(), 2);
}

#[test]
fn malformed_and_huge_payloads_yield_no_line() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(judge(b"{not json", home.path(), &test_env), None);
    assert_eq!(judge(b"", home.path(), &test_env), None);
    let huge = json!({ "tool_name": "Bash", "tool_input": { "command": format!("rm -rf ~ {}", "a".repeat(2 << 20)) } });
    assert_eq!(
        judge(huge.to_string().as_bytes(), home.path(), &test_env),
        None
    );
    let deep = format!("{}rm -rf ~{}", "$(".repeat(5000), ")".repeat(5000));
    let raw = json!({ "tool_name": "Bash", "tool_input": { "command": deep } });
    // Bounded recursion: no stack overflow; the answer itself does not matter.
    let _ = judge(raw.to_string().as_bytes(), home.path(), &test_env);
}

#[test]
fn a_panicking_judge_is_caught() {
    let home = tempfile::tempdir().unwrap();
    let raw = json!({ "tool_name": "Bash", "tool_input": { "command": "rm -rf ~" } }).to_string();
    let mut reached = false;
    guarded(|| {
        let _ = judge(raw.as_bytes(), home.path(), &|_, _| panic!("parser panic"));
        reached = true;
    });
    assert!(!reached, "the panic unwound out of judge and was caught");
}

#[test]
fn an_unwritable_log_is_dropped() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(".trusty-mpm"), "a file, not a dir").unwrap();
    append_log(home.path(), &json!({ "rule": "R2" }));
    assert!(!home.path().join(LOG_PATH).exists());
}

#[test]
fn the_r5_allow_list_is_per_session_and_empty_by_default() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("shadow.toml");
    assert!(r5_allow(&path, "s-1").is_empty());
    std::fs::write(
        &path,
        "[r5_allow]\n\"s-1\" = [\"gh api -X DELETE repos/a/\"]\n",
    )
    .unwrap();
    assert_eq!(r5_allow(&path, "s-1"), ["gh api -X DELETE repos/a/"]);
    assert!(r5_allow(&path, "s-2").is_empty());
    std::fs::write(&path, "not = = toml").unwrap();
    assert!(r5_allow(&path, "s-1").is_empty());
}
