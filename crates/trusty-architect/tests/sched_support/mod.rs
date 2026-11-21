//! Shared seams for the scheduler tests: a scripted runner and fixtures.
//! Nothing here starts a real process; the agent kind is only ever run
//! through [`FakeRunner`].
#![allow(dead_code)]

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use trusty_architect::sched::config::Registry;
use trusty_architect::sched::state::SchedState;
use trusty_architect::sched::{Engine, JobRunner, Layout, RunResult, RunSpec};
use trusty_architect::{Clock, FakeClock};

/// 2023-11-15T00:00:00Z, a UTC midnight.
pub const T0: i64 = 1_700_006_400;

/// Records every call; results come from a queue (default: exit 0).
#[derive(Default)]
pub struct FakeRunner {
    pub calls: Mutex<Vec<RunSpec>>,
    pub results: Mutex<VecDeque<RunResult>>,
    pub hold: AtomicBool,
    pub stdout_text: Mutex<Option<String>>,
}

impl FakeRunner {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn push(&self, r: RunResult) {
        self.results.lock().unwrap().push_back(r);
    }
    pub fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
    pub fn last(&self) -> RunSpec {
        self.calls.lock().unwrap().last().cloned().expect("a call")
    }
}

impl JobRunner for FakeRunner {
    fn run(&self, spec: &RunSpec) -> RunResult {
        self.calls.lock().unwrap().push(spec.clone());
        while self.hold.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(text) = self.stdout_text.lock().unwrap().clone() {
            // The real runner creates the log directory; so does the fake.
            std::fs::create_dir_all(spec.stdout.parent().unwrap()).unwrap();
            std::fs::write(&spec.stdout, text).unwrap();
        }
        self.results.lock().unwrap().pop_front().unwrap_or(ok())
    }
}

pub fn ok() -> RunResult {
    RunResult {
        exit: Some(0),
        ..RunResult::default()
    }
}

pub fn failed() -> RunResult {
    RunResult {
        exit: Some(1),
        ..RunResult::default()
    }
}

pub fn timed_out() -> RunResult {
    RunResult {
        timed_out: true,
        ..RunResult::default()
    }
}

pub fn layout(dir: &Path) -> Layout {
    Layout::new(dir, dir)
}

/// A registry with the poll job off, plus `jobs` (a JSON array body).
pub fn reg(jobs: &str) -> Registry {
    reg_with(jobs, r#""poll": {"enabled": false}"#)
}

pub fn reg_with(jobs: &str, extra: &str) -> Registry {
    let text = format!(r#"{{"version": 1, {extra}, "jobs": [{jobs}]}}"#);
    Registry::parse(&text, Path::new("test-registry.json")).expect("valid registry")
}

/// A script job running every `every` with extra top-level `fields`
/// (leading comma not needed; pass `""` for none).
pub fn script(id: &str, every: &str, fields: &str) -> String {
    let more = if fields.is_empty() {
        String::new()
    } else {
        format!(",{fields}")
    };
    format!(
        r#"{{"id":"{id}","title":"t","owner":"architect","kind":"script",
        "script":{{"argv":["job-{id}","x"]}},"schedule":{{"every":"{every}"}},
        "enabled":true{more}}}"#
    )
}

pub fn engine<'a>(
    dir: &Path,
    clock: &'a FakeClock,
    runner: &Arc<FakeRunner>,
    reg: Registry,
) -> Engine<'a> {
    let env = vec![
        ("PATH".to_owned(), "/bin".to_owned()),
        ("SECRET_TOKEN".to_owned(), "nope".to_owned()),
    ];
    Engine::new(
        layout(dir),
        clock,
        runner.clone(),
        env,
        reg,
        SchedState::default(),
        dir.join("scratch"),
    )
}

pub fn advance(clock: &FakeClock, secs: u64) {
    clock.sleep(Duration::from_secs(secs));
}

/// Ticks, then waits for every started run; returns every record.
pub fn tick_and_wait(
    e: &mut Engine<'_>,
) -> (Vec<String>, Vec<trusty_architect::sched::runlog::RunRecord>) {
    let rep = e.tick().expect("tick");
    let mut records = rep.records;
    records.extend(e.wait_idle(Duration::from_secs(10)).records);
    (rep.started, records)
}

pub fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|t| t.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

pub fn now(clock: &FakeClock) -> i64 {
    clock.now_unix() as i64
}
