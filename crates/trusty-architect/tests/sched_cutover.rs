//! Cutover, rollback, the plist and `status` (MB1595). Every side effect is a
//! fake or a temp directory; nothing here touches a real Architect.

mod sched_support;

use std::path::PathBuf;
use std::sync::Mutex;

use sched_support::*;
use trusty_architect::sched::SchedError;
use trusty_architect::sched::cutover::{Mode, Ops, Thresholds, cutover, read_mode, rollback};
use trusty_architect::sched::daemon::{STALE_AFTER_S, write_alive};
use trusty_architect::sched::plist::{LABEL, PlistSpec};
use trusty_architect::sched::shadow::ShadowRecord;
use trusty_architect::sched::slot::parse_iso;
use trusty_architect::sched::state::{JobState, SchedState};
use trusty_architect::sched::{Layout, status};

#[derive(Default)]
struct FakeOps {
    calls: Mutex<Vec<&'static str>>,
    fail_stop: bool,
    fail_start: bool,
}

impl FakeOps {
    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

impl Ops for FakeOps {
    fn stop_supervisor(&self) -> Result<String, String> {
        self.calls.lock().unwrap().push("stop");
        if self.fail_stop {
            Err("tmux refused".into())
        } else {
            Ok("stopped tm-architect-poll".into())
        }
    }
    fn start_supervisor(&self) -> Result<String, String> {
        self.calls.lock().unwrap().push("start");
        if self.fail_start {
            Err("script failed".into())
        } else {
            Ok("started".into())
        }
    }
}

const T: &str = "2026-11-21T14:00:00Z";

/// `n` shadow cycles one minute apart with no alerts and no wakes (perfect
/// parity against empty live files).
fn seed_shadow(l: &Layout, n: i64) -> i64 {
    let t = parse_iso(T).unwrap();
    let mut text = String::new();
    for i in 0..n {
        let rec = ShadowRecord {
            ts: "x".into(),
            job: "poll".into(),
            slot: t + i * 60,
            mode: "shadow".into(),
            interval_s: 60,
            ok: true,
            panes: Some(1),
            new: Some(0),
            woken: Some(0),
            logged: Some(0),
            suppressed: Some(0),
            wake_phrase: Some("none".into()),
            wake_target: None,
            alerts: vec![],
            digest_sha: "d".into(),
            error: None,
        };
        text += &format!("{}\n", serde_json::to_string(&rec).unwrap());
    }
    std::fs::create_dir_all(l.root.join("inbox")).unwrap();
    std::fs::write(l.shadow_log(), text).unwrap();
    t + n * 60
}

fn th(min_cycles: u64) -> Thresholds {
    Thresholds {
        since_s: 86_400,
        min_cycles,
        min_match_pct: 95.0,
        force: false,
    }
}

#[test]
fn cutover_refuses_below_the_parity_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let ops = FakeOps::default();
    let now = seed_shadow(&l, 2);
    let e = cutover(&l, &ops, &th(120), now).unwrap_err();
    assert!(
        matches!(e, SchedError::Cutover(ref m) if m.contains("parity below")),
        "{e}"
    );
    assert!(ops.calls().is_empty(), "nothing is stopped on a refusal");
    assert_eq!(read_mode(&l).unwrap(), Mode::Shadow);
    // Too many mismatches refuse too: the poller raised an alert the shadow missed.
    std::fs::write(
        l.alerts(),
        "{\"ts\":\"2026-11-21T14:00:10Z\",\"session\":\"s\",\"pane\":\"\",\"kind\":\"ctx\",\"fingerprint\":\"1\",\"detail\":\"d\",\"display\":\"d\"}\n",
    )
    .unwrap();
    let e = cutover(&l, &ops, &th(2), now).unwrap_err();
    assert!(
        matches!(e, SchedError::Cutover(ref m) if m.contains("parity below")),
        "{e}"
    );
    // No shadow record at all is an error, never a pass.
    let empty = tempfile::tempdir().unwrap();
    assert!(cutover(&layout(empty.path()), &ops, &th(0), now).is_err());
    assert!(ops.calls().is_empty());
}

#[test]
fn cutover_flips_to_live_after_stopping_the_supervisor() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let ops = FakeOps::default();
    let now = seed_shadow(&l, 5);
    let msg = cutover(&l, &ops, &th(5), now).unwrap();
    assert!(msg.starts_with("live:"), "{msg}");
    assert_eq!(ops.calls(), ["stop"]);
    assert_eq!(read_mode(&l).unwrap(), Mode::Live);
    let log = lines(&l.cutover_log());
    assert_eq!(log.len(), 1);
    assert!(
        log[0].contains("cutover") && log[0].starts_with("2026-11-21T14:05:00Z"),
        "{log:?}"
    );
    // Forcing skips parity and says so.
    let other = tempfile::tempdir().unwrap();
    let ol = layout(other.path());
    let ops2 = FakeOps::default();
    let forced = Thresholds {
        force: true,
        ..th(120)
    };
    assert!(
        cutover(&ol, &ops2, &forced, now)
            .unwrap()
            .contains("FORCED")
    );
    assert_eq!(ops2.calls(), ["stop"]);
}

#[test]
fn cutover_is_idempotent_when_already_live() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let ops = FakeOps::default();
    let now = seed_shadow(&l, 3);
    cutover(&l, &ops, &th(3), now).unwrap();
    let again = cutover(&l, &ops, &th(3), now + 60).unwrap();
    assert!(again.contains("already live"));
    assert_eq!(ops.calls(), ["stop"], "the supervisor is stopped once");
    assert_eq!(lines(&l.cutover_log()).len(), 1);
}

#[test]
fn cutover_changes_nothing_when_the_supervisor_will_not_stop() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let ops = FakeOps {
        fail_stop: true,
        ..FakeOps::default()
    };
    let now = seed_shadow(&l, 3);
    let e = cutover(&l, &ops, &th(3), now).unwrap_err();
    assert!(e.to_string().contains("tmux refused"));
    assert_eq!(read_mode(&l).unwrap(), Mode::Shadow);
}

#[test]
fn cutover_restores_the_supervisor_when_the_marker_cannot_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let ops = FakeOps::default();
    let now = seed_shadow(&l, 3);
    // The marker's temp file path is a directory, so the write fails.
    std::fs::create_dir_all(l.cutover().with_extension("json.tmp")).unwrap();
    let e = cutover(&l, &ops, &th(3), now).unwrap_err();
    assert!(e.to_string().contains("marker not written"), "{e}");
    assert_eq!(ops.calls(), ["stop", "start"], "the poller is brought back");
    assert_eq!(read_mode(&l).unwrap(), Mode::Shadow);
}

#[test]
fn rollback_flips_to_shadow_then_starts_the_supervisor() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let ops = FakeOps::default();
    let now = seed_shadow(&l, 3);
    cutover(&l, &ops, &th(3), now).unwrap();
    let msg = rollback(&l, &ops, now + 60).unwrap();
    assert!(msg.starts_with("shadow:"), "{msg}");
    assert_eq!(ops.calls(), ["stop", "start"]);
    assert_eq!(read_mode(&l).unwrap(), Mode::Shadow);
    // Idempotent: again ensures the supervisor runs (the script is itself idempotent).
    rollback(&l, &ops, now + 120).unwrap();
    assert_eq!(ops.calls(), ["stop", "start", "start"]);
    assert_eq!(read_mode(&l).unwrap(), Mode::Shadow);
    // A failed start leaves the mode on shadow and says so.
    let bad = FakeOps {
        fail_start: true,
        ..FakeOps::default()
    };
    let e = rollback(&l, &bad, now + 180).unwrap_err();
    assert!(e.to_string().contains("mode is shadow") && e.to_string().contains("script failed"));
    assert_eq!(read_mode(&l).unwrap(), Mode::Shadow);
}

#[test]
fn an_unreadable_mode_marker_is_an_error_not_shadow() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    std::fs::create_dir_all(l.cutover().parent().unwrap()).unwrap();
    std::fs::write(l.cutover(), r#"{"mode":"turbo","at":"x"}"#).unwrap();
    assert!(read_mode(&l).is_err());
    std::fs::write(l.cutover(), "").unwrap();
    assert!(read_mode(&l).is_err());
}

#[test]
fn plist_has_keepalive_label_and_program_arguments() {
    let spec = PlistSpec {
        exe: PathBuf::from("/home/user/.cargo/bin/trusty-architect-sched"),
        root: PathBuf::from("/arch"),
        log_dir: PathBuf::from("/arch/local/logs/sched"),
        path_env: PlistSpec::default_path(std::path::Path::new("/home/user")),
    };
    let x = spec.render();
    assert_eq!(LABEL, "com.trusty.architect-sched");
    for want in [
        "<key>Label</key>\n  <string>com.trusty.architect-sched</string>",
        "<key>KeepAlive</key>\n  <true/>",
        "<key>RunAtLoad</key>\n  <true/>",
        "<key>ThrottleInterval</key>\n  <integer>10</integer>",
        "<key>ExitTimeOut</key>\n  <integer>60</integer>",
        "<string>/home/user/.cargo/bin/trusty-architect-sched</string>\n    <string>daemon</string>\n    <string>--root</string>\n    <string>/arch</string>",
        "<key>StandardOutPath</key>\n  <string>/arch/local/logs/sched/stdout.log</string>",
        "<key>NumberOfFiles</key>\n    <integer>8192</integer>",
        "/home/user/.cargo/bin:/home/user/.local/bin:/opt/homebrew/bin",
    ] {
        assert!(x.contains(want), "plist lacks {want:?}:\n{x}");
    }
    assert!(x.starts_with("<?xml") && x.trim_end().ends_with("</plist>"));
    // The golden's key order: Label, ProgramArguments, KeepAlive, RunAtLoad.
    let at = |k: &str| x.find(k).unwrap();
    assert!(
        at("Label") < at("ProgramArguments")
            && at("ProgramArguments") < at("KeepAlive")
            && at("KeepAlive") < at("RunAtLoad")
    );
}

#[test]
fn plist_escapes_xml_and_carries_no_credentials() {
    let spec = PlistSpec {
        exe: PathBuf::from("/a&b/<sched>"),
        root: PathBuf::from("/arch"),
        log_dir: PathBuf::from("/l"),
        path_env: "/bin".into(),
    };
    let x = spec.render();
    assert!(x.contains("/a&amp;b/&lt;sched&gt;"));
    let env_keys: Vec<&str> = x
        .split("<key>")
        .filter_map(|s| s.split("</key>").next())
        .collect();
    for k in env_keys {
        assert!(
            !k.contains("TOKEN") && !k.contains("SECRET") && !k.contains("KEY") || k == "KeepAlive",
            "{k}"
        );
    }
}

#[test]
fn status_reports_liveness_and_job_rows() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    let jobs = format!(
        "{},{}",
        script("job1", "10m", ""),
        script("mng", "1m", r#""managed_by":"meta_due""#)
    );
    let reg = reg(&jobs);
    let mut st = SchedState::default();
    st.jobs.insert(
        "job1".into(),
        JobState {
            next_due: T0 + 600,
            last_result: Some("ok".into()),
            misses: 1,
            overruns: 2,
            ..JobState::default()
        },
    );
    // No liveness file: not alive, the verb will exit non-zero.
    let (text, healthy) = status::render(&l, &reg, &st, T0, false);
    assert!(!healthy && text.contains("NOT ALIVE"), "{text}");
    write_alive(&l, T0, "abcdef0123456789", 2, Mode::Shadow).unwrap();
    let (text, healthy) = status::render(&l, &reg, &st, T0 + 4, false);
    assert!(healthy);
    for want in [
        "alive pid",
        "mode shadow",
        "config abcdef01",
        "1 scheduler-run, 1 managed by meta_due",
        "job1",
        "last ok",
        "misses 1",
        "overruns 2",
        "2023-11-15T00:10:00Z",
    ] {
        assert!(text.contains(want), "status lacks {want:?}:\n{text}");
    }
    let (json, _) = status::render(&l, &reg, &st, T0 + 4, true);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["healthy"], true);
    assert_eq!(v["scheduler_run"][0]["id"], "job1");
}

#[test]
fn status_fails_when_the_liveness_file_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    let l = layout(dir.path());
    write_alive(&l, T0, "sha", 1, Mode::Live).unwrap();
    let r = reg(&script("job1", "10m", ""));
    let (text, healthy) = status::render(
        &l,
        &r,
        &SchedState::default(),
        T0 + STALE_AFTER_S + 1,
        false,
    );
    assert!(!healthy && text.contains("STALE"), "{text}");
    let (_, healthy) = status::render(&l, &r, &SchedState::default(), T0 + STALE_AFTER_S, false);
    assert!(healthy);
}
