//! Is a human currently attending this assistant instance? (#4652, epic #4646)
//!
//! Why: nothing in the codebase could answer that question, and B2's
//! `notify_owner` tool (#4653) cannot exist without an answer — an assistant
//! that speaks unprompted while the owner is sitting right there is noise, and
//! one that stays silent while the owner is away is useless. The three signals
//! that look like they might serve were each verified NOT to (epic #4646,
//! 2026-08-03 research pass):
//!
//! - `SessionRecord.last_activity_at` (`trusty-mpm`,
//!   `session_manager/record.rs:204`) advances on TOOL and HOOK activity. An
//!   assistant grinding through a long solo task advances it exactly as a human
//!   typing does, so it answers "is something happening?", never "is someone
//!   here?".
//! - Telegram/Slack `focus` (`trusty-mpm/src/telegram/focus.rs`) is a
//!   per-conversation routing `HashMap` with no lease and no heartbeat. It
//!   answers "where do inbound messages go", which survives the human walking
//!   away unchanged.
//! - SSE (`api/server/events_sse.rs`) tracks no connection count at all.
//!
//! So this is a NEW signal, built to the owner's binding decision **D3**
//! (2026-08-03): unattended is a LAST-HUMAN-TURN TIMEOUT with one tunable
//! threshold. Explicitly not SSE connection counting, and not a manual
//! do-not-disturb toggle.
//!
//! What:
//! - [`TurnOrigin`] — the crux of the whole module. A turn is either
//!   [`TurnOrigin::Human`] or [`TurnOrigin::Assistant`], and ONLY the former
//!   advances the clock. Every other kind of activity an instance produces
//!   (its own replies, tool calls, tool results, hook fires, background wakes)
//!   is [`TurnOrigin::Assistant`] and is inert here by construction — that is
//!   precisely the distinction `last_activity_at` cannot make.
//! - [`AttendanceTracker`] — records human turns and answers
//!   [`AttendanceTracker::is_unattended`] for one instance.
//! - [`Attendance`] — the three-way answer, including the "no human has ever
//!   spoken to this instance" case that a bare timestamp cannot express.
//! - [`AttendanceConfig`] — the single tunable threshold, defaulting to
//!   [`DEFAULT_UNATTENDED_AFTER`] and overridable via
//!   [`UNATTENDED_AFTER_ENV`].
//!
//! DURABLE. State is one small JSON file per instance under
//! `~/.trusty-agents/attendance/<instance>.json`, written through
//! [`crate::state_writer::atomic_write`] (the same lock+tmp+rename path every
//! other trusty-agents state file uses), so the answer survives a restart. That
//! matters for correctness, not just tidiness: an in-memory tracker would
//! report a fresh process as never-attended, and a never-attended instance is
//! unattended (see [`Attendance::NeverAttended`]) — so restarting the API
//! server while the owner was mid-conversation would hand B2 a licence to
//! notify a human who is demonstrably right there.
//!
//! Scope: this module delivers the SIGNAL only. It sends nothing, queues
//! nothing, and knows nothing about Telegram, Slack or the `notify_owner` tool
//! — those are #4653/#4654/#4655/#4657.
//!
//! Test: `tests` — the whole module.

use std::time::Duration;

mod tracker;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use tracker::{
    AttendanceTracker, attendance_root, default_attendance_root, note_command_turn_in,
    note_human_turn, note_human_turn_in,
};

/// How long after the last human turn an instance counts as unattended.
///
/// Why: the threshold has to clear an ordinary interruption — reading a
/// message, taking a call, refilling a coffee — because notifying a human who
/// is about to look back at the screen is the failure mode D3 exists to avoid.
/// It also has to be short enough that a genuine walk-away is noticed inside
/// one break rather than one workday. Fifteen minutes is the shortest span that
/// comfortably clears the first without approaching the second, and it matches
/// the idle convention users already expect from chat clients.
/// What: 15 minutes, as a [`Duration`]. Tunable per [`AttendanceConfig`].
/// Test: `default_threshold_is_fifteen_minutes`.
pub const DEFAULT_UNATTENDED_AFTER: Duration = Duration::from_secs(15 * 60);

/// Environment override for the unattended threshold, in whole minutes.
///
/// Why: D3 requires the threshold be tunable, and an env var is the override
/// mechanism the rest of this crate already uses for operator-facing paths
/// (`TAGENT_ASSISTANTS_DIR`).
/// What: `TAGENT_UNATTENDED_AFTER_MINS`. Parsed by
/// [`AttendanceConfig::from_env`]; see [`parse_threshold_minutes`] for the
/// rejection rules.
/// Test: `parse_threshold_accepts_whole_minutes`.
pub const UNATTENDED_AFTER_ENV: &str = "TAGENT_UNATTENDED_AFTER_MINS";

/// Who produced a turn — the distinction this whole module exists to make.
///
/// Why: `SessionRecord.last_activity_at` cannot tell an assistant's own tool
/// call from a human typing, which is why it cannot answer "is someone here?".
/// Making the origin an explicit, exhaustive parameter of
/// [`AttendanceTracker::record_turn`] moves that decision to the ONE place that
/// knows the answer — the entry point the turn arrived through — and makes
/// "assistant activity must not count as attendance" a property the type system
/// carries rather than a convention a future call site can forget.
/// What: two variants, and deliberately only two. Anything that is not a human
/// putting words in front of this assistant is [`Self::Assistant`]: the
/// assistant's own replies, its tool calls and their results, hook fires,
/// scheduled wakes, and event-listener-triggered turns. Passing
/// [`Self::Assistant`] is always SAFE — it is a documented no-op, not an
/// error — so a call site that is unsure has a correct default.
/// Test: `assistant_turns_never_advance_the_clock`,
/// `assistant_only_activity_leaves_no_record_at_all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TurnOrigin {
    /// A person addressed this assistant: a chat message they typed, a
    /// Telegram/Slack message they sent, a REPL prompt they submitted.
    Human,
    /// Anything the assistant itself produced or was driven by: its own
    /// replies, tool calls, tool results, hooks, timers, event wakes.
    Assistant,
}

impl TurnOrigin {
    /// Whether a turn of this origin advances the last-human-turn clock.
    ///
    /// Test: `assistant_turns_never_advance_the_clock`.
    pub fn is_human(self) -> bool {
        matches!(self, Self::Human)
    }
}

/// Whether a human is attending an instance, and for how long they have not
/// spoken.
///
/// Why: a bare `Option<DateTime>` forces every caller to re-derive the
/// threshold comparison, and re-derivation is where two callers disagree.
/// Returning the verdict WITH the idle duration also lets a caller log why it
/// decided what it decided without a second query.
/// What: three states. [`Self::NeverAttended`] is not a degenerate
/// [`Self::Unattended`] — it means no human turn has EVER been recorded for
/// this instance, which a duration cannot express. Both it and
/// [`Self::Unattended`] answer `true` to [`Self::is_unattended`]: the question
/// is "is a human attending", and no evidence of a human is not evidence of a
/// human.
/// Test: `fresh_human_turn_is_attended`, `past_threshold_is_unattended`,
/// `never_attended_counts_as_unattended`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attendance {
    /// A human turn landed less than the threshold ago.
    Attended {
        /// Time since that turn.
        idle_for: Duration,
    },
    /// A human turn was recorded, but at least the threshold ago.
    Unattended {
        /// Time since that turn.
        idle_for: Duration,
    },
    /// No human turn has ever been recorded for this instance.
    NeverAttended,
}

impl Attendance {
    /// Whether B2 may treat the owner as away.
    ///
    /// Why: the one predicate `notify_owner` (#4653) gates on, so the
    /// never-attended policy lives here rather than being re-decided per
    /// caller.
    /// What: `true` for [`Self::Unattended`] and [`Self::NeverAttended`],
    /// `false` for [`Self::Attended`].
    /// Test: `never_attended_counts_as_unattended`,
    /// `threshold_boundary_is_unattended_at_exactly_the_threshold`.
    pub fn is_unattended(self) -> bool {
        !matches!(self, Self::Attended { .. })
    }

    /// Time since the last human turn, when there has been one.
    ///
    /// Test: `fresh_human_turn_is_attended`.
    pub fn idle_for(self) -> Option<Duration> {
        match self {
            Self::Attended { idle_for } | Self::Unattended { idle_for } => Some(idle_for),
            Self::NeverAttended => None,
        }
    }
}

/// The single tunable this module exposes (D3).
///
/// Why: D3 specifies "one tunable threshold". Keeping it a struct rather than a
/// loose `Duration` argument means adding a second knob later does not change
/// every [`AttendanceTracker`] construction site.
/// What: one field. [`Default`] is [`DEFAULT_UNATTENDED_AFTER`] and reads no
/// environment; [`Self::from_env`] is the only environment-reading
/// constructor, so tests configure a tracker directly and never race on a
/// process-global variable.
/// Test: `default_threshold_is_fifteen_minutes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttendanceConfig {
    /// Idle time at or beyond which an instance counts as unattended.
    pub unattended_after: Duration,
}

impl Default for AttendanceConfig {
    fn default() -> Self {
        Self {
            unattended_after: DEFAULT_UNATTENDED_AFTER,
        }
    }
}

impl AttendanceConfig {
    /// The configuration with [`UNATTENDED_AFTER_ENV`] applied, if it is set.
    ///
    /// Why: an operator tuning the threshold must not have to edit a file the
    /// app also writes.
    /// What: delegates to [`parse_threshold_minutes`]; an unset, blank or
    /// unparseable value leaves the default in place rather than failing —
    /// a typo in an env var must not stop an assistant from running.
    /// Test: `parse_threshold_accepts_whole_minutes`,
    /// `parse_threshold_rejects_junk_and_zero`.
    pub fn from_env() -> Self {
        let raw = std::env::var(UNATTENDED_AFTER_ENV).ok();
        Self {
            unattended_after: parse_threshold_minutes(raw.as_deref())
                .unwrap_or(DEFAULT_UNATTENDED_AFTER),
        }
    }
}

/// Parse an [`UNATTENDED_AFTER_ENV`] value into a threshold.
///
/// Why: a free function taking the raw value (rather than reading the variable
/// itself) is what makes the parsing rules testable without mutating
/// process-global state — the exact race that makes
/// `assistants::tests::home_tests::for_instance_validates_the_id` flaky
/// (#4611).
/// What: `Some(duration)` for a positive whole number of minutes; `None` for
/// absent, blank, non-numeric, negative or zero input. Zero is rejected rather
/// than honoured because a zero threshold makes every instance permanently
/// unattended, which is a footgun disguised as a setting.
/// Test: `parse_threshold_accepts_whole_minutes`,
/// `parse_threshold_rejects_junk_and_zero`.
pub fn parse_threshold_minutes(raw: Option<&str>) -> Option<Duration> {
    let mins: u64 = raw?.trim().parse().ok()?;
    (mins > 0).then(|| Duration::from_secs(mins * 60))
}
