//! [`MemoryTier`] — the machine-size band every trusty-* daemon reads its
//! defaults from.
//!
//! Why: tier selection drives the default caps for every memory-bounded
//! structure in the suite. `trusty-search` owned this enum until #6820, and
//! `trusty-memory` had no tier at all, so the two daemons could not agree on
//! what host they were running on.
//! What: [`MemoryTier`] plus [`MemoryTier::from_total_ram_mb`]. The 16/32/64 GB
//! boundaries are moved unchanged from `trusty-search`; the [`MemoryTier::Degraded`]
//! band below 16 GB is new — it replaces `trusty-search`'s hard exit with a
//! documented reduced-cap posture.
//! Test: `super::tests` — `tier_boundaries`, `degrade_tier_below_minimum`,
//! `medium_tier_at_the_minimum`, `batch_size_hard_cap_table`.

use std::fmt;

use super::constants::{LARGE_TIER_MAX_GB, MEDIUM_TIER_MAX_GB, MINIMUM_SUPPORTED_RAM_MB};

/// Machine-size band selected from total system RAM. The tier picks default
/// caps; env vars override individual fields.
///
/// Why: a daemon needs one coarse answer to "how big is this machine" so its
/// dozen caps do not each re-derive it and drift. The bands are deliberately
/// wide: within a band the caps still scale continuously off actual RAM
/// (see [`super::compute`]), so 24 GB and 31 GB share the [`Self::Medium`] band
/// while getting different budgets.
/// What: four variants, boundaries in GB — `Degraded` (< 16), `Medium`
/// (16–31), `Large` (32–63), `XLarge` (>= 64). Deliberately NOT
/// `#[non_exhaustive]`: every consumer matching on it must be forced by the
/// compiler to say what it does in the degrade posture, which a wildcard arm
/// would hide.
/// Test: `tier_boundaries` pins all four bands and their edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryTier {
    /// Below 16 GB total RAM — under the documented minimum (#6820).
    ///
    /// Not an error: the daemon runs with reduced caps and warns once at
    /// startup. Before #6820 `trusty-search` exited instead, so a 12 GB host
    /// got no search at all rather than a smaller one.
    Degraded,
    /// 16–31 GB total RAM. The minimum supported configuration (16 GB) and the
    /// primary supported target (24 GB) both live here.
    Medium,
    /// 32–63 GB total RAM.
    Large,
    /// >= 64 GB total RAM.
    XLarge,
}

impl fmt::Display for MemoryTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MemoryTier::Degraded => "Degraded",
            MemoryTier::Medium => "Medium",
            MemoryTier::Large => "Large",
            MemoryTier::XLarge => "XLarge",
        })
    }
}

impl MemoryTier {
    /// Pick a tier from total RAM in megabytes.
    ///
    /// Why: one boundary table, read by both daemons. 24 GB needs no boundary
    /// of its own — it resolves to [`Self::Medium`] and the proportional
    /// formulas scale across the band continuously.
    /// What: GB boundaries — `< 16` Degraded, `16–31` Medium, `32–63` Large,
    /// `>= 64` XLarge. Integer division by 1024, so a 16 383 MB host (just
    /// under 16 GB) is Degraded and 16 384 MB is Medium.
    /// Test: `tier_boundaries`, `degrade_tier_below_minimum`,
    /// `medium_tier_at_the_minimum`.
    pub fn from_total_ram_mb(total_ram_mb: u64) -> Self {
        if total_ram_mb < MINIMUM_SUPPORTED_RAM_MB {
            return MemoryTier::Degraded;
        }
        let gb = total_ram_mb / 1024;
        if gb <= MEDIUM_TIER_MAX_GB {
            MemoryTier::Medium
        } else if gb <= LARGE_TIER_MAX_GB {
            MemoryTier::Large
        } else {
            MemoryTier::XLarge
        }
    }

    /// Whether this tier is below the documented 16 GB minimum.
    ///
    /// Why: callers that log the degrade advisory or shrink a cap want to ask
    /// the question without matching the variant and going stale when a band is
    /// added below it.
    /// Test: `degrade_tier_below_minimum`.
    pub fn is_degraded(self) -> bool {
        matches!(self, MemoryTier::Degraded)
    }

    /// Tier-specific hard cap on `max_batch_size`. Conservative bound that
    /// protects against runaway env-var overrides on memory-constrained hosts
    /// (issue #89).
    ///
    /// Why: `TRUSTY_MAX_BATCH_SIZE` is a runtime knob. An operator who sets it
    /// to 2048 on a 16 GB box (the Medium tier) will trigger the same ORT
    /// transient-arena spike that auto-tuning was designed to prevent. The
    /// auto-derived defaults already sit well below these caps, so this hard
    /// ceiling only kicks in for explicit overrides — exactly the case where
    /// additional safety is warranted.
    /// What: Degraded=64, Medium=128, Large=256, XLarge=512. The Medium/Large/
    /// XLarge row is unchanged from issue #19 (raised there from
    /// {16, 32, 64} once the CPU path disabled the ORT arena allocator, cutting
    /// per-slot transient cost to ~32 MB); Degraded halves Medium's because a
    /// sub-16 GB host has proportionally less headroom for the same spike.
    /// Test: `batch_size_hard_cap_table` covers the table.
    pub fn batch_size_hard_cap(self) -> usize {
        match self {
            MemoryTier::Degraded => 64,
            MemoryTier::Medium => 128,
            MemoryTier::Large => 256,
            MemoryTier::XLarge => 512,
        }
    }
}
