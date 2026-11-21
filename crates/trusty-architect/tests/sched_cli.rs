//! The command line: argument parsing and every verb, with injected seams
//! (MB1595). `cutover` and `rollback` run against a temp directory and a fake.

mod sched_support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sched_support::*;
use trusty_architect::FakeClock;
use trusty_architect::sched::SchedError;
use trusty_architect::sched::cli::{Deps, execute};
use trusty_architect::sched::cli_args::{VERBS, parse};
use trusty_architect::sched::cutover::Ops;
use trusty_architect::sched::logger;

fn argv(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

#[derive(Default)]
struct Calls(Mutex<Vec<&'static str>>);

impl Ops for Calls {
    fn stop_supervisor(&self) -> Result<String, String> {
        self.0.lock().unwrap().push("stop");
        Ok("stopped".into())
    }
    fn start_supervisor(&self) -> Result<String, String> {
        self.0.lock().unwrap().push("start");
        Ok("started".into())
    }
}

fn deps<'a>(
    dir: &std::path::Path,
    clock: &'a FakeClock,
    runner: Arc<FakeRunner>,
    ops: Option<&'a Calls>,
) -> Deps<'a> {
    Deps {
        clock,
        runner,
        ops: ops.map(|o| o as &dyn Ops),
        env: vec![
            ("PATH".into(), "/bin".into()),
            ("GH_TOKEN".into(), "secret".into()),
        ],
        home: dir.to_owned(),
        cwd: PathBuf::from("/nonexistent-cwd"),
        scratch: dir.join("scratch"),
    }
}

fn setup(dir: &std::path::Path, jobs: &str) {
    let path = dir.join("local/meta/registry.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!(r#"{{"version":1,"jobs":[{jobs}]}}"#)).unwrap();
}

fn run(
    dir: &std::path::Path,
    line: &str,
    clock: &FakeClock,
    runner: &Arc<FakeRunner>,
    ops: Option<&Calls>,
) -> Result<(String, i32), SchedError> {
    let out = execute(&argv(line), &deps(dir, clock, runner.clone(), ops))?;
    Ok((out.stdout, out.code))
}

#[test]
fn args_parse_every_verb_and_flag() {
    for verb in VERBS {
        let line = if verb == "run" {
            format!("{verb} --once")
        } else {
            verb.to_owned()
        };
        assert_eq!(parse(&argv(&line)).unwrap().verb, verb);
    }
    let a = parse(&argv("cutover --root /r --config /c.json --json --since 2h --force --min-cycles 10 --min-match-pct 97.5 --exe /bin/x")).unwrap();
    assert_eq!(a.root, Some("/r".into()));
    assert_eq!(a.config, Some("/c.json".into()));
    assert_eq!(
        (a.json, a.force, a.since_s, a.min_cycles, a.min_match_pct),
        (true, true, Some(7200), Some(10), Some(97.5))
    );
    assert_eq!(a.exe, Some("/bin/x".into()));
}

#[test]
fn args_reject_unknown_and_malformed() {
    for bad in [
        "",
        "frobnicate",
        "run",
        "status --bogus",
        "status --root",
        "parity --since soon",
        "cutover --min-cycles ten",
        "cutover --min-match-pct 101",
        "cutover --min-match-pct x",
    ] {
        let e = parse(&argv(bad)).unwrap_err();
        assert!(
            matches!(e, SchedError::Usage(_)) && e.exit_code() == 2,
            "{bad:?}: {e}"
        );
    }
}

#[test]
fn validate_verb_counts_jobs_by_who_runs_them() {
    let dir = tempfile::tempdir().unwrap();
    let managed = script("mng", "1m", r#""managed_by":"meta_due""#);
    let off = script("off", "1m", r#""reason":"paused""#)
        .replace(r#""enabled":true"#, r#""enabled":false"#);
    setup(
        dir.path(),
        &format!("{},{managed},{off}", script("job1", "1m", "")),
    );
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let (out, code) = run(
        dir.path(),
        &format!("validate --root {}", dir.path().display()),
        &clock,
        &runner,
        None,
    )
    .unwrap();
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "ok: 3 jobs (1 scheduler-run, 1 meta_due, 0 external, 1 disabled)\n"
    );
    // An empty config is a hard error with exit code 2.
    std::fs::write(dir.path().join("local/meta/registry.json"), "").unwrap();
    let e = run(
        dir.path(),
        &format!("validate --root {}", dir.path().display()),
        &clock,
        &runner,
        None,
    )
    .unwrap_err();
    assert_eq!(e.exit_code(), 2);
}

#[test]
fn status_verb_exits_nonzero_without_a_live_scheduler() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path(), &script("job1", "1m", ""));
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let (out, code) = run(
        dir.path(),
        &format!("status --root {}", dir.path().display()),
        &clock,
        &runner,
        None,
    )
    .unwrap();
    assert_eq!(code, 1);
    assert!(out.contains("NOT ALIVE"));
}

#[test]
fn run_once_ticks_once_and_waits_for_what_it_started() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path(), &script("job1", "1m", ""));
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let line = format!("run --once --root {}", dir.path().display());
    // First sight: only the poll job (shadow) is due; the 1 m job anchors.
    let (out, code) = run(dir.path(), &line, &clock, &runner, None).unwrap();
    assert_eq!(code, 0);
    assert!(
        out.contains("1 run(s) started") && out.contains("poll"),
        "{out}"
    );
    assert_eq!(runner.count(), 1);
    assert_eq!(
        runner.last().argv.last().map(String::as_str),
        Some("--dry-run")
    );
    // Secrets in the process environment never reach a child.
    assert!(runner.last().env.iter().all(|(k, _)| k != "GH_TOKEN"));
    // State persisted: a minute later the job is due and runs.
    advance(&clock, 60);
    let (out, _) = run(dir.path(), &line, &clock, &runner, None).unwrap();
    assert!(out.contains("job1 slot") && out.contains("ok"), "{out}");
}

#[test]
fn cutover_and_rollback_verbs_use_the_injected_ops() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path(), &script("job1", "1m", ""));
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let ops = Calls::default();
    let root = dir.path().display();
    // Without parity evidence the cutover verb fails and stops nothing.
    assert!(
        run(
            dir.path(),
            &format!("cutover --root {root}"),
            &clock,
            &runner,
            Some(&ops)
        )
        .is_err()
    );
    assert!(ops.0.lock().unwrap().is_empty());
    let (out, code) = run(
        dir.path(),
        &format!("cutover --force --root {root}"),
        &clock,
        &runner,
        Some(&ops),
    )
    .unwrap();
    assert!(out.starts_with("live:") && code == 0, "{out}");
    let (out, _) = run(
        dir.path(),
        &format!("rollback --root {root}"),
        &clock,
        &runner,
        Some(&ops),
    )
    .unwrap();
    assert!(out.starts_with("shadow:"), "{out}");
    assert_eq!(*ops.0.lock().unwrap(), ["stop", "start"]);
}

#[test]
fn parity_and_plist_verbs_print_and_write_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path(), &script("job1", "1m", ""));
    let (clock, runner) = (FakeClock::new(T0 as f64), FakeRunner::new());
    let root = dir.path().display();
    // No shadow record yet: an error, never a zero report.
    assert!(
        run(
            dir.path(),
            &format!("parity --root {root}"),
            &clock,
            &runner,
            None
        )
        .is_err()
    );
    let (xml, _) = run(
        dir.path(),
        &format!("plist --root {root} --exe /x/sched"),
        &clock,
        &runner,
        None,
    )
    .unwrap();
    assert!(
        xml.contains("com.trusty.architect-sched") && xml.contains("<string>/x/sched</string>")
    );
    assert!(!dir.path().join("Library").exists(), "plist is render-only");
}

#[test]
fn logger_installs_once() {
    assert!(logger::init().is_ok());
    assert!(logger::init().is_err());
    // An event after install must not panic.
    tracing::warn!(job = "x", "logger smoke");
}
