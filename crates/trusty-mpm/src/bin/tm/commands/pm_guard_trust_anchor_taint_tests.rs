//! #8433 P0: the channel taint directory is a trust anchor for every session.

use super::super::tests::{Fixture, allowlist, architect_env, fixture, payload, pm_env};
use super::super::{HookEnv, evaluate};
use serde_json::{Value, json};
use trusty_mpm::core::channels_taint::{TaintMark, taint_dir, taint_managed_session};
use trusty_mpm::core::config::MpmConfig;

/// [`fixture`] with one live taint: a file for `s-1` and its managed mark.
fn tainted() -> Fixture {
    let fx = fixture();
    let mark = TaintMark {
        channel: "slack".into(),
        route: "owner-dm".into(),
        question_id: "Q-1".into(),
        tainted_at: chrono::Utc::now(),
        workspace: std::path::PathBuf::from("/"),
    };
    taint_managed_session(&taint_dir(&fx.home), "m-1", "s-1", &mark).expect("taint");
    fx
}

fn verdict(fx: &Fixture, env: &HookEnv, tool: &str, input: Value) -> Option<String> {
    evaluate(&payload(fx, tool, input), env, || allowlist(fx))
}

fn bash(fx: &Fixture, env: &HookEnv, command: &str) -> Option<String> {
    verdict(fx, env, "Bash", json!({ "command": command }))
}

const FILE: &str = "~/.trusty-tools/trusty-mpm/channels/taint/s-1.json";

/// 🔴 #8433: `rm`, `unlink` and a glob delete of a taint file are denied.
#[test]
fn anchor_denies_rm_of_taint_file() {
    let fx = tainted();
    let env = pm_env(&fx);
    for command in [
        format!("rm {FILE}"),
        format!("rm -f {FILE}"),
        format!("unlink {FILE}"),
        "rm -rf ~/.trusty-tools/trusty-mpm/channels/taint/*".to_owned(),
        "rm ~/.trusty-tools/trusty-mpm/channels/taint/managed/m-1.json".to_owned(),
        "cd ~/.trusty-tools/trusty-mpm/channels/taint && rm s-1.json".to_owned(),
    ] {
        assert!(bash(&fx, &env, &command).is_some(), "allowed: {command}");
    }
}

/// 🔴 #8433: writing, moving or replacing the taint directory or a file in it
/// is denied — Edit/Write, a redirect, `cp`, `mv` of the file and of the dir.
#[test]
fn anchor_denies_write_and_mv_of_taint_dir() {
    let fx = tainted();
    let env = pm_env(&fx);
    let path = taint_dir(&fx.home).join("s-1.json");
    for tool in ["Write", "Edit", "MultiEdit"] {
        let input = json!({ "file_path": path.display().to_string(), "content": "{}" });
        assert!(verdict(&fx, &env, tool, input).is_some(), "allowed: {tool}");
    }
    for command in [
        format!("echo x > {FILE}"),
        format!("cp /dev/null {FILE}"),
        format!("mv {FILE} /tmp/away.json"),
        "mv ~/.trusty-tools/trusty-mpm/channels/taint /tmp/away".to_owned(),
        "mv ~/.trusty-tools/trusty-mpm/channels /tmp/away".to_owned(),
        "rm -rf ~/.trusty-tools/trusty-mpm/channels".to_owned(),
        "ln -sf /tmp/x ~/.trusty-tools/trusty-mpm/channels/taint/s-2.json".to_owned(),
    ] {
        assert!(bash(&fx, &env, &command).is_some(), "allowed: {command}");
    }
    assert_eq!(
        bash(&fx, &env, "echo x > notes.md"),
        None,
        "an unrelated write"
    );
}

/// 🔴 #8433: emptying a taint file in place is denied (redirect, `truncate`).
#[test]
fn anchor_denies_truncate_of_taint_file() {
    let fx = tainted();
    let env = pm_env(&fx);
    for command in [
        format!(": > {FILE}"),
        format!("truncate -s 0 {FILE}"),
        format!("cat /dev/null > {FILE}"),
    ] {
        assert!(bash(&fx, &env, &command).is_some(), "allowed: {command}");
    }
}

/// 🔴 #8433: the Architect's main thread, exempt from the #8878 anchors, may
/// not untaint itself while a taint is live; with none live its writes are
/// judged as before.
#[test]
fn the_architect_cannot_write_a_live_taint() {
    let fx = tainted();
    let env = architect_env(&fx);
    let reason = bash(&fx, &env, &format!("rm {FILE}")).expect("the Architect is denied");
    assert!(reason.contains("#8433"), "{reason}");
    let clean = fixture();
    let env = architect_env(&clean);
    assert_eq!(
        bash(&clean, &env, "rm -rf ~/.trusty-tools/trusty-mpm/channels"),
        None
    );
    assert_eq!(
        evaluate(
            &payload(&clean, "Bash", json!({ "command": "echo x > notes.md" })),
            &pm_env(&clean),
            MpmConfig::default
        ),
        None
    );
}

/// 🔴 #8433 (r-078): while a taint is live, a write the guard cannot place
/// (`xargs cp`) is denied to the Architect too.
#[test]
fn the_architect_cannot_untaint_through_an_unplaceable_write() {
    let fx = tainted();
    let env = architect_env(&fx);
    let reason = bash(&fx, &env, "ls | xargs cp a.txt").expect("denied");
    assert!(reason.contains("#8433"), "{reason}");
    let clean = fixture();
    let env = architect_env(&clean);
    assert_eq!(
        bash(&clean, &env, "ls | xargs cp a.txt"),
        None,
        "no taint, no rule"
    );
}

/// 🔴 #8433 (r-078): a taint directory that does not resolve (a link loop)
/// still reads live, fences everything under it and guards the directories
/// above it.
#[test]
fn an_unresolvable_taint_dir_still_guards_the_dirs_above_it() {
    let fx = fixture();
    let channels = fx.home.join(".trusty-tools/trusty-mpm/channels");
    std::fs::create_dir_all(&channels).expect("mkdir");
    std::os::unix::fs::symlink("taint", channels.join("taint")).expect("loop");
    let env = architect_env(&fx);
    for command in [
        "rm -rf ~/.trusty-tools/trusty-mpm/channels",
        "mv ~/.trusty-tools/trusty-mpm/channels /tmp/away",
        "echo x > ~/.trusty-tools/trusty-mpm/channels/taint/s.json",
    ] {
        assert!(bash(&fx, &env, command).is_some(), "allowed: {command}");
    }
}

/// #8433 (D09): the hook's double-failure record sits in the taint dir, so
/// no session may delete it.
#[test]
fn anchor_denies_rm_of_the_reapply_failures_record() {
    let fx = fixture();
    let dir = taint_dir(&fx.home);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join(trusty_mpm::core::channels_taint::REAPPLY_FAILURES_FILE),
        "{}\n",
    )
    .expect("record");
    let command = "rm ~/.trusty-tools/trusty-mpm/channels/taint/reapply-failures.jsonl";
    assert!(bash(&fx, &pm_env(&fx), command).is_some(), "a PM");
    assert!(
        bash(&fx, &architect_env(&fx), command).is_some(),
        "the Architect"
    );
}
