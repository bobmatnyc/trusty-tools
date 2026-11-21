//! The schema's rules, by hand.
//!
//! Why: the crate has no JSON Schema validator and takes no new dependency;
//! a schema-failing file must keep the previous config, so the loader needs
//! the rules as code (`registry.schema.json`, `meta_registry.py lint`).
//! What: [`problems`] returns every rule a [`Registry`] breaks, one string
//! each (job id first), so `validate` prints the lot. Rules beyond the
//! schema: `schedule.tz` other than UTC, an `overlap` other than `skip` and
//! an `output_file` that is absolute or climbs out of the root are refused.
//! Test: `validate_reports_each_broken_rule`, `shipped_shape_registry_loads`.

use super::config::{Job, Registry};
use super::cron::Cron;
use super::slot::parse_every;

/// `^[A-Za-z][A-Za-z0-9_-]{1,31}$`.
pub fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && (2..=32).contains(&id.len())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn valid_managed_by(m: &str) -> bool {
    m == "meta_due"
        || m.strip_prefix("external:")
            .is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
}

/// Every rule `reg` breaks; empty means valid.
pub fn problems(reg: &Registry) -> Vec<String> {
    let mut out = Vec::new();
    if reg.version != 1 {
        out.push(format!("version must be 1, got {}", reg.version));
    }
    if reg.jobs.is_empty() {
        out.push("jobs must hold at least one job".to_owned());
    }
    if reg.poll.every_s == 0 || reg.poll.timeout_s == 0 {
        out.push("poll: every_s and timeout_s must be at least 1".to_owned());
    }
    if reg.poll.shadow_argv.is_empty() || reg.poll.live_argv.is_empty() {
        out.push("poll: shadow_argv and live_argv must not be empty".to_owned());
    }
    let mut seen = std::collections::HashSet::new();
    for job in &reg.jobs {
        if !seen.insert(job.id.as_str()) {
            out.push(format!("{}: duplicate id", job.id));
        }
        job_problems(job, &mut out);
    }
    out
}

fn job_problems(job: &Job, out: &mut Vec<String>) {
    let mut bad = |msg: &str| out.push(format!("{}: {msg}", job.id));
    if !valid_id(&job.id) {
        bad("id must match ^[A-Za-z][A-Za-z0-9_-]{1,31}$");
    }
    if job.title.chars().count() > 100 {
        bad("title is over 100 characters");
    }
    let owner_ok =
        job.owner == "architect" || job.owner.strip_prefix("pm:").is_some_and(|s| !s.is_empty());
    if !owner_ok {
        bad("owner must be architect or pm:<session>");
    }
    if !job.enabled && job.reason.as_deref().is_none_or(str::is_empty) {
        bad("reason is required when enabled is false");
    }
    if job
        .managed_by
        .as_deref()
        .is_some_and(|m| !valid_managed_by(m))
    {
        bad("managed_by must be meta_due or external:<runner>");
    }
    match (&job.schedule.every, &job.schedule.cron) {
        (Some(e), None) if parse_every(e).is_none() => bad("schedule.every must be <N>[smhd]"),
        (None, Some(c)) => {
            if let Err(e) = Cron::parse(c) {
                bad(&format!("schedule.cron: {e}"));
            }
        }
        (Some(_), None) => {}
        _ => bad("schedule needs exactly one of every or cron"),
    }
    if let Some(c) = &job.schedule.catch_up
        && c != "run_once"
        && c != "skip"
    {
        bad("schedule.catch_up must be run_once or skip");
    }
    if job.schedule.tz.as_deref().is_some_and(|tz| tz != "UTC") {
        bad("schedule.tz: only UTC is supported (open risk: no tz database)");
    }
    if job.overlap.as_deref().is_some_and(|o| o != "skip") {
        bad("overlap: only skip (one run at a time) exists");
    }
    if job.timeout_s == Some(0) {
        bad("timeout_s must be at least 1");
    }
    if let Some(c) = &job.cwd
        && (c.starts_with('/') || c.split('/').any(|p| p == ".."))
    {
        bad("cwd must be relative and stay inside the root");
    }
    kind_problems(job, &mut bad);
    if let Some(a) = job.check.as_ref().map(|c| &c.artifact) {
        let known = ["glob", "jsonl_event", "json_field", "stamp"];
        if !known.contains(&a.kind.as_str()) {
            bad("check.artifact.type is not glob, jsonl_event, json_field or stamp");
        }
        if a.max_age_h <= 0.0 || !a.max_age_h.is_finite() {
            bad("check.artifact.max_age_h must be above 0");
        }
    }
}

fn kind_problems(job: &Job, bad: &mut impl FnMut(&str)) {
    if job.managed_by.is_some() {
        return;
    }
    match job.kind.as_deref() {
        None => bad("kind is required without managed_by"),
        Some("script") => {
            if job.script.as_ref().is_none_or(|s| s.argv.is_empty()) {
                bad("kind script needs script.argv");
            }
        }
        Some("agent") => match &job.agent {
            None => bad("kind agent needs an agent"),
            Some(a) => {
                if a.name.is_empty() || a.model.is_empty() || a.timeout_s == 0 {
                    bad("agent needs name, model and timeout_s >= 1");
                }
                if let Some(f) = &a.output_file
                    && (f.starts_with('/') || f.split('/').any(|p| p == ".."))
                {
                    bad("agent.output_file must stay inside the root");
                }
            }
        },
        Some(_) => bad("kind must be script or agent"),
    }
}
