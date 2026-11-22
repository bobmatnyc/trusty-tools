//! PR-B2: the scheduler's `dep_degraded` step, the same rules as
//! `fleet_poll_deps.py`. Every test is new behaviour: the module, the kind and
//! the triage rule do not exist at the base commit.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;

use serde_json::{Map, Value, json};
use trusty_architect::classify::alert;
use trusty_architect::poller::dep_health::{
    DepState, Reading, classify, dep_health_step, read_tm_health, tm_socket_path,
};
use trusty_architect::poller::triage_rules::{Rules, decide};

mod cycle_support;

use cycle_support::{Env, QuietHost, fleet, screen};
use trusty_architect::FakeClock;
use trusty_architect::poller::{PollSession, PollState};
use trusty_architect::sched::shadow::{ShadowAlert, parse_decision};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn down() -> Value {
    json!({"status": "degraded", "degraded": ["trusty-memory: down — refused"], "pending": []})
}

fn up() -> Value {
    json!({"status": "ok", "degraded": [], "pending": []})
}

fn pending() -> Value {
    json!({"status": "ok", "degraded": [], "pending": ["trusty-memory: pending"]})
}

/// Runs one step with `result` as tm's answer; returns `(kind, fingerprint, session)`.
fn run(other: &mut Map<String, Value>, result: Value) -> Vec<(String, String, String)> {
    let read = move |_: &Path| Reading::Ok(result.clone());
    let (found, _) = dep_health_step(Some(Path::new("/x")), &read, other, 1.0);
    found
        .into_iter()
        .map(|a| (a.kind, a.fingerprint, a.session))
        .collect()
}

#[test]
fn one_degraded_read_raises_nothing() {
    assert!(run(&mut Map::new(), down()).is_empty());
}

#[test]
fn two_consecutive_raise_exactly_one() {
    let mut other = Map::new();
    run(&mut other, down());
    let found = run(&mut other, down());
    let want = ("dep_degraded".into(), "trusty-memory".into(), String::new());
    assert_eq!(found, vec![want]);
}

#[test]
fn third_read_does_not_duplicate() {
    let mut other = Map::new();
    run(&mut other, down());
    run(&mut other, down());
    assert!(run(&mut other, down()).is_empty());
}

#[test]
fn recovery_then_two_degraded_alerts_again() {
    let mut other = Map::new();
    run(&mut other, down());
    run(&mut other, down());
    assert!(run(&mut other, up()).is_empty());
    assert_eq!(other["dep_health"], json!({}));
    assert!(run(&mut other, down()).is_empty());
    assert_eq!(run(&mut other, down()).len(), 1);
}

#[test]
fn pending_read_does_not_count() -> TestResult {
    let mut other = Map::new();
    assert!(run(&mut other, pending()).is_empty());
    assert!(run(&mut other, pending()).is_empty());
    assert!(
        run(&mut other, down()).is_empty(),
        "pending counted nothing"
    );
    assert!(run(&mut other, pending()).is_empty(), "and reset nothing");
    assert_eq!(run(&mut other, down()).len(), 1);
    let legacy = json!({"degraded": ["trusty-search: pending (first probe still running)"]});
    assert_eq!(classify(&legacy)?[1], ("trusty-search", DepState::Pending));
    Ok(())
}

#[test]
fn each_dependency_dedupes_on_its_own() {
    let both = json!({"degraded": ["trusty-memory: timeout", "trusty-search: down — x"]});
    let mut other = Map::new();
    run(&mut other, both.clone());
    let fps: Vec<String> = run(&mut other, both.clone())
        .into_iter()
        .map(|f| f.1)
        .collect();
    assert_eq!(fps, ["trusty-memory", "trusty-search"]);
    assert!(run(&mut other, both).is_empty());
}

#[test]
fn garbage_reply_is_the_error_arm() {
    let mut other = Map::new();
    other.insert(
        "dep_health".into(),
        json!({"trusty-memory": {"streak": 1, "alerted": false}}),
    );
    let readings = [
        Reading::Ok(json!("not a dict")),
        Reading::Ok(json!({"degraded": "x"})),
        Reading::Error("unparseable", "junk".into()),
    ];
    for r in readings {
        let read = move |_: &Path| r.clone();
        let (found, _) = dep_health_step(Some(Path::new("/x")), &read, &mut other, 1.0);
        let kinds: Vec<&str> = found.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(kinds, ["dep_health_error"]);
    }
    assert_eq!(
        other["dep_health"]["trusty-memory"]["streak"], 1,
        "no false healthy"
    );
}

#[test]
fn unreachable_socket_raises_dep_health_error() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut other = Map::new();
    let held = json!({"trusty-memory": {"streak": 1, "alerted": false}});
    other.insert("dep_health".into(), held.clone());
    let sock = dir.path().join("absent.sock");
    let (found, lines) = dep_health_step(Some(&sock), &read_tm_health, &mut other, 1.0);
    let key = |a: &trusty_architect::classify::Alert| {
        (a.kind.clone(), a.fingerprint.clone(), a.session.clone())
    };
    let want = (
        "dep_health_error".to_owned(),
        "unreachable".to_owned(),
        String::new(),
    );
    assert_eq!(
        found.iter().map(key).collect::<Vec<_>>(),
        std::slice::from_ref(&want)
    );
    assert!(
        found[0].detail.contains("unreachable"),
        "{}",
        found[0].detail
    );
    assert!(lines.is_empty(), "{lines:?}");
    assert_eq!(other["dep_health"], held, "streak held, never healthy");
    // Dedupe key is per error class: a repeat read repeats it, garbage differs.
    let (again, _) = dep_health_step(Some(&sock), &read_tm_health, &mut other, 2.0);
    assert_eq!(key(&again[0]), want);
    let junk = |_: &Path| Reading::Ok(json!("junk"));
    let (garbage, _) = dep_health_step(Some(&sock), &junk, &mut other, 3.0);
    assert_ne!(key(&garbage[0]), want);
    assert_eq!(other["dep_health"], held);
    Ok(())
}

#[test]
fn corrupt_state_book_recovers_and_still_alerts() {
    let corrupt = [
        json!("junk"),
        json!([1]),
        json!({"trusty-memory": "x"}),
        json!({"trusty-memory": {}}),
        json!({"trusty-memory": {"streak": "n", "alerted": "yes"}}),
    ];
    for bad in corrupt {
        let mut other = Map::new();
        other.insert("dep_health".into(), bad.clone());
        assert!(run(&mut other, down()).is_empty(), "{bad}");
        assert!(other["dep_health"].is_object(), "{bad}");
        assert_eq!(run(&mut other, down()).len(), 1, "{bad}");
        assert!(run(&mut other, down()).is_empty(), "{bad}");
    }
}

fn serve_once(path: &Path, reply: &'static [u8]) -> TestResult {
    let listener = UnixListener::bind(path)?;
    std::thread::spawn(move || {
        if let Ok((mut conn, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            let _ = conn.write_all(reply);
        }
    });
    Ok(())
}

#[test]
fn wire_read_parses_a_real_socket_and_garbage() -> TestResult {
    let dir = tempfile::Builder::new().prefix("pd").tempdir_in("/tmp")?;
    let ok = dir.path().join("a.sock");
    serve_once(
        &ok,
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"degraded\":[]}}\n",
    )?;
    assert_eq!(read_tm_health(&ok), Reading::Ok(json!({"degraded": []})));
    let bad = dir.path().join("b.sock");
    serve_once(&bad, b"<html>nope\n")?;
    assert!(matches!(
        read_tm_health(&bad),
        Reading::Error("unparseable", _)
    ));
    Ok(())
}

#[test]
fn off_switch_turns_the_step_off() {
    let calls = RefCell::new(0);
    let read = |_: &Path| {
        *calls.borrow_mut() += 1;
        Reading::Ok(down())
    };
    assert_eq!(
        dep_health_step(None, &read, &mut Map::new(), 1.0),
        (Vec::new(), Vec::new())
    );
    assert_eq!(*calls.borrow(), 0);
}

#[test]
fn socket_path_follows_tm_derivation() {
    let get = |k: &str| (k == "TRUSTY_DATA_DIR_OVERRIDE").then(|| "/x".to_owned());
    assert_eq!(
        tm_socket_path(Path::new("/h"), &get),
        Path::new("/x/trusty-mpm/trusty-mpm.sock")
    );
}

#[test]
fn triage_wakes_on_dep_degraded() {
    let a = alert("dep_degraded", "trusty-memory", "d", "", "", 1.0);
    let rules = Rules {
        holds: &[],
        schedule: Path::new("/nonexistent"),
    };
    let d = decide(&a, &rules, 1.0, &mut Map::new());
    assert!(d.is_some_and(|d| d.wake && d.path == "rule:always_wake"));
}

/// A fake tm socket answering `n` connections with `result`.
fn serve_n(path: &Path, n: usize, result: Value) -> TestResult {
    let listener = UnixListener::bind(path)?;
    std::thread::spawn(move || {
        for _ in 0..n {
            let Ok((mut conn, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            let reply = json!({"jsonrpc": "2.0", "id": 1, "result": result});
            let _ = conn.write_all(format!("{reply}\n").as_bytes());
        }
    });
    Ok(())
}

/// The full cycle: two degraded reads write exactly one `dep_degraded`
/// (session empty, fingerprint the service), a third writes none.
#[test]
fn cycle_writes_one_dep_degraded_after_two_degraded_reads() -> TestResult {
    let sockdir = tempfile::Builder::new().prefix("pd").tempdir_in("/tmp")?;
    let sock = sockdir.path().join("tm.sock");
    serve_n(&sock, 3, down())?;
    let sock_text = sock.to_string_lossy().into_owned();
    let env = Env::new(&[
        ("TM_HEALTH", "on"),
        ("TM_HEALTH_SOCKET", sock_text.as_str()),
    ])?;
    let tmux = fleet(&[("tm-a", "%2", "claude")], &screen(10));
    let clock = FakeClock::new(0.0);
    let mut session = PollSession::new(PollState::default());
    for _ in 0..3 {
        env.runner(&tmux, &clock, &QuietHost)
            .run_once(&mut session)?;
    }
    let keys: Vec<String> = env
        .keys()?
        .into_iter()
        .filter(|k| k.contains("dep_"))
        .collect();
    assert_eq!(
        keys,
        [r#"""|""|"dep_degraded"|"trusty-memory""#],
        "{}",
        env.log()
    );
    Ok(())
}

/// The shadow's parity join reads the dry run's `would alert:` line as
/// session `""` and kind `dep_degraded`, the form the live alert joins on.
#[test]
fn shadow_reads_dep_degraded_in_the_parity_join_form() {
    let a = alert(
        "dep_degraded",
        "trusty-memory",
        "tm health degraded: trusty-memory: down — refused",
        "",
        "",
        1.0,
    );
    let out = format!("  would alert: {}\n", a.display);
    let want = ShadowAlert {
        session: String::new(),
        kind: "dep_degraded".to_owned(),
    };
    assert_eq!(parse_decision(&out).alerts, vec![want]);
}
