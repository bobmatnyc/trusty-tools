//! Freshness checks and escalation for scheduler-run jobs.
//!
//! Why: a job that "ran" but left no artifact is the failure mode behind the
//! lapsed routines; the contract says a scheduler-run job stale past its slot
//! plus `grace_s` is a miss, and `escalate_after` misses raise one `meta_miss`
//! per streak (contract sections 3 and 4).
//! What: [`check`] reads the job's artifact: `glob` (newest mtime of a path
//! glob in its last segment; `~` expands) or `stamp` (`stamps/<id>.json`).
//! `jsonl_event` and `json_field` are `Unsupported` here: `meta_due.py`
//! checks those (open risk). [`evaluate`] runs once per slot, after the
//! grace: fresh resets the misses, stale adds one, and at `escalate_after`
//! (default 2) it returns one `meta_miss` [`Alert`], not another until the
//! artifact turns fresh. [`meta_alert`] carries job id, title, path and age
//! only, never a value from the job.
//! Test: `glob_freshness_uses_the_newest_mtime`, `stamp_freshness_reads_the_at_field`,
//! `stale_artifact_escalates_once_per_streak_and_fresh_resets`.

use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use super::Layout;
use super::config::Job;
use super::slot::parse_iso;
use super::state::JobState;
use crate::classify::alerts::{Alert, DETAIL_MAX, iso};

/// What the artifact check found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Freshness {
    /// Newer than `max_age_h`; the age in seconds.
    Fresh(i64),
    /// Older than `max_age_h`; the age in seconds.
    Stale(i64),
    /// No artifact at all.
    Missing,
    /// A type this scheduler does not check.
    Unsupported,
}

/// `*` and `?` wildcards over one path segment.
fn wild(pat: &[u8], s: &[u8]) -> bool {
    match (pat.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => wild(&pat[1..], s) || (!s.is_empty() && wild(pat, &s[1..])),
        (Some(b'?'), Some(_)) => wild(&pat[1..], &s[1..]),
        (Some(a), Some(b)) if a == b => wild(&pat[1..], &s[1..]),
        _ => false,
    }
}

/// The artifact path of `job`: `~` expanded, relative to the root.
pub fn artifact_path(job: &Job, layout: &Layout) -> Option<PathBuf> {
    let art = &job.check.as_ref()?.artifact;
    if art.kind == "stamp" {
        return Some(layout.stamp(&job.id));
    }
    let raw = art.path.as_deref()?;
    Some(match raw.strip_prefix("~/") {
        Some(rest) => layout.home.join(rest),
        None => layout.root.join(raw),
    })
}

fn newest_mtime(pattern: &std::path::Path) -> Option<i64> {
    let (dir, name) = (pattern.parent()?, pattern.file_name()?.to_str()?);
    let mut newest = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        if !wild(
            name.as_bytes(),
            entry.file_name().to_string_lossy().as_bytes(),
        ) {
            continue;
        }
        let secs = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        newest = newest.max(secs);
    }
    newest
}

/// Reads `job`'s artifact against `max_age_h` at `now`.
pub fn check(job: &Job, layout: &Layout, now: i64) -> Freshness {
    let (Some(c), Some(path)) = (&job.check, artifact_path(job, layout)) else {
        return Freshness::Unsupported;
    };
    let stamped = match c.artifact.kind.as_str() {
        "glob" => newest_mtime(&path),
        "stamp" => std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| parse_iso(v["at"].as_str()?)),
        _ => return Freshness::Unsupported,
    };
    let Some(at) = stamped else {
        return Freshness::Missing;
    };
    let age = (now - at).max(0);
    if (age as f64) <= c.artifact.max_age_h * 3600.0 {
        Freshness::Fresh(age)
    } else {
        Freshness::Stale(age)
    }
}

/// A `meta_miss` alert: `detail` must hold no secret value.
pub fn meta_alert(now: i64, fingerprint: &str, detail: &str) -> Alert {
    let detail: String = detail.chars().take(DETAIL_MAX).collect();
    Alert {
        ts: iso(now as f64),
        session: String::new(),
        pane: String::new(),
        kind: "meta_miss".to_owned(),
        fingerprint: fingerprint.to_owned(),
        display: format!("meta_miss: {detail}"),
        detail,
    }
}

/// Checks `job` once per slot, after its grace; updates `js`.
///
/// Returns the one `meta_miss` alert when this check reaches
/// `escalate_after` misses for the first time in a streak.
pub fn evaluate(job: &Job, js: &mut JobState, layout: &Layout, now: i64) -> Option<Alert> {
    let c = job.check.as_ref()?;
    let grace = c.grace_s.unwrap_or(3600) as i64;
    if js.last_slot == 0 || js.checked_slot == js.last_slot || now < js.last_slot + grace {
        return None;
    }
    js.checked_slot = js.last_slot;
    let age = match check(job, layout, now) {
        Freshness::Fresh(_) => {
            js.misses = 0;
            js.alerted = false;
            return None;
        }
        Freshness::Unsupported => return None,
        Freshness::Stale(age) => format!("{}h", age / 3600),
        Freshness::Missing => "missing".to_owned(),
    };
    js.misses += 1;
    if js.misses < c.escalate_after.unwrap_or(2) || js.alerted {
        return None;
    }
    js.alerted = true;
    let path = artifact_path(job, layout).map_or_else(String::new, |p| p.display().to_string());
    Some(meta_alert(
        now,
        &job.id,
        &format!("{} {}: artifact {path} age {age}", job.id, job.title),
    ))
}
