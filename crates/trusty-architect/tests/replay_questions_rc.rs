//! #12029 P2f: the `questions_rc.py` and `start-architect-questions.sh`
//! recordings replay clean against the port.
//!
//! Recorded from the frozen scripts through `questions_rc/driver.py` on
//! synthetic stores, queues and panes, with tmux, the three action scripts,
//! `python3` and `relay.sh` declared as fakes. `examples/replay_port.rs`
//! dispatches them to `questions::cases::replay`. Every tmux call names the
//! private socket, and every write lands under the sandbox root.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use std::time::Duration;

use trusty_architect::questions::cases::REPLAY_SOCKET;
use trusty_architect::replay::{Recording, Target, load, replay};

mod replay_support;

const DIR: &str = "questions_rc";

fn fixtures() -> Vec<(String, Recording)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/replay")
        .join(DIR);
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let rec = load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        out.push((name, rec));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Replays every recording whose name `pick` accepts; returns the names.
fn replay_where(pick: impl Fn(&str) -> bool) -> Vec<String> {
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec![DIR.to_owned()],
    };
    let parent = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let recs: Vec<(String, Recording)> = fixtures().into_iter().filter(|(n, _)| pick(n)).collect();
    let mut failed = Vec::new();
    for (name, rec) in &recs {
        let found = replay(rec, &target, &parent, Duration::from_secs(120)).unwrap();
        if !found.is_empty() {
            failed.push(format!("{name}: {found:?}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    recs.into_iter().map(|(n, _)| n).collect()
}

#[test]
fn questions_rc_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_where(|n| n.starts_with("rc-")),
        [
            "rc-dry-run-off.json",
            "rc-dry-run.json",
            "rc-env-paths.json",
            "rc-no-store.json",
            "rc-none-down.json",
            "rc-none-on.json",
            "rc-off.json",
            "rc-on.json",
            "rc-start.json"
        ]
    );
}

#[test]
fn start_recordings_replay_clean_against_the_port() {
    assert_eq!(
        replay_where(|n| n.starts_with("start-")),
        [
            "start-bad-flag.json",
            "start-bad-id.json",
            "start-config-file.json",
            "start-custom-queue.json",
            "start-dry-down.json",
            "start-dry-up.json",
            "start-empty-arg.json",
            "start-no-config.json",
            "start-up-relay-fails.json",
            "start-up-relay.json",
            "start-up-typed.json",
            "start-up-unknown.json",
            "start-usage.json"
        ]
    );
}

#[test]
fn questions_cases_replay_clean_against_the_port() {
    assert_eq!(
        replay_where(|n| !n.starts_with("rc-") && !n.starts_with("start-")),
        ["cases.json", "driver-usage.json"]
    );
}

/// C2: every recording that reaches tmux declares a tmux fake, and every
/// recorded call and canned answer names the private socket first.
#[test]
fn questions_replay_tmux_calls_use_the_private_socket() {
    let private = ["-L", REPLAY_SOCKET];
    let mut calls = 0;
    for (name, rec) in fixtures() {
        let Some(fakes) = &rec.fakes else { continue };
        for answer in fakes.stubs.get("tmux").into_iter().flatten() {
            let argv = answer.argv.as_deref().unwrap_or_default();
            assert!(
                argv.starts_with(&private.map(String::from)),
                "{name}: {argv:?}"
            );
        }
        for call in fakes.calls.iter().filter(|c| c[0] == "tmux") {
            assert_eq!(call[1..3], private, "{name}: {call:?}");
            calls += 1;
        }
    }
    assert!(calls >= 40, "only {calls} tmux calls recorded");
}

/// A recorded path: relative, with no `..` or root component.
fn inside_sandbox(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
}

/// C3: every write is relative to the sandbox root, and every path the
/// recordings hand the scripts through the environment is too.
#[test]
fn questions_replay_writes_stay_in_the_sandbox() {
    let mut writes = 0;
    for (name, rec) in fixtures() {
        for path in rec.writes.keys() {
            assert!(inside_sandbox(path), "{name}: {path}");
            writes += 1;
        }
        for (k, v) in &rec.env {
            let path_var = [
                "ARCH_Q_QUEUE",
                "QUESTIONS_RC_STATE",
                "RC_URL_LOG",
                "CLAUDE_CONFIG_DIR",
            ];
            if path_var.contains(&k.as_str()) && !v.is_empty() {
                let rel = v.strip_prefix("{ROOT}/").unwrap_or(v);
                assert!(inside_sandbox(rel), "{name}: {k}={v}");
            }
        }
    }
    assert!(writes >= 15, "only {writes} writes recorded");
}

/// The P2d rule: no two recorded paths, and no two fixture names, differ
/// only by letter case, so the set replays alike on Linux and macOS.
#[test]
fn questions_fixture_paths_never_differ_only_by_case() {
    let folded = |names: &mut dyn Iterator<Item = String>| -> Vec<String> {
        let mut seen = BTreeMap::new();
        let mut clashes = Vec::new();
        for n in names {
            if let Some(prev) = seen.insert(n.to_lowercase(), n.clone())
                && prev != n
            {
                clashes.push(format!("{prev} / {n}"));
            }
        }
        clashes
    };
    // The check itself can fail: two names one case apart clash.
    let pair = ["items/D01.json", "items/d01.json"].map(String::from);
    assert_eq!(folded(&mut pair.into_iter()).len(), 1);
    let recs = fixtures();
    assert!(folded(&mut recs.iter().map(|(n, _)| n.clone())).is_empty());
    for (name, rec) in &recs {
        let paths: BTreeSet<String> = rec.files.keys().chain(rec.writes.keys()).cloned().collect();
        let clashes = folded(&mut paths.into_iter());
        assert!(clashes.is_empty(), "{name}: {clashes:?}");
    }
}
