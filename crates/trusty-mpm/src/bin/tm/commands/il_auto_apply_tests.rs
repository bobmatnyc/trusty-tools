//! G0b: a block in the session's own turn (`Stop`) is applied to its store;
//! a block from a handback or an inbound message is only a proposal; a
//! refused block writes nothing and lands in the denial store.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use trusty_architect::FakeClock;
use trusty_architect::ledger::il;
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
        claude_config: Some(root.join("claude")),
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

/// A `Stop` payload whose transcript's last assistant message is `text`.
fn stop_payload(t: &Target, text: &str) -> serde_json::Value {
    let dir = t.claude_config.as_ref().expect("config").join("projects");
    fs::create_dir_all(&dir).expect("projects dir");
    let path = dir.join("s.jsonl");
    let line =
        json!({"type": "assistant", "message": {"content": [{"type": "text", "text": text}]}});
    fs::write(&path, format!("{{\"type\":\"user\"}}\n{line}\n")).expect("transcript");
    json!({"transcript_path": path})
}

/// The session's own turn ends with `text`.
fn own(text: &str, t: &Target) -> Outcome {
    go("Stop", &stop_payload(t, text), t)
}

/// A block from outside the own turn: proposal, ledger and log unchanged,
/// one `il-auto-apply:PROPOSAL` record per packet even when it is replayed.
fn assert_proposal(event: &str, payload: &serde_json::Value, session: &str, entry: Entry) {
    let (tmp, dir) = repo(session);
    let t = target(tmp.path(), session);
    let before = (
        fs::read(dir.join("state.json")).expect("state"),
        fs::read(dir.join("log.jsonl")).expect("log"),
    );
    assert_eq!(
        turn_text(event, payload, Some(session), None).map(|(e, _)| e),
        Some(entry)
    );
    for _ in 0..2 {
        match go(event, payload, &t) {
            Outcome::Proposed(p) => assert_eq!(Some(p), il::packet_id(BLOCK)),
            other => panic!("{session}: {other:?}"),
        }
    }
    let after = (
        fs::read(dir.join("state.json")).expect("state"),
        fs::read(dir.join("log.jsonl")).expect("log"),
    );
    assert_eq!(before, after, "{session}: a proposal changed the ledger");
    let rec = denials(tmp.path());
    assert_eq!(rec.matches("il-auto-apply:PROPOSAL").count(), 1, "{rec}");
    assert!(rec.contains(entry.label()), "{rec}");
    assert!(
        rec.contains(&il::packet_id(BLOCK).expect("packet")),
        "{rec}"
    );
}

#[test]
fn a_handback_block_is_a_proposal_and_leaves_the_ledger_unchanged() {
    let payload = json!({"tool_name": "Agent", "tool_response": {"content": [{"type": "text", "text": BLOCK}]}});
    assert_proposal("PostToolUse", &payload, "pm-a", Entry::AgentHandback);
}

#[test]
fn an_inbound_block_is_a_proposal_and_leaves_the_ledger_unchanged() {
    let from_pm = json!({"prompt": format!("[from pm-b]\n{BLOCK}")});
    assert_proposal("UserPromptSubmit", &from_pm, "pm-b", Entry::InboundMessage);
    let architect = json!({"prompt": BLOCK});
    assert_proposal(
        "UserPromptSubmit",
        &architect,
        "tm-architect",
        Entry::ArchitectPrompt,
    );
}

#[test]
fn an_own_turn_block_is_applied_and_its_replay_is_a_no_op() {
    let (tmp, dir) = repo("pm-a");
    let t = target(tmp.path(), "pm-a");
    let payload = stop_payload(&t, &format!("done.\n{BLOCK}"));
    assert_eq!(
        turn_text("Stop", &payload, Some("pm-a"), t.claude_config.as_deref()).map(|(e, _)| e),
        Some(Entry::OwnTurn)
    );
    match go("Stop", &payload, &t) {
        Outcome::Applied(a) => assert!(!a.replayed && a.acts == 1, "{a:?}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(next_of(&dir), "ship it");
    let log = fs::read_to_string(dir.join("log.jsonl")).expect("log");
    assert!(log.contains("\"kind\":\"act\",\"session\":\"pm-a\""));
    assert!(matches!(go("Stop", &payload, &t), Outcome::Replayed(_)));
    assert!(denials(tmp.path()).is_empty());
}

#[test]
fn a_stop_transcript_outside_the_config_dir_is_not_read() {
    let (tmp, dir) = repo("pm-a");
    let t = target(tmp.path(), "pm-a");
    let stray = tmp.path().join("stray.jsonl");
    let line = json!({"type": "assistant", "message": {"content": BLOCK}});
    fs::write(&stray, format!("{line}\n")).expect("stray");
    let before = next_of(&dir);
    let got = go("Stop", &json!({"transcript_path": stray}), &t);
    assert!(matches!(got, Outcome::NoBlock), "{got:?}");
    assert_eq!(next_of(&dir), before);
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
    let got = own(&bad, &t);
    assert!(matches!(&got, Outcome::Failed(c) if c == "LONG"), "{got:?}");
    assert_eq!(fs::read(dir.join("state.json")).expect("state"), before);
    let rec = fs::read_to_string(tmp.path().join("denials.jsonl")).expect("a denial record");
    assert!(rec.contains("il-auto-apply:LONG"), "{rec}");
    assert!(
        rec.contains("il auto-apply refused il:own-turn [LONG]"),
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
    let got = own(BLOCK, &t);
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
    assert!(matches!(own("hi", &t), Outcome::NoBlock));
    let read = json!({"tool_name": "Read", "tool_response": BLOCK});
    assert!(matches!(go("PostToolUse", &read, &t), Outcome::NoBlock));
    let empty = tempfile::tempdir().expect("tempdir");
    let t2 = target(empty.path(), "pm-a");
    assert!(matches!(own(BLOCK, &t2), Outcome::NoLedger));
    // G0b critic: recorded once per packet, never silently dropped.
    let again = own(BLOCK, &t2);
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
    let got = own(BLOCK, &t);
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
    let got = own(&prompt, &t);
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
    let again = own(&prompt, &t);
    assert!(matches!(again, Outcome::Replayed(_)), "{again:?}");
    assert_eq!(calls(tmp.path()).len(), 1, "ilio ran twice");
    // Replaces the old "store empty after success": the store is append-only,
    // so success leaves PENDING-UNKNOWN then PENDING-RESOLVED, and the replay
    // adds nothing and never reports PENDING-UNKNOWN.
    assert_eq!(codes(tmp.path()), ["PENDING-UNKNOWN", "PENDING-RESOLVED"]);
}

/// The `il-auto-apply` codes in the denial store, in file order.
fn codes(root: &Path) -> Vec<String> {
    denials(root)
        .lines()
        .filter_map(|l| l.split_once("check=il-auto-apply:"))
        .map(|(_, r)| r.split(' ').next().unwrap_or("").to_owned())
        .collect()
}

/// Why: nothing rewrites the denial store (G0b ruling): a writer that appends
/// during an ilio run must not lose its line.
/// What: a fake ilio appends a foreign line mid-run; afterwards the store is
/// the pre-run bytes, the foreign line, then the RESOLVED record, and the
/// pending reader treats the packet as cleared.
/// Test: itself.
#[test]
fn a_concurrent_append_during_the_run_is_preserved() {
    use trusty_mpm::daemon::bug_report::drop_resolved_pending;
    let (tmp, _dir) = repo("pm-a");
    let mut t = target(tmp.path(), "pm-a");
    let store = tmp.path().join("denials.jsonl");
    let tail =
        format!("open({store:?}, 'a').write('{{\"foreign\": 1}}\\n')\nprint('wrote 2 op(s)')");
    t.ilio = Some(fake_ilio(tmp.path(), &tail));
    let got = own(NEW_BLOCK, &t);
    assert!(matches!(got, Outcome::IlioApplied(..)), "{got:?}");
    let lines: Vec<String> = denials(tmp.path()).lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].contains("il-auto-apply:PENDING-UNKNOWN"));
    assert_eq!(lines[1], "{\"foreign\": 1}");
    assert!(lines[2].contains("il-auto-apply:PENDING-RESOLVED"));
    let recs: Vec<_> = lines[0..1]
        .iter()
        .chain(&lines[2..])
        .map(|l| serde_json::from_str(l).expect("record"))
        .collect();
    assert!(
        drop_resolved_pending(recs).is_empty(),
        "pending not cleared"
    );
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
        let got = own(NEW_BLOCK, &t);
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
        let got = own(&block, &t);
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
    let payload = stop_payload(&t, NEW_BLOCK);
    let got: Vec<Outcome> = std::thread::scope(|s| {
        let runs: Vec<_> = (0..2)
            .map(|_| s.spawn(|| go("Stop", &payload, &t)))
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
    let (tmp, dir) = repo("pm-a");
    let mut t = target(tmp.path(), "pm-a");
    t.ilio = Some(fake_ilio(tmp.path(), "print('wrote 2 op(s)')"));
    let paths = trusty_architect::ledger::store::StorePaths::at(dir);
    let packet = il::packet_id(NEW_BLOCK).expect("packet");
    // A run killed after it started: a marker and no applied record.
    il::mark_pending(&paths, &packet).expect("pending");
    let got = own(NEW_BLOCK, &t);
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

/// Why: the installed `Stop` hook is killed at 5 s; a record must outlive it.
/// What: a hung ilio under the real `ILIO_TIMEOUT` is killed inside 4 s with
/// `ILIO-TIMEOUT`, and the `PENDING-UNKNOWN` record (naming the marker) was
/// already in the store while ilio was still running.
/// Test: itself.
#[test]
fn a_hung_ilio_is_killed_inside_the_hook_bound_and_a_record_exists_before_it_runs() {
    use super::super::il_auto_apply_ilio::ILIO_TIMEOUT;
    let (tmp, _dir) = repo("pm-a");
    let mut t = target(tmp.path(), "pm-a");
    let seen = tmp.path().join("seen-by-ilio.txt");
    let tail = format!(
        "import shutil, time\nshutil.copy({:?}, {:?})\ntime.sleep(60)",
        tmp.path().join("denials.jsonl"),
        seen
    );
    t.ilio = Some(fake_ilio(tmp.path(), &tail));
    t.ilio_timeout = ILIO_TIMEOUT;
    let started = std::time::Instant::now();
    let got = own(NEW_BLOCK, &t);
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    assert!(
        matches!(&got, Outcome::Failed(c) if c == "ILIO-TIMEOUT"),
        "{got:?}"
    );
    let before = fs::read_to_string(&seen).expect("ilio saw the store");
    assert!(before.contains("il-auto-apply:PENDING-UNKNOWN"), "{before}");
    let packet = il::packet_id(NEW_BLOCK).expect("packet");
    assert!(before.contains(&format!("il-pending/{packet}")), "{before}");
    assert!(denials(tmp.path()).contains("il-auto-apply:PENDING-UNKNOWN"));
}

/// Why: a proposal must say what it proposes, and distinct proposals must
/// not collapse into one filed bug.
/// What: two proposals carry their own body in `command` and their packet in
/// the message (distinct fingerprints); a token in a body is scrubbed.
/// Test: itself.
#[test]
fn each_proposal_carries_its_scrubbed_body_and_its_own_fingerprint() {
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789"; // pragma: allowlist secret
    let (tmp, _dir) = repo("pm-a");
    let t = target(tmp.path(), "pm-a");
    let one = "```il\ntask: free: one\ndone: d\npct: 5\nnext: first move\n```";
    let two = format!("```il\ntask: free: two\ndone: d\npct: 5\nnext: use {token}\n```");
    for block in [one, two.as_str()] {
        let payload = json!({"prompt": format!("[from pm-b]\n{block}")});
        assert!(matches!(
            go("UserPromptSubmit", &payload, &t),
            Outcome::Proposed(_)
        ));
    }
    let recs: Vec<serde_json::Value> = denials(tmp.path())
        .lines()
        .map(|l| serde_json::from_str(l).expect("record"))
        .collect();
    assert_eq!(recs.len(), 2);
    assert_ne!(recs[0]["fingerprint"], recs[1]["fingerprint"]);
    let f = |i: usize| recs[i]["fields"].as_str().expect("fields").to_owned();
    assert!(f(0).contains("next: first move"), "{}", f(0));
    assert!(f(1).contains("task: free: two"), "{}", f(1));
    assert!(!f(1).contains(token), "{}", f(1));
    for (i, block) in [one, two.as_str()].iter().enumerate() {
        let packet = il::packet_id(block).expect("packet");
        let msg = recs[i]["message"].as_str().expect("message");
        assert!(msg.contains(&packet), "{msg}");
    }
}
