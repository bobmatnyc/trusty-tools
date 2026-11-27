//! Port of the Architect's `spec_drift_check.py`, the anti-drift check for
//! SPEC-ARCHITECT-COMPONENTS (#12028 P1b).
//!
//! Why: epic #12025 moves the Architect's scripts to Rust, each once its
//! port reproduces the script's recorded runs byte for byte.
//! What: [`run`] parses the script's arguments (argparse's messages and exit
//! code 2 included), reads the manifest under the root, runs the ten checks,
//! prints one `INV-nn STATUS message` line each, appends one sorted-key JSON
//! line to `local/soak/spec-drift.jsonl` and, on a FAIL, one `soak` line to
//! `local/improvements.md`, and exits 1 on any FAIL. Time comes only from the
//! [`Clock`] the caller passes; nothing here reads the environment.
//! Known gaps, all outside the recorded cases: a manifest that is not JSON
//! reports `serde_json`'s message (only an empty one gets Python's text); an
//! exception the script does not catch (a `TypeError` from a mistyped
//! manifest, a `re` pattern outside [`Pattern::compile_python`], a store
//! error) prints one line instead of a traceback; directory listing order
//! follows the host, as `glob` does.
//! Test: `tests/replay_spec_drift_check.rs`, `tests/spec_drift_check_smoke.rs`.
//!
//! [`Pattern::compile_python`]: crate::dashboard::schema_check::pattern::Pattern::compile_python

mod args;
mod checks;
mod py;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::classify::alerts::civil_from_days;
use crate::clock::Clock;
use args::Parsed;
use py::Raise;

const SPEC: &str = "docs/specs/SPEC-ARCHITECT-COMPONENTS.md";
const MANIFEST: &str = "docs/specs/architect-components.manifest.json";
const SOAK_LOG: &str = "local/soak/spec-drift.jsonl";
const LEDGER: &str = "local/improvements.md";
const INDEX_PATH: &str = ".claude/skills/architect/SKILL.md";

/// What the caller supplies besides argv and the clock.
#[derive(Debug, Clone)]
pub struct Options {
    /// The program name argparse prints in usage and error lines.
    pub prog: String,
    /// The root when `--root` is absent (the script's own checkout).
    pub default_root: PathBuf,
    /// `DASHBOARD_FORMAT`, which the script's store reads (`dir` when unset).
    pub dashboard_format: Option<String>,
}

/// A finished run: the streams and the exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
    /// The exit code.
    pub code: i32,
}

/// One invariant's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The invariant holds.
    Pass,
    /// Not yet enforced and not yet met.
    Warn,
    /// The invariant is broken.
    Fail,
    /// Waits for a later phase.
    Pending,
}

impl Status {
    /// The word the script prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Pending => "PENDING",
        }
    }
}

fn outcome(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// `(%Y-%m-%d, %H:%M:%S)` in UTC for the clock's current second.
fn utc_now(clock: &dyn Clock) -> (String, String) {
    let t = clock.now_unix().floor() as i64;
    let (y, mo, d) = civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    (
        format!("{y:04}-{mo:02}-{d:02}"),
        format!("{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60),
    )
}

/// `json.loads` failure text: Python's for an empty document, else serde's.
fn json_error(s: &str, e: &serde_json::Error) -> String {
    let idx = s.len() - s.trim_start_matches([' ', '\t', '\n', '\r']).len();
    if idx < s.len() {
        return e.to_string();
    }
    let line = s.matches('\n').count() + 1;
    let col = s.rfind('\n').map_or(idx + 1, |nl| idx - nl);
    format!("Expecting value: line {line} column {col} (char {idx})")
}

/// Runs `spec_drift_check` with `args` (argv after the program name).
///
/// Why: the shipped command and the replay example share one body, so the
/// replayed code is the shipped code (#12028 D6a).
/// What: see the module doc. File writes happen here; a write failure is
/// reported on stderr with exit 1 after the result lines.
/// Test: `spec_drift_recordings_replay_clean_against_the_port`,
/// `shipped_spec_drift_command_prints_ten_results_and_logs_one_line`.
pub fn run(args: &[String], opts: &Options, clock: &dyn Clock) -> Outcome {
    let (root, no_log) = match args::parse(args) {
        Parsed::Run { root, no_log } => (
            root.map_or_else(|| opts.default_root.clone(), PathBuf::from),
            no_log,
        ),
        Parsed::Help => return outcome(args::help(&opts.prog), String::new(), 0),
        Parsed::Error(msg) => return outcome(String::new(), args::error(&opts.prog, &msg), 2),
    };
    let cannot = |msg: String| {
        outcome(
            String::new(),
            format!("FAIL spec_drift_check: cannot read {MANIFEST}: {msg}\n"),
            1,
        )
    };
    let raw = match py::read(&root.join(MANIFEST)) {
        Ok(t) => t,
        Err(e) => return cannot(e.msg),
    };
    let manifest: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return cannot(json_error(&raw, &e)),
    };
    let mut results = Vec::new();
    for (n, check) in checks::CHECKS.iter().enumerate() {
        let inv = format!("INV-{:02}", n + 1);
        let (status, msg) = match check(&root, &manifest, opts) {
            Ok(v) => v,
            Err(Raise::Caught(e)) => (Status::Fail, format!("check error: {}: {}", e.class, e.msg)),
            // See #12028: an exception the script does not catch ends the run.
            Err(Raise::Crash(msg)) => {
                return outcome(
                    String::new(),
                    format!("spec_drift_check: {inv}: {msg}\n"),
                    1,
                );
            }
        };
        results.push((inv, status, msg));
    }
    let failed = results.iter().any(|(_, s, _)| *s == Status::Fail);
    let stdout: String = results
        .iter()
        .map(|(i, s, m)| format!("{i} {} {m}\n", s.as_str()))
        .collect();
    let code = i32::from(failed);
    if !no_log {
        let logged =
            append_soak(&root, &results, clock).and_then(|()| file_finding(&root, &results, clock));
        if let Err(e) = logged {
            return outcome(stdout, format!("spec_drift_check: {e}\n"), 1);
        }
    }
    outcome(stdout, String::new(), code)
}

fn append(path: &Path, line: &str) -> Result<(), String> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .map_err(|e| format!("cannot append to {}: {e}", path.display()))
}

/// `append_soak`: one `json.dumps(rec, sort_keys=True)` line.
fn append_soak(
    root: &Path,
    results: &[(String, Status, String)],
    clock: &dyn Clock,
) -> Result<(), String> {
    let (date, time) = utc_now(clock);
    let path = root.join(SOAK_LOG);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let failed = results.iter().any(|(_, s, _)| *s == Status::Fail);
    let mut detail = Map::new();
    let mut statuses = Map::new();
    for (inv, status, msg) in results {
        statuses.insert(inv.clone(), Value::from(status.as_str()));
        if *status != Status::Pass {
            detail.insert(inv.clone(), Value::from(msg.as_str()));
        }
    }
    // Keys inserted in sorted order, so the line is sorted with or without
    // serde_json's `preserve_order`.
    let mut rec = Map::new();
    rec.insert("date".into(), Value::from(date.clone()));
    rec.insert("detail".into(), Value::Object(detail));
    rec.insert("exit".into(), Value::from(i32::from(failed)));
    rec.insert("results".into(), Value::Object(statuses));
    rec.insert("ts".into(), Value::from(format!("{date}T{time}Z")));
    let line =
        crate::pyjson::to_python_json(&Value::Object(rec), true).map_err(|e| e.to_string())?;
    append(&path, &(line + "\n"))
}

/// `file_soak_finding`: one ledger line when any check failed.
fn file_finding(
    root: &Path,
    results: &[(String, Status, String)],
    clock: &dyn Clock,
) -> Result<(), String> {
    let fails: Vec<String> = results
        .iter()
        .filter(|(_, s, _)| *s == Status::Fail)
        .map(|(i, _, m)| format!("{i}: {m}"))
        .collect();
    if fails.is_empty() {
        return Ok(());
    }
    let (date, time) = utc_now(clock);
    let line = format!(
        "- {date}T{}Z soak FINDING (spec_drift_check): {} FAIL. {}. Fix: repair the file or the manifest named, then re-run the check. Status: open.\n",
        &time[..5],
        fails.len(),
        fails.join(" | ")
    );
    append(&root.join(LEDGER), &line)
}
