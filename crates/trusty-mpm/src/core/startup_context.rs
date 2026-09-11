//! Per-session startup-context budget — the turn-1 reading, its store, and the
//! verdict `tm doctor` renders from it (#7424, parent #4513).
//!
//! Why: the startup-context audit has been manual and ad hoc (2026-08-01,
//! 2026-09-11). The 2026-09-11 pass measured turn-1 totals of 98k–107k tokens
//! against a 50k target, and the creep between audits is many small additions
//! rather than one reviewable commit — nothing catches it in between. This
//! module is the standing measurement: every managed session records what its
//! first assistant turn re-sent, and `tm doctor` compares the recent
//! distribution against a ceiling.
//!
//! What: three parts, deliberately in one file so the number's definition, its
//! storage and its verdict cannot drift apart.
//!
//! - **The reading** comes from
//!   [`crate::core::transcript_usage::first_turn_context_tokens`] — the same
//!   transcript reader and the same `input + cache_creation + cache_read`
//!   definition the `💸` savings surfaces already use. No second parser.
//! - **The store** is [`crate::core::session_record`] under
//!   [`KIND_STARTUP_CONTEXT`](crate::core::session_record::KIND_STARTUP_CONTEXT),
//!   which puts it in `<root>/usage/` beside the savings ledger and keys it by
//!   the CLAUDE session id, exactly as the model and transcript-path records
//!   are keyed. [`record_startup_context`] is WRITE-ONCE: turn 1 happens once,
//!   so a second observation is a later turn misread as the first and is
//!   refused rather than overwriting the measurement.
//! - **The verdict** is [`evaluate_startup_context`], a pure function over a
//!   newest-first sample and a ceiling. It never fails a run — the ceiling is a
//!   budget the operator sets, and a doctor that hard-failed on a prompt the
//!   operator deliberately grew would be reporting a preference as a defect.
//!
//! **Project scoping is a property of the RECORD, not of the reader.** A
//! machine's store holds every project's sessions, and the doctor check must
//! answer for the project it was run in. The reading therefore carries the
//! session's own working directory, and [`startup_context_for_project`] keeps
//! only the records whose directory is the project directory, sits under it, or
//! contains it — the last arm is what keeps a worktree under
//! `<project>/.claude/worktrees/` attached to the project it belongs to. No
//! transcript is opened at doctor time at all: the check reads recorded
//! numbers, so it cannot reach another project's transcript even by accident.
//!
//! Everything fails soft. An unwritable root, an unparseable record, and a
//! transcript with no assistant turn yet each leave the store untouched and the
//! sample short; a missing reading is `None`, never a recorded zero, because a
//! zero would enter the sample as a real and very small startup.
//!
//! Test: the inline suite in `startup_context_tests.rs` —
//! `a_reading_round_trips_through_the_store`,
//! `a_second_observation_never_overwrites_the_first`,
//! `a_transcript_with_no_assistant_turn_records_nothing`,
//! `samples_exclude_another_projects_sessions`,
//! `a_worktree_under_the_project_is_the_same_project`,
//! `samples_are_newest_first_and_capped`,
//! `an_empty_sample_is_no_samples`,
//! `a_sample_under_the_ceiling_is_within`,
//! `a_median_over_the_ceiling_is_over`,
//! `a_latest_over_the_ceiling_is_over`,
//! `the_default_ceiling_is_fifty_thousand`,
//! `config_overrides_the_ceiling_and_sample_size`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::session_record::{
    KIND_STARTUP_CONTEXT, read_session_record, record_session_value, session_record_ids,
};

/// Startup context, in tokens, at or above which `tm doctor` warns.
///
/// Why (#7424): the owner's stated target. #4513's 2026-09-11 measurement put
/// real turn-1 totals at 98k–107k, so this ships as a ceiling the current tree
/// exceeds — deliberately, since a ceiling set above the measured state would
/// certify it.
/// What: the [`StartupContextConfig::ceiling_tokens`] default, in tokens.
/// Test: `the_default_ceiling_is_fifty_thousand`.
pub const DEFAULT_CEILING_TOKENS: u64 = 50_000;

/// How many recent sessions the doctor check samples for one project.
///
/// Why: one session's reading is noise — a resumed session, or one launched
/// with an unusual flag set, is not the project's startup cost. A median over
/// the newest few is, and reading ten small files costs nothing on a command
/// that already probes the filesystem dozens of times.
/// What: the [`StartupContextConfig::sessions`] default.
/// Test: `samples_are_newest_first_and_capped`.
pub const DEFAULT_SESSION_SAMPLE: usize = 10;

/// `startup_context:` — the doctor ceiling, in
/// `~/.trusty-tools/trusty-mpm/config.yaml`.
///
/// Why: the right ceiling depends on what a project's CLAUDE.md and instruction
/// set legitimately need, so it belongs in operator config rather than in this
/// binary — the same reasoning, and the same shape, as
/// [`crate::core::agent_cost::AgentCostConfig`]'s `warn_tokens`. It sits on the
/// YAML host config rather than the legacy `config.toml` because that is the
/// file `tm doctor`'s own entry point already loads
/// ([`crate::core::trusty_tools_config::TrustyToolsConfig`]).
/// What: `enabled` (master switch), `ceiling_tokens`, and `sessions` (the
/// sample size). Every field optional so an absent section is the shipped
/// default; `0` in either number falls back to the default rather than meaning
/// "no limit", because a zeroed ceiling would warn on every session.
/// Test: `config_overrides_the_ceiling_and_sample_size`,
/// `a_zeroed_config_falls_back_to_the_defaults`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct StartupContextConfig {
    /// Whether `tm doctor` evaluates the reading at all. Absent → enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Startup context, in tokens, at or above which the check warns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling_tokens: Option<u64>,
    /// How many of the project's newest sessions the check samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<usize>,
}

/// The settings the check actually runs with.
///
/// Why: resolution is separated from the config shape so the fallback rules are
/// asserted once, here, rather than re-derived at the call site — the pattern
/// [`crate::core::trusty_tools_config::resolve_untracked_sync`] established.
/// What: every absent or zeroed field replaced by its constant above.
/// Test: `config_overrides_the_ceiling_and_sample_size`,
/// `a_zeroed_config_falls_back_to_the_defaults`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedStartupContext {
    /// Whether the check evaluates at all.
    pub enabled: bool,
    /// Tokens at or above which the check warns.
    pub ceiling_tokens: u64,
    /// How many of the project's newest sessions to sample.
    pub sessions: usize,
}

/// Resolve the `startup_context:` section against the shipped defaults.
///
/// Why/What: see [`ResolvedStartupContext`]. `None` — no section at all — is
/// the shipped default, which is the state on every machine that has never
/// edited the file.
/// Test: `config_overrides_the_ceiling_and_sample_size`,
/// `a_zeroed_config_falls_back_to_the_defaults`.
pub fn resolve_startup_context(config: Option<&StartupContextConfig>) -> ResolvedStartupContext {
    let ceiling = config
        .and_then(|c| c.ceiling_tokens)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_CEILING_TOKENS);
    let sessions = config
        .and_then(|c| c.sessions)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_SESSION_SAMPLE);
    ResolvedStartupContext {
        enabled: config.and_then(|c| c.enabled).unwrap_or(true),
        ceiling_tokens: ceiling,
        sessions,
    }
}

/// One session's turn-1 startup-context reading, as stored.
///
/// Why: the number alone cannot be scoped to a project or ordered against
/// another session's, and both are what the doctor check needs — so the record
/// carries the session's working directory and the moment the reading was
/// taken alongside it.
/// What: JSON, one object per session, written once.
/// Test: `a_reading_round_trips_through_the_store`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupContextRecord {
    /// Turn-1 `input + cache_creation + cache_read`, in tokens.
    pub tokens: u64,
    /// The session's own working directory when the reading was taken.
    pub project: String,
    /// RFC 3339 UTC timestamp of the reading.
    pub recorded_at: String,
}

/// Measure and store this session's turn-1 startup context, once.
///
/// Why: turn 1 happens exactly once, so the reading is taken at the first
/// render that can see it and never revised. Re-measuring on a later render
/// would read whatever turn the head scan reached first after a compaction
/// rewrote the transcript, which is not the same number.
/// What: returns immediately when a record already exists; otherwise folds
/// `transcript` through
/// [`crate::core::transcript_usage::first_turn_context_tokens`] and writes the
/// record. Returns the tokens written, or `None` when a record already existed
/// or no assistant turn has landed yet — both of which are ordinary states, not
/// failures.
/// Test: `a_reading_round_trips_through_the_store`,
/// `a_second_observation_never_overwrites_the_first`,
/// `a_transcript_with_no_assistant_turn_records_nothing`.
pub fn record_startup_context(
    root: &Path,
    session_id: &str,
    project_dir: &Path,
    transcript: &Path,
) -> Option<u64> {
    if read_startup_context(root, session_id).is_some() {
        return None;
    }
    let tokens = crate::core::transcript_usage::first_turn_context_tokens(transcript)?;
    let record = StartupContextRecord {
        tokens,
        project: project_dir.to_string_lossy().into_owned(),
        recorded_at: crate::core::savings::now_ts(),
    };
    let encoded = serde_json::to_string(&record).ok()?;
    record_session_value(root, KIND_STARTUP_CONTEXT, session_id, &encoded);
    Some(tokens)
}

/// Read back one session's stored reading.
///
/// What: `None` when nothing is stored for `session_id` or the stored value
/// does not parse — a record written by a future schema is skipped, never
/// guessed at.
/// Test: `a_reading_round_trips_through_the_store`.
pub fn read_startup_context(root: &Path, session_id: &str) -> Option<StartupContextRecord> {
    let raw = read_session_record(root, KIND_STARTUP_CONTEXT, session_id)?;
    serde_json::from_str(&raw).ok()
}

/// Is a record's recorded directory this project?
///
/// Why: a managed session runs in a worktree or a provisioned workspace, not
/// necessarily in the directory `tm doctor` is invoked from, so an equality
/// test alone would drop every worktree session from its own project's sample.
/// Containment in EITHER direction covers both shapes — a worktree under
/// `<project>/.claude/worktrees/`, and a doctor run from inside one — while
/// still excluding an unrelated checkout, which is the scoping the check owes.
/// What: canonicalizes both sides where it can (a path under `/tmp` resolves
/// through a symlink on macOS, so a raw comparison would miss), falling back to
/// the raw spelling for a path that no longer exists.
/// Test: `samples_exclude_another_projects_sessions`,
/// `a_worktree_under_the_project_is_the_same_project`.
pub fn belongs_to_project(recorded: &str, project_dir: &Path) -> bool {
    fn resolve(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }
    let recorded = resolve(Path::new(recorded.trim()));
    let project = resolve(project_dir);
    recorded.starts_with(&project) || project.starts_with(&recorded)
}

/// This project's newest stored readings, newest first.
///
/// Why: the doctor check reports on a project, and the store holds the whole
/// machine. Opening no transcripts here is the point — every number was
/// measured at the session that owned it, so this read cannot reach another
/// project's transcript however the scoping rule is tuned.
/// What: every parseable record whose [`belongs_to_project`] holds, sorted by
/// `recorded_at` descending and truncated to `limit`. An absent store yields an
/// empty vector.
/// Test: `samples_exclude_another_projects_sessions`,
/// `samples_are_newest_first_and_capped`.
pub fn startup_context_for_project(
    root: &Path,
    project_dir: &Path,
    limit: usize,
) -> Vec<StartupContextRecord> {
    let mut records: Vec<StartupContextRecord> = session_record_ids(root, KIND_STARTUP_CONTEXT)
        .into_iter()
        .filter_map(|id| read_startup_context(root, &id))
        .filter(|record| belongs_to_project(&record.project, project_dir))
        .collect();
    // RFC 3339 UTC timestamps sort lexicographically in time order, which is
    // why `now_ts` is the one producer of this field.
    records.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
    records.truncate(limit);
    records
}

/// What the doctor check found.
///
/// Why: the three outcomes read differently and only one of them is a warning —
/// "nothing measured yet" is not a pass and not a problem, and a check that
/// collapsed it into either would be lying in one direction or the other.
/// What: `NoSamples` when the project has no reading; otherwise the median and
/// the latest of the sample beside the ceiling they were judged against.
/// Test: `an_empty_sample_is_no_samples`, `a_sample_under_the_ceiling_is_within`,
/// `a_median_over_the_ceiling_is_over`, `a_latest_over_the_ceiling_is_over`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupContextVerdict {
    /// The project has no stored reading.
    NoSamples,
    /// Both the median and the latest reading sit below the ceiling.
    Within {
        /// Median of the sample, in tokens.
        median: u64,
        /// The newest reading, in tokens.
        latest: u64,
        /// The ceiling they were judged against.
        ceiling: u64,
    },
    /// The median OR the latest reading reached the ceiling.
    Over {
        /// Median of the sample, in tokens.
        median: u64,
        /// The newest reading, in tokens.
        latest: u64,
        /// The ceiling they were judged against.
        ceiling: u64,
    },
}

/// Judge a newest-first sample against `ceiling`.
///
/// Why: kept pure — it takes the already-read numbers rather than touching the
/// filesystem — so every arm is unit-testable without a store, matching
/// [`crate::core::agent_cost::evaluate_cost`]'s split between policy and I/O.
/// Both the median and the latest are tested because they answer different
/// questions: the median is what the project costs, and the latest is whether
/// the change in front of the operator just pushed it over.
/// What: `Over` when EITHER reaches `ceiling`, `Within` otherwise, `NoSamples`
/// on an empty sample. The median of an even-length sample is the lower of the
/// two middle values, which understates rather than overstates — a warning
/// should never be produced by a rounding choice.
/// Test: `an_empty_sample_is_no_samples`, `a_sample_under_the_ceiling_is_within`,
/// `a_median_over_the_ceiling_is_over`, `a_latest_over_the_ceiling_is_over`.
pub fn evaluate_startup_context(newest_first: &[u64], ceiling: u64) -> StartupContextVerdict {
    let Some(&latest) = newest_first.first() else {
        return StartupContextVerdict::NoSamples;
    };
    let mut sorted = newest_first.to_vec();
    sorted.sort_unstable();
    let median = sorted[(sorted.len() - 1) / 2];
    if median >= ceiling || latest >= ceiling {
        StartupContextVerdict::Over {
            median,
            latest,
            ceiling,
        }
    } else {
        StartupContextVerdict::Within {
            median,
            latest,
            ceiling,
        }
    }
}

#[cfg(test)]
#[path = "startup_context_tests.rs"]
mod tests;
