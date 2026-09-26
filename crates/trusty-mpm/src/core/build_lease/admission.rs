//! Whether one more heavy build may start right now (#8261).
//!
//! Why: the decision is the part a reviewer must be able to read in one place,
//! and every failure path runs through it. Keeping it pure — every reading is
//! an argument — makes each arm of the fail-open table in
//! [`crate::core::build_lease`] a scripted test with no machine and no sleep.
//!
//! What: [`decide`] folds the ceiling, the live lease count, the memory
//! pressure, the load average and the foreign-build census into a
//! [`Decision`]. [`decide_unleased`] is the bound for a build that could not
//! take a lease at all.
//!
//! **Order of the gates.** Pressure first: a reading above
//! `builders.memory_pressure_max`, or available memory under
//! `builders.min_available_pct`, admits no NEW build whatever the count —
//! running builds are never touched. Then load: above `logical cores x
//! builders.load_factor` a build is admitted only when no lease is held. Then
//! the count: `held < n_effective`, `n_effective = max(1, ceiling - foreign)`.
//!
//! **Why the leased reduction floors at one.** The census matches process
//! names; a false positive (an IDE's background `cargo check`) must not starve
//! every leased build. The UNLEASED bound has no such floor: a build that holds
//! no lease is itself only visible to the census, so the census is the whole
//! bound there.
//! Test: the `#[cfg(test)]` suite below.

use trusty_common::memory_pressure::MemoryPressure;

use super::config::BuildLeaseConfig;
use crate::core::build_probe::BuildGroup;
use crate::core::builders::BuildersConfig;

/// The readings one decision is taken from. Each is independently fallible.
///
/// Test: `a_quiet_machine_admits_up_to_the_ceiling`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Readings {
    /// The kernel's memory pressure, or why it could not be read.
    pub pressure: Result<MemoryPressure, String>,
    /// The 1-minute load average, or why it could not be read.
    pub load_avg_1min: Result<f64, String>,
    /// Logical cores; zero reads as one.
    pub logical_cores: usize,
    /// Compiler groups holding no lease, or why the census failed.
    pub foreign: Result<Vec<BuildGroup>, String>,
    /// This machine's host name, for messages.
    pub host: String,
}

impl Readings {
    /// Readings from their parts.
    #[must_use]
    pub fn new(
        pressure: Result<MemoryPressure, String>,
        load_avg_1min: Result<f64, String>,
        logical_cores: usize,
        foreign: Result<Vec<BuildGroup>, String>,
        host: impl Into<String>,
    ) -> Self {
        Self {
            pressure,
            load_avg_1min,
            logical_cores,
            foreign,
            host: host.into(),
        }
    }
}

/// What [`decide`] concluded.
///
/// Test: every test below.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Decision {
    /// Whether a new build may start now.
    pub admit: bool,
    /// The slots this machine has room for right now.
    pub n_effective: u32,
    /// The configured ceiling.
    pub ceiling: u32,
    /// Live leases counted.
    pub held: u32,
    /// Why a build was withheld, one line each. Empty when admitted.
    pub withheld: Vec<String>,
    /// Readings that could not be taken, and what was done instead.
    pub degraded: Vec<String>,
    /// Every reading, rendered for a message or a log line.
    pub readings: String,
}

/// The floor on the foreign-reduced LEASED slot count. See the module doc.
pub const MIN_SLOTS: u32 = 1;

/// Decide whether one more leased build may start.
///
/// What: see the module doc. Unreadable inputs degrade as the fail-open table
/// in [`crate::core::build_lease`] states. An invalid config value falls back
/// to that key's default — the pressure gate still runs, on the default
/// `normal` / 10% limits. Never unlimited: the ceiling and the lease count
/// always apply, and a ceiling of `0` admits nothing.
/// Test: `a_quiet_machine_admits_up_to_the_ceiling`,
/// `warn_pressure_refuses_a_new_build_but_counts_the_holders`,
/// `low_available_memory_refuses_even_at_normal_pressure`,
/// `a_loaded_machine_admits_only_when_nothing_is_held`,
/// `foreign_builds_reduce_the_slots_but_never_below_one`,
/// `unreadable_pressure_uses_the_ceiling_and_warns`,
/// `an_unreadable_load_skips_only_the_load_gate`,
/// `an_unreadable_census_counts_leases_only`,
/// `an_invalid_config_still_gates_on_pressure`, `a_zero_ceiling_admits_nothing`.
#[must_use]
pub fn decide(
    builders: &BuildersConfig,
    lease: &BuildLeaseConfig,
    ceiling: u32,
    held: u32,
    readings: &Readings,
) -> Decision {
    let mut withheld = Vec::new();
    let mut degraded = Vec::new();
    let (builders, lease) = valid_or_default(builders, lease, &mut degraded);

    let foreign_count = match &readings.foreign {
        Ok(groups) if lease.effective_count_foreign_builds() => {
            u32::try_from(groups.len()).unwrap_or(u32::MAX)
        }
        Ok(_) => 0,
        Err(err) => {
            degraded.push(format!(
                "process census unreadable ({err}) — counting leases only"
            ));
            0
        }
    };
    let n_effective = if ceiling == 0 {
        0
    } else {
        ceiling.saturating_sub(foreign_count).max(MIN_SLOTS)
    };

    pressure_gate(&lease, &readings.pressure, &mut withheld, &mut degraded);
    let cores = readings.logical_cores.max(1);
    #[allow(clippy::cast_precision_loss)]
    let threshold = cores as f64 * builders.effective_load_factor();
    match &readings.load_avg_1min {
        Ok(load) if *load > threshold && held > 0 => withheld.push(format!(
            "1-minute load {load:.2} is above {threshold:.2} (logical cores x \
             builders.load_factor) and {held} build(s) already hold a lease"
        )),
        Ok(_) => {}
        Err(err) => degraded.push(format!(
            "load average unreadable ({err}) — load gate skipped"
        )),
    }
    if held >= n_effective {
        withheld.push(format!(
            "{held} of {n_effective} slot(s) held (ceiling {ceiling} \
             builders.max_concurrent, minus {foreign_count} foreign build(s))"
        ));
    }
    Decision {
        admit: withheld.is_empty(),
        n_effective,
        ceiling,
        held,
        withheld,
        degraded,
        readings: render_readings(readings, threshold, held, ceiling),
    }
}

/// Decide whether a build that could take NO lease may run anyway.
///
/// Why: the fail-open arms (no slot directory, no admission lock, every slot
/// file broken) must not admit unbounded builds. Such a build is visible only
/// to the census, so the census bounds it: foreign compiler groups — every
/// unleased build already running, and every orphan a SIGKILLed holder left
/// behind — must be below the ceiling, with NO floor.
/// What: admits when the census reads fewer groups than `ceiling` and the
/// pressure gate passes. A census that cannot be read either — lease AND
/// census both failed — WITHHOLDS: nothing would bound the build, and the
/// 2026-09-21 owner ruling on #8261 forbids unlimited admission. The build
/// waits and times out with both failures named; `tm doctor` names the repair.
/// Test: `unleased_builds_are_bounded_by_the_census`,
/// `no_lease_and_no_census_admits_nothing`.
#[must_use]
pub fn decide_unleased(lease: &BuildLeaseConfig, ceiling: u32, readings: &Readings) -> Decision {
    let mut withheld = Vec::new();
    let mut degraded = Vec::new();
    let default_lease = BuildLeaseConfig::default();
    let lease = if lease.validate().is_ok() {
        lease
    } else {
        &default_lease
    };
    pressure_gate(lease, &readings.pressure, &mut withheld, &mut degraded);
    let foreign = match &readings.foreign {
        Ok(groups) => {
            let n = u32::try_from(groups.len()).unwrap_or(u32::MAX);
            if n >= ceiling {
                withheld.push(format!(
                    "no lease could be taken, and {n} build(s) holding no lease already \
                     fill the ceiling {ceiling} (builders.max_concurrent)"
                ));
            }
            n
        }
        // #8261 (owner ruling 2026-09-21): never unlimited admission.
        Err(err) => {
            withheld.push(format!(
                "no lease could be taken AND the process census is unreadable ({err}), so \
                 nothing bounds this build; it does not start until one of them recovers — \
                 `tm doctor`'s builder_cap row names the fault"
            ));
            0
        }
    };
    #[allow(clippy::cast_precision_loss)]
    let threshold =
        readings.logical_cores.max(1) as f64 * crate::core::builders::DEFAULT_LOAD_FACTOR;
    Decision {
        admit: withheld.is_empty(),
        n_effective: ceiling.saturating_sub(foreign),
        ceiling,
        held: 0,
        withheld,
        degraded,
        readings: render_readings(readings, threshold, 0, ceiling),
    }
}

/// The configs to decide with: each as given when valid, else its default.
fn valid_or_default(
    builders: &BuildersConfig,
    lease: &BuildLeaseConfig,
    degraded: &mut Vec<String>,
) -> (BuildersConfig, BuildLeaseConfig) {
    let builders = match builders.validate() {
        Ok(()) => builders.clone(),
        Err(err) => {
            degraded.push(format!("{err} — using that section's defaults"));
            BuildersConfig::default()
        }
    };
    let lease = match lease.validate() {
        Ok(()) => lease.clone(),
        Err(err) => {
            degraded.push(format!("{err} — using the build-lease defaults"));
            BuildLeaseConfig::default()
        }
    };
    (builders, lease)
}

/// The pressure gate: pushes a withheld line, or a degraded one.
fn pressure_gate(
    lease: &BuildLeaseConfig,
    pressure: &Result<MemoryPressure, String>,
    withheld: &mut Vec<String>,
    degraded: &mut Vec<String>,
) {
    let reading = match pressure {
        Ok(reading) => reading,
        Err(err) => {
            degraded.push(format!(
                "memory pressure unreadable ({err}) — pressure gate skipped, ceiling and load \
                 still apply"
            ));
            return;
        }
    };
    let max = lease.effective_memory_pressure_max();
    if reading.level > max {
        withheld.push(format!(
            "memory pressure is {} ({}), above builders.memory_pressure_max = {max}; no new \
             build starts until it falls, running builds continue",
            reading.level,
            reading.render_signals()
        ));
    }
    let min_pct = lease.effective_min_available_pct();
    if let Some(pct) = reading.available_pct
        && pct < min_pct
    {
        withheld.push(format!(
            "available memory {pct:.0}% is below builders.min_available_pct = {min_pct:.0}%"
        ));
    }
}

/// `host …, memory pressure …, 1-min load …, lease holders N, foreign builds …, ceiling C`.
fn render_readings(readings: &Readings, threshold: f64, held: u32, ceiling: u32) -> String {
    let pressure = match &readings.pressure {
        Ok(p) => format!("memory pressure {} ({})", p.level, p.render_signals()),
        Err(err) => format!("memory pressure UNREADABLE ({err})"),
    };
    let load = match &readings.load_avg_1min {
        Ok(l) => format!(
            "1-min load {l:.2} (threshold {threshold:.2}, {} cores)",
            readings.logical_cores
        ),
        Err(err) => format!("load UNREADABLE ({err})"),
    };
    let foreign = match &readings.foreign {
        Ok(groups) if groups.is_empty() => "foreign builds 0".to_string(),
        Ok(groups) => format!(
            "foreign builds {}: {}",
            groups.len(),
            groups
                .iter()
                .map(BuildGroup::render)
                .collect::<Vec<_>>()
                .join("; ")
        ),
        Err(err) => format!("census UNREADABLE ({err})"),
    };
    format!(
        "host {}, {pressure}, {load}, lease holders {held}, {foreign}, ceiling {ceiling}",
        readings.host
    )
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
