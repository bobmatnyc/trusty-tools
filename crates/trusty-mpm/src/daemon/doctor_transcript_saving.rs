//! Doctor probe: would a managed spawn have Claude Code transcript saving
//! DISABLED? (issue #4467)
//!
//! Why: this defect went unnoticed until Claude Code started printing a warning
//! about it. Nothing in tm reported it, and the symptom — "the session had no
//! history to resume from" — only surfaces after the session is already gone.
//! That is the same shape as #4451: a silent failure with no check capable of
//! failing, which `agent_reachability` was added for. This probe is that check.
//!
//! What it asserts is the invariant the spawn builders encode: the managed
//! spawn's `env` prefix must UNSET
//! [`crate::core::claude_env_scrub::TRANSCRIPT_SUPPRESSING_MARKER`], and must
//! NOT unset `CLAUDE_CONFIG_DIR`. Both directions matter and they fail in
//! opposite ways — under-scrub costs the session its transcripts (#4467),
//! over-scrub costs it the entire bundled agent roster (#4451, via #4455).
//!
//! Both sides are read from PRODUCTION code, not restated: the scrub set is
//! parsed out of the real [`crate::runtime::claude_code::env_bin_prefix`] output
//! by [`crate::core::claude_env_scrub::parse_env_unset_vars`]. Drop the flags
//! from the spawn builder and this check fails — which a check that compared the
//! marker constant against itself could never do.
//!
//! Deliberately STATIC rather than spawning a probe `claude -p` and inspecting
//! the transcript directory afterwards: a live spawn costs tens of seconds,
//! needs network and auth, and would make `tm doctor` slow and flaky on the one
//! command operators reach for when things are already broken. The static form
//! catches the whole failure class at zero cost. The set of markers live in the
//! current environment is reported as CONTEXT in the message, never as the
//! pass/fail condition — `tm doctor` run from a plain shell has no markers set,
//! and the check must still be meaningful there.
//!
//! Test: `crates/trusty-mpm/src/daemon/doctor_transcript_saving_tests.rs`.

use crate::core::claude_env_scrub::{
    TRANSCRIPT_SUPPRESSING_MARKER, markers_present_in_env, parse_env_unset_vars,
};
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Name of this check as it appears in `tm doctor` output.
const CHECK_NAME: &str = "transcript_saving";

/// Probe whether a managed spawn preserves Claude Code transcript saving.
///
/// Why: see the module doc. Hard `Fail`, not `Warn`: when it trips, every
/// managed session is unrecoverable — no native `--resume`, no `--continue`, no
/// `/rewind`, and absent from `--resume` listings — so a session that dies loses
/// everything since tm's last summary-only snapshot. That is data loss, not a
/// configuration preference.
/// What: builds the REAL managed-spawn env prefix (with a representative config
/// dir, so the `CLAUDE_CONFIG_DIR` over-scrub branch is exercised too), parses
/// the variables it unsets, and delegates to [`verdict`].
/// Test: `production_spawn_preserves_transcript_saving`.
pub(super) fn check_transcript_saving() -> DoctorCheck {
    // A representative config dir: `env_bin_prefix` emits CLAUDE_CONFIG_DIR as
    // an ASSIGNMENT, so passing one is what lets the over-scrub check see
    // whether the variable was (wrongly) added to the unset list instead.
    let config_dir = std::path::PathBuf::from("/managed/claude-config");
    let prefix = crate::runtime::env_bin_prefix("claude", Some(&config_dir), None);
    verdict(&parse_env_unset_vars(&prefix), &markers_present_in_env())
}

/// Pure verdict over the spawn prefix's unset list.
///
/// Why: separated from [`check_transcript_saving`] so BOTH failure branches are
/// directly testable. The real prefix comes from production code, so a test that
/// could only call the wrapper would exercise the happy path and silently lose
/// the regression guard — the same blind spot #4467 itself is.
/// What: `Fail` when `unset` omits [`TRANSCRIPT_SUPPRESSING_MARKER`] (transcripts
/// would be lost) or when it contains `CLAUDE_CONFIG_DIR` (the roster would be
/// lost). `Ok` otherwise, naming the marker count and any markers live in the
/// current environment. `present` is context for the message only.
/// Test: `ok_when_the_marker_is_scrubbed`,
/// `fails_when_the_suppressing_marker_is_not_scrubbed`,
/// `fails_when_the_scrub_would_take_the_config_dir`,
/// `failure_is_a_hard_fail_not_a_warn`,
/// `ok_message_reports_markers_live_in_the_environment`,
/// `config_dir_over_scrub_takes_precedence_in_the_message`.
fn verdict(unset: &[&str], present: &[&'static str]) -> DoctorCheck {
    if unset.contains(&"CLAUDE_CONFIG_DIR") {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            "a managed spawn would UNSET `CLAUDE_CONFIG_DIR`. That relocation is \
             what puts the bundled agent roster in the `user` settings tier the \
             spawn's `--setting-sources` flag loads (#4455), so unsetting it \
             restores issue #4451: every delegation degrades to `general-purpose` \
             with `Agent type '<name>' not found`. Remove CLAUDE_CONFIG_DIR from \
             `core::claude_env_scrub::INHERITED_SESSION_MARKERS`."
                .to_owned(),
        );
    }
    if !unset.contains(&TRANSCRIPT_SUPPRESSING_MARKER) {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!(
                "a managed spawn does NOT unset `{TRANSCRIPT_SUPPRESSING_MARKER}`, so when \
                 `tm` is invoked from inside a Claude Code session the spawned `claude` \
                 inherits it and turns session persistence OFF (\"Transcript saving is off \
                 — inherited {TRANSCRIPT_SUPPRESSING_MARKER} marker\"). The managed session \
                 then has no native --resume/--continue/rewind recovery and never appears \
                 in `--resume`, so if it dies everything since tm's last snapshot is lost \
                 (issue #4467). Add it back to \
                 `core::claude_env_scrub::INHERITED_SESSION_MARKERS`. Currently unsets: [{}].",
                unset.join(", ")
            ),
        );
    }
    let context = if present.is_empty() {
        "no inherited markers in this environment".to_owned()
    } else {
        format!(
            "inherited here and scrubbed: {} — this `tm` is itself running with the leak \
             present",
            present.join(", ")
        )
    };
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Ok,
        format!(
            "a managed spawn unsets [{}] while keeping `CLAUDE_CONFIG_DIR`, so transcript \
             saving and native --resume stay available; {context}",
            unset.join(", ")
        ),
    )
}

#[cfg(test)]
#[path = "doctor_transcript_saving_tests.rs"]
mod tests;
