//! G0b: each hook entry point applies an IL block to its session's store;
//! a refused block writes nothing and lands in the denial store.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use trusty_architect::FakeClock;
use trusty_architect::ledger::writer::Writer;

use super::{Entry, Outcome, Target, run, turn_text};

/// Fully covered: a `free:` task, so `pct` lands on no task (ilio `task_id`).
const BLOCK: &str =
    "```il\ntask: free: ad hoc\ndone: d\npct: 5\nnext: ship it\nact: ran the gates\n```";
/// Carries `new:`, so only ilio can apply it.
const NEW_BLOCK: &str =
    "```il\ntask: free: x\ndone: d\npct: 5\nnew: A \"call the vendor\"\nnext: n\n```";

fn no_secrets(_: &str) -> Option<String> {
    None
}

/// A temp repo holding `pm-small` as `.trusty-mpm/datg/<session>`.
fn repo(session: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::create_dir(tmp.path().join(".git")).expect(".git");
    let dir = tmp.path().join(".trusty-mpm/datg").join(session);
    fs::create_dir_all(&dir).expect("store dir");
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../trusty-architect/tests/fixtures/ledger/pm-small");
    for e in fs::read_dir(src).expect("fixture") {
        let e = e.expect("entry");
        fs::copy(e.path(), dir.join(e.file_name())).expect("copy");
    }
    (tmp, dir)
}

fn target(root: &Path, session: &str) -> Target {
    Target {
        cwd: root.to_path_buf(),
        session: Some(session.to_owned()),
        denials: Some(root.join("denials.jsonl")),
        ilio: None,
        ilio_timeout: std::time::Duration::from_secs(5),
    }
}

fn go(event: &str, payload: &serde_json::Value, t: &Target) -> Outcome {
    let clock = FakeClock::new(1_792_576_800.0);
    let scan = no_secrets;
    run(event, payload, "sid-1", t, &Writer::new(&clock, &scan))
}

fn next_of(dir: &Path) -> String {
    let st: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("state.json")).expect("state"))
            .expect("json");
    st["session"]["next"].as_str().unwrap_or("").to_owned()
}

#[test]
fn each_entry_point_applies_the_block_to_its_own_session() {
    let cases = [
        (
            "PostToolUse",
            json!({"tool_name": "Agent", "tool_response": {"content": [{"type": "text", "text": BLOCK}]}}),
            "pm-a",
            Entry::AgentHandback,
        ),
        (
            "UserPromptSubmit",
            json!({"prompt": format!("[from pm-b]\n{BLOCK}")}),
            "pm-b",
            Entry::InboundMessage,
        ),
        (
            "UserPromptSubmit",
            json!({"prompt": BLOCK}),
            "tm-architect",
            Entry::ArchitectPrompt,
        ),
    ];
    for (event, payload, session, entry) in cases {
        let (tmp, dir) = repo(session);
        let t = target(tmp.path(), session);
        assert_eq!(
            turn_text(event, &payload, Some(session)).map(|(e, _)| e),
            Some(entry)
        );
        match go(event, &payload, &t) {
            Outcome::Applied(a) => assert!(!a.replayed && a.acts == 1, "{session}: {a:?}"),
            other => panic!("{session}: {other:?}"),
        }
        assert_eq!(next_of(&dir), "ship it", "{session}");
        let log = fs::read_to_string(dir.join("log.jsonl")).expect("log");
        assert!(log.contains(&format!("\"kind\":\"act\",\"session\":\"{session}\"")));
        match go(event, &payload, &t) {
            Outcome::Replayed(_) => {}
            other => panic!("{session} replay: {other:?}"),
        }
    }
}

#[test]
fn a_rejected_block_writes_nothing_and_is_recorded_for_list_recent_errors() {
    let (tmp, dir) = repo("pm-a");
    let t = target(tmp.path(), "pm-a");
    let before = fs::read(dir.join("state.json")).expect("state");
    let bad = format!(
        "```il\ntask: xa-T01\ndone: d\npct: 5\nnext: n\nact: {}\n```",
        "y".repeat(250)
    );
    let got = go("UserPromptSubmit", &json!({"prompt": bad}), &t);
    assert!(matches!(&got, Outcome::Failed(c) if c == "LONG"), "{got:?}");
    assert_eq!(fs::read(dir.join("state.json")).expect("state"), before);
    let rec = fs::read_to_string(tmp.path().join("denials.jsonl")).expect("a denial record");
    assert!(rec.contains("il-auto-apply:LONG"), "{rec}");
    assert!(
        rec.contains("il auto-apply refused il:inbound-message [LONG]"),
        "{rec}"
    );
}

#[test]
fn a_failed_write_is_never_reported_as_applied() {
    use std::os::unix::fs::PermissionsExt;
    let (tmp, dir) = repo("pm-a");
    let t = target(tmp.path(), "pm-a");
    let _ = fs::remove_file(dir.join(".lock"));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("chmod");
    let got = go("UserPromptSubmit", &json!({"prompt": BLOCK}), &t);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(
        matches!(&got, Outcome::Failed(c) if c == "WRITE"),
        "{got:?}"
    );
    let rec = fs::read_to_string(tmp.path().join("denials.jsonl")).expect("a denial record");
    assert!(rec.contains("il-auto-apply:WRITE"), "{rec}");
}

#[test]
fn no_block_and_no_ledger_do_nothing() {
    let (tmp, _dir) = repo("pm-a");
    let t = target(tmp.path(), "pm-a");
    assert!(matches!(
        go("UserPromptSubmit", &json!({"prompt": "hi"}), &t),
        Outcome::NoBlock
    ));
    let read = json!({"tool_name": "Read", "tool_response": BLOCK});
    assert!(matches!(go("PostToolUse", &read, &t), Outcome::NoBlock));
    let empty = tempfile::tempdir().expect("tempdir");
    let t2 = target(empty.path(), "pm-a");
    assert!(matches!(
        go("UserPromptSubmit", &json!({"prompt": BLOCK}), &t2),
        Outcome::NoLedger
    ));
    // G0b critic: recorded once per packet, never silently dropped.
    let again = go("UserPromptSubmit", &json!({"prompt": BLOCK}), &t2);
    assert!(matches!(again, Outcome::NoLedger));
    let rec = denials(empty.path());
    assert_eq!(rec.matches("il-auto-apply:NO-LEDGER").count(), 1, "{rec}");
}

/// A fake `ilio.py` that records argv, the store env and stdin to
/// `<dir>/ilio-call.json`, then runs `tail` (Python).
fn fake_ilio(dir: &Path, tail: &str) -> PathBuf {
    let path = dir.join("ilio.py");
    let body = format!(
        "import json, os, sys\n\
         rec = {{'argv': sys.argv[1:], 'stdin': sys.stdin.read(),\n\
         'env': [os.environ.get(k) for k in ('ILIO_PROJECT', 'DATG_SESSION', 'ILIO_WHO')]}}\n\
         p = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ilio-call.json')\n\
         calls = json.load(open(p)) if os.path.exists(p) else []\n\
         json.dump(calls + [rec], open(p, 'w'))\n{tail}\n"
    );
    fs::write(&path, body).expect("fake ilio");
    path
}

fn calls(dir: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(dir.join("ilio-call.json"))
        .map(|t| serde_json::from_str(&t).expect("calls json"))
        .unwrap_or_default()
}

fn denials(root: &Path) -> String {
    fs::read_to_string(root.join("denials.jsonl")).unwrap_or_default()
}

#[test]
fn a_fully_covered_block_takes_the_rust_path_and_never_runs_ilio() {
    let (tmp, dir) = repo("pm-a");
    let mut t = target(tmp.path(), "pm-a");
    t.ilio = Some(fake_ilio(tmp.path(), "print('wrote 1 op(s)')"));
    let got = go("UserPromptSubmit", &json!({"prompt": BLOCK}), &t);
    assert!(
        matches!(&got, Outcome::Applied(a) if a.acts == 1),
        "{got:?}"
    );
    assert_eq!(next_of(&dir), "ship it");
    assert!(calls(tmp.path()).is_empty(), "ilio ran on a covered block");
}

#[test]
fn a_block_with_new_runs_ilio_once_with_the_block_on_stdin() {
    let (tmp, dir) = repo("pm-a");
    let mut t = target(tmp.path(), "pm-a");
    let script = fake_ilio(tmp.path(), "print('wrote 2 op(s)')");
    t.ilio = Some(script.clone());
    let before = fs::read(dir.join("state.json")).expect("state");
    let prompt = format!("[from pm-a]\n{NEW_BLOCK}");
    let got = go("UserPromptSubmit", &json!({"prompt": prompt}), &t);
    assert!(
        matches!(&got, Outcome::IlioApplied(_, s) if s == "wrote 2 op(s)"),
        "{got:?}"
    );
    let c = calls(tmp.path());
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(c[0]["argv"], json!(["apply"]));
    // Only the fence goes to ilio, not the relay header around it.
    assert_eq!(c[0]["stdin"], json!(format!("{NEW_BLOCK}\n")));
    let root = tmp.path().display().to_string();
    assert_eq!(c[0]["env"], json!([root, "pm-a", "pm-a"]));
    // The Rust ops did not also run.
    assert_eq!(fs::read(dir.join("state.json")).expect("state"), before);
    // A replay is a no-op on the ilio path.
    let again = go("UserPromptSubmit", &json!({"prompt": prompt}), &t);
    assert!(matches!(again, Outcome::Replayed(_)), "{again:?}");
    assert_eq!(calls(tmp.path()).len(), 1, "ilio ran twice");
    assert!(denials(tmp.path()).is_empty());
}

#[test]
fn ilio_failures_are_never_applied() {
    let cases = [
        (None, "ILIO-MISSING"),
        (Some("print('IL-REJECT KIND new: no'); sys.exit(1)"), "KIND"),
        (Some("sys.exit(2)"), "ILIO-EXIT"),
        (Some("import time; time.sleep(30)"), "ILIO-TIMEOUT"),
    ];
    for (tail, code) in cases {
        let (tmp, dir) = repo("pm-a");
        let mut t = target(tmp.path(), "pm-a");
        t.ilio = Some(match tail {
            Some(tail) => fake_ilio(tmp.path(), tail),
            None => tmp.path().join("absent/ilio.py"),
        });
        t.ilio_timeout = std::time::Duration::from_millis(1500);
        let got = go("UserPromptSubmit", &json!({"prompt": NEW_BLOCK}), &t);
        assert!(
            matches!(&got, Outcome::Failed(c) if c == code),
            "{code}: {got:?}"
        );
        assert!(
            denials(tmp.path()).contains(&format!("il-auto-apply:{code}")),
            "{code}: {}",
            denials(tmp.path())
        );
        assert!(
            !dir.join(trusty_architect::ledger::il::APPLIED_FILE)
                .exists(),
            "{code}"
        );
    }
}

#[test]
fn each_new_check_rejects_before_any_apply() {
    let (tmp, dir) = repo("pm-a");
    let mut t = target(tmp.path(), "pm-a");
    t.ilio = Some(fake_ilio(tmp.path(), "print('wrote')"));
    let state = dir.join("state.json");
    let mut st: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state).expect("state")).expect("json");
    st["tasks"][0]["pct"] = json!(40);
    fs::write(&state, st.to_string()).expect("pct 40");
    let before = fs::read(&state).expect("state");
    let cases = [
        ("task: xa-To1\\ndone: d\\npct: 5\\nnext: n", "BADID"),
        (
            "task: xa-T01\\ndone: d\\npct: 50\\nremind: xa-T01 tomorrow\\nnext: n",
            "REMIND",
        ),
        (
            "task: xa-T01\\ndone: wired it\\npct: 40\\nnext: n",
            "PCT-UNCHANGED",
        ),
    ];
    for (body, code) in cases {
        let block = format!("```il\n{}\n```", body.replace("\\n", "\n"));
        let got = go("UserPromptSubmit", &json!({"prompt": block}), &t);
        assert!(
            matches!(&got, Outcome::Failed(c) if c == code),
            "{code}: {got:?}"
        );
        assert!(
            denials(tmp.path()).contains(&format!("il-auto-apply:{code}")),
            "{code}"
        );
    }
    assert!(calls(tmp.path()).is_empty(), "ilio ran on a rejected block");
    assert_eq!(fs::read(&state).expect("state"), before);
}

#[test]
fn ilio_script_comes_from_the_env() {
    use super::super::il_auto_apply_ilio::ilio_script;
    let env = |k: &str| (k == "ARCHITECT_PROJECT_DIR").then(|| "/a/arch".to_owned());
    assert_eq!(
        ilio_script(&env, None),
        Some(PathBuf::from("/a/arch/scripts/ilio.py"))
    );
    let explicit = |k: &str| (k == "ILIO_SCRIPT").then(|| "/x/ilio.py".to_owned());
    assert_eq!(
        ilio_script(&explicit, None),
        Some(PathBuf::from("/x/ilio.py"))
    );
}

#[test]
fn a_hung_ilio_with_a_large_body_times_out_inside_the_deadline() {
    use super::super::il_auto_apply_ilio::{IlioCall, run_ilio};
    let tmp = tempfile::tempdir().expect("tempdir");
    let script = tmp.path().join("ilio.py");
    fs::write(&script, "import time\ntime.sleep(60)\n").expect("hung ilio");
    let text = format!("```il\nnext: {}\n```", "x".repeat(70_000));
    let (tx, rx) = std::sync::mpsc::channel();
    let root = tmp.path().to_path_buf();
    std::thread::spawn(move || {
        let call = IlioCall {
            script: Some(&script),
            project: &root,
            session: "pm-a",
            who: "pm-a",
            timeout: std::time::Duration::from_secs(1),
        };
        let _ = tx.send(run_ilio(&call, &text));
    });
    let got = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("run_ilio blocked past its deadline");
    assert!(
        matches!(&got, Err((c, _)) if c == "ILIO-TIMEOUT"),
        "{got:?}"
    );
}

#[test]
fn two_concurrent_deliveries_apply_once() {
    let (tmp, dir) = repo("pm-a");
    let mut t = target(tmp.path(), "pm-a");
    t.ilio = Some(fake_ilio(
        tmp.path(),
        "import time; time.sleep(0.5); print('wrote 2 op(s)')",
    ));
    let payload = json!({"prompt": NEW_BLOCK});
    let got: Vec<Outcome> = std::thread::scope(|s| {
        let runs: Vec<_> = (0..2)
            .map(|_| s.spawn(|| go("UserPromptSubmit", &payload, &t)))
            .collect();
        runs.into_iter().map(|h| h.join().expect("join")).collect()
    });
    assert_eq!(calls(tmp.path()).len(), 1, "ilio ran twice: {got:?}");
    let applied = got
        .iter()
        .filter(|o| matches!(o, Outcome::IlioApplied(..)))
        .count();
    let replayed = got
        .iter()
        .filter(|o| matches!(o, Outcome::Replayed(_)))
        .count();
    assert_eq!((applied, replayed), (1, 1), "{got:?}");
    let pending = dir.join(trusty_architect::ledger::il::PENDING_DIR);
    assert_eq!(fs::read_dir(pending).expect("pending dir").count(), 0);
}

#[test]
fn a_stale_pending_marker_is_not_re_run() {
    use trusty_architect::ledger::il;
    let (tmp, dir) = repo("pm-a");
    let mut t = target(tmp.path(), "pm-a");
    t.ilio = Some(fake_ilio(tmp.path(), "print('wrote 2 op(s)')"));
    let paths = trusty_architect::ledger::store::StorePaths::at(dir);
    let packet = il::packet_id(NEW_BLOCK).expect("packet");
    // A run killed after it started: a marker and no applied record.
    il::mark_pending(&paths, &packet).expect("pending");
    let got = go("UserPromptSubmit", &json!({"prompt": NEW_BLOCK}), &t);
    assert!(
        matches!(&got, Outcome::Failed(c) if c == "PENDING-UNKNOWN"),
        "{got:?}"
    );
    assert!(
        calls(tmp.path()).is_empty(),
        "a stale pending packet re-ran ilio"
    );
    assert!(denials(tmp.path()).contains("il-auto-apply:PENDING-UNKNOWN"));
}
