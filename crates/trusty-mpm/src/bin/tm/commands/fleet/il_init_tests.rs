//! Tests for the Architect's Intent Ledger and hooks on install and upgrade
//! (#10571, #10579). They run the seeded `datg.py`, so they need `python3`.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use super::super::Step;
use super::super::il_init::{self, hook_command, state_path};
use super::Fixture;

fn settings(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap())
        .unwrap()
}

/// Every hook command under `event`.
fn commands(dir: &Path, event: &str) -> Vec<String> {
    settings(dir)["hooks"][event]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
        .filter_map(|h| h["command"].as_str().map(str::to_owned))
        .collect()
}

/// Run the installed hook script (no session filter) with `stdin` for `il`.
fn hook(dir: &Path, il: &Path, stdin: &str) -> (bool, String) {
    let mut child = Command::new("/bin/sh")
        .arg(dir.join("scripts/datg-hook.sh"))
        .arg("")
        .arg(il)
        .env_remove("TMUX_PANE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn datg(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("python3")
        .arg(dir.join("scripts/datg.py"))
        .arg("--project")
        .arg(dir.join(il_init::IL_DIR))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn a_fresh_install_creates_the_ledger_and_both_hooks() {
    let fx = Fixture::new();
    let report = fx.init().unwrap();
    let dir = fx.dir();
    assert!(state_path(&dir).is_file(), "{}", report.render());
    assert!(report.steps.contains(&Step::Changed(
        "created the Intent Ledger in local/il".to_owned()
    )));
    let want = hook_command(&dir, "tm-architect");
    for event in il_init::EVENTS {
        assert_eq!(commands(&dir, event), vec![want.clone()], "{event}");
    }
    assert!(
        !want.contains('$') && want.starts_with("/bin/sh /"),
        "{want}"
    );
    assert!(
        want.ends_with("/local/il") && want.contains(" tm-architect "),
        "{want}"
    );
    for event in il_init::EVENTS {
        let entry = &settings(&dir)["hooks"][event][0];
        assert!(
            entry.get("matcher").is_none(),
            "no matcher, so `clear` fires it too"
        );
    }
}

#[test]
fn an_upgrade_of_a_pre_ledger_install_adds_the_ledger_hooks_and_hold() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    // A pre-ledger shape: no state.json, a HOLD in state.md, the old wording, a hook of the owner's own.
    std::fs::remove_dir_all(dir.join("local/il")).unwrap();
    let user = r#"{"model":"opus","hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo mine"}]}]}}"#;
    std::fs::write(dir.join(".claude/settings.json"), user).unwrap();
    std::fs::write(
        dir.join("local/state.md"),
        "# State\n- HOLD: no merges until Monday\n- task: the P1 bundle\n- next: the sandbox check\n",
    )
    .unwrap();
    let policy =
        "Rule 1 stays.\nBefore a clear, take a pause snapshot or palace note.\nRule 3 stays.\n";
    std::fs::write(dir.join("local/usage-policy.md"), policy).unwrap();
    let report = fx.init().unwrap();
    assert!(!report.failed(), "{}", report.render());
    assert!(state_path(&dir).is_file());
    let st: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(state_path(&dir)).unwrap()).unwrap();
    assert_eq!(st["session"]["hold"], "no merges until Monday");
    assert_eq!(
        commands(&dir, "Stop"),
        vec!["echo mine".to_owned(), hook_command(&dir, "tm-architect")]
    );
    assert_eq!(settings(&dir)["model"], "opus");
    assert_eq!(
        std::fs::read_to_string(dir.join("local/usage-policy.md")).unwrap(),
        "Rule 1 stays.\nBefore a clear, take a write the IL with datg.py session set / digest.\nRule 3 stays.\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("local/state.md")).unwrap(),
        "# State\n- HOLD: no merges until Monday\n- task: the P1 bundle\n- next: the sandbox check\n",
        "state.md is never rewritten"
    );
}

#[test]
fn an_upgrade_keeps_existing_ledger_state_byte_identical_and_reruns_change_nothing() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    assert!(
        datg(
            &dir,
            &[
                "add", "G", "--title", "Keep me", "--rank", "1", "--finish", "kept"
            ]
        )
        .status
        .success()
    );
    let before = std::fs::read(state_path(&dir)).unwrap();
    let settings_before = std::fs::read(dir.join(".claude/settings.json")).unwrap();
    let again = fx.init().unwrap();
    assert_eq!(std::fs::read(state_path(&dir)).unwrap(), before);
    assert_eq!(
        std::fs::read(dir.join(".claude/settings.json")).unwrap(),
        settings_before
    );
    assert!(!again.changed(), "{}", again.render());
    for event in il_init::EVENTS {
        assert_eq!(commands(&dir, event).len(), 1, "{event} duplicated");
    }
}

#[test]
fn an_edited_ledger_hook_is_kept() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    let edited = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/bin/sh scripts/datg-hook.sh mine local/il"}]}],"SessionStart":[]}}"#;
    std::fs::write(dir.join(".claude/settings.json"), edited).unwrap();
    fx.init().unwrap();
    assert_eq!(
        commands(&dir, "Stop"),
        vec!["/bin/sh scripts/datg-hook.sh mine local/il".to_owned()]
    );
    assert_eq!(
        commands(&dir, "SessionStart"),
        vec![hook_command(&dir, "tm-architect")]
    );
}

#[test]
fn the_hooks_stamp_the_turn_print_the_digest_and_never_block() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    let il = dir.join(il_init::IL_DIR);
    // (a) Stop writes the stamp; `fresh` then reports it.
    let (ok, _) = hook(&dir, &il, r#"{"hook_event_name":"Stop","session_id":"s"}"#);
    assert!(ok);
    let fresh = datg(&dir, &["fresh"]);
    assert!(
        fresh.status.success(),
        "{}",
        String::from_utf8_lossy(&fresh.stderr)
    );
    assert!(String::from_utf8_lossy(&fresh.stdout).contains("fresh"));
    // (b) SessionStart, source clear: the digest with its SESSION and GOALS blocks.
    let (ok, out) = hook(
        &dir,
        &il,
        r#"{"hook_event_name":"SessionStart","source":"clear"}"#,
    );
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.contains("SESSION") && ctx.contains("GOALS"), "{ctx}");
    // (c) A missing ledger: exit 0, nothing harmful printed.
    let missing = dir.join("local/no-il");
    for event in ["Stop", "SessionStart"] {
        let (ok, out) = hook(
            &dir,
            &missing,
            &format!(r#"{{"hook_event_name":"{event}"}}"#),
        );
        assert!(ok && out.trim().is_empty(), "{event}: {out}");
    }
}

#[test]
fn hold_lines_are_read_from_state_md() {
    let read = |s: &str| il_init::hold_in_state_md(s).unwrap();
    assert_eq!(read("x\n- HOLD: no merges\n").as_deref(), Some("no merges"));
    assert_eq!(
        read("**HOLD**: wait for the reviewer\n").as_deref(),
        Some("wait for the reviewer")
    );
    assert_eq!(read("- HOLD: none\n"), None);
    assert_eq!(read("holding pattern\n"), None);
    assert_eq!(read("- usage state: normal\n"), None);
}

/// The fixture of record: a synthetic `local/state.md` in the shape of a real
/// pre-ledger install, with every hold kind the classifier must sort. It carries
/// an active usage HOLD (line 26), a lifted dispatch hold (line 8), a historical
/// D03 mention (line 87), a historical builder band (line 31), a closed
/// QUESTION 17 entry (line 95) and the QUESTION 17 C no-merge rule (lines 36
/// and 117).
const SAMPLE_STATE: &str = include_str!("il_init_sample_state.md");

fn reinit_with_state(state: &[u8]) -> (Fixture, std::path::PathBuf, super::super::InitReport) {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    std::fs::remove_dir_all(dir.join("local/il")).unwrap();
    std::fs::write(dir.join("local/state.md"), state).unwrap();
    let report = fx.init().unwrap();
    (fx, dir, report)
}

fn session(dir: &Path) -> serde_json::Value {
    let st: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(state_path(dir)).unwrap()).unwrap();
    st["session"].clone()
}

#[test]
fn the_state_md_fixture_of_record_classifies_every_hold() {
    let holds = il_init::holds_in_state_md(SAMPLE_STATE).unwrap();
    // Active usage HOLD (line 26) with its context lines 27-30.
    let usage = holds.usage.clone().unwrap();
    assert!(
        usage.starts_with("HOLD since 2026-01-01T01:49Z, kept on reviewer instruction."),
        "{usage}"
    );
    assert!(
        usage.contains("projection 94.1%") && usage.contains("processing answers proceed."),
        "{usage}"
    );
    // The no-merge rule (QUESTION 17 C) is its own entry, never folded into the usage HOLD.
    let merge = holds.merge.clone().unwrap();
    assert!(
        merge.contains("On hold (owner, QUESTION 17 C): no merge now."),
        "{merge}"
    );
    assert!(!usage.contains("merge"), "{usage}");
    // Lifted (line 8), historical D03 (line 87) and builder band (line 31) are not carried.
    for (text, what) in [
        ("DISPATCH HOLD LIFTED", "lifted"),
        ("deferred under usage HOLD", "historical D03"),
        ("STOP", "builder band"),
    ] {
        assert!(
            !usage.contains(text) && !merge.contains(text),
            "{what} carried"
        );
    }
    // Through init: the migration completes and the ledger carries both holds and the work.
    let (_fx, dir, report) = reinit_with_state(SAMPLE_STATE.as_bytes());
    assert!(!report.failed(), "{}", report.render());
    let ses = session(&dir);
    assert_eq!(ses["hold"].as_str(), Some(usage.as_str()));
    assert_eq!(ses["merge_hold"].as_str(), Some(merge.as_str()));
    assert!(
        ses["task"]
            .as_str()
            .unwrap()
            .starts_with("Waiting for the owner's #12 Q40/Q41 answer")
    );
    assert!(
        ses["next"]
            .as_str()
            .unwrap()
            .starts_with("Record the #12 answer the reviewer relays")
    );
    // No open goal or task item yet, so a clear stays blocked.
    let ready = datg(&dir, &["ready"]);
    assert_eq!(ready.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&ready.stdout).contains("no open goal or task item"));
}

#[test]
fn a_hold_with_no_task_or_next_migrates_with_placeholders_that_block_a_clear() {
    let (_fx, dir, report) = reinit_with_state(b"# State\n- usage state: HOLD since 01:49Z\n");
    assert!(!report.failed(), "{}", report.render());
    assert!(
        report.steps.iter().any(|s| matches!(s, Step::Skipped(t)
            if t.contains("Nothing is needed from you") && !t.contains("datg.py") && !t.contains("--"))),
        "{}",
        report.render()
    );
    let ses = session(&dir);
    assert_eq!(ses["hold"], "HOLD since 01:49Z");
    assert!(
        ses["task"]
            .as_str()
            .unwrap()
            .starts_with("needs owner input:")
    );
    assert!(
        ses["next"]
            .as_str()
            .unwrap()
            .starts_with("needs owner input:")
    );
    let ready = datg(&dir, &["ready"]);
    let out = String::from_utf8_lossy(&ready.stdout);
    assert_eq!(ready.status.code(), Some(1));
    assert!(
        out.contains("no task recorded")
            && out.contains("Set the current task and next step: run python3 scripts/datg.py"),
        "{out}"
    );
}

#[test]
fn lowercase_holds_are_never_dropped() {
    let read = |s: &str| il_init::holds_in_state_md(s).unwrap();
    assert_eq!(
        read("- hold: wait for the reviewer\n").usage.as_deref(),
        Some("wait for the reviewer")
    );
    assert!(
        read("- hold all merges\n")
            .merge
            .unwrap()
            .contains("hold all merges")
    );
    assert!(
        il_init::holds_in_state_md("- we hold the release\n").is_err(),
        "unclassifiable"
    );
    assert_eq!(read("- holding pattern\n"), il_init::Holds::default());
}

#[test]
fn an_unreadable_state_md_stops_before_the_ledger() {
    let (_fx, dir, report) = reinit_with_state(&[0xff, 0xfe, b'H', b'O', b'L', b'D', b'\n']);
    assert!(report.failed(), "{}", report.render());
    assert!(
        report.steps.iter().any(
            |s| matches!(s, Step::Failed(t) if t.starts_with("local/state.md could not be read"))
        ),
        "{}",
        report.render()
    );
    assert!(!state_path(&dir).exists());
}

#[test]
fn ambiguous_hold_text_fails_before_the_ledger_is_created() {
    for (text, line) in [
        ("# State\n- usage: we are on hold until Monday\n", 2),
        ("# State\n\nThe HOLD is in place\n", 3),
    ] {
        assert_eq!(
            il_init::hold_in_state_md(text).unwrap_err().0,
            line,
            "{text}"
        );
        let fx = Fixture::new();
        fx.init().unwrap();
        let dir = fx.dir();
        std::fs::remove_dir_all(dir.join("local/il")).unwrap();
        std::fs::write(dir.join("local/state.md"), text).unwrap();
        let report = fx.init().unwrap();
        assert!(report.failed(), "{}", report.render());
        assert!(
            report.steps.iter().any(|s| matches!(s, Step::Failed(t)
                if t.starts_with(&format!("local/state.md line {line} mentions a hold")))),
            "{}",
            report.render()
        );
        assert!(!state_path(&dir).exists(), "no ledger without its HOLD");
    }
}

/// Critic HIGH: a re-init never overwrites the ledger's own task and next with
/// placeholders; the new HOLD is still recorded.
#[test]
fn a_reinit_keeps_the_ledgers_task_and_next_and_records_the_hold() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    assert!(
        datg(
            &dir,
            &["session", "set", "--task", "X task", "--next", "X next"]
        )
        .status
        .success()
    );
    std::fs::write(
        dir.join("local/state.md"),
        "# State\n- usage state: HOLD since 01:49Z\n",
    )
    .unwrap();
    let report = fx.init().unwrap();
    assert!(!report.failed(), "{}", report.render());
    let ses = session(&dir);
    assert_eq!(
        (ses["task"].as_str(), ses["next"].as_str()),
        (Some("X task"), Some("X next"))
    );
    assert_eq!(ses["hold"], "HOLD since 01:49Z");
}

/// Critic MEDIUM: holds that cannot be recorded, because the ledger cannot be
/// read, fail the run instead of vanishing.
#[test]
fn holds_with_an_unreadable_ledger_fail_visibly() {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    std::fs::write(state_path(&dir), "{not json").unwrap();
    std::fs::write(dir.join("local/state.md"), "# State\n- HOLD: no merges\n").unwrap();
    let report = fx.init().unwrap();
    assert!(
        report.steps.iter().any(|s| matches!(s, Step::Failed(t)
            if t.starts_with("could not read the Intent Ledger to record the holds"))),
        "{}",
        report.render()
    );
}
