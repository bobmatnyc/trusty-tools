//! [`MachineBudget`] — what this machine is, and how many MB a daemon may hold
//! on it.
//!
//! Why: "24 GB supported, 16 GB minimum" needs one enforcement point, or each
//! daemon answers it differently (#6820). `trusty-search` derived these four
//! numbers inside its own `MemoryPolicy`; `trusty-memory` derived none of them
//! and sized its palace cap off a fixed constant regardless of the host.
//! What: reads total RAM once, resolves a [`MemoryTier`], and computes the two
//! proportional soft limits. It carries no per-daemon caps and reads no env
//! vars — a daemon layers its own tunables on top (`trusty-search`'s
//! `MemoryPolicy` does exactly that).
//! Test: `super::tests` — `supported_target_resolves_to_medium_with_pinned_budget`,
//! `budget_is_continuous_across_tier_boundaries`, `degrade_budget_is_reduced_not_zero`.

use super::compute::{compute_index_memory_limit_mb, compute_memory_limit_mb};
use super::constants::{FALLBACK_RAM_MB, MINIMUM_SUPPORTED_RAM_MB};
use super::detect::detect_total_ram_mb;
use super::tier::MemoryTier;

/// The machine's size and the memory budget that follows from it.
///
/// Why: one struct so a daemon takes the tier AND the limits derived from the
/// same RAM reading. Deriving them separately is how a cgroup-clamped host got a
/// tier from one number and a limit from another.
/// What: `total_ram_mb` is the reading (cgroup-clamped on Linux); `tier` is the
/// band; `memory_limit_mb` is 25% of RAM and `index_memory_limit_mb` is 75%,
/// both clamped. `#[non_exhaustive]` so a later field is not a 0.x break for a
/// published consumer.
/// Test: `supported_target_resolves_to_medium_with_pinned_budget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MachineBudget {
    /// Total RAM this process may draw on, in MB. On Linux this is the host's
    /// `MemTotal` clamped to any enclosing cgroup ceiling (issue #3657), so a
    /// container capped at 24 GB on a 128 GB host reports 24 GB.
    pub total_ram_mb: u64,
    /// The band `total_ram_mb` falls in.
    pub tier: MemoryTier,
    /// Steady-state soft cap: 25% of `total_ram_mb`, clamped to
    /// `[MEMORY_LIMIT_FLOOR_MB, MEMORY_LIMIT_CEIL_MB]`.
    pub memory_limit_mb: usize,
    /// Indexing-pipeline soft cap: 75% of `total_ram_mb`, clamped to
    /// `[INDEX_MEMORY_LIMIT_FLOOR_MB, INDEX_MEMORY_LIMIT_CEIL_MB]`. Always
    /// >= [`Self::memory_limit_mb`].
    pub index_memory_limit_mb: usize,
}

impl MachineBudget {
    /// Read total RAM from the platform and resolve the budget.
    ///
    /// Why: the entry point for a daemon at startup. Detection failure is not
    /// fatal — an unsupported OS or an unreadable `/proc` still needs a
    /// well-defined budget.
    /// What: [`detect_total_ram_mb`], falling back to [`FALLBACK_RAM_MB`] (8 GB,
    /// which resolves to [`MemoryTier::Degraded`]) with a warning. Spawns
    /// `sysctl` once on macOS; reads `/proc/meminfo` plus the cgroup files on
    /// Linux.
    /// Test: `detect_returns_a_plausible_budget` on the host running the suite.
    pub fn detect() -> Self {
        let total_ram_mb = detect_total_ram_mb().unwrap_or_else(|| {
            tracing::warn!(
                "machine_tier: could not detect total system RAM — \
                 falling back to {FALLBACK_RAM_MB} MB (Degraded tier)"
            );
            FALLBACK_RAM_MB
        });
        Self::from_total_ram_mb(total_ram_mb)
    }

    /// Resolve the budget from a caller-supplied RAM reading.
    ///
    /// Why: the whole tier/budget table is testable without a host that has that
    /// much RAM, and a caller that already measured RAM does not measure twice.
    /// What: pure — no env reads, no syscalls, no logging.
    /// Test: `supported_target_resolves_to_medium_with_pinned_budget` and every
    /// other table test in `super::tests`.
    pub fn from_total_ram_mb(total_ram_mb: u64) -> Self {
        Self {
            total_ram_mb,
            tier: MemoryTier::from_total_ram_mb(total_ram_mb),
            memory_limit_mb: compute_memory_limit_mb(total_ram_mb),
            index_memory_limit_mb: compute_index_memory_limit_mb(total_ram_mb),
        }
    }

    /// Whether this machine is below the documented 16 GB minimum.
    ///
    /// Test: `degrade_budget_is_reduced_not_zero`.
    pub fn is_below_minimum(&self) -> bool {
        self.total_ram_mb < MINIMUM_SUPPORTED_RAM_MB
    }

    /// The one-line startup advisory a sub-minimum host earns, or `None`.
    ///
    /// Why: the degrade posture is only useful if the operator learns about it,
    /// and every daemon should say the same thing. This returns a STRING rather
    /// than logging or erroring so the caller decides the channel — and so the
    /// "below minimum does not abort startup" contract is visible in the
    /// signature. `trusty-search` returned an `Err` here before #6820 and the
    /// daemon exited.
    /// What: `Some(message)` naming the detected size, the 16 GB minimum, the
    /// 24 GB supported target, and the resolved tier; `None` at or above the
    /// minimum.
    /// Test: `advisory_present_below_minimum_absent_at_or_above`.
    pub fn minimum_advisory(&self) -> Option<String> {
        if !self.is_below_minimum() {
            return None;
        }
        Some(format!(
            "detected {} MB ({:.1} GB) of usable RAM, below the {} GB minimum — \
             running in the {} tier with reduced caps. The supported target is {} GB. \
             Indexing large codebases on this host will be slower and may evict \
             resident state aggressively.",
            self.total_ram_mb,
            self.total_ram_mb as f64 / 1024.0,
            MINIMUM_SUPPORTED_RAM_MB / 1024,
            self.tier,
            super::constants::SUPPORTED_TARGET_RAM_MB / 1024,
        ))
    }
}
