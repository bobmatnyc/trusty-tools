//! Integration test: the egress guard taints a Claude id no SessionStart
//! reached, when a live owner-answer mark covers its launch directory, on its
//! first gated call (#9923, #8433 P6).
//!
//! Why: a new id whose SessionStart never reached the daemon has no seen line
//! and no taint file, so `deliver` taints only the record's old id while the
//! pane runs the new one. Only the guard, at the point of use, closes that
//! (option (c), owner ruling Def; Architect Q10 keys it on the launch dir).
//! What: a temp HOME holds managed session [`MANAGED`]'s mark, scoped to `ws`,
//! and the record id [`OLD`]'s taint. The built `tm hook --egress-guard` runs a
//! `Read` for the hook-less id [`NEW`], with `CLAUDE_PROJECT_DIR` as the launch
//! directory. Cases: launched in `ws` (deny, `NEW.json` attributed to the mark,
//! and a retire clears it); launched in `ws` then `cd` out (deny); launched
//! outside with or without a mark (allow, nothing written); no launch
//! directory, a mark that cannot be read, marks that cannot be listed, and a
//! taint write that fails (each deny).
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_egress_lazy_taint_9923::`.

use crate::common;

use std::io::Write;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use trusty_mpm::core::channels_taint::{self, TaintMark, TaintRecord, TaintState};

/// The loopback URL nothing listens on: the policy load runs inline.
const LOOPBACK: &str = "http://127.0.0.1:1";
/// The managed session the owner's answer was delivered to.
const MANAGED: &str = "m-9923";
/// The Claude id the managed record holds, tainted by `deliver`.
const OLD: &str = "550e8400-e29b-41d4-a716-446655449923";
/// The id the pane runs now; its SessionStart never reached the daemon.
const NEW: &str = "550e8400-e29b-41d4-a716-446655449924";

/// A canonical temp root: `home` (with the mark), `ws` (the marked
/// workspace), `out` (a directory no mark covers) and `data`.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    ws: PathBuf,
    out: PathBuf,
}

impl Fixture {
    /// The tree, with no mark under `home`.
    fn bare() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical");
        let (home, ws, out) = (root.join("home"), root.join("ws"), root.join("out"));
        for dir in [&home, &ws, &out, &root.join("data")] {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        Self {
            _tmp: tmp,
            root,
            home,
            ws,
            out,
        }
    }

    /// The tree, with [`MANAGED`] marked for `ws` and [`OLD`] tainted.
    fn marked() -> Self {
        let fx = Self::bare();
        let mark = TaintMark {
            channel: "slack".into(),
            route: "owner-dm".into(),
            question_id: "Q-9923".into(),
            tainted_at: chrono::Utc::now(),
            workspace: fx.ws.clone(),
        };
        channels_taint::taint_managed_session(&fx.dir(), MANAGED, OLD, &mark).expect("taint");
        fx
    }

    fn dir(&self) -> PathBuf {
        channels_taint::taint_dir(&self.home)
    }

    /// [`NEW`]'s taint file.
    fn new_taint(&self) -> PathBuf {
        channels_taint::taint_path(&self.dir(), NEW).expect("path")
    }

    /// The guard's output for a `Read` from [`NEW`]: launched in `launch`
    /// (`None`: no `CLAUDE_PROJECT_DIR`), its live cwd `cwd`.
    fn guard(&self, launch: Option<&Path>, cwd: &Path) -> Output {
        let payload = serde_json::json!({
            "session_id": NEW,
            "hook_event_name": "PreToolUse",
            "cwd": cwd.display().to_string(),
            "tool_name": "Read",
            "tool_input": { "file_path": cwd.join("notes.md").display().to_string() },
        });
        let mut cmd = common::tm_command_in(&self.home);
        cmd.args(["--url", LOOPBACK, "hook", "--egress-guard"])
            .current_dir(cwd)
            .env("TRUSTY_DATA_DIR_OVERRIDE", self.root.join("data"))
            .env_remove("CLAUDE_PROJECT_DIR")
            .env_remove("TRUSTY_MPM_SESSION_PROFILE")
            .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
            .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
            .env_remove("CLAUDE_MPM_SUB_AGENT")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(launch) = launch {
            cmd.env("CLAUDE_PROJECT_DIR", launch);
        }
        let mut child = cmd.spawn().expect("spawn tm hook --egress-guard");
        let mut pipe = child.stdin.take().expect("stdin");
        let _ = pipe.write_all(payload.to_string().as_bytes());
        drop(pipe);
        let out = child.wait_with_output().expect("wait");
        assert!(out.status.success(), "the egress guard exits 0: {out:?}");
        out
    }
}

/// The #9923 deny reason in `out`, or a panic naming `case`.
fn deny_reason(out: &Output, case: &str) -> String {
    let reason = any_deny_reason(out, case);
    assert!(reason.contains("#9923"), "{case}: {reason}");
    reason
}

/// The deny reason in `out`, whatever its cause, or a panic naming `case`.
fn any_deny_reason(out: &Output, case: &str) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("{case}: no deny JSON ({e}): {stdout:?}"));
    assert_eq!(
        value["hookSpecificOutput"]["permissionDecision"], "deny",
        "{case}: {value}"
    );
    value["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Assert `out` is an allow: exit 0, nothing printed.
fn assert_allowed(out: &Output, case: &str) {
    assert!(
        out.stdout.is_empty(),
        "{case}: the guard printed a decision: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// 🔴 #9923: a hook-less id launched in the marked workspace is denied on its
/// first gated call, its taint file is written for the mark's managed
/// session, and that session's retire removes it.
#[test]
fn a_hook_less_id_launched_under_a_live_mark_is_denied_and_tainted_for_the_mark() {
    let fx = Fixture::marked();
    assert_eq!(
        channels_taint::read_state(&fx.dir(), NEW),
        TaintState::Untainted,
        "fixture: no SessionStart reached the daemon for the new id"
    );
    let reason = deny_reason(&fx.guard(Some(&fx.ws), &fx.ws), "launched in ws");
    assert!(reason.contains("channel-tainted"), "{reason}");
    let record: TaintRecord =
        serde_json::from_slice(&std::fs::read(fx.new_taint()).expect("NEW.json written"))
            .expect("a taint record");
    assert_eq!(record.session_id, NEW);
    assert_eq!(record.managed_session, MANAGED);
    assert_eq!(record.workspace.as_deref(), Some(fx.ws.as_path()));
    // Later calls hit the file.
    let again = any_deny_reason(&fx.guard(Some(&fx.ws), &fx.ws), "second call");
    assert!(again.contains("Tainted(Present)"), "{again}");

    channels_taint::retire(&fx.dir(), MANAGED).expect("retire");
    assert!(!fx.new_taint().exists(), "the retire removes NEW.json");
    assert_eq!(
        channels_taint::read_state(&fx.dir(), NEW),
        TaintState::Untainted
    );
}

/// 🔴 #9923 (Architect Q10): the launch directory decides, so a session that
/// `cd`s out of the marked workspace is still denied.
#[test]
fn a_hook_less_id_that_cds_out_of_the_marked_workspace_is_still_denied() {
    let fx = Fixture::marked();
    deny_reason(
        &fx.guard(Some(&fx.ws), &fx.out),
        "launched in ws, cwd outside",
    );
    assert!(fx.new_taint().exists(), "NEW.json written");
}

/// #9923: a session launched outside every mark's scope is allowed, as is
/// any session when no mark exists, and nothing is written.
#[test]
fn a_session_no_mark_covers_is_allowed_and_nothing_is_written() {
    let fx = Fixture::marked();
    assert_allowed(&fx.guard(Some(&fx.out), &fx.out), "launched outside ws");
    assert!(
        !fx.new_taint().exists(),
        "nothing written outside the scope"
    );
    let bare = Fixture::bare();
    assert_allowed(&bare.guard(Some(&bare.ws), &bare.ws), "no mark");
    assert_allowed(&bare.guard(None, &bare.ws), "no mark, no launch dir");
    assert!(!bare.new_taint().exists(), "nothing written with no mark");
}

/// 🔴 #9923 (fail closed): with a mark present, a launch directory the guard
/// cannot read is covered, so the call is denied.
#[test]
fn an_unresolvable_launch_dir_under_a_live_mark_denies() {
    let fx = Fixture::marked();
    deny_reason(&fx.guard(None, &fx.out), "no CLAUDE_PROJECT_DIR");
}

/// 🔴 #9923 (fail closed): a mark that cannot be read denies an uncovered call.
#[test]
fn an_unreadable_mark_denies() {
    let fx = Fixture::marked();
    std::fs::write(fx.dir().join("managed").join("m-bad.json"), b"not json").expect("write");
    deny_reason(&fx.guard(Some(&fx.out), &fx.out), "malformed mark");
    assert!(
        !fx.new_taint().exists(),
        "no record written for an unread mark"
    );
}

/// 🔴 #9923 (fail closed): marks that cannot be listed deny the call.
#[test]
fn marks_that_cannot_be_listed_deny() {
    let fx = Fixture::marked();
    let managed = fx.dir().join("managed");
    std::fs::set_permissions(&managed, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let out = fx.guard(Some(&fx.out), &fx.out);
    std::fs::set_permissions(&managed, std::fs::Permissions::from_mode(0o700)).expect("restore");
    deny_reason(&out, "managed/ unlistable");
}

/// 🔴 #9923 (fail closed): a taint write that fails still denies the call.
#[test]
fn a_taint_write_failure_denies_the_call() {
    let fx = Fixture::marked();
    let dir = fx.dir();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let out = fx.guard(Some(&fx.ws), &fx.ws);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
    deny_reason(&out, "taint dir read-only");
    assert!(!fx.new_taint().exists(), "the write failed");
}
