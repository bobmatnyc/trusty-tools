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

/// #11431 review: no argument value reaches the log, only names and counts.
#[test]
fn the_log_holds_no_argument_values() {
    let home = tempfile::tempdir().unwrap();
    for (cmd, rule) in [
        ("git push --force https://u:ghp_X@github.com/o/r main", "R2"),
        ("kubectl delete pod p --token=abc", "R5"),
        (
            "gh api -X DELETE -H \"Authorization: token X\" repos/o/r",
            "R5",
        ),
    ] {
        let raw =
            json!({ "session_id": "s", "tool_name": "Bash", "tool_input": { "command": cmd } });
        let line = judge(raw.to_string().as_bytes(), home.path(), &test_env).expect(cmd);
        assert_eq!(line["rule"], rule, "{cmd}");
        let text = line.to_string();
        for secret in [
            "ghp_X",
            "u:",
            "abc",
            "Authorization",
            "token X",
            "o/r",
            "DELETE",
        ] {
            assert!(!text.contains(secret), "{secret:?} leaked by {cmd}: {text}");
        }
    }
    let raw =
        json!({ "tool_name": "Bash", "tool_input": { "command": "git push --force origin main" } });
    let line = judge(raw.to_string().as_bytes(), home.path(), &test_env).unwrap();
    assert_eq!(line["fact"]["branch"], "main");
    assert_eq!(line["fact"]["subcommand"], "push");
    assert_eq!(line["fact"]["flags"], json!(["--force"]));
    assert_eq!(line["fact"]["positional_count"], 3);
}

/// #11431: a token in a URL query string never reaches the log.
#[test]
fn the_log_drops_url_query_tokens() {
    let home = tempfile::tempdir().unwrap();
    let repo = home.path().join("session");
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
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .status();
        assert!(ok.is_ok_and(|s| s.success()));
    }
    let env = |_: &Value, home: &Path| GuardEnv {
        project_dir: Some(home.join("session")),
        cwd: home.join("session"),
        ..test_env(&Value::Null, home)
    };
    let cmd = "git push https://github.com/o/r.git?access_token=X main";
    let raw = json!({ "tool_name": "Bash", "tool_input": { "command": cmd } });
    let line = judge(raw.to_string().as_bytes(), home.path(), &env).expect("R3");
    assert_eq!(line["rule"], "R3");
    assert_eq!(
        line["fact"]["remote_repo"], "o/r",
        "query cut before the slug"
    );
    let text = line.to_string().to_lowercase();
    assert!(!text.contains("access_token"), "{text}");
    assert!(!text.contains("=x"), "{text}");
}

/// #11431: a symlink planted at the log path is not followed.
#[test]
fn a_symlink_at_the_log_path_is_not_followed() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let victim = home.path().join("victim.txt");
    std::fs::write(&victim, "keep").unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
    let path = home.path().join(LOG_PATH);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&victim, &path).unwrap();
    append_log(home.path(), &json!({ "rule": "R2" }));
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    let mode = std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o644);
}

/// #11431 review: the log is 0600 in a 0700 directory and rotates once past 10 MiB.
#[test]
fn the_log_is_private_and_rotates_at_the_cap() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(LOG_PATH);
    append_log(home.path(), &json!({ "rule": "R2" }));
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_LOG_BYTES + 1)
        .unwrap();
    append_log(home.path(), &json!({ "rule": "R5" }));
    assert!(path.with_extension("jsonl.1").exists(), "rotated once");
    assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
    assert_eq!(mode(&path), 0o600);
}
