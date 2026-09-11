//! The `disk.max_usage_pct` gate every worktree-creation path consults (#7497).
//!
//! Why: a worktree is the largest thing tm creates on demand — a checkout plus
//! whatever the session builds in it — and nothing measured the volume first.
//! The owner's request is that a nearly-full disk REFUSE a new worktree rather
//! than fill the last of it, and that the refusal say which mount, how full,
//! against what threshold, and which key changes it.
//!
//! What: one threshold (`disk.max_usage_pct`, default
//! [`DEFAULT_MAX_USAGE_PCT`]), one measurement
//! ([`trusty_common::host_metrics::mount_for_path`], the mount holding the
//! path — not the cross-mount aggregate, which stays healthy while one volume
//! fills), and two decisions over them:
//!
//! | Caller | Cannot measure | Why |
//! |---|---|---|
//! | [`check_worktree_creation`] — daemon/CLI provisioning | REFUSE | ADR-0037: an explicit worktree request that cannot be honoured is a failure, never a quieter placement |
//! | [`bash_refusal`] — the PreToolUse Bash guard | ALLOW, with a `warn!` | every classifier in that guard family fails open; a guard that denied what it could not read would block ordinary work on any host it does not understand |
//!
//! Both read the SAME threshold and the SAME measurement, so the two surfaces
//! cannot drift into disagreeing about whether the disk is full.
//!
//! Under a `cargo test` harness an ABSENT key disables the gate entirely
//! ([`active_threshold_from`]) — a suite that creates worktrees as fixtures must
//! not go red because of the developer's free space, which is a property of the
//! machine and not of the change. An EXPLICIT key still applies, in a test
//! process exactly as in production, which is how the gate's own end-to-end
//! coverage drives the real path.
//!
//! Test: `disk_usage_guard_tests.rs`, plus the end-to-end
//! `tests/worktree_disk_usage_gate.rs` and the `pm_guard_*disk*` cases in
//! `tests/tm_hook_pm_guard.rs`.

use std::path::Path;

use tracing::warn;

use crate::core::trusty_tools_config::{CRATE_NAME, TrustyToolsConfig};

/// The threshold applied when `disk.max_usage_pct` is absent or unusable.
///
/// Owner request (#7497): "prevent new worktrees from being created if disk
/// usage exceeds a configurable size (default: 90%)".
pub const DEFAULT_MAX_USAGE_PCT: u8 = 90;

/// The config key, spelled once so every message names it identically.
pub const MAX_USAGE_PCT_KEY: &str = "disk.max_usage_pct";

/// Shown when the config path cannot be resolved (no home directory).
const CONFIG_PATH_FALLBACK: &str = "~/.trusty-tools/trusty-mpm/config.yaml";

/// One mount's measured usage, as the gate saw it.
///
/// Why: the decision functions take an ALREADY-MEASURED value so every
/// threshold branch is testable with synthetic percentages — no test asserts
/// anything about the runner's real disk.
/// What: the mount point the OS reported and its used percentage, 0..=100.
/// Test: `over_threshold_refuses`, `at_threshold_refuses`, `under_threshold_allows`.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasuredMount {
    /// Mount point path (e.g. `/`, `/System/Volumes/Data`).
    pub mount_point: String,
    /// Used capacity of that mount as a percentage, 0..=100.
    pub usage_pct: f32,
}

/// Why a worktree was refused (#7497).
///
/// Why: the caller formats nothing — the refusal an operator reads has to name
/// the mount, the measurement, the threshold and the key that changes it, and a
/// per-call-site `format!` is how one of those goes missing.
/// What: [`Self::OverThreshold`] is the measured refusal; [`Self::Unmeasurable`]
/// is the fail-closed refusal the provisioning path raises when the mount could
/// not be read at all.
/// Test: `refusal_names_the_mount_the_threshold_and_the_key`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum WorktreeDiskError {
    /// The mount holding the target is at or above the configured threshold.
    #[error(
        "refusing to create a worktree: {mount_point} is at {usage_pct:.1}% disk usage, at or \
         above the configured disk.max_usage_pct threshold of {threshold_pct}% — free space or \
         raise disk.max_usage_pct in {config_path}"
    )]
    OverThreshold {
        /// Mount point the measurement came from.
        mount_point: String,
        /// Measured used percentage of that mount.
        usage_pct: f32,
        /// Threshold in force at the time of the decision.
        threshold_pct: u8,
        /// Config file the operator edits to change the threshold.
        config_path: String,
    },

    /// The mount holding the target could not be measured.
    #[error(
        "refusing to create a worktree: disk usage for {path} could not be measured, and the \
         disk.max_usage_pct threshold of {threshold_pct}% cannot be proved satisfied — set \
         disk.max_usage_pct in {config_path}, or free space and retry"
    )]
    Unmeasurable {
        /// The path whose mount could not be resolved.
        path: String,
        /// Threshold that would have applied.
        threshold_pct: u8,
        /// Config file the operator edits to change the threshold.
        config_path: String,
    },
}

/// A threshold resolved from config, with the reason any value was rejected.
///
/// Why: `disk.max_usage_pct: 0` and `: 101` are not thresholds — one refuses
/// every worktree, the other can never fire. Silently applying either turns an
/// operator typo into a broken machine or a disabled gate, so both are rejected
/// and reported, the way an unreadable keep-list is (#6927).
/// What: `threshold_pct` is always usable; `rejected` names the discarded value
/// when one was discarded.
/// Test: `zero_is_rejected_and_falls_back_to_the_default`,
/// `above_one_hundred_is_rejected_and_falls_back_to_the_default`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedThreshold {
    /// The percentage actually in force.
    pub threshold_pct: u8,
    /// `Some(reason)` when a configured value was discarded.
    pub rejected: Option<String>,
}

/// Resolve `disk.max_usage_pct` into a usable threshold.
///
/// Why: the one place "absent means 90" and "out of range means 90, loudly"
/// are decided, so the gate, `tm doctor`, and any future reader agree.
/// What: `Some(1..=100)` is taken as written; `None` is
/// [`DEFAULT_MAX_USAGE_PCT`]; anything else is [`DEFAULT_MAX_USAGE_PCT`] plus a
/// rejection reason. A rejected value NEVER becomes a permissive threshold.
/// Test: `absent_resolves_to_the_default`,
/// `zero_is_rejected_and_falls_back_to_the_default`,
/// `above_one_hundred_is_rejected_and_falls_back_to_the_default`,
/// `an_in_range_value_is_taken_as_written`.
#[must_use]
pub fn resolve_max_usage_pct(configured: Option<u8>) -> ResolvedThreshold {
    match configured {
        None => ResolvedThreshold {
            threshold_pct: DEFAULT_MAX_USAGE_PCT,
            rejected: None,
        },
        Some(pct) if (1..=100).contains(&pct) => ResolvedThreshold {
            threshold_pct: pct,
            rejected: None,
        },
        Some(pct) => ResolvedThreshold {
            threshold_pct: DEFAULT_MAX_USAGE_PCT,
            rejected: Some(format!(
                "{MAX_USAGE_PCT_KEY}: {pct} is outside 1..=100 and was ignored; \
                 using the default {DEFAULT_MAX_USAGE_PCT}%"
            )),
        },
    }
}

/// The refusal a measured mount earns against `threshold_pct`, if any.
///
/// Why: the FAIL-OPEN half — an unmeasured mount yields no refusal, which is
/// what the Bash guard needs and what the provisioning path deliberately does
/// not use.
/// What: `Some` when `usage_pct >= threshold_pct` (at the threshold refuses, per
/// #7497), `None` when below it or when `measured` is `None`.
/// Test: `over_threshold_refuses`, `at_threshold_refuses`,
/// `under_threshold_allows`, `an_unmeasured_mount_yields_no_refusal`.
#[must_use]
pub fn refusal_if_over(
    measured: Option<&MeasuredMount>,
    threshold_pct: u8,
) -> Option<WorktreeDiskError> {
    let measured = measured?;
    if measured.usage_pct < f32::from(threshold_pct) {
        return None;
    }
    Some(WorktreeDiskError::OverThreshold {
        mount_point: measured.mount_point.clone(),
        usage_pct: measured.usage_pct,
        threshold_pct,
        config_path: config_path_display(),
    })
}

/// The FAIL-CLOSED decision: refuse when over threshold OR unmeasurable.
///
/// Why: ADR-0037 — a provisioning path that explicitly asked for a worktree
/// must not proceed on a measurement it could not take. "I could not check" is
/// not "it is fine".
/// What: `measured == None` is [`WorktreeDiskError::Unmeasurable`]; otherwise
/// [`refusal_if_over`] decides.
/// Test: `an_unmeasurable_mount_fails_closed`, `over_threshold_refuses`.
pub fn check_usage(
    measured: Option<&MeasuredMount>,
    threshold_pct: u8,
    path: &Path,
) -> Result<(), WorktreeDiskError> {
    let Some(measured) = measured else {
        return Err(WorktreeDiskError::Unmeasurable {
            path: path.display().to_string(),
            threshold_pct,
            config_path: config_path_display(),
        });
    };
    match refusal_if_over(Some(measured), threshold_pct) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// Measure the mount that holds `path`.
///
/// Why: the ONE sysinfo-touching line in this module, so every decision above
/// it stays pure.
/// What: delegates to [`trusty_common::host_metrics::mount_for_path`], which
/// resolves the nearest existing ancestor (the worktree does not exist yet) and
/// picks its mount by device id. `None` when the OS lists no matching mount.
/// Test: covered live by `tests/worktree_disk_usage_gate.rs`; the selection
/// rule itself is tested in `trusty_common::host_metrics`.
#[must_use]
pub fn measure(path: &Path) -> Option<MeasuredMount> {
    trusty_common::host_metrics::mount_for_path(path).map(|m| MeasuredMount {
        mount_point: m.mount_point,
        usage_pct: m.usage_pct,
    })
}

/// Gate the creation of a new worktree at `path` (fail closed).
///
/// Why: the single entry point every tm-owned provisioning path calls, so
/// `tm session new`, `tm launch`, the MCP `session_new` and the console all
/// refuse identically.
/// What: resolves the threshold ([`active_threshold`] — `None` disables the
/// gate), measures `path`'s mount, and applies [`check_usage`].
/// Test: `tests/worktree_disk_usage_gate.rs`.
pub fn check_worktree_creation(path: &Path) -> Result<(), WorktreeDiskError> {
    let Some(threshold_pct) = active_threshold() else {
        return Ok(());
    };
    check_usage(measure(path).as_ref(), threshold_pct, path)
}

/// The refusal reason a `git worktree add` targeting `path` earns (fail open).
///
/// Why: the PreToolUse Bash guard's half. It denies a command outright, so an
/// unmeasurable mount must ALLOW — every other classifier in that guard family
/// fails open, and a guard that denied what it could not read would block
/// ordinary work on any host it does not understand. The `warn!` is what keeps
/// that silence from being total.
/// What: `None` when the gate is off, the mount is unmeasurable, or usage is
/// below the threshold; `Some(message)` otherwise.
/// Test: `an_unmeasured_mount_yields_no_refusal`, and the `pm_guard_*disk*`
/// cases in `tests/tm_hook_pm_guard.rs`.
#[must_use]
pub fn bash_refusal(path: &Path) -> Option<String> {
    let threshold_pct = active_threshold()?;
    let measured = measure(path);
    if measured.is_none() {
        // #7497: fail OPEN here — see this function's doc for why the Bash
        // guard's posture differs from the provisioning path's.
        warn!(
            path = %path.display(),
            threshold_pct,
            "disk-usage gate: could not measure the mount holding this worktree target — allowing"
        );
        return None;
    }
    refusal_if_over(measured.as_ref(), threshold_pct).map(|e| e.to_string())
}

/// The threshold in force for this process, or `None` when the gate is off.
///
/// Why: named so the test-harness rule (see the module doc) is stated once
/// rather than at each gate.
/// What: reads `disk.max_usage_pct` from the operator's config under
/// `dirs::home_dir()` and applies [`active_threshold_from`].
/// Test: `active_threshold_from` carries the tested decision table.
#[must_use]
pub fn active_threshold() -> Option<u8> {
    let Some(home) = dirs::home_dir() else {
        // No home is "no config", which is the documented absent case.
        return active_threshold_from(None, trusty_common::running_under_test_harness());
    };
    active_threshold_at(&home, trusty_common::running_under_test_harness())
}

/// [`active_threshold`] against an explicit home directory (hermetic).
///
/// Why: the production reader resolves `dirs::home_dir()`, which a test must
/// not depend on. Pointing `base` at a `tempfile::TempDir` is what lets the
/// config→threshold path be asserted without touching the operator's file.
/// What: [`read_configured`] under `base`, then [`active_threshold_from`].
/// Test: `active_threshold_at_reads_the_operators_value`,
/// `an_unreadable_config_leaves_the_default_in_force`.
#[must_use]
pub fn active_threshold_at(base: &Path, under_test_harness: bool) -> Option<u8> {
    active_threshold_from(read_configured(base), under_test_harness)
}

/// The decision table behind [`active_threshold`], with both inputs injected.
///
/// Why: env-free and filesystem-free, so the test-harness rule is asserted
/// without a test process having to lie about what it is.
/// What: an EXPLICIT value always applies (validated through
/// [`resolve_max_usage_pct`], so a rejected one becomes the default rather than
/// a hole). An ABSENT value is the default in production and NO GATE under a
/// cargo test harness — a fixture worktree must not depend on the developer's
/// free space.
/// Test: `an_explicit_threshold_applies_under_a_test_harness`,
/// `an_absent_threshold_disables_the_gate_under_a_test_harness`,
/// `an_absent_threshold_is_the_default_in_production`.
#[must_use]
pub fn active_threshold_from(configured: Option<u8>, under_test_harness: bool) -> Option<u8> {
    if configured.is_none() && under_test_harness {
        return None;
    }
    let resolved = resolve_max_usage_pct(configured);
    if let Some(reason) = &resolved.rejected {
        warn!("disk-usage gate: {reason}");
    }
    Some(resolved.threshold_pct)
}

/// Read `disk.max_usage_pct` from the config under `base`.
///
/// Why: the gate must distinguish "the operator set a threshold" from "no
/// section" — the test-harness rule above turns on exactly that difference.
/// What: `<base>/.trusty-tools/trusty-mpm/config.yaml`. An absent file is
/// `None`; an unreadable one is `None` plus a `warn!`, which leaves the
/// PRODUCTION default in force rather than disabling a protective gate.
/// Test: `active_threshold_at_reads_the_operators_value`,
/// `an_unreadable_config_leaves_the_default_in_force`.
pub(crate) fn read_configured(base: &Path) -> Option<u8> {
    let path = trusty_common::crate_config::crate_config_path_at(base, CRATE_NAME);
    match trusty_common::crate_config::load_at::<TrustyToolsConfig>(&path) {
        Ok(Some(config)) => config.disk.as_ref().and_then(|d| d.max_usage_pct),
        Ok(None) => None,
        Err(e) => {
            warn!(path = %path.display(), "disk-usage gate: config unreadable ({e}) — applying the default threshold");
            None
        }
    }
}

/// The config file path a refusal names.
fn config_path_display() -> String {
    trusty_common::crate_config::crate_config_path(CRATE_NAME)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| CONFIG_PATH_FALLBACK.to_string())
}

#[cfg(test)]
#[path = "disk_usage_guard_tests.rs"]
mod disk_usage_guard_tests;
