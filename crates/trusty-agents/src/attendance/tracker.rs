//! Durable last-human-turn state, and the attended/unattended query (#4652).
//!
//! Why: see the module doc on [`super`] for why this signal is new work. This
//! file holds the half that touches disk, kept apart from the vocabulary
//! ([`TurnOrigin`], [`Attendance`], [`AttendanceConfig`]) so the types a caller
//! reasons about are readable without the I/O.
//! What: [`AttendanceTracker`] over a directory of one JSON record per
//! instance. Writes go through [`crate::state_writer::atomic_write`], so a GUI,
//! an `--api` sidecar and a REPL sharing `~/.trusty-agents/` cannot tear each
//! other's records.
//! Test: `super::tests` — the whole module.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{Attendance, AttendanceConfig, TurnOrigin};
use crate::assistants::AssistantInstanceId;
use crate::state_writer;

/// Directory name holding one attendance record per instance.
const ATTENDANCE_DIR: &str = "attendance";

/// The attendance directory under an explicit app-state root.
///
/// Why: the injectable form. Tests point it at a temp dir and never touch
/// `$HOME`, so nothing here can race a concurrently-running test on a
/// process-global (#4611).
/// What: `<base>/attendance`. Touches no filesystem.
/// Test: `default_root_is_under_the_app_state_tree`.
pub fn attendance_root(base: impl AsRef<Path>) -> PathBuf {
    base.as_ref().join(ATTENDANCE_DIR)
}

/// The attendance directory under `~/.trusty-agents`.
///
/// Why: one definition, so a writer and a reader in different processes cannot
/// disagree about where the signal lives.
/// What: `~/.trusty-agents/attendance`. The DOTTED, app-private tree — this is
/// machine state, not the user-browsable `~/trusty-agents/<instance>/` home
/// that `assistants::home` deliberately hands to the user.
/// Test: `default_root_is_under_the_app_state_tree`.
pub fn default_attendance_root() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home directory; cannot locate attendance state")?;
    Ok(attendance_root(home.join(".trusty-agents")))
}

/// One instance's persisted last-human-turn timestamp.
///
/// What: `instance` is carried for legibility when a human opens the file;
/// `last_human_turn_at` is the value. Unknown keys are ignored so a later
/// field addition cannot make an existing record unreadable.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AttendanceRecord {
    /// The instance this record belongs to.
    instance: String,
    /// RFC3339 instant of the most recent [`TurnOrigin::Human`] turn.
    last_human_turn_at: DateTime<Utc>,
}

/// Records human turns and answers whether an instance is unattended (D3).
///
/// Why: the consumable seam #4652 owes B2 (#4653). Holding the threshold on the
/// tracker (rather than passing it per query) means every caller in a process
/// answers the question the same way.
/// What: a root directory plus one [`AttendanceConfig`]. Construction touches
/// no filesystem; the directory is created lazily on the first recorded human
/// turn, so an instance nobody has ever spoken to leaves no trace on disk.
///
/// `now` is a PARAMETER on every query rather than read from the system clock
/// inside, which is what lets the tests drive the threshold boundary exactly
/// instead of sleeping.
/// Test: `super::tests` — the whole module.
#[derive(Debug, Clone)]
pub struct AttendanceTracker {
    root: PathBuf,
    config: AttendanceConfig,
}

impl AttendanceTracker {
    /// A tracker over an explicit attendance directory.
    ///
    /// Test: `fresh_human_turn_is_attended`.
    pub fn new(root: impl Into<PathBuf>, config: AttendanceConfig) -> Self {
        Self {
            root: root.into(),
            config,
        }
    }

    /// A tracker over [`default_attendance_root`].
    ///
    /// Test: `default_root_is_under_the_app_state_tree`.
    pub fn with_default_root(config: AttendanceConfig) -> Result<Self> {
        Ok(Self::new(default_attendance_root()?, config))
    }

    /// The threshold this tracker applies. Test: `default_threshold_is_fifteen_minutes`.
    pub fn config(&self) -> AttendanceConfig {
        self.config
    }

    /// Where `instance`'s record lives.
    ///
    /// Why: exposed so an operator-facing diagnostic can name the file. Safe as
    /// a path segment because [`AssistantInstanceId`] is validated at
    /// construction — it cannot contain a separator, `.` or `..`.
    /// Test: `record_path_is_one_file_per_instance`.
    pub fn record_path(&self, instance: &AssistantInstanceId) -> PathBuf {
        self.root.join(format!("{instance}.json"))
    }

    /// Record a turn, advancing the last-human-turn clock only if it was a
    /// human's (#4652).
    ///
    /// Why: THE distinction the whole issue turns on. `last_activity_at` is
    /// unusable precisely because every write to it looks alike; here the
    /// origin is an explicit argument, so a call site that hands over the
    /// assistant's own work cannot silently manufacture attendance. Making the
    /// [`TurnOrigin::Assistant`] case a no-op rather than an error means the
    /// safe choice is also the easy one for an unsure caller.
    /// What: for [`TurnOrigin::Human`], persists `at` and returns `Ok(true)` —
    /// unless the stored timestamp is already at or after `at`, in which case
    /// nothing is written and it returns `Ok(false)`. That monotonic guard
    /// stops an out-of-order or replayed turn from REWINDING attendance, which
    /// would make an absent owner look present. For [`TurnOrigin::Assistant`]
    /// it touches no file and returns `Ok(false)`.
    ///
    /// Errors are I/O only. A caller should log and continue: failing to record
    /// attendance degrades the signal (the instance looks less attended than it
    /// is, which biases toward silence), it does not break the turn.
    /// Test: `fresh_human_turn_is_attended`,
    /// `assistant_turns_never_advance_the_clock`,
    /// `assistant_only_activity_leaves_no_record_at_all`,
    /// `an_older_human_turn_never_rewinds_the_clock`.
    pub fn record_turn(
        &self,
        instance: &AssistantInstanceId,
        origin: TurnOrigin,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        if !origin.is_human() {
            return Ok(false);
        }
        if self
            .last_human_turn(instance)?
            .is_some_and(|stored| stored >= at)
        {
            return Ok(false);
        }
        let record = AttendanceRecord {
            instance: instance.as_str().to_string(),
            last_human_turn_at: at,
        };
        let body = serde_json::to_vec_pretty(&record).context("serializing attendance record")?;
        let path = self.record_path(instance);
        state_writer::atomic_write(&path, &body)
            .with_context(|| format!("writing attendance record {}", path.display()))?;
        Ok(true)
    }

    /// When a human last addressed `instance`, if ever.
    ///
    /// Why: the raw value, for callers that want to render "last seen 4 minutes
    /// ago" rather than a verdict.
    /// What: `Ok(None)` when no record exists — an instance nobody has spoken
    /// to is a normal state, not a fault. A record that exists but cannot be
    /// read or parsed IS an error: silently treating corruption as "never
    /// attended" would hand B2 a licence to notify on a bad file.
    /// Test: `fresh_human_turn_is_attended`,
    /// `unreadable_record_is_an_error_not_a_silent_never`.
    pub fn last_human_turn(&self, instance: &AssistantInstanceId) -> Result<Option<DateTime<Utc>>> {
        let path = self.record_path(instance);
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading attendance record {}", path.display()))?;
        let record: AttendanceRecord = serde_json::from_str(&raw)
            .with_context(|| format!("attendance record {} is not valid JSON", path.display()))?;
        Ok(Some(record.last_human_turn_at))
    }

    /// Whether a human is attending `instance` as of `now`.
    ///
    /// Why: the query D3 asks for, with the threshold comparison in ONE place.
    /// What: [`Attendance::NeverAttended`] with no record;
    /// [`Attendance::Unattended`] when `now - last >= unattended_after`;
    /// [`Attendance::Attended`] otherwise. The boundary is inclusive on the
    /// unattended side so a threshold of N minutes means "N minutes of silence
    /// is enough", which is how an operator reads the setting.
    ///
    /// A stored timestamp in the FUTURE (clock skew, an edited file) yields an
    /// idle time of zero — i.e. attended. Biasing skew toward silence is the
    /// safe direction: the cost of a missed notification is a delay, the cost
    /// of a wrong one is interrupting a human who is present.
    /// Test: `fresh_human_turn_is_attended`, `past_threshold_is_unattended`,
    /// `threshold_boundary_is_unattended_at_exactly_the_threshold`,
    /// `never_attended_counts_as_unattended`,
    /// `a_future_timestamp_reads_as_attended`.
    pub fn attendance(
        &self,
        instance: &AssistantInstanceId,
        now: DateTime<Utc>,
    ) -> Result<Attendance> {
        let Some(last) = self.last_human_turn(instance)? else {
            return Ok(Attendance::NeverAttended);
        };
        let idle_for = (now - last).to_std().unwrap_or(Duration::ZERO);
        Ok(if idle_for >= self.config.unattended_after {
            Attendance::Unattended { idle_for }
        } else {
            Attendance::Attended { idle_for }
        })
    }

    /// The one-line form of [`Self::attendance`] for callers that only gate.
    ///
    /// Test: `past_threshold_is_unattended`, `never_attended_counts_as_unattended`.
    pub fn is_unattended(
        &self,
        instance: &AssistantInstanceId,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        Ok(self.attendance(instance, now)?.is_unattended())
    }
}

/// Record that a human just addressed `instance`, best-effort (#4652).
///
/// Why: the four surfaces a person can actually talk to an assistant through
/// (`POST /api/task`, the REPL, Telegram, Slack) each hold a persona NAME and a
/// message they know came from a human — and nothing else. Handing them one
/// infallible call, rather than a tracker to build and a `Result` to handle,
/// is what makes the hook a single line at each site and keeps the
/// human-vs-assistant judgement where the evidence is. It is also why this
/// function exists at all instead of the handlers calling
/// [`AttendanceTracker::record_turn`]: an assistant-side caller has no
/// `TurnOrigin` argument here to get wrong.
/// What: validates the name, resolves [`default_attendance_root`], and records
/// a [`TurnOrigin::Human`] turn at `now`. Returns whether the clock advanced.
/// Every failure — an unusable name, no home directory, an I/O error — is
/// logged at debug and swallowed: attendance is a hint for #4653, never a
/// reason to fail a turn the user is waiting on. A swallowed failure biases the
/// instance toward looking LESS attended, i.e. toward B2 staying quiet.
/// Test: `note_human_turn_records_a_turn_and_is_infallible`,
/// `note_human_turn_swallows_an_unusable_instance_name`.
pub fn note_human_turn_in(root: impl Into<PathBuf>, instance: &str, now: DateTime<Utc>) -> bool {
    match record_human(root, instance, now) {
        Ok(advanced) => advanced,
        Err(error) => {
            tracing::debug!(instance, %error, "could not record human turn for attendance");
            false
        }
    }
}

/// [`note_human_turn_in`] over [`default_attendance_root`] and the system
/// clock — the form the four human-facing call sites use.
///
/// The root and `now` are injectable one layer down (rather than sandboxed via
/// `$HOME`) for the reason `test_env` states outright: injection is the durable
/// fix, `HOME_LOCK` is the legacy one. So every test drives
/// [`note_human_turn_in`] against a temp directory and this wrapper stays a
/// two-line delegation with nothing of its own to get wrong.
/// Test: covered through `note_human_turn_in`; the root resolution it adds is
/// pinned by `default_root_is_under_the_app_state_tree`.
pub fn note_human_turn(instance: &str) -> bool {
    let Ok(root) = default_attendance_root() else {
        tracing::debug!(instance, "no home directory; not recording human turn");
        return false;
    };
    note_human_turn_in(root, instance, Utc::now())
}

/// The fallible body of [`note_human_turn_in`], split out so the error can be
/// logged in one place.
fn record_human(root: impl Into<PathBuf>, instance: &str, now: DateTime<Utc>) -> Result<bool> {
    let id = AssistantInstanceId::new(instance)?;
    AttendanceTracker::new(root, AttendanceConfig::default()).record_turn(
        &id,
        TurnOrigin::Human,
        now,
    )
}
