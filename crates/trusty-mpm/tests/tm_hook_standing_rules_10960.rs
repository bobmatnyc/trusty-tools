//! End-to-end proof of `tm hook --standing-rules` (#10960).
//!
//! Why: an Architect lost the owner's standing rules after each `/clear` and
//! resume. Only the built binary proves the hook reads the launch stamp, the
//! allowlist and `local/standing-rules.md`, and answers Claude Code in the
//! shape that puts the rules into context.
//! What: spawns the hook with a scratch `$HOME` (its config is the Architect
//! allowlist), `CLAUDE_PROJECT_DIR` naming a temp Architect project and an
//! unreachable daemon, on `SessionStart` (resume) and `UserPromptSubmit`
//! payloads. Every call must exit 0.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_standing_rules_10960::`.

use crate::common;

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

/// The rules every test writes.
const RULES: &str = "- Every reply to the owner carries one Fable-use line.\n\
                     - Ordinary cases first; no extra verification paperwork.\n";

/// A session's launch-time inputs.
struct Session {
    project: PathBuf,
    home: tempfile::TempDir,
    /// `TRUSTY_MPM_SESSION_PROFILE`, when stamped.
    stamp: Option<&'static str>,
    _dir: tempfile::TempDir,
}

/// A project granted the Architect profile (file, allowlist and stamp) when
/// `architect`, else a PM project, with `rules` at `local/standing-rules.md`.
fn session(architect: bool, rules: Option<&[u8]>) -> Session {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("architect");
    std::fs::create_dir_all(project.join("local")).expect("mkdir local");
    let profile = if architect { "supervisor" } else { "pm" };
    std::fs::write(
        project.join(".trusty-mpm.toml"),
        format!("profile = \"{profile}\"\n"),
    )
    .expect("write project config");
    if let Some(rules) = rules {
        std::fs::write(project.join("local/standing-rules.md"), rules).expect("write rules");
    }
    let home = tempfile::tempdir().expect("home");
    let root = home.path().join(".trusty-mpm");
    std::fs::create_dir_all(&root).expect("mkdir root");
    if architect {
        std::fs::write(
            root.join("config.toml"),
            format!(
                "[architect]\nprojects = [{:?}]\n",
                project.display().to_string()
            ),
        )
        .expect("write user config");
    }
    Session {
        project,
        home,
        stamp: architect.then_some("supervisor"),
        _dir: dir,
    }
}

/// Run the hook for `s` on `event`; return (stdout, stderr). Asserts exit 0.
fn run(s: &Session, event: &str) -> (String, String) {
    let payload = serde_json::json!({
        "hook_event_name": event,
        "session_id": "s-10960",
        "cwd": s.project.display().to_string(),
        "source": "resume",
        "prompt": "what is your mandate?",
    })
    .to_string();
    let mut cmd = common::tm_command_in(s.home.path());
    cmd.args(["--url", "http://127.0.0.1:1", "hook", "--standing-rules"])
        .current_dir(&s.project)
        .env("CLAUDE_PROJECT_DIR", &s.project)
        .env_remove("TRUSTY_MPM_SESSION_PROFILE")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT");
    if let Some(stamp) = s.stamp {
        cmd.env("TRUSTY_MPM_SESSION_PROFILE", stamp);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tm hook --standing-rules`");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "the hook exits 0: {out:?}");
    (
        String::from_utf8(out.stdout).expect("utf-8 stdout"),
        String::from_utf8(out.stderr).expect("utf-8 stderr"),
    )
}

/// The parsed JSON response line on `stdout`.
fn response(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout:?}"))
}

#[test]
fn an_architect_session_gets_the_rules_at_resume_and_per_turn() {
    let s = session(true, Some(RULES.as_bytes()));
    for event in ["SessionStart", "UserPromptSubmit"] {
        let (stdout, stderr) = run(&s, event);
        let out = response(&stdout);
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], event);
        let context = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("additionalContext");
        assert!(
            context.ends_with(RULES),
            "verbatim rules on {event}: {context}"
        );
        assert!(out.get("systemMessage").is_none(), "{out}");
        assert!(!stderr.contains("standing rules"), "{stderr}");
    }
}

#[test]
fn a_pm_session_gets_no_rules() {
    let s = session(false, Some(RULES.as_bytes()));
    for event in ["SessionStart", "UserPromptSubmit"] {
        let (stdout, stderr) = run(&s, event);
        assert_eq!(stdout.trim(), "", "{event}");
        assert!(!stderr.contains("standing rules"), "{stderr}");
    }
}

#[test]
fn an_absent_rules_file_is_a_silent_no_op() {
    let s = session(true, None);
    let (stdout, stderr) = run(&s, "UserPromptSubmit");
    assert_eq!(stdout.trim(), "");
    assert!(!stderr.contains("standing rules"), "{stderr}");
}

#[test]
fn an_over_cap_rules_file_warns_and_injects_at_most_the_cap() {
    let big = "- a rule that the owner keeps adding to the standing block\n".repeat(60);
    let s = session(true, Some(big.as_bytes()));
    let (stdout, stderr) = run(&s, "UserPromptSubmit");
    let out = response(&stdout);
    let warning = out["systemMessage"].as_str().expect("a visible warning");
    assert!(warning.contains("over the 2048-byte cap"), "{warning}");
    assert!(stderr.contains("over the 2048-byte cap"), "{stderr}");
    let context = out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("the lines within the cap");
    let injected = context.split_once('\n').map_or("", |(_, rest)| rest);
    assert!(
        injected.len() <= 2048 && injected.ends_with('\n'),
        "{injected:?}"
    );
    assert!(big.starts_with(injected));
}

#[test]
fn an_unreadable_rules_file_warns_and_the_turn_proceeds() {
    let s = session(true, Some(b"- rule\n\xff\xfe\n"));
    for event in ["SessionStart", "UserPromptSubmit"] {
        let (stdout, stderr) = run(&s, event);
        let out = response(&stdout);
        assert!(
            out.get("hookSpecificOutput").is_none(),
            "no partial rules: {out}"
        );
        assert!(out.get("decision").is_none() && out.get("continue").is_none());
        assert!(
            out["systemMessage"]
                .as_str()
                .is_some_and(|m| m.contains("not valid UTF-8")),
            "{out}"
        );
        assert!(stderr.contains("could not be read"), "{stderr}");
    }
}
