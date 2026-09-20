//! How many builders this machine has room for right now (#8261).
//!
//! Why: #6892 capped builders at a number derived from the host's RAM TIER — a
//! property of the machine that never changes. The machine's actual saturation
//! does change, minute to minute, and the cap could not see it: the 2026-09-19
//! evidence has one session's refusal naming three holders while a second
//! session ran two more engineers and a qa agent, so the count undercounted what
//! was actually compiling. This module replaces the fixed answer with one
//! derived from measured 1-minute load average and measured free memory, bounded
//! above by the operator's ceiling.
//!
//! What: [`resolve_capacity`] is pure — it takes the readings as arguments — so
//! every branch is assertable from a scripted reading with no sleeps, no real
//! machine, and no state directory. [`CapacityReadings`] is what a caller
//! samples; [`QuietWindow`] is the one piece of carried state, and it exists
//! only to stop N oscillating upward.
//!
//! **Three invariants this module exists to hold.**
//! 1. N drops AT ONCE when a reading goes bad; under-admitting is the safe
//!    direction, and the machine that crashed on 2026-08-08 is the cost of the
//!    other one.
//! 2. N rises only after a full [`QuietWindow`] interval with BOTH checks
//!    passing, so a reading that flickers under the threshold cannot admit a
//!    builder the machine does not have room for.
//! 3. An UNREADABLE reading fails CLOSED to the fixed ceiling — #6892's exact
//!    behaviour, which is the conservative fallback and NOT unlimited admission
//!    — and says which reading failed, with its errno, in the refusal and in
//!    `tm doctor`. [`FailClosedSurface`] names the two surfaces #8298's
//!    acceptance criteria named.
//!
//! A granted lease is never revoked by anything here: [`resolve_capacity`]
//! floors its answer at the current holder count, so a drop in N refuses the
//! NEXT builder rather than evicting a running one.
//!
//! Test: the `#[cfg(test)]` suite below.

use std::fmt;

use crate::core::builders::{BuildersConfig, BuildersConfigError};

/// Which capacity reading could not be taken.
///
/// Why: #8298's acceptance criteria name these two surfaces by string, and the
/// refusal message and the `tm doctor` row both have to print the name the
/// criteria use. A `bool` would lose which one failed, which is the whole point
/// of distinguishing "unreadable" from "exceeded".
/// What: [`Self::name`] renders the criterion's own identifier.
/// Test: `the_fail_closed_surfaces_use_the_acceptance_criteria_names`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailClosedSurface {
    /// The 1-minute load average could not be read.
    Load,
    /// Free memory could not be read.
    Memory,
}

impl FailClosedSurface {
    /// The identifier #8298's acceptance criteria use for this surface.
    ///
    /// Test: `the_fail_closed_surfaces_use_the_acceptance_criteria_names`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Load => "builder-cap-load-read-failure",
            Self::Memory => "builder-cap-memory-read-failure",
        }
    }
}

impl fmt::Display for FailClosedSurface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One capacity reading that could not be taken.
///
/// Why: carrying the errno alongside the surface is what lets the warn log and
/// the `tm doctor` row say WHY the read failed, which is the difference between
/// an operator fixing a permission and an operator restarting a daemon.
/// What: `detail` is the source error rendered; `errno` is the raw OS code when
/// there was one (a malformed `/proc/loadavg` line has none).
/// Test: `an_unreadable_load_fails_closed_to_the_ceiling`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFailure {
    /// Which reading failed.
    pub surface: FailClosedSurface,
    /// The underlying error, rendered.
    pub detail: String,
    /// The OS errno, when the failure carried one.
    pub errno: Option<i32>,
}

/// The two measured inputs to the formula, each independently fallible.
///
/// Why: a struct of two `Result`s rather than a `Result` of a struct, because
/// the surfaces fail independently and the refusal must name the one that did.
/// What: `load_avg_1min` is run-queue length, NOT normalised by cores;
/// `available_bytes` is the OS "available" figure. `logical_cores` is read once
/// at process start and is not fallible in practice — a zero is treated as one,
/// because dividing the machine into zero cores would make every threshold zero
/// and refuse every builder.
/// Test: `a_quiet_machine_resolves_to_the_ceiling`.
#[derive(Debug, Clone, PartialEq)]
pub struct CapacityReadings {
    /// The 1-minute load average, or why it could not be read.
    pub load_avg_1min: Result<f64, ReadFailure>,
    /// OS-available memory in bytes, or why it could not be read.
    pub available_bytes: Result<u64, ReadFailure>,
    /// This host's logical core count.
    pub logical_cores: usize,
}

/// Why N is what it is.
///
/// Why: the refusal message must name which condition failed, show the measured
/// reading AND the configured limit, and distinguish a reading that could not be
/// taken from one that was exceeded (#8261 closure condition). A bare number
/// cannot do any of that, so the number never travels alone.
/// What: one variant per path through [`resolve_capacity`].
/// Test: one test per variant, named on each.
#[derive(Debug, Clone, PartialEq)]
pub enum CapacityReason {
    /// Both readings pass and the quiet window is satisfied: N is the ceiling.
    /// Test: `a_quiet_machine_resolves_to_the_ceiling`.
    AtCeiling {
        /// The measured 1-minute load average.
        load: f64,
        /// `logical_cores * load_factor`.
        load_threshold: f64,
        /// The measured available bytes.
        available_bytes: u64,
        /// The configured floor in bytes.
        floor_bytes: u64,
    },
    /// The load average is above its threshold.
    /// Test: `admission_refused_at_high_load`.
    LoadAboveThreshold {
        /// The measured 1-minute load average.
        load: f64,
        /// `logical_cores * load_factor`.
        threshold: f64,
    },
    /// Free memory is under the configured floor.
    /// Test: `admission_refused_when_memory_under_floor_at_low_load`.
    MemoryBelowFloor {
        /// The measured available bytes.
        available_bytes: u64,
        /// The configured floor in bytes.
        floor_bytes: u64,
    },
    /// Both readings pass now, but a bad reading inside the quiet window means
    /// N has not been allowed to climb back to the ceiling yet.
    /// Test: `n_rises_only_after_a_full_quiet_window`.
    WaitingOutQuietWindow {
        /// Seconds still to wait before N may rise.
        remaining_secs: i64,
    },
    /// A reading could not be taken at all: N is the fixed ceiling, exactly as
    /// it was before this module existed.
    /// Test: `an_unreadable_load_fails_closed_to_the_ceiling`,
    /// `an_unreadable_memory_reading_fails_closed_to_the_ceiling`.
    FailedClosedToCeiling(ReadFailure),
    /// The `[builders]` section carries a value this harness will not act on.
    /// Test: `an_invalid_config_fails_closed_to_the_ceiling_naming_the_key`.
    ConfigRefused(String),
}

impl CapacityReason {
    /// Did a reading fail rather than a limit being exceeded?
    ///
    /// Why: `tm doctor` reports a fail-closed posture differently from a busy
    /// machine — one is a defect to fix, the other is the mechanism working.
    /// Test: `an_unreadable_load_fails_closed_to_the_ceiling`.
    #[must_use]
    pub fn fail_closed_surface(&self) -> Option<FailClosedSurface> {
        match self {
            Self::FailedClosedToCeiling(failure) => Some(failure.surface),
            _ => None,
        }
    }
}

impl fmt::Display for CapacityReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AtCeiling {
                load,
                load_threshold,
                available_bytes,
                floor_bytes,
            } => write!(
                f,
                "capacity available (1-min load {load:.2} <= {load_threshold:.2}, \
                 free memory {} MB >= {} MB)",
                available_bytes / MB,
                floor_bytes / MB
            ),
            Self::LoadAboveThreshold { load, threshold } => write!(
                f,
                "1-minute load average {load:.2} is above the threshold {threshold:.2} \
                 (logical cores x builders.load_factor)"
            ),
            Self::MemoryBelowFloor {
                available_bytes,
                floor_bytes,
            } => write!(
                f,
                "free memory {} MB is below the floor {} MB \
                 (builders.free_memory_floor_mb)",
                available_bytes / MB,
                floor_bytes / MB
            ),
            Self::WaitingOutQuietWindow { remaining_secs } => write!(
                f,
                "both readings pass but the machine was overloaded within the last \
                 re-evaluation interval; {remaining_secs}s left before the slot count may rise"
            ),
            Self::FailedClosedToCeiling(failure) => write!(
                f,
                "{} — the {:?} reading could not be taken ({}{}), so the slot count \
                 failed CLOSED to the fixed ceiling",
                failure.surface,
                failure.surface,
                failure.detail,
                match failure.errno {
                    Some(errno) => format!(", errno {errno}"),
                    None => String::new(),
                }
            ),
            Self::ConfigRefused(detail) => write!(
                f,
                "{detail} — the slot count failed CLOSED to the fixed ceiling"
            ),
        }
    }
}

/// Bytes in a megabyte, for the human-readable halves of the messages above.
const MB: u64 = 1024 * 1024;

/// One full re-evaluation interval, in seconds.
///
/// Why: N may rise only after a quiet window of one full interval with both
/// checks passing (#8261). 60 seconds because the input IS a 1-minute load
/// average — a shorter window would let N rise on a reading that still carries
/// the load it is supposed to have waited out.
/// Test: `n_rises_only_after_a_full_quiet_window`.
pub const QUIET_WINDOW_SECS: i64 = 60;

/// The only state the formula carries between decisions.
///
/// Why: the formula is otherwise pure, and this is the one thing it cannot
/// derive from a single reading — "was the machine overloaded a moment ago".
/// Keeping it a separate, owned value means the daemon holds one and every test
/// holds its own, with no global.
/// What: the timestamp of the last reading that failed either check, or `None`
/// for a machine that has never been seen overloaded. [`Self::observe`] is the
/// only mutator.
/// Test: `n_rises_only_after_a_full_quiet_window`,
/// `a_fresh_window_permits_the_ceiling_immediately`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuietWindow {
    last_bad_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl QuietWindow {
    /// Record this decision's verdict and answer whether N may rise.
    ///
    /// Why: recording and asking are one step for the same reason the builder
    /// claim is — two steps race, and this one races with itself across
    /// concurrent admissions.
    /// What: a bad reading stamps `now` and returns `false` (N may not rise;
    /// it is about to drop anyway). A good reading returns `true` only when
    /// `window_secs` have passed since the last bad one. Returns
    /// `Err(remaining_secs)` while the window is still open, so the caller can
    /// report how long is left without recomputing it.
    ///
    /// # Errors
    ///
    /// The seconds still to wait, when the window has not elapsed.
    ///
    /// Test: `n_rises_only_after_a_full_quiet_window`,
    /// `a_fresh_window_permits_the_ceiling_immediately`.
    pub fn observe(
        &mut self,
        readings_ok: bool,
        now: chrono::DateTime<chrono::Utc>,
        window_secs: i64,
    ) -> Result<(), i64> {
        if !readings_ok {
            self.last_bad_at = Some(now);
            return Err(window_secs);
        }
        let Some(last_bad) = self.last_bad_at else {
            return Ok(());
        };
        // Clamped at zero so a clock stepping backwards cannot manufacture a
        // window that never closes.
        let elapsed = (now - last_bad).num_seconds().max(0);
        if elapsed >= window_secs {
            self.last_bad_at = None;
            return Ok(());
        }
        Err(window_secs - elapsed)
    }
}

/// What the machine has room for, and why.
///
/// Why: `n_effective` alone cannot build a refusal message or a doctor row, and
/// re-deriving the reason at those sites would let the number and the
/// explanation disagree. They travel together, always.
/// What: `n_effective` is already clamped to `floor..=ceiling` and floored at
/// `held`, so a caller compares `holders.len() < n_effective` and nothing else.
/// Test: `a_quiet_machine_resolves_to_the_ceiling`, and the per-variant tests.
#[derive(Debug, Clone, PartialEq)]
pub struct Capacity {
    /// How many builders may hold a slot right now.
    pub n_effective: u32,
    /// The operator's hard ceiling.
    pub ceiling: u32,
    /// Why `n_effective` is what it is.
    pub reason: CapacityReason,
}

/// The floor on N.
///
/// Why: a host that can run `trusty-mpm` at all can run one builder, and an N
/// of zero on a busy machine would deadlock the harness — every builder refused
/// forever, including the one whose completion would lower the load.
/// Test: `a_loaded_machine_with_no_holders_still_admits_one`.
pub const MIN_SLOTS: u32 = 1;

/// How many builders this machine has room for right now (#8261).
///
/// Why: THE formula, in one pure function, so the three invariants in the module
/// doc are reviewable as one expression rather than assembled from the daemon's
/// control flow.
/// What: with both readings taken and both checks passing and the quiet window
/// closed, `N = ceiling`. With either check failing, `N = max(MIN_SLOTS, held)` —
/// which refuses the next builder without revoking any granted lease. With
/// either reading UNAVAILABLE, or the config refused, `N = ceiling`: the
/// fail-CLOSED fallback to #6892's fixed behaviour, never to unlimited.
///
/// `held` is the current holder count, and flooring at it is invariant 3 in the
/// module doc — a lease, once granted, is never revoked by a capacity drop.
///
/// Test: `a_quiet_machine_resolves_to_the_ceiling`,
/// `admission_refused_at_high_load`,
/// `admission_refused_when_memory_under_floor_at_low_load`,
/// `an_unreadable_load_fails_closed_to_the_ceiling`,
/// `an_unreadable_memory_reading_fails_closed_to_the_ceiling`,
/// `an_invalid_config_fails_closed_to_the_ceiling_naming_the_key`,
/// `n_is_never_revoked_mid_lease`, `n_rises_only_after_a_full_quiet_window`,
/// `a_loaded_machine_with_no_holders_still_admits_one`.
#[must_use]
pub fn resolve_capacity(
    readings: &CapacityReadings,
    config: &BuildersConfig,
    ceiling: u32,
    held: u32,
    quiet: &mut QuietWindow,
    now: chrono::DateTime<chrono::Utc>,
) -> Capacity {
    // A config the harness will not act on is the same class of event as an
    // unreadable metric: the inputs are unusable, so fall back to the fixed
    // ceiling rather than guessing at the operator's intent.
    if let Err(err) = config.validate() {
        return fail_closed(ceiling, CapacityReason::ConfigRefused(err.to_string()));
    }
    let load = match &readings.load_avg_1min {
        Ok(load) => *load,
        Err(failure) => {
            return fail_closed(ceiling, CapacityReason::FailedClosedToCeiling(failure.clone()));
        }
    };
    let available_bytes = match &readings.available_bytes {
        Ok(bytes) => *bytes,
        Err(failure) => {
            return fail_closed(ceiling, CapacityReason::FailedClosedToCeiling(failure.clone()));
        }
    };

    // A zero core count would make every threshold zero and refuse forever.
    let cores = readings.logical_cores.max(1);
    #[allow(clippy::cast_precision_loss)]
    let load_threshold = cores as f64 * config.effective_load_factor();
    let floor_bytes = config.effective_free_memory_floor_bytes();
    let load_ok = load <= load_threshold;
    let mem_ok = available_bytes >= floor_bytes;

    let throttled = |reason| Capacity {
        n_effective: held.max(MIN_SLOTS).min(ceiling.max(MIN_SLOTS)),
        ceiling,
        reason,
    };
    match quiet.observe(load_ok && mem_ok, now, QUIET_WINDOW_SECS) {
        Err(remaining_secs) => {
            if !load_ok {
                return throttled(CapacityReason::LoadAboveThreshold {
                    load,
                    threshold: load_threshold,
                });
            }
            if !mem_ok {
                return throttled(CapacityReason::MemoryBelowFloor {
                    available_bytes,
                    floor_bytes,
                });
            }
            throttled(CapacityReason::WaitingOutQuietWindow { remaining_secs })
        }
        Ok(()) => Capacity {
            n_effective: ceiling,
            ceiling,
            reason: CapacityReason::AtCeiling {
                load,
                load_threshold,
                available_bytes,
                floor_bytes,
            },
        },
    }
}

/// The fail-CLOSED answer: #6892's fixed ceiling, with the reason attached.
///
/// Why: named rather than inlined three times, so "unreadable falls back to the
/// ceiling, not to unlimited" is one line a reviewer can check.
/// What: also emits the warn the operator surface promises, with the errno.
/// Test: `an_unreadable_load_fails_closed_to_the_ceiling`.
fn fail_closed(ceiling: u32, reason: CapacityReason) -> Capacity {
    tracing::warn!(
        surface = ?reason.fail_closed_surface(),
        "builder capacity failed closed to the fixed ceiling {ceiling}: {reason}"
    );
    Capacity {
        n_effective: ceiling,
        ceiling,
        reason,
    }
}

/// The config error type, re-exported so a caller matching on
/// [`CapacityReason::ConfigRefused`] can find its source next door.
pub use BuildersConfigError as CapacityConfigError;

/// Take both readings from THIS machine, right now.
///
/// Why: the one impure function in this module, kept to exactly the two reads
/// and no decision, so [`resolve_capacity`] stays assertable from scripted
/// readings. Every test above injects a [`CapacityReadings`] instead of calling
/// this, which is what makes the suite deterministic with no sleeps.
/// What: the 1-minute load average from
/// [`trusty_common::load_average`], and available bytes from a
/// [`HostSampler`](trusty_common::host_metrics::HostSampler) memory sample.
/// Neither failure is fatal and neither is substituted — each becomes a
/// [`ReadFailure`] that [`resolve_capacity`] fails CLOSED on.
///
/// The sampler is constructed per call. `sysinfo`'s memory refresh needs no
/// warm-up interval (unlike its CPU usage, which is why this reads a load
/// average instead), so a fresh sampler's first memory read is already correct.
/// Test: `live_readings_are_plausible_on_this_host`.
#[must_use]
pub fn sample_capacity_readings() -> CapacityReadings {
    let load_avg_1min = trusty_common::load_average::read_load_average()
        .map(|avg| avg.one_minute)
        .map_err(|err| ReadFailure {
            surface: FailClosedSurface::Load,
            errno: err.errno(),
            detail: err.to_string(),
        });
    let mut sampler = trusty_common::host_metrics::HostSampler::new();
    let memory = sampler.sample().memory;
    // A `total_bytes` of zero is `sysinfo` reporting that it could not read the
    // machine's memory at all — the ONLY way this read fails, and the shape the
    // `builder-cap-memory-read-failure` surface exists for. Treating the
    // accompanying `available_bytes` of zero as a real reading would refuse
    // every builder on a machine that is not actually short of memory.
    let available_bytes = if memory.total_bytes == 0 {
        Err(ReadFailure {
            surface: FailClosedSurface::Memory,
            detail: "sysinfo reported a total memory of 0 bytes".to_string(),
            errno: None,
        })
    } else {
        Ok(memory.available_bytes)
    };
    CapacityReadings {
        load_avg_1min,
        available_bytes,
        logical_cores: std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A machine with room: 16 cores, load well under 32, 64 GB free.
    fn quiet_readings() -> CapacityReadings {
        CapacityReadings {
            load_avg_1min: Ok(4.0),
            available_bytes: Ok(64 * 1024 * MB),
            logical_cores: 16,
        }
    }

    fn at(secs: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(1_800_000_000 + secs, 0).expect("a valid fixed timestamp")
    }

    fn failure(surface: FailClosedSurface) -> ReadFailure {
        ReadFailure {
            surface,
            detail: "operation not permitted".to_string(),
            errno: Some(1),
        }
    }

    #[test]
    fn the_fail_closed_surfaces_use_the_acceptance_criteria_names() {
        assert_eq!(
            FailClosedSurface::Load.name(),
            "builder-cap-load-read-failure"
        );
        assert_eq!(
            FailClosedSurface::Memory.name(),
            "builder-cap-memory-read-failure"
        );
    }

    #[test]
    fn a_quiet_machine_resolves_to_the_ceiling() {
        // The correct degenerate case: on a quiet host the formula reduces to
        // exactly #6892's fixed cap.
        let capacity = resolve_capacity(
            &quiet_readings(),
            &BuildersConfig::default(),
            4,
            0,
            &mut QuietWindow::default(),
            at(0),
        );
        assert_eq!(capacity.n_effective, 4);
        assert!(
            matches!(capacity.reason, CapacityReason::AtCeiling { .. }),
            "{:?}",
            capacity.reason
        );
    }

    #[test]
    fn admission_refused_at_high_load() {
        let readings = CapacityReadings {
            // 40 on 16 cores is 2.5x, past the default 2.0x threshold of 32.
            load_avg_1min: Ok(40.0),
            ..quiet_readings()
        };
        let capacity = resolve_capacity(
            &readings,
            &BuildersConfig::default(),
            4,
            2,
            &mut QuietWindow::default(),
            at(0),
        );
        assert_eq!(
            capacity.n_effective, 2,
            "N drops to the holder count, refusing the next builder"
        );
        let msg = capacity.reason.to_string();
        assert!(msg.contains("40.00") && msg.contains("32.00"), "{msg}");
        assert_eq!(
            capacity.reason.fail_closed_surface(),
            None,
            "an EXCEEDED limit is not an UNREADABLE one"
        );
    }

    #[test]
    fn admission_refused_when_memory_under_floor_at_low_load() {
        let readings = CapacityReadings {
            // Load is fine; only memory is short.
            available_bytes: Ok(2 * 1024 * MB),
            ..quiet_readings()
        };
        let capacity = resolve_capacity(
            &readings,
            &BuildersConfig::default(),
            4,
            1,
            &mut QuietWindow::default(),
            at(0),
        );
        assert_eq!(capacity.n_effective, 1);
        let msg = capacity.reason.to_string();
        assert!(
            msg.contains("2048 MB") && msg.contains("8192 MB"),
            "the message must show the reading AND the floor: {msg}"
        );
    }

    #[test]
    fn an_unreadable_load_fails_closed_to_the_ceiling() {
        let readings = CapacityReadings {
            load_avg_1min: Err(failure(FailClosedSurface::Load)),
            ..quiet_readings()
        };
        let capacity = resolve_capacity(
            &readings,
            &BuildersConfig::default(),
            4,
            0,
            &mut QuietWindow::default(),
            at(0),
        );
        assert_eq!(
            capacity.n_effective, 4,
            "fail CLOSED means #6892's fixed ceiling, never unlimited"
        );
        assert_eq!(
            capacity.reason.fail_closed_surface(),
            Some(FailClosedSurface::Load)
        );
        let msg = capacity.reason.to_string();
        assert!(msg.contains("builder-cap-load-read-failure"), "{msg}");
        assert!(msg.contains("errno 1"), "the errno must be named: {msg}");
    }

    #[test]
    fn an_unreadable_memory_reading_fails_closed_to_the_ceiling() {
        let readings = CapacityReadings {
            available_bytes: Err(failure(FailClosedSurface::Memory)),
            ..quiet_readings()
        };
        let capacity = resolve_capacity(
            &readings,
            &BuildersConfig::default(),
            3,
            0,
            &mut QuietWindow::default(),
            at(0),
        );
        assert_eq!(capacity.n_effective, 3);
        assert!(
            capacity
                .reason
                .to_string()
                .contains("builder-cap-memory-read-failure"),
            "{:?}",
            capacity.reason
        );
    }

    #[test]
    fn an_invalid_config_fails_closed_to_the_ceiling_naming_the_key() {
        let config = BuildersConfig {
            load_factor: Some(200.0),
            ..BuildersConfig::default()
        };
        let capacity = resolve_capacity(
            &quiet_readings(),
            &config,
            4,
            0,
            &mut QuietWindow::default(),
            at(0),
        );
        assert_eq!(capacity.n_effective, 4);
        assert!(
            capacity.reason.to_string().contains("builders.load_factor"),
            "{:?}",
            capacity.reason
        );
    }

    #[test]
    fn n_is_never_revoked_mid_lease() {
        // Four leases granted on a quiet machine; the readings then worsen.
        let loaded = CapacityReadings {
            load_avg_1min: Ok(99.0),
            available_bytes: Ok(1024 * MB),
            logical_cores: 16,
        };
        let capacity = resolve_capacity(
            &loaded,
            &BuildersConfig::default(),
            4,
            4,
            &mut QuietWindow::default(),
            at(0),
        );
        assert_eq!(
            capacity.n_effective, 4,
            "N floors at the holder count: a granted lease is never revoked"
        );
    }

    #[test]
    fn a_loaded_machine_with_no_holders_still_admits_one() {
        let loaded = CapacityReadings {
            load_avg_1min: Ok(99.0),
            ..quiet_readings()
        };
        let capacity = resolve_capacity(
            &loaded,
            &BuildersConfig::default(),
            4,
            0,
            &mut QuietWindow::default(),
            at(0),
        );
        assert_eq!(
            capacity.n_effective, MIN_SLOTS,
            "N of zero would deadlock: no build could ever run to lower the load"
        );
    }

    #[test]
    fn a_fresh_window_permits_the_ceiling_immediately() {
        // A machine that has never been seen overloaded waits for nothing.
        let mut quiet = QuietWindow::default();
        assert_eq!(quiet.observe(true, at(0), QUIET_WINDOW_SECS), Ok(()));
    }

    #[test]
    fn n_rises_only_after_a_full_quiet_window() {
        let mut quiet = QuietWindow::default();
        let config = BuildersConfig::default();
        let loaded = CapacityReadings {
            load_avg_1min: Ok(99.0),
            ..quiet_readings()
        };

        // t=0: overloaded, N drops at once.
        let dropped = resolve_capacity(&loaded, &config, 4, 1, &mut quiet, at(0));
        assert_eq!(dropped.n_effective, 1);

        // t=30: readings are fine again, but the window has not elapsed.
        let waiting = resolve_capacity(&quiet_readings(), &config, 4, 1, &mut quiet, at(30));
        assert_eq!(waiting.n_effective, 1, "N must not rise inside the window");
        match waiting.reason {
            CapacityReason::WaitingOutQuietWindow { remaining_secs } => {
                assert_eq!(remaining_secs, 30);
            }
            other => panic!("expected WaitingOutQuietWindow, got {other:?}"),
        }

        // t=60: a full interval of good readings — N may rise to the ceiling.
        let risen = resolve_capacity(&quiet_readings(), &config, 4, 1, &mut quiet, at(60));
        assert_eq!(risen.n_effective, 4);
        assert!(
            matches!(risen.reason, CapacityReason::AtCeiling { .. }),
            "{:?}",
            risen.reason
        );
    }

    #[test]
    fn a_bad_reading_inside_the_window_restarts_it() {
        let mut quiet = QuietWindow::default();
        assert_eq!(quiet.observe(false, at(0), QUIET_WINDOW_SECS), Err(60));
        assert_eq!(quiet.observe(true, at(50), QUIET_WINDOW_SECS), Err(10));
        // A second bad reading at t=50 restarts the clock from there.
        assert_eq!(quiet.observe(false, at(50), QUIET_WINDOW_SECS), Err(60));
        assert_eq!(quiet.observe(true, at(100), QUIET_WINDOW_SECS), Err(10));
        assert_eq!(quiet.observe(true, at(110), QUIET_WINDOW_SECS), Ok(()));
    }

    /// The one test that touches the real machine. Asserts only what is true of
    /// every host — a threshold assertion here would flake on a loaded runner.
    #[test]
    fn live_readings_are_plausible_on_this_host() {
        let readings = sample_capacity_readings();
        let load = readings
            .load_avg_1min
            .expect("a unix host exposes a 1-minute load average");
        assert!(load.is_finite() && load >= 0.0, "implausible load {load}");
        let available = readings
            .available_bytes
            .expect("a host running this suite can report its memory");
        assert!(available > 0, "a running host has some available memory");
        assert!(readings.logical_cores >= 1);
    }

    #[test]
    fn a_backwards_clock_cannot_hold_the_window_open_forever() {
        let mut quiet = QuietWindow::default();
        assert_eq!(quiet.observe(false, at(1000), QUIET_WINDOW_SECS), Err(60));
        // Now moves BACKWARDS; elapsed clamps at 0 rather than going negative.
        assert_eq!(quiet.observe(true, at(0), QUIET_WINDOW_SECS), Err(60));
    }
}
