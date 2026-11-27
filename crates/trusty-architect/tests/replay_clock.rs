//! #12028 AC2: a frozen clock lets time-stamped output replay without a diff.
//!
//! Each test records [`SCRIPT`], a Python script that prints the time and
//! appends a time-stamped line to `soak.log`, with `record --clock`, then
//! replays it against `examples/replay_standin_clock.rs`. The tests drive the
//! CLI and read the fixture as `serde_json::Value`, so they name no library
//! type the change adds.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use serde_json::Value;

mod replay_support;

const BIN: &str = env!("CARGO_BIN_EXE_trusty-architect-replay");
/// 2023-11-14T22:13:20Z.
const INSTANT: &str = "1700000000";
const SCRIPT: &str = r#"import datetime, time
print(time.time())
print(time.strftime("%Y-%m-%d %H:%M:%S %Z", time.localtime()))
print(datetime.datetime.now().isoformat())
with open("soak.log", "a", encoding="utf-8") as f:
    f.write(time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime()) + " soak ok\n")
"#;

fn standin() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| replay_support::build_example("replay_standin_clock"))
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Records [`SCRIPT`] at [`INSTANT`] into `dir/clock.json` and returns its path.
fn record_clocked(dir: &Path) -> String {
    let script = dir.join("soak.py");
    std::fs::write(&script, SCRIPT).unwrap();
    let out = dir.join("clock.json");
    let o = out.to_string_lossy().into_owned();
    let rec = run(&[
        "record",
        "--clock",
        INSTANT,
        "--out",
        &o,
        "--",
        script.to_str().unwrap(),
    ]);
    assert_eq!(rec.status.code(), Some(0), "{}", text(&rec.stderr));
    o
}

#[test]
fn a_clocked_script_replays_clean_against_a_stand_in_that_reads_the_seam() {
    let dir = tempfile::tempdir().unwrap();
    let o = record_clocked(dir.path());
    let fx: Value = serde_json::from_str(&std::fs::read_to_string(&o).unwrap()).unwrap();
    assert_eq!(fx["version"], 3, "a recording with a clock is version 3");
    assert_eq!(fx["clock"], 1_700_000_000_i64);
    assert_eq!(fx["env"]["TZ"], "UTC", "the zone is pinned and recorded");
    assert_eq!(
        fx["stdout"],
        "1700000000.0\n2023-11-14 22:13:20 UTC\n2023-11-14T22:13:20\n"
    );
    assert_eq!(fx["writes"]["soak.log"], "2023-11-14T22:13:20 soak ok\n");
    let program = standin().to_str().unwrap();
    let rep = run(&["replay", &o, "--", program]);
    assert_eq!(rep.status.code(), Some(0), "{}", text(&rep.stdout));
    // The Python script replays clean against itself under the same wrapper.
    let script = dir.path().join("soak.py");
    let own = run(&["replay", &o, "--", script.to_str().unwrap()]);
    assert_eq!(own.status.code(), Some(0), "{}", text(&own.stdout));
}

#[test]
fn clock_on_a_program_the_wrapper_cannot_freeze_is_refused() {
    // A non-`.py` program runs without the wrapper, so a recording would
    // claim an instant the run never saw.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("sh.json");
    let o = out.to_str().unwrap();
    let rec = run(&[
        "record", "--clock", INSTANT, "--out", o, "--", "/bin/sh", "-c", "date",
    ]);
    assert_eq!(rec.status.code(), Some(2), "{}", text(&rec.stderr));
    assert!(
        text(&rec.stderr).contains("--clock"),
        "{}",
        text(&rec.stderr)
    );
    assert!(!out.exists(), "no fixture is written");
}

#[test]
fn a_stand_in_that_ignores_the_clock_seam_reports_a_diff() {
    let dir = tempfile::tempdir().unwrap();
    let o = record_clocked(dir.path());
    let program = standin().to_str().unwrap();
    let rep = run(&["replay", &o, "--", program, "--ignore-clock"]);
    let stdout = text(&rep.stdout);
    assert_eq!(rep.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("DIFF"), "{stdout}");
    assert!(stdout.contains("Stdout:"), "{stdout}");
    assert!(stdout.contains("file soak.log:"), "{stdout}");
}
