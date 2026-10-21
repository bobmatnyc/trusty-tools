//! The CTO tripwire arm of `tm hook --egress-guard` (#8454 G3, slice S6 prep).
//!
//! Why: a session that received a Slack-originated answer must not reach a
//! configured project's files. The blanket `egress-channel-tainted` deny
//! already refuses every gated call of a tainted session; the tripwire names a
//! call that lands in a project dir with its own slug, [`SENSITIVE_CHECK`],
//! and appends a durable record, so a trip is visible (Architect Q2, Q4).
//! Every `HostChannel::projects()` dir is sensitive, read from the host config
//! on each call, so a project added there needs no code change (Q3).
//! What: [`tripwire_verdict`] runs after the policy readiness gate passed, for
//! a call the taint arm refuses. It [`judge`]s where the call lands — see
//! `egress_guard_tripwire_paths.rs` for each tool — and denies as
//! [`SENSITIVE_CHECK`] with one [`TripLine`] appended, or keeps the taint arm's
//! [`TAINT_CHECK`] deny for a call that lands outside every project. Fail
//! closed: a path it cannot place, a host config it cannot read and a panic
//! while judging are each a trip, never an allow and never a narrower deny;
//! a record that cannot be written leaves the deny as it is. It never calls
//! `ChannelPolicy::check_egress` and never reads an environment variable or
//! the payload's `agent_id`.
//! Residual: a hard link into a project; a path an interpreter builds at run
//! time from text that never names the project.
//! Test: `egress_guard_tripwire_tests.rs`; end to end in
//! `tests/tm_hook_tripwire_8454.rs`, run by name by
//! `scripts/check_tripwire_gate.sh`.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_channels::policy::{Channel, MAX_FILE_BYTES, parse_host};
use trusty_mpm::core::channels_tripwire::{self, FORMAT_VERSION, TripLine, clip};

use crate::commands::egress_guard::Verdict;
use crate::commands::egress_guard_taint::TAINT_CHECK;

#[path = "egress_guard_tripwire_paths.rs"]
mod paths;
pub(crate) use paths::judge;

/// The audit slug of a tripwire deny.
pub(crate) const SENSITIVE_CHECK: &str = "egress-channel-sensitive-path";

/// Where a tainted gated call lands, as the tripwire judges it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Touch {
    /// Outside every listed project: the blanket taint deny stands.
    Clear,
    /// The spelled path lands inside a listed project, or above one.
    Inside(String),
    /// The guard cannot place this path or command, so it may land in one.
    Unplaceable(String),
    /// The listed projects could not be read, for this reason.
    ProjectsUnknown(String),
    /// Judging the call panicked.
    Panicked,
}

impl Touch {
    /// The record's `cause` word.
    fn cause(&self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Inside(_) => "inside",
            Self::Unplaceable(_) => "unplaceable",
            Self::ProjectsUnknown(_) => "projects-unknown",
            Self::Panicked => "panicked",
        }
    }

    /// The path the record keeps, when the cause names one.
    fn path(&self) -> Option<String> {
        match self {
            Self::Inside(path) | Self::Unplaceable(path) => Some(clip(path)),
            _ => None,
        }
    }
}

/// Every project directory the host config lists, on any channel.
///
/// Why (Architect Q3): the sensitive set is the policy's, never code's.
/// What: reads `config.yaml` under `home` (a regular file of at most
/// `MAX_FILE_BYTES`), parses its ceiling with `parse_host`, and returns the
/// `projects()` of every channel it names, deduplicated. A missing
/// `config.yaml` lists no project, so it is `Ok(empty)`: the call keeps the
/// blanket taint deny (the readiness gate already denies that case as not
/// ready). Every other fault is an `Err` with a fixed text that holds no
/// config value, and trips.
/// Test: `tripwire_places_each_tool_path`,
/// `tripwire_new_project_needs_no_code_change`.
pub(crate) fn host_project_dirs(home: &Path) -> Result<Vec<PathBuf>, String> {
    let path = trusty_common::crate_config::crate_config_path_at(home, "trusty-mpm");
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        // #8454 S6: no host file lists no project; the call is still denied.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("config.yaml cannot be read ({})", e.kind())),
    };
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return Err("config.yaml is not a regular file within the size cap".into());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("config.yaml cannot be read ({})", e.kind()))?;
    let host = parse_host(&text, Some(home))
        .map_err(|_| "the channel host ceiling in config.yaml does not parse".to_owned())?;
    let mut dirs: Vec<PathBuf> = Channel::ALL
        .iter()
        .filter_map(|channel| host.channel(*channel))
        .flat_map(|channel| channel.projects().iter().cloned())
        .collect();
    dirs.sort();
    dirs.dedup();
    Ok(dirs)
}

/// The deny for a tainted gated call once the policy is ready: the tripwire's
/// when it trips, else the taint arm's `taint` deny.
///
/// What: [`tripwire_verdict_with`] over [`host_project_dirs`] and
/// [`channels_tripwire::append`].
/// Test: `tripwire_blocks_tainted_session_read_of_sensitive_path`,
/// `tripwire_keeps_the_blanket_taint_deny_for_non_sensitive_calls`.
pub(crate) fn tripwire_verdict(payload: &Value, home: Option<&Path>, taint: String) -> Verdict {
    tripwire_verdict_with(
        payload,
        home,
        taint,
        host_project_dirs,
        channels_tripwire::append,
    )
}

/// [`tripwire_verdict`] with the project list and the record writer injected.
///
/// Why (fail closed): narrowing a deny is where a tripwire can fail open, so
/// no fault here may produce anything but a deny.
/// What: [`judge`] under `catch_unwind`; a panic is [`Touch::Panicked`]. A
/// [`Touch::Clear`] keeps `taint` as a [`TAINT_CHECK`] deny. Any other touch
/// writes one [`TripLine`] through `record` into the record directory, also
/// under `catch_unwind`, and denies as [`SENSITIVE_CHECK`]; a failed or
/// panicking write, or an unknown home, is reported on stderr only.
/// Test: `tripwire_arm_panic_denies_and_records`,
/// `tripwire_record_failure_keeps_the_deny`.
pub(crate) fn tripwire_verdict_with(
    payload: &Value,
    home: Option<&Path>,
    taint: String,
    projects: impl FnOnce(&Path) -> Result<Vec<PathBuf>, String>,
    record: impl FnOnce(&Path, &TripLine) -> std::io::Result<()>,
) -> Verdict {
    // #8454 S6: a panic while judging is a trip, never an allow.
    let touch = std::panic::catch_unwind(AssertUnwindSafe(|| judge(payload, home, projects)))
        .unwrap_or(Touch::Panicked);
    if touch == Touch::Clear {
        return Verdict::Deny {
            check: TAINT_CHECK,
            reason: taint,
        };
    }
    let line = trip_line(payload, &touch);
    // #8454 S6: the record is best-effort; its failure never changes the deny.
    let written = std::panic::catch_unwind(AssertUnwindSafe(|| match home {
        Some(home) => record(&channels_tripwire::record_dir(home), &line)
            .map_err(|e| format!("cannot write the record ({e})")),
        None => Err("the home directory is unknown".to_owned()),
    }));
    match written {
        Ok(Ok(())) => {}
        Ok(Err(why)) => eprintln!("tm hook --egress-guard: tripwire record not written: {why}"),
        Err(_) => eprintln!("tm hook --egress-guard: tripwire record not written: it panicked"),
    }
    Verdict::Deny {
        check: SENSITIVE_CHECK,
        reason: sensitive_reason(payload, &touch, &taint),
    }
}

/// The record line for one trip.
fn trip_line(payload: &Value, touch: &Touch) -> TripLine {
    let text = |key: &str| payload.get(key).and_then(Value::as_str).map(clip);
    TripLine {
        v: FORMAT_VERSION,
        at: chrono::Utc::now(),
        session_id: text("session_id"),
        tool_name: text("tool_name").unwrap_or_default(),
        cause: touch.cause().to_owned(),
        path: touch.path(),
    }
}

/// The deny reason for a trip; `taint` keeps the taint arm's own reason.
fn sensitive_reason(payload: &Value, touch: &Touch, taint: &str) -> String {
    let tool = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("this tool");
    let what = match touch {
        Touch::Inside(path) => format!("lands at `{}`, in or above", clip(path)),
        Touch::Unplaceable(path) => format!(
            "names `{}`, which the guard cannot place, so it may land in",
            clip(path)
        ),
        Touch::ProjectsUnknown(why) => {
            format!("cannot be judged against the listed projects ({why}), so it may land in")
        }
        Touch::Panicked | Touch::Clear => {
            "could not be judged (the tripwire failed), so it may land in".to_owned()
        }
    };
    format!(
        "tm hook --egress-guard denies `{tool}`: this channel-tainted session's call {what} a \
         project the channel host config lists, so the CTO tripwire refuses it and records the \
         trip (#8454). `tm doctor` shows trips in its `tripwire_record` row. Ask the operator. \
         The taint check refuses it too: {taint}"
    )
}

#[cfg(test)]
#[path = "egress_guard_tripwire_tests.rs"]
mod tests;
