//! Shadow mode: the poll cycle as a dry-run script job, and its record.
//!
//! Why: the Architect decides the cutover from evidence that the scheduler
//! would fire the same wakes as the Python poller. A dry run writes nothing,
//! so each run needs its own record that a parity join can read, and the run
//! must never touch the live inbox, take `poll.lock` or send keys (R4).
//! What: [`plan`] gives the dry run its own state file in the scratch inbox
//! (`local/meta/shadow/dry-run-state.json`), seeded once from the live
//! `poll-state.json` and then kept, so the dry run dedupes across cycles. It
//! builds the dry-run [`RunSpec`] with `ARCHITECT_INBOX_DIR` pointing at the
//! scratch inbox, `ARCHITECT_READ_INBOX_DIR` at the live one (read-only
//! queues, MB1595) and `ARCHITECT_DRY_RUN_STATE` at that file. [`parse_decision`]
//! reads the dry-run's stdout: the cycle line (`panes= new= woken= logged=
//! suppressed= wake=`) and one `would alert:` line per new alert. [`record`]
//! appends one [`ShadowRecord`] to `inbox/shadow-wakes.jsonl`; the alert detail
//! text is dropped, only session and kind are kept (dry-run prints no
//! fingerprint, so the join key is session and kind).
//! Test: `parse_decision_reads_the_cycle_and_alerts`,
//! `shadow_plan_seeds_a_scratch_inbox_and_never_writes_the_live_one`,
//! `shadow_plan_keeps_the_dry_runs_own_state`,
//! `shadow_record_carries_the_contract_fields`, `failed_shadow_run_is_recorded_as_failed`.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::config::PollJob;
use super::engine_run::POLL_JOB;
use super::runlog::append_jsonl;
use super::slot::day_key;
use super::spawn::{RunResult, RunSpec};
use super::{Layout, SchedError};
use crate::classify::alerts::iso;
use crate::sha256::sha256_hex;

/// What a finished shadow run needs to write its record.
#[derive(Debug, Clone)]
pub struct ShadowCtx {
    /// The file the dry-run's stdout went to.
    pub stdout: PathBuf,
    /// Seconds between cycles; the record's join window.
    pub interval_s: u64,
}

/// One alert the shadow cycle would raise.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShadowAlert {
    /// tmux session, or empty.
    pub session: String,
    /// Alert kind.
    pub kind: String,
}

/// One `inbox/shadow-wakes.jsonl` line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowRecord {
    /// When the cycle ran (UTC ISO).
    pub ts: String,
    /// Always `poll`.
    pub job: String,
    /// The slot, Unix seconds.
    pub slot: i64,
    /// Always `shadow`.
    pub mode: String,
    /// Seconds in this record's join window.
    pub interval_s: u64,
    /// False when the dry-run failed or printed no cycle line.
    pub ok: bool,
    /// Panes seen.
    pub panes: Option<u64>,
    /// New alerts.
    pub new: Option<u64>,
    /// Alerts that would wake the Architect.
    pub woken: Option<u64>,
    /// Alerts logged only.
    pub logged: Option<u64>,
    /// Alerts suppressed.
    pub suppressed: Option<u64>,
    /// The wake decision, as the poller prints it.
    pub wake_phrase: Option<String>,
    /// The pane a wake would go to.
    pub wake_target: Option<String>,
    /// The alerts, without detail text.
    pub alerts: Vec<ShadowAlert>,
    /// A short hash of the sorted alert keys.
    pub digest_sha: String,
    /// Why the run failed, when it did.
    pub error: Option<String>,
    /// Woken alerts no triage rule decided (the Haiku path, MB1595); parity
    /// leaves them out. Absent in records written before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub haiku_woken: Option<u64>,
}

/// The parsed dry-run output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decision {
    /// `panes=`.
    pub panes: Option<u64>,
    /// `new=`.
    pub new: Option<u64>,
    /// `woken=`.
    pub woken: Option<u64>,
    /// `logged=`.
    pub logged: Option<u64>,
    /// `suppressed=`.
    pub suppressed: Option<u64>,
    /// `wake=` up to `architect=`.
    pub wake: Option<String>,
    /// The alerts.
    pub alerts: Vec<ShadowAlert>,
    /// `triage haiku-path wakes: N`.
    pub haiku_woken: Option<u64>,
}

fn kv<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(&format!(" {key}="))? + key.len() + 2;
    let rest = &line[at..];
    Some(rest.split(' ').next().unwrap_or(rest))
}

/// Reads a dry-run's stdout; see the module doc.
pub fn parse_decision(stdout: &str) -> Decision {
    let mut d = Decision::default();
    for line in stdout.lines() {
        if let Some(alert) = line.trim_start().strip_prefix("would alert: ") {
            d.alerts.push(parse_display(alert));
        } else if let Some(n) = line.trim_start().strip_prefix("triage haiku-path wakes: ") {
            d.haiku_woken = n.trim().parse().ok();
        } else if line.contains(" panes=") && line.contains(" woken=") {
            let num = |k| kv(line, k).and_then(|v| v.parse().ok());
            (d.panes, d.new, d.woken) = (num("panes"), num("new"), num("woken"));
            (d.logged, d.suppressed) = (num("logged"), num("suppressed"));
            d.wake = line
                .split_once(" wake=")
                .map(|(_, w)| w.split(" architect=").next().unwrap_or(w).trim().to_owned());
        }
    }
    d
}

/// `[session]: kind: detail` (or `kind: detail`) into session and kind.
fn parse_display(display: &str) -> ShadowAlert {
    let (session, rest) = match display.strip_prefix('[').and_then(|r| r.split_once("]: ")) {
        Some((s, r)) => (s, r),
        None => ("", display),
    };
    ShadowAlert {
        session: session.to_owned(),
        kind: rest.split(':').next().unwrap_or(rest).trim().to_owned(),
    }
}

fn wake_target(phrase: &str) -> Option<String> {
    let rest = phrase
        .strip_prefix("dry-run: would send to ")
        .or_else(|| phrase.strip_prefix("sent to "))?;
    let target = rest.split([':', ' ']).next()?;
    (!target.is_empty()).then(|| target.to_owned())
}

/// The state key that says [`seed_live_offsets`] already ran.
pub const LIVE_OFFSETS_SEEDED: &str = "shadow_live_offsets_seeded";

/// Sets `pm_offset` and `events_offset` in the dry run's own `state` to the
/// current end of the live `pm-messages.jsonl` and `events.jsonl`, once
/// (marked by [`LIVE_OFFSETS_SEEDED`]), so the first read of the live queues
/// is not a catch-up burst (MB1595). It only stats the live files; every
/// write goes to `state`, in the scratch dir, through a temp file and rename.
fn seed_live_offsets(state: &std::path::Path, live: &std::path::Path) -> Result<(), SchedError> {
    use serde_json::{Map, Value};
    let mut doc: Map<String, Value> = std::fs::read_to_string(state)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if doc.get(LIVE_OFFSETS_SEEDED) == Some(&Value::Bool(true)) {
        return Ok(());
    }
    let end = |name: &str| std::fs::metadata(live.join(name)).map_or(0, |m| m.len());
    doc.insert("pm_offset".to_owned(), end("pm-messages.jsonl").into());
    doc.insert("events_offset".to_owned(), end("events.jsonl").into());
    doc.insert(LIVE_OFFSETS_SEEDED.to_owned(), Value::Bool(true));
    let tmp = state.with_extension("json.tmp");
    let text = serde_json::to_string(&doc).map_err(|e| SchedError::Parse {
        what: state.display().to_string(),
        detail: e.to_string(),
    })?;
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, state))
        .map_err(|e| SchedError::io(format!("seed offsets {}", state.display()), e))
}

/// The dry run's own state file, in the scratch inbox.
pub const DRY_RUN_STATE: &str = "dry-run-state.json";

/// Builds the shadow dry-run for `now`; see the module doc.
///
/// # Errors
/// [`SchedError::Io`] when the scratch inbox cannot be made or the live state
/// cannot be copied (an absent live state is not an error: the dry run then
/// starts empty).
pub fn plan(
    layout: &Layout,
    poll: &PollJob,
    mut env: Vec<(String, String)>,
    now: i64,
) -> Result<(RunSpec, ShadowCtx), SchedError> {
    let dir = layout.shadow_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| SchedError::io(format!("create {}", dir.display()), e))?;
    // A copy made every run threw away what the dry run had seen, so every
    // cycle re-alerted everything the live state lacked: seed once, then keep.
    let state = dir.join(DRY_RUN_STATE);
    if !state.exists() {
        match std::fs::copy(layout.root.join("inbox/poll-state.json"), &state) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(SchedError::io(format!("seed {}", state.display()), e)),
        }
    }
    seed_live_offsets(&state, &layout.root.join("inbox"))?;
    let stdout = dir.join("last-stdout.txt");
    std::fs::write(&stdout, b"")
        .map_err(|e| SchedError::io(format!("write {}", stdout.display()), e))?;
    env.retain(|(k, _)| {
        !matches!(
            k.as_str(),
            "ARCHITECT_INBOX_DIR"
                | "ARCHITECT_PROJECT_DIR"
                | "ARCHITECT_DRY_RUN_STATE"
                | "ARCHITECT_READ_INBOX_DIR"
        )
    });
    env.push(("ARCHITECT_INBOX_DIR".to_owned(), dir.display().to_string()));
    // See MB1595: selftest, ctx-cycle and PM-message queues are read live.
    env.push((
        "ARCHITECT_READ_INBOX_DIR".to_owned(),
        layout.root.join("inbox").display().to_string(),
    ));
    env.push((
        "ARCHITECT_DRY_RUN_STATE".to_owned(),
        state.display().to_string(),
    ));
    env.push((
        "ARCHITECT_PROJECT_DIR".to_owned(),
        layout.root.display().to_string(),
    ));
    let spec = RunSpec {
        argv: poll.shadow_argv.clone(),
        cwd: layout.root.clone(),
        env,
        stdin: None,
        stdout: stdout.clone(),
        stderr: layout.job_log(POLL_JOB, &day_key(now)),
        timeout: Duration::from_secs(poll.timeout_s),
    };
    Ok((
        spec,
        ShadowCtx {
            stdout,
            interval_s: poll.every_s,
        },
    ))
}

/// Appends the record for a finished shadow run.
///
/// # Errors
/// [`SchedError::Io`] when `inbox/shadow-wakes.jsonl` cannot be written.
pub fn record(
    layout: &Layout,
    ctx: &ShadowCtx,
    result: &RunResult,
    slot: i64,
    start: i64,
) -> Result<(), SchedError> {
    let out = std::fs::read_to_string(&ctx.stdout).unwrap_or_default();
    let d = parse_decision(&out);
    let error = if result.timed_out {
        Some("timeout".to_owned())
    } else if let Some(e) = &result.error {
        Some(e.clone())
    } else if result.exit != Some(0) {
        Some(format!("exit {:?}", result.exit))
    } else if d.panes.is_none() {
        Some("no cycle line in the dry-run output".to_owned())
    } else {
        None
    };
    let mut keys: Vec<String> = d
        .alerts
        .iter()
        .map(|a| format!("{}|{}", a.session, a.kind))
        .collect();
    keys.sort();
    let rec = ShadowRecord {
        ts: iso(start as f64),
        job: POLL_JOB.to_owned(),
        slot,
        mode: "shadow".to_owned(),
        interval_s: ctx.interval_s,
        ok: error.is_none(),
        panes: d.panes,
        new: d.new,
        woken: d.woken,
        logged: d.logged,
        suppressed: d.suppressed,
        wake_target: d.wake.as_deref().and_then(wake_target),
        wake_phrase: d.wake,
        alerts: d.alerts,
        digest_sha: sha256_hex(keys.join("\n").as_bytes())[..12].to_owned(),
        error,
        haiku_woken: d.haiku_woken,
    };
    append_jsonl(&layout.shadow_log(), &rec)
}
