//! #9171 A3a: `poll-state.json` and `alerts.jsonl` survive a writer killed at
//! any point. Every file lives in a tempdir; no test touches a live inbox.

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trusty_architect::StateError;
use trusty_architect::classify::{Alert, DedupState, PaneState, alert, dedup};
use trusty_architect::poller::{
    PollState, SaveStep, StoredCount, append_alerts, load_state, save_state, save_state_with,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn sample(n: u64) -> PollState {
    let mut st = PollState {
        events_offset: Some(n),
        band: Some("open".to_owned()),
        ..PollState::default()
    };
    st.panes.insert(
        "tm-a|%1".to_owned(),
        PaneState {
            idle_since: Some(5.0),
            errors: Some(vec![]),
        },
    );
    st.wake_pending = Some(StoredCount::Count(n));
    st
}

/// A kill after any step leaves `path` holding the old state or the new one.
/// Red against a save that writes `path` in place (a kill after `Written`
/// would leave the new bytes half-synced in the only copy) or that renames
/// before writing.
#[test]
fn a_save_killed_between_steps_leaves_a_state_that_parses() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("poll-state.json");
    save_state(&path, &sample(1))?;
    for (stop, want) in [
        (SaveStep::Written, 1),
        (SaveStep::Synced, 1),
        (SaveStep::Renamed, 2),
    ] {
        save_state(&path, &sample(1))?;
        let mut at = |s: SaveStep| {
            if s == stop {
                Err(io::Error::other("killed"))
            } else {
                Ok(())
            }
        };
        let r = save_state_with(&path, &sample(2), &mut at);
        assert!(matches!(r, Err(StateError::Io { .. })), "{stop:?}: {r:?}");
        let (st, err) = load_state(&path);
        assert!(err.is_none(), "{stop:?}: {err:?}");
        assert_eq!(st, sample(want), "{stop:?}");
    }
    Ok(())
}

const CHILD_ENV: &str = "TRUSTY_ARCHITECT_STATE_KILL_CHILD";

/// Saves a growing state in a loop until killed (or 20 s pass).
fn child_loop(path: &Path) -> TestResult {
    let start = Instant::now();
    let mut n = 0u64;
    while start.elapsed() < Duration::from_secs(20) {
        n += 1;
        let mut st = sample(n);
        // A large value so each write spans several syscalls.
        st.other
            .insert("pad".to_owned(), json!("x".repeat(256 * 1024)));
        save_state(path, &st)?;
    }
    Ok(())
}

/// A real SIGKILL mid-save, repeated at several delays: the state on disk
/// always parses and is never empty. The child is this test binary re-run on
/// this one test with an env var set.
#[test]
fn state_survives_sigkill_mid_save() -> TestResult {
    if let Some(path) = std::env::var_os(CHILD_ENV) {
        return child_loop(Path::new(&path));
    }
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("poll-state.json");
    save_state(&path, &sample(0))?;
    for delay_ms in [30u64, 70, 110, 170, 230] {
        let mut child = Command::new(std::env::current_exe()?)
            .args([
                "state_survives_sigkill_mid_save",
                "--exact",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, &path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        std::thread::sleep(Duration::from_millis(delay_ms));
        child.kill()?; // SIGKILL on Unix
        child.wait()?;
        let (st, err) = load_state(&path);
        assert!(err.is_none(), "after kill at {delay_ms} ms: {err:?}");
        assert!(
            st.events_offset.is_some(),
            "after kill at {delay_ms} ms the state is empty"
        );
    }
    Ok(())
}

/// Keys the Rust types do not know (wake counters, rc, usage, ...) come back
/// unchanged; a dedup map keeps its insertion order.
#[test]
fn unknown_state_keys_round_trip() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("poll-state.json");
    // Written as Python's json.dump would: dict order, not sorted.
    let text = r#"{"panes": {"tm-a|%1": {"idle_since": 0.0, "errors": ["Error: Exit code 1"]}}, "sessions": {"tm-a": "%1"}, "rc": {"tm-a": {"last": 12.5, "skips": 2}}, "alerts": {"z|%1|idle|f": {"first": 1.0, "last": 2.0}, "a|%2|ctx|25": {"first": 3.0, "last": 4.0}}, "events_offset": 432, "band": "hold", "wake_pending": 3, "usage": {"nested": [1, null, "\u00e9", {"k": true}]}}"#;
    fs::write(&path, text)?;
    let python: Value = serde_json::from_str(text)?;
    let (st, err) = load_state(&path);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(
        st.alerts.keys().collect::<Vec<_>>(),
        ["z|%1|idle|f", "a|%2|ctx|25"]
    );
    assert_eq!(
        (st.events_offset, st.band.as_deref()),
        (Some(432), Some("hold"))
    );
    save_state(&path, &st)?;
    let back: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
    assert_eq!(back, python);
    let text = fs::read_to_string(&path)?;
    assert!(
        text.find("z|%1|idle|f") < text.find("a|%2|ctx|25"),
        "dedup order kept: {text}"
    );
    assert!(text.contains("\\u00e9"), "json.dump's ensure_ascii: {text}");
    // A dedup round trip through the state keeps the A2 semantics.
    let mut d: DedupState = st.alerts.clone();
    let a = alert("idle", "f", "d", "z", "%1", 0.0);
    assert_eq!(dedup(&mut d, &[a], 10.0).1, 1);
    Ok(())
}

/// A corrupt or truncated file reads as an empty state plus the error to log;
/// a missing file is empty with no error.
#[test]
fn corrupt_state_loads_empty_and_reports_it() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("poll-state.json");
    assert!(matches!(load_state(&path), (ref s, None) if *s == PollState::default()));
    for bad in ["{\"events_offset\": 4", "[]", "", "{\"band\": 5}"] {
        fs::write(&path, bad)?;
        let (st, err) = load_state(&path);
        assert_eq!(st, PollState::default(), "{bad:?}");
        assert!(
            matches!(err, Some(StateError::Corrupt { .. })),
            "{bad:?}: {err:?}"
        );
    }
    Ok(())
}

fn lines_parse(path: &Path) -> io::Result<Vec<bool>> {
    Ok(fs::read_to_string(path)?
        .lines()
        .map(|l| serde_json::from_str::<Alert2>(l).is_ok())
        .collect())
}

#[derive(serde::Deserialize)]
#[allow(dead_code)] // fields exist to make the line parse as an alert row
struct Alert2 {
    kind: String,
    fingerprint: String,
}

/// A writer killed mid-append leaves a partial last line; the next append
/// starts on a fresh line, so only the partial line is lost.
#[test]
fn a_truncated_last_alert_line_does_not_corrupt_the_next_append() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("alerts.jsonl");
    let a: Vec<Alert> = ["idle", "ctx"]
        .iter()
        .map(|k| alert(k, "f", "d", "tm-x", "%1", 0.0))
        .collect();
    append_alerts(&path, &a[..1])?;
    let whole = fs::read_to_string(&path)?;
    fs::write(&path, format!("{whole}{}", &whole[..30]))?;
    append_alerts(&path, &a[1..])?;
    assert_eq!(lines_parse(&path)?, [true, false, true]);
    Ok(())
}

/// Names in `dir`, sorted.
fn names(dir: &Path) -> io::Result<Vec<String>> {
    let mut v = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<Vec<_>>>()?;
    v.sort();
    Ok(v)
}

/// Two writers in flight at once never share a temporary file. Writer A is
/// held after its bytes are written while writer B completes a whole save;
/// A must then finish too. Red against a fixed `<path>.tmp`: B truncates and
/// renames A's file, so A's sync-then-rename fails.
#[test]
fn concurrent_saves_use_distinct_temporary_files() -> TestResult {
    use std::sync::mpsc::channel;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("poll-state.json");
    let (parked_tx, parked_rx) = channel::<()>();
    let (go_tx, go_rx) = channel::<()>();
    let a = {
        let path = path.clone();
        std::thread::spawn(move || {
            let mut at = |s: SaveStep| {
                if s == SaveStep::Written {
                    parked_tx.send(()).map_err(io::Error::other)?;
                    go_rx.recv().map_err(io::Error::other)?;
                }
                Ok(())
            };
            save_state_with(&path, &sample(1), &mut at)
        })
    };
    parked_rx.recv()?;
    let b = save_state(&path, &sample(2));
    go_tx.send(())?;
    let a = a.join().map_err(|_| "writer A panicked")?;
    assert!(b.is_ok(), "writer B: {b:?}");
    assert!(a.is_ok(), "writer A: {a:?}");
    let (st, err) = load_state(&path);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(st, sample(1), "A renamed last, so A's bytes are on disk");
    assert_eq!(names(dir.path())?, ["poll-state.json"], "no stray tmp");
    Ok(())
}

/// A save that fails after its temporary file exists removes that file.
#[test]
fn a_failed_save_leaves_no_temporary_file() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("poll-state.json");
    save_state(&path, &sample(1))?;
    for stop in [SaveStep::Written, SaveStep::Synced, SaveStep::Renamed] {
        let mut at = |s: SaveStep| {
            if s == stop {
                Err(io::Error::other("killed"))
            } else {
                Ok(())
            }
        };
        assert!(save_state_with(&path, &sample(2), &mut at).is_err());
        let left = names(dir.path())?;
        assert_eq!(left, ["poll-state.json"], "after {stop:?}: {left:?}");
        save_state(&path, &sample(1))?;
    }
    Ok(())
}
