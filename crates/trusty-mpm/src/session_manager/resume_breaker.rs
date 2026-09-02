//! Auto-resume circuit breaker for a session whose runtime keeps dying (#6568).
//!
//! Why: `runtime_reap` marks a session `Stopped` when the pane's runtime exits,
//! and the supervisor poller auto-resumes any `Stopped` record whose stop nobody
//! asked for. For a session whose pane SURVIVED the runtime exit, `resume` takes
//! the #2148 re-attach branch — it verifies the pane is alive and flips the
//! record back to `Active` WITHOUT relaunching anything in that pane. The pane
//! is still the bare shell the reaper just classified, so the next reap tick
//! marks it `Stopped` again. That is a stable two-step cycle, and it ran for
//! seven sessions for the full 48 hours of the audit: 2,170 stops against 2,128
//! resumes, one pair every 60-70 seconds, each doing real tmux and store work.
//!
//! What: the policy is a counter and a window. Each auto-resume stamps
//! `last_auto_resume_at`. Each runtime-exit stop asks [`evaluate_breaker_verdict`] whether this
//! death came within [`ResumeBreakerConfig::flap_window`] of that stamp; if it
//! did, the consecutive count rises, and at
//! [`ResumeBreakerConfig::max_consecutive`] the session is PARKED — the reaper
//! writes [`StopCause::ResumeFlapping`](super::record::StopCause::ResumeFlapping)
//! instead of `Unexpected`, `is_auto_resumable` goes false, and no automatic
//! path relaunches it again. A death OUTSIDE the window resets the count: a
//! session that ran for a while and then crashed is not flapping.
//!
//! This does not change `auto_resume` control-state semantics. `auto_resume`
//! still decides WHETHER automatic resumes happen at all; the breaker only
//! decides that ONE session has exhausted its budget, and it is expressed
//! through the existing `stop_cause` gate every automatic caller already reads.
//! An operator's own `tm session resume` is unaffected and clears the park.
//!
//! The counter is kept beside the store rather than on `SessionRecord` because
//! it is supervision bookkeeping, not session identity: the daemon and the
//! `tm supervisor` process are separate processes, so it must be persisted, but
//! losing the file only costs a session one extra resume cycle. The PARK itself
//! lives on the record, so a lost sidecar can never un-park anything.
//!
//! Test: `resume_breaker_tests.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::record::ManagedSessionId;

/// Default flap window: a death this soon after an auto-resume counts (#6568).
///
/// The observed cycle was 60-70 seconds end to end, so 120 seconds catches it
/// with margin while leaving a session that ran for minutes uncounted.
pub const DEFAULT_FLAP_WINDOW_SECS: u64 = 120;

/// Default consecutive fast deaths before auto-resume is parked (#6568).
///
/// Five cycles is roughly five minutes of thrash — long enough that a genuine
/// transient (a machine waking, a tmux server restart) rides it out, short
/// enough that a permanent flap stops costing work almost immediately.
pub const DEFAULT_MAX_CONSECUTIVE: u32 = 5;

/// Environment override for [`DEFAULT_FLAP_WINDOW_SECS`]. Zero disables the
/// breaker (no death is ever inside a zero-length window).
pub const ENV_FLAP_WINDOW_SECS: &str = "TRUSTY_MPM_RESUME_FLAP_WINDOW_SECS";

/// Environment override for [`DEFAULT_MAX_CONSECUTIVE`].
pub const ENV_MAX_CONSECUTIVE: &str = "TRUSTY_MPM_RESUME_FLAP_THRESHOLD";

/// The breaker's two tunables.
///
/// Why: N and K have to be settable without a rebuild — the right values depend
/// on how long a healthy session on this host takes to come up — and settable
/// from a test without touching the process environment.
/// What: `flap_window` is N (how soon after an auto-resume a death counts) and
/// `max_consecutive` is K (how many in a row park the session).
/// Test: `config_defaults`, `config_env_parsing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeBreakerConfig {
    /// A runtime exit within this long of the last auto-resume is a flap.
    pub flap_window: Duration,
    /// Consecutive flaps that park the session. `0` is normalised to `1`.
    pub max_consecutive: u32,
}

impl Default for ResumeBreakerConfig {
    fn default() -> Self {
        Self {
            flap_window: Duration::from_secs(DEFAULT_FLAP_WINDOW_SECS),
            max_consecutive: DEFAULT_MAX_CONSECUTIVE,
        }
    }
}

impl ResumeBreakerConfig {
    /// Build the config from the process environment.
    ///
    /// Why: the daemon is launched unattended by launchd, which can only pass
    /// configuration through the environment.
    /// What: delegates to [`Self::from_env_with`] over [`std::env::var`].
    /// Test: covered through `from_env_with` by `config_env_parsing`.
    pub fn from_env() -> Self {
        Self::from_env_with(|k| std::env::var(k).ok())
    }

    /// Build the config from an injectable environment resolver.
    ///
    /// What: an absent or unparsable value keeps the default. A zero window is
    /// honoured (it disables the breaker); a zero threshold is normalised to
    /// `1`, because parking on the zeroth flap would park a session that never
    /// flapped.
    /// Test: `config_defaults`, `config_env_parsing`,
    /// `a_zero_window_disables_the_breaker`.
    pub fn from_env_with(get: impl Fn(&str) -> Option<String>) -> Self {
        let defaults = Self::default();
        let flap_window = get(ENV_FLAP_WINDOW_SECS)
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(defaults.flap_window);
        let max_consecutive = get(ENV_MAX_CONSECUTIVE)
            .and_then(|v| v.trim().parse::<u32>().ok())
            .map(|k| k.max(1))
            .unwrap_or(defaults.max_consecutive);
        Self {
            flap_window,
            max_consecutive,
        }
    }
}

/// One session's flap bookkeeping.
///
/// Why: the decision needs both halves — when the session was last auto-resumed
/// and how many fast deaths have followed in a row — and they must move
/// together.
/// What: `last_auto_resume_at` is `None` until an automatic path resumes this
/// session; `consecutive` counts deaths inside the window since the last reset.
/// Test: `resume_breaker_tests.rs`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlapState {
    /// When an automatic path last resumed this session.
    #[serde(default)]
    pub last_auto_resume_at: Option<DateTime<Utc>>,
    /// Consecutive runtime exits inside the flap window.
    #[serde(default)]
    pub consecutive: u32,
}

/// What [`evaluate_breaker_verdict`] decided about one runtime-exit stop.
///
/// Why: the caller has to distinguish "not flapping" from "flapping but not yet
/// parked" from "park it", and a bool cannot carry the count the log line needs.
/// What: `Reset` when this death is not attributable to a recent auto-resume;
/// `Counting` while the streak is below the threshold; `Park` once it reaches it.
/// Test: `evaluate_*` in `resume_breaker_tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerVerdict {
    /// Not a flap — the streak restarts at zero.
    Reset,
    /// A flap, but the session still has budget left.
    Counting {
        /// The streak length after counting this death.
        consecutive: u32,
    },
    /// The streak reached the threshold — park auto-resume for this session.
    Park {
        /// The streak length that tripped the breaker.
        consecutive: u32,
    },
}

impl BreakerVerdict {
    /// The streak length this verdict carries (`0` for [`Self::Reset`]).
    pub fn consecutive(&self) -> u32 {
        match self {
            Self::Reset => 0,
            Self::Counting { consecutive } | Self::Park { consecutive } => *consecutive,
        }
    }
}

/// Decide what one runtime-exit stop means for a session's resume budget.
///
/// Why: this is the whole policy, and keeping it a pure function of (config,
/// state, now) means the flap can be reproduced in a unit test in microseconds
/// rather than by waiting out real 60-second cycles.
/// What: a death with no recorded auto-resume, or one that landed at or after
/// `flap_window` past the last auto-resume, is [`BreakerVerdict::Reset`] — the
/// session ran long enough that relaunching it is still the right answer.
/// Otherwise the streak increments; at `max_consecutive` it is
/// [`BreakerVerdict::Park`], below it [`BreakerVerdict::Counting`]. A `now`
/// EARLIER than the stamp (a clock step backwards) is treated as outside the
/// window and resets, because a negative age is not evidence of a fast death.
/// Test: `evaluate_resets_without_a_prior_auto_resume`,
/// `evaluate_resets_for_a_slow_death`, `evaluate_counts_a_fast_death`,
/// `evaluate_parks_at_the_threshold`, `evaluate_resets_on_a_backwards_clock`,
/// `a_zero_window_disables_the_breaker`.
pub fn evaluate_breaker_verdict(
    cfg: &ResumeBreakerConfig,
    state: &FlapState,
    now: DateTime<Utc>,
) -> BreakerVerdict {
    let Some(last) = state.last_auto_resume_at else {
        // Nothing auto-resumed this session, so this death is not evidence that
        // auto-resume is failing to fix anything.
        return BreakerVerdict::Reset;
    };
    let age = now.signed_duration_since(last);
    // A negative age means the clock moved; `to_std` fails and we reset rather
    // than treat an impossible age as the fastest possible death.
    let Ok(age) = age.to_std() else {
        return BreakerVerdict::Reset;
    };
    if age >= cfg.flap_window {
        return BreakerVerdict::Reset;
    }
    let consecutive = state.consecutive.saturating_add(1);
    if consecutive >= cfg.max_consecutive.max(1) {
        BreakerVerdict::Park { consecutive }
    } else {
        BreakerVerdict::Counting { consecutive }
    }
}

/// Persisted per-session flap counters, stored beside the session store.
///
/// Why: the reaper (in the daemon) and the poller (in `tm supervisor`) run in
/// SEPARATE PROCESSES, so an in-memory counter never sees the other half of the
/// cycle — the daemon's `last_auto_resume_at` stays `None`, every death
/// evaluates as `Reset`, and the breaker cannot trip. The file is therefore
/// authoritative, exactly as `sessions.json` is: every read reloads it and every
/// write goes out atomically, through the shared [`super::json_file`]
/// primitives `SessionStore` uses for the sibling file in the same directory.
/// What: a `HashMap<session id, FlapState>`. [`Self::reload`] re-reads on every
/// access; [`Self::persist`] stages through a per-instance temp file and renames.
///
/// Deliberately WITHOUT the (mtime, len) fingerprint short-circuit
/// `SessionStore` uses, because this payload defeats it. One cycle restamps a
/// fixed-width timestamp and steps a counter `1`→`2`→`3`, so the serialized
/// length does not move and freshness would rest on mtime alone; on a
/// filesystem with 1-second mtime resolution the supervisor's stamp and the
/// daemon's read can land in the same tick, compare equal, and skip the reload
/// that is the entire point of this file. The sidecar is a few hundred bytes
/// read about once a minute per process, so the stat saves nothing worth that
/// risk. `SessionStore`'s own use of the fingerprint is unaffected — its
/// records change length on almost every transition.
///
/// Not a lock: two processes read-modify-writing concurrently can still lose an
/// update. That is tolerable here and only here — a lost counter delays a park
/// by one cycle, and the PARK itself lives on the session record, so no lost
/// write can un-park anything or cause a park that was not earned.
///
/// Every I/O failure is logged and swallowed for the same reason: failing a
/// runtime-exit reconcile because a counter could not be written would be
/// strictly worse than losing the counter.
/// Test: `store_round_trips_state`, `store_survives_a_missing_file`,
/// `store_survives_a_corrupt_file`,
/// `two_managers_over_one_data_dir_still_park_a_flapping_session`.
#[derive(Debug)]
pub struct ResumeBreakerStore {
    path: PathBuf,
    tmp_path: PathBuf,
    states: HashMap<String, FlapState>,
}

impl ResumeBreakerStore {
    /// File name of the sidecar inside the session-manager data directory.
    pub const FILE_NAME: &'static str = "resume-breaker.json";

    /// Open the sidecar under `data_dir`, loading whatever is there.
    ///
    /// What: reads `<data_dir>/resume-breaker.json`. A missing file, an
    /// unreadable one, and an unparsable one all yield an EMPTY map — never an
    /// error — because a lost counter can only delay a park, never cause one.
    /// Test: `store_survives_a_missing_file`, `store_survives_a_corrupt_file`.
    pub async fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(Self::FILE_NAME);
        let tmp_path = super::json_file::staging_path(&path);
        let mut store = Self {
            path,
            tmp_path,
            states: HashMap::new(),
        };
        store.reload().await;
        store
    }

    /// Re-read the sidecar from disk, unconditionally.
    ///
    /// Why (#6568): this is the whole cross-process fix. `note_auto_resume` runs
    /// in `tm supervisor` and `record_death` runs in the daemon; without a
    /// reload the daemon never observes the supervisor's stamp, so every death
    /// evaluates as `Reset` and the breaker is inert in production while passing
    /// every single-process test.
    ///
    /// Unconditional, with no (mtime, len) short-circuit: this payload keeps a
    /// constant serialized length across a cycle, so the fingerprint would rest
    /// on mtime alone and a coarse-mtime filesystem could skip the very reload
    /// this exists for. See the type's doc for the full reasoning.
    /// What: reads and replaces the map. A read or parse failure keeps the map
    /// already in memory and logs; it never errors. A file that is simply absent
    /// is a legitimate empty state, not a failure.
    /// Test: `an_external_write_is_picked_up_by_the_next_read`,
    /// `two_managers_over_one_data_dir_still_park_a_flapping_session`.
    async fn reload(&mut self) {
        match tokio::fs::read_to_string(&self.path).await {
            Ok(body) => match serde_json::from_str(&body) {
                Ok(states) => self.states = states,
                Err(e) => warn!(
                    path = %self.path.display(),
                    "resume-breaker: sidecar unparsable ({e}); keeping the counters already in memory"
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Absent is a legitimate state, not a failure: nothing has
                // flapped yet, or the file was cleaned up between passes.
                self.states.clear();
            }
            Err(e) => warn!(
                path = %self.path.display(),
                "resume-breaker: sidecar unreadable ({e}); keeping the counters already in memory"
            ),
        }
    }

    /// This session's current bookkeeping, reconciled with disk first.
    pub async fn state_of(&mut self, id: &ManagedSessionId) -> FlapState {
        self.reload().await;
        self.states
            .get(&id.to_string())
            .cloned()
            .unwrap_or_default()
    }

    /// Record that an AUTOMATIC path just resumed this session.
    ///
    /// Why: the stamp is what makes the next death attributable, and it is
    /// written in a DIFFERENT process from the one that reads it — so it
    /// reloads before mutating, or it would write back a map missing every row
    /// the daemon added since this process last looked.
    /// What: reloads, stamps `last_auto_resume_at = now` (keeping
    /// `consecutive`), persists.
    /// Test: `a_fast_death_after_an_auto_resume_counts`,
    /// `two_managers_over_one_data_dir_still_park_a_flapping_session`.
    pub async fn note_auto_resume(&mut self, id: &ManagedSessionId, now: DateTime<Utc>) {
        self.reload().await;
        let entry = self.states.entry(id.to_string()).or_default();
        entry.last_auto_resume_at = Some(now);
        self.persist().await;
    }

    /// Record that an OPERATOR resumed this session, clearing its streak.
    ///
    /// Why: a manual resume is the operator saying the cause is addressed. If it
    /// did not clear the streak, a session unparked by hand would re-park on its
    /// very next fast death instead of getting a fresh budget.
    /// What: reloads, drops the entry — an absent entry IS the zero state, so
    /// this also keeps the sidecar from accumulating rows for healthy sessions —
    /// and persists.
    /// Test: `an_operator_resume_forgives_the_streak`,
    /// `an_in_place_reactivate_forgives_the_streak`.
    pub async fn note_operator_resume(&mut self, id: &ManagedSessionId) {
        self.reload().await;
        if self.states.remove(&id.to_string()).is_some() {
            self.persist().await;
        }
    }

    /// Apply one runtime-exit stop and return what it means.
    ///
    /// What: reloads (so the other process's stamp is visible), evaluates
    /// through [`evaluate_breaker_verdict`], then stores the resulting streak — `Reset` clears
    /// the entry, including the stamp, so the NEXT death is not attributed to a
    /// resume that is now old news; `Counting`/`Park` keep the stamp and store
    /// the new count.
    /// Test: `a_reset_forgets_the_stamp_as_well_as_the_count`,
    /// `a_fast_death_after_an_auto_resume_counts`,
    /// `two_managers_over_one_data_dir_still_park_a_flapping_session`.
    pub async fn record_death(
        &mut self,
        id: &ManagedSessionId,
        cfg: &ResumeBreakerConfig,
        now: DateTime<Utc>,
    ) -> BreakerVerdict {
        let state = self.state_of(id).await;
        let verdict = evaluate_breaker_verdict(cfg, &state, now);
        match verdict {
            BreakerVerdict::Reset => {
                self.states.remove(&id.to_string());
            }
            BreakerVerdict::Counting { consecutive } | BreakerVerdict::Park { consecutive } => {
                self.states.insert(
                    id.to_string(),
                    FlapState {
                        last_auto_resume_at: state.last_auto_resume_at,
                        consecutive,
                    },
                );
            }
        }
        self.persist().await;
        verdict
    }

    /// Write the map back atomically, best-effort.
    ///
    /// Why: a plain truncate-and-rewrite lets the other process read a torn
    /// file — the hazard `SessionStore::save` documents for the sibling file in
    /// this same directory. Routing through [`super::json_file::write_atomic`]
    /// means there is one implementation of that rule, not two.
    /// What: serialises, stages through this instance's private temp path, and
    /// renames. Records no fingerprint — [`Self::reload`] re-reads
    /// unconditionally, so there is nothing for one to short-circuit. Failures
    /// log and return; see the type's doc for why this never errors.
    async fn persist(&mut self) {
        let body = match serde_json::to_string_pretty(&self.states) {
            Ok(b) => b,
            Err(e) => {
                warn!("resume-breaker: could not serialise counters ({e}); not persisted");
                return;
            }
        };
        if let Err(e) = super::json_file::write_atomic(&self.path, &self.tmp_path, &body).await {
            warn!(
                path = %self.path.display(),
                "resume-breaker: could not persist counters ({e}); they will restart empty"
            );
        }
    }
}

/// The manager-side wiring the breaker owns (#6568).
///
/// Why: `manager.rs` sits at the 500-SLOC production cap, and these methods are
/// the breaker's rather than the manager's — keeping them beside the policy
/// they implement means the resume/park pair cannot drift from [`evaluate_breaker_verdict`].
/// Same extraction precedent `reconcile.rs` and `create.rs` set for that file.
impl super::SessionManager {
    /// Resume a session on behalf of an AUTOMATIC path.
    ///
    /// Why: the breaker only means anything if the two halves of the cycle can
    /// be told apart. An auto-resume must STAMP the attempt so the next runtime
    /// exit can be attributed to it; an operator's resume must CLEAR the streak.
    /// One function doing both would let every auto-resume reset the counter it
    /// is supposed to be filling, and the breaker could never trip.
    /// What: [`super::SessionManager::resume`]'s shared body, followed by
    /// [`ResumeBreakerStore::note_auto_resume`]. The caller still owns the
    /// `is_auto_resumable` gate — this does not re-check it, exactly as `resume`
    /// does not.
    /// Test: `the_supervisor_parks_a_flapping_session_after_k_cycles` in
    /// `resume_breaker_tests.rs`.
    pub async fn resume_auto(
        &self,
        id: &ManagedSessionId,
    ) -> Result<super::SessionRecord, super::manager::ManagedError> {
        let record = self.resume_inner(id).await?;
        self.resume_breaker
            .write()
            .await
            .note_auto_resume(id, Utc::now())
            .await;
        Ok(record)
    }

    /// Resume a session on behalf of an OPERATOR, forgiving the flap streak.
    ///
    /// What: [`super::SessionManager::resume`] calls this after its shared body;
    /// see that method's doc for the resume contract itself.
    /// Test: `an_operator_resume_forgives_the_streak`.
    pub(crate) async fn note_operator_resume(&self, id: &ManagedSessionId) {
        self.resume_breaker
            .write()
            .await
            .note_operator_resume(id)
            .await;
    }

    /// Decide the `stop_cause` one runtime-exit stop earns.
    ///
    /// Why: this is where the breaker actually trips, and the only place a
    /// [`BreakerVerdict`] becomes a persisted decision — so the log line and the
    /// recorded cause cannot disagree about what happened.
    /// What: records the death against this session's counter, then maps the
    /// verdict. `Park` warns and returns
    /// [`StopCause::ResumeFlapping`](super::record::StopCause::ResumeFlapping);
    /// `Counting` and `Reset` both return
    /// [`StopCause::Unexpected`](super::record::StopCause::Unexpected), which is
    /// the pre-#6568 behavior.
    /// Test: `the_supervisor_parks_a_flapping_session_after_k_cycles`,
    /// `a_session_that_dies_slowly_is_never_parked`.
    pub(crate) async fn runtime_exit_stop_cause(
        &self,
        id: &ManagedSessionId,
        tmux_name: &str,
    ) -> super::record::StopCause {
        use super::record::StopCause;
        let verdict = self
            .resume_breaker
            .write()
            .await
            .record_death(id, &self.resume_breaker_cfg, Utc::now())
            .await;
        match verdict {
            BreakerVerdict::Park { consecutive } => {
                warn!(
                    id = %id,
                    name = %tmux_name,
                    consecutive,
                    window_secs = self.resume_breaker_cfg.flap_window.as_secs(),
                    "runtime-reap: parking auto-resume — this session's runtime exited within the \
                     flap window of its own auto-resume {consecutive} times in a row (#6568). \
                     Resume it by hand once the cause is fixed; that clears the park"
                );
                StopCause::ResumeFlapping
            }
            BreakerVerdict::Counting { consecutive } => {
                tracing::debug!(
                    id = %id,
                    name = %tmux_name,
                    consecutive,
                    threshold = self.resume_breaker_cfg.max_consecutive,
                    "runtime-reap: auto-resume flap streak (#6568)"
                );
                StopCause::Unexpected
            }
            BreakerVerdict::Reset => StopCause::Unexpected,
        }
    }

    /// Override the breaker tunables on an already-built manager.
    ///
    /// Why: the policy is time-based, and a test that had to wait out a real
    /// 120-second window five times over is not a test anyone runs. This is the
    /// seam that lets the flap be reproduced in milliseconds.
    /// What: replaces `resume_breaker_cfg`. Production never calls it —
    /// `SessionManager::new` reads the environment.
    /// Test: `resume_breaker_tests.rs`.
    #[cfg(test)]
    pub(crate) fn set_resume_breaker_config(&mut self, cfg: ResumeBreakerConfig) {
        self.resume_breaker_cfg = cfg;
    }
}
