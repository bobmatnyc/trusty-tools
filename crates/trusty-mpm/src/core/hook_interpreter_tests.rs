//! Tests for `core::hook_interpreter` and `core::hook_interpreter_doctor`
//! (#10263). Every file lives under a temp dir; nothing reads the real home.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::core::doctor::CheckStatus;
use crate::core::doctor_repair::{RepairMode, StepStatus};
use crate::core::hook_interpreter_doctor::{
    HOOK_INTERPRETER_CHECK, NOTIFY_ASSET_CHECK, hook_interpreter_rows, notify_asset_row,
    repair_hook_interpreter,
};

/// A temp dir holding `scripts/<name>` for each table script.
fn seeded() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let scripts = dir.path().join("scripts");
    std::fs::create_dir_all(&scripts).expect("scripts dir");
    let notify = scripts.join("notify-supervisor.sh");
    let datg = scripts.join("datg-hook.sh");
    std::fs::write(&notify, "#!/bin/bash\nexit 0\n").expect("notify");
    std::fs::write(&datg, "#!/bin/sh\nexit 0\n").expect("datg");
    (dir, notify, datg)
}

/// Write `value` as `<dir>/.claude/settings.json` and return its path.
fn settings(dir: &Path, value: &Value) -> PathBuf {
    let claude = dir.join(".claude");
    std::fs::create_dir_all(&claude).expect(".claude");
    let path = claude.join("settings.json");
    std::fs::write(&path, serde_json::to_string_pretty(value).expect("json")).expect("write");
    path
}

/// A settings document with one hook per command under `Notification`.
fn hooks_doc(commands: &[&str]) -> Value {
    let entries: Vec<Value> = commands
        .iter()
        .map(|c| json!({"type": "command", "command": c}))
        .collect();
    json!({
        "model": "opus",
        "permissions": {"allow": ["Bash(ls)"]},
        "hooks": {"Notification": [{"matcher": "", "hooks": entries}]}
    })
}

fn rewrite(cmd: &str) -> (String, bool) {
    match classify_command(cmd) {
        Verdict::Rewrite {
            new,
            missing_session,
        } => (new, missing_session),
        other => panic!("expected a rewrite of {cmd:?}, got {other:?}"),
    }
}

#[test]
fn a_bare_table_script_is_rewritten_with_its_interpreter() {
    let (_dir, notify, datg) = seeded();
    let n = notify.display().to_string();
    let d = datg.display().to_string();
    assert_eq!(rewrite(&n), (format!("/bin/bash \"{n}\""), false));
    assert_eq!(
        rewrite(&format!("{d} tm-dogfood")),
        (format!("/bin/sh \"{d}\" tm-dogfood"), false)
    );
    // A quoted bare path is the same script.
    assert_eq!(rewrite(&format!("\"{n}\"")).0, format!("/bin/bash \"{n}\""));
}

#[test]
fn a_datg_hook_without_a_session_is_flagged() {
    let (_dir, _notify, datg) = seeded();
    let d = datg.display().to_string();
    assert_eq!(rewrite(&d), (format!("/bin/sh \"{d}\""), true));
}

#[test]
fn every_unsafe_shape_is_refused() {
    let (dir, notify, _datg) = seeded();
    let n = notify.display().to_string();
    let other = dir.path().join("scripts").join("statusline-tee.sh");
    std::fs::write(&other, "#!/bin/bash\n").expect("other script");
    let missing = dir.path().join("gone").join("notify-supervisor.sh");
    let cases = [
        (format!("FOO=1 {n}"), "env-var"),
        ("~/scripts/notify-supervisor.sh".to_string(), "`~`"),
        ("scripts/notify-supervisor.sh".to_string(), "relative"),
        (format!("{n} ; rm -rf /"), "metacharacters"),
        (format!("{n} $HOME"), "metacharacters"),
        (format!("{n} a|b"), "metacharacters"),
        (format!("\"{n}\"x"), "metacharacters"),
        (format!("\"{n}"), "unbalanced"),
        (other.display().to_string(), "outside the interpreter table"),
        (missing.display().to_string(), "missing on disk"),
    ];
    for (cmd, want) in cases {
        match classify_command(&cmd) {
            Verdict::Refused(why) => assert!(why.contains(want), "{cmd:?}: {why}"),
            v => panic!("{cmd:?} must be refused, got {v:?}"),
        }
    }
}

#[test]
fn interpreter_shaped_commands_are_clean() {
    let (_dir, notify, datg) = seeded();
    for cmd in [
        format!("/bin/bash \"{}\"", notify.display()),
        format!("/bin/sh {} tm-x", datg.display()),
        "/usr/local/bin/tm hook".to_string(),
        "echo hi".to_string(),
        String::new(),
    ] {
        assert_eq!(classify_command(&cmd), Verdict::Clean, "{cmd:?}");
    }
}

#[test]
fn scan_collects_the_notify_script_in_either_shape() {
    let (dir, notify, _datg) = seeded();
    let n = notify.display().to_string();
    let path = settings(dir.path(), &hooks_doc(&[&n, &format!("/bin/bash \"{n}\"")]));
    let found = scan_settings_file(&path).expect("scan").expect("exists");
    assert_eq!(found.notify_scripts, vec![notify.clone(), notify]);
}

#[test]
fn scan_reports_without_writing() {
    let (dir, notify, _datg) = seeded();
    let n = notify.display().to_string();
    let mut doc = hooks_doc(&[&n]);
    doc["statusLine"] = json!({"type": "command", "command": n});
    let path = settings(dir.path(), &doc);
    let before = std::fs::read(&path).expect("read");
    let found = scan_settings_file(&path).expect("scan").expect("exists");
    assert_eq!(found.rewrites.len(), 2, "hook and statusLine: {found:?}");
    assert_eq!(
        std::fs::read(&path).expect("read"),
        before,
        "nothing written"
    );
    assert_eq!(
        scan_settings_file(&dir.path().join("absent.json")),
        Ok(None)
    );
}

#[test]
fn an_unparseable_file_is_an_error() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "{ not json").expect("write");
    assert!(matches!(
        scan_settings_file(&path),
        Err(FileError::Unparseable(_))
    ));
    std::fs::write(&path, "[1, 2]").expect("write");
    assert!(matches!(
        scan_settings_file(&path),
        Err(FileError::Unparseable(_))
    ));
}

#[test]
fn repair_dry_run_writes_nothing() {
    let (dir, notify, _datg) = seeded();
    let path = settings(dir.path(), &hooks_doc(&[&notify.display().to_string()]));
    let before = std::fs::read(&path).expect("read");
    let steps = repair_hook_interpreter(std::slice::from_ref(&path), RepairMode::DryRun);
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].status, StepStatus::Planned);
    assert_eq!(std::fs::read(&path).expect("read"), before);
    let siblings = std::fs::read_dir(path.parent().expect("parent"))
        .expect("list")
        .count();
    assert_eq!(siblings, 1, "no backup or lock sidecar in a dry run");
}

#[test]
fn repair_applies_backs_up_and_is_idempotent() {
    let (dir, notify, datg) = seeded();
    let n = notify.display().to_string();
    let d = datg.display().to_string();
    let original = hooks_doc(&[&n, &format!("{d} tm-a"), &d, "/usr/local/bin/tm hook"]);
    let path = settings(dir.path(), &original);
    let before = std::fs::read_to_string(&path).expect("read");

    let steps = repair_hook_interpreter(std::slice::from_ref(&path), RepairMode::Apply);
    let backup = match &steps[0].status {
        StepStatus::Applied { backup: Some(b) } => b.clone(),
        other => panic!("expected an applied step with a backup, got {other:?}"),
    };
    assert_eq!(std::fs::read_to_string(&backup).expect("backup"), before);
    // The session-less datg-hook entry is a separate warning, never a guess.
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert!(matches!(steps[1].status, StepStatus::Refused(_)));
    assert!(steps[1].what.contains("no session argument"));

    let after: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
    let cmds: Vec<&str> = after["hooks"]["Notification"][0]["hooks"]
        .as_array()
        .expect("hooks")
        .iter()
        .filter_map(|e| e["command"].as_str())
        .collect();
    assert_eq!(
        cmds,
        vec![
            format!("/bin/bash \"{n}\""),
            format!("/bin/sh \"{d}\" tm-a"),
            format!("/bin/sh \"{d}\""),
            "/usr/local/bin/tm hook".to_string(),
        ]
    );
    // Every other key is untouched.
    let mut expected = original.clone();
    expected["hooks"] = after["hooks"].clone();
    assert_eq!(after, expected);

    let again = repair_hook_interpreter(std::slice::from_ref(&path), RepairMode::Apply);
    assert!(again.is_empty(), "a second run plans nothing: {again:?}");
}

#[test]
fn repair_refuses_an_unparseable_file() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "{ not json").expect("write");
    let steps = repair_hook_interpreter(std::slice::from_ref(&path), RepairMode::Apply);
    assert_eq!(steps.len(), 1);
    assert!(
        matches!(steps[0].status, StepStatus::Refused(_)),
        "{steps:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "{ not json");
}

/// The `hook-interpreter` row out of [`hook_interpreter_rows`].
fn hook_row(files: &[PathBuf]) -> crate::core::doctor::DoctorCheck {
    let rows = hook_interpreter_rows(files);
    assert_eq!(rows[1].name, NOTIFY_ASSET_CHECK);
    rows.into_iter()
        .find(|r| r.name == HOOK_INTERPRETER_CHECK)
        .expect("hook row")
}

#[test]
fn the_row_is_ok_only_when_nothing_is_found() {
    let (dir, notify, _datg) = seeded();
    let n = notify.display().to_string();
    let clean = settings(dir.path(), &hooks_doc(&[&format!("/bin/bash \"{n}\"")]));
    assert_eq!(
        hook_row(std::slice::from_ref(&clean)).status,
        CheckStatus::Ok
    );

    let bare = settings(&dir.path().join("p2"), &hooks_doc(&[&n]));
    let row = hook_row(&[clean, bare]);
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(
        row.message.contains("tm doctor --fix --yes"),
        "{}",
        row.message
    );
}

/// Fail-open guard (#10263): a refusal must never read as a pass.
#[test]
fn a_refusal_keeps_the_row_non_ok() {
    let dir = tempfile::tempdir().expect("dir");
    let path = settings(dir.path(), &hooks_doc(&["scripts/notify-supervisor.sh"]));
    let row = hook_row(&[path]);
    assert_ne!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(row.message.contains("refused"), "{}", row.message);
}

/// Fail-open guard (#10263): an unparseable file must never read as a pass.
#[test]
fn an_unparseable_file_keeps_the_row_non_ok() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "{ not json").expect("write");
    let row = hook_row(&[path]);
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
    assert!(row.message.contains("not scanned"), "{}", row.message);
}

/// Fail-open guard (#10263): a file that exists and cannot be read (here a
/// directory in the settings file's place) must never read as a pass.
#[test]
fn an_unreadable_file_keeps_the_row_non_ok() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("settings.json");
    std::fs::create_dir_all(&path).expect("a directory where the file should be");
    let row = hook_row(std::slice::from_ref(&path));
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
    let steps = repair_hook_interpreter(&[path], RepairMode::Apply);
    assert!(
        matches!(steps[0].status, StepStatus::Failed(_)),
        "{steps:?}"
    );
}

#[test]
fn a_drifted_seed_is_reported_with_both_hashes() {
    let (_dir, notify, _datg) = seeded();
    let before = std::fs::read(&notify).expect("read");
    let row = notify_asset_row(&BTreeSet::from([notify.clone()]), b"shipped bytes\n");
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    let shipped = format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(b"shipped bytes\n")
    );
    let actual = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&before));
    assert!(row.message.contains(&shipped), "{}", row.message);
    assert!(row.message.contains(&actual), "{}", row.message);
    assert_eq!(
        std::fs::read(&notify).expect("read"),
        before,
        "never overwritten"
    );
}

#[test]
fn a_matching_seed_is_ok() {
    let (_dir, notify, _datg) = seeded();
    let bytes = std::fs::read(&notify).expect("read");
    let row = notify_asset_row(&BTreeSet::from([notify]), &bytes);
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    assert_eq!(
        notify_asset_row(&BTreeSet::new(), &bytes).status,
        CheckStatus::Ok
    );
}
