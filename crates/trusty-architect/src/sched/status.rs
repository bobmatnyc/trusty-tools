//! `status`: what the scheduler knows about itself and its jobs.
//!
//! Why: an operator, and the Architect's cutover decision, need one place that
//! says whether the scheduler is alive, which mode it is in, and when each job
//! runs next, without reading three files.
//! What: [`render`] prints the liveness line (pid, age, mode, config hash), a
//! count of jobs by who runs them, one row per scheduler-run job (next due,
//! last result, misses, overruns, runs today) and the poll job. With `json`
//! it returns the same facts as one JSON document. The second value is false
//! when the liveness file is missing or older than [`STALE_AFTER_S`]: the
//! verb then exits non-zero.
//! Test: `status_reports_liveness_and_job_rows`, `status_fails_when_the_liveness_file_is_stale`.

use serde_json::json;

use super::config::{Kind, Registry};
use super::daemon::{STALE_AFTER_S, read_alive};
use super::state::SchedState;
use super::{Layout, engine::NEVER};
use crate::classify::alerts::iso;

fn when(t: i64) -> String {
    if t == 0 {
        "not scheduled yet".to_owned()
    } else if t >= NEVER {
        "never".to_owned()
    } else {
        iso(t as f64)
    }
}

/// The status text (or JSON) and whether the scheduler looks healthy.
pub fn render(
    layout: &Layout,
    reg: &Registry,
    st: &SchedState,
    now: i64,
    as_json: bool,
) -> (String, bool) {
    let alive = read_alive(layout);
    let age = alive.as_ref().map(|a| now - a.unix);
    let healthy = age.is_some_and(|a| a <= STALE_AFTER_S);
    let mut run = Vec::new();
    let (mut managed, mut external, mut disabled) = (0, 0, 0);
    for j in &reg.jobs {
        match (j.run_kind(), j.managed_by.as_deref(), j.enabled) {
            (_, _, false) => disabled += 1,
            (Kind::NotRun, Some("meta_due"), _) => managed += 1,
            (Kind::NotRun, ..) => external += 1,
            _ => run.push(j),
        }
    }
    let row = |id: &str| st.jobs.get(id).cloned().unwrap_or_default();
    if as_json {
        let jobs: Vec<_> = run
            .iter()
            .map(|j| {
                let s = row(&j.id);
                json!({"id": j.id, "next_due": s.next_due, "last_run": s.last_run,
                       "last_result": s.last_result, "misses": s.misses,
                       "overruns": s.overruns, "runs_today": s.runs_today})
            })
            .collect();
        let doc = json!({"healthy": healthy, "age_s": age, "alive": alive,
                         "scheduler_run": jobs, "meta_due": managed, "external": external,
                         "disabled": disabled, "poll_next": st.poll_next});
        return (format!("{doc}\n"), healthy);
    }
    let mut out = match (&alive, age) {
        (Some(a), Some(age)) if healthy => format!(
            "scheduler: alive pid {} age {age}s mode {} config {}\n",
            a.pid,
            a.mode,
            &a.config_sha[..a.config_sha.len().min(8)]
        ),
        (Some(_), Some(age)) => {
            format!("scheduler: STALE, liveness file is {age}s old (limit {STALE_AFTER_S}s)\n")
        }
        _ => "scheduler: NOT ALIVE, no liveness file\n".to_owned(),
    };
    out += &format!(
        "jobs: {} scheduler-run, {managed} managed by meta_due, {external} external, {disabled} disabled\n",
        run.len()
    );
    for j in run {
        let s = row(&j.id);
        out += &format!(
            "  {:<12} next {}  last {}  misses {}  overruns {}  runs today {}\n",
            j.id,
            when(s.next_due),
            s.last_result.as_deref().unwrap_or("-"),
            s.misses,
            s.overruns,
            s.runs_today
        );
    }
    out += &format!("  {:<12} next {}\n", "poll", when(st.poll_next));
    (out, healthy)
}
