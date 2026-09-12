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
//! | [`check_measured`] — daemon/CLI provisioning | REFUSE | ADR-0037: an explicit worktree request that cannot be honoured is a failure, never a quieter placement |
//! | [`bash_refusal`] — the PreToolUse Bash guard | ALLOW, with a `warn!` | every classifier in that guard family fails open; a guard that denied what it could not read would block ordinary work on any host it does not understand |
//!
//! Both read the SAME threshold and the SAME measurement, so the two surfaces
//! cannot drift into disagreeing about whether the disk is full.
//!
//! The gate has NO ambient off switch (#7497 review). An earlier revision
//! disabled the default threshold under `running_under_test_harness()`, which
//! honours an inherited `TRUSTY_TEST_HARNESS=1` — one exported variable would
//! have disabled the shipped 90% gate in an installed `tm` with nothing in the
//! log. Determinism for tests comes from the other direction instead: a test
//! writes an EXPLICIT `disk.max_usage_pct` into its own config home, or calls
//! the ungated `create_session_worktree_unchecked`.
//!
//! Test: `disk_usage_guard_tests.rs`, plus the end-to-end
//! `tests/worktree_disk_usage_gate.rs` and the `pm_guard_*disk*` cases in
//! `tests/tm_hook_pm_guard.rs`.
//!
//! [`DEFAULT_MAX_USAGE_PCT`]: crate::core::disk_usage_guard::DEFAULT_MAX_USAGE_PCT
//! [`check_measured`]: crate::core::disk_usage_guard::check_measured
//! [`bash_refusal`]: crate::core::disk_usage_guard::bash_refusal

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

/// Render a usage percentage, TRUNCATED rather than rounded.
///
/// Why: rounding prints `90.0%` for 89.96% — a number that reads as "at the
/// threshold" beside an `Ok` status, and as a contradiction beside a refusal
/// that did not happen. Truncating guarantees the printed figure is never
/// above the measured one, so `90.0%` can only appear when the gate really is
/// at or above 90 (#7497 review, LOW).
/// What: one decimal place, toward zero.
/// Test: `a_percent_is_truncated_not_rounded`.
#[must_use]
pub fn fmt_pct(usage_pct: &f32) -> String {
    format!("{:.1}%", (usage_pct * 10.0).floor() / 10.0)
}

/// Why a worktree was refused (#7497).
///
/// Why: the caller formats nothing — the refusal an operator reads has to name
/// the mount, the measurement, the threshold and the key that changes it, and a
/// per-call-site `format!` is how one of those goes missing.
/// What: [`Self::OverThreshold`] is the measured refusal; [`Self::Unmeasurable`]
/// is the fail-closed refusal the provisioning path raises when the mount could
/// not be read at all. `rejected_note` is empty unless a configured value was
/// discarded, in which case the message must not call the default "configured".
/// Test: `refusal_names_the_mount_the_threshold_and_the_key`,
/// `a_rejected_value_is_named_in_the_refusal`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum WorktreeDiskError {
    /// The mount holding the target is at or above the configured threshold.
    #[error(
        "refusing to create a worktree: {mount_point} is at {} disk usage, at or above the \
         disk.max_usage_pct threshold of {threshold_pct}%{rejected_note} — free space or raise \
         disk.max_usage_pct in {config_path}",
        fmt_pct(usage_pct)
    )]
    OverThreshold {
        /// Mount point the measurement came from.
        mount_point: String,
        /// Measured used percentage of that mount.
        usage_pct: f32,
        /// Threshold in force at the time of the decision.
        threshold_pct: u8,
        /// Empty, or the parenthetical naming a discarded configured value.
        rejected_note: String,
        /// Config file the operator edits to change the threshold.
        config_path: String,
    },

    /// The mount holding the target could not be measured.
    #[error(
        "refusing to create a worktree: disk usage for {path} could not be measured, and the \
         disk.max_usage_pct threshold of {threshold_pct}%{rejected_note} cannot be proved \
         satisfied — set disk.max_usage_pct in {config_path}, or free space and retry"
    )]
    Unmeasurable {
        /// The path whose mount could not be resolved.
        path: String,
        /// Threshold that would have applied.
        threshold_pct: u8,
        /// Empty, or the parenthetical naming a discarded configured value.
        rejected_note: String,
        /// Config file the operator edits to change the threshold.
        config_path: String,
    },
}

/// A threshold resolved from config, with the reason any value was rejected.
///
/// Why: `disk.max_usage_pct: 0` and `: 101` are not thresholds — one refuses
/// every worktree, the other can never fire. Silently applying either turns an
/// operator typo into a broken machine or a disabled gate, so both are rejected
/// and reported, the way an unreadable keep-list is (#6927). The rejection
/// travels WITH the threshold rather than only into a `warn!`, so `tm doctor`
/// and the refusal itself can both say the default is in force because a
/// configured value was discarded (#7497 review).
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

impl ResolvedThreshold {
    /// The parenthetical a message appends after the threshold, or `""`.
    ///
    /// Why: a refusal that calls 90 "the configured threshold" while the
    /// operator wrote 150 sends them looking for a 90 that is not in the file.
    /// What: `" (the configured disk.max_usage_pct 150 is outside 1..=100 and
    /// was ignored)"`, or empty when nothing was discarded.
    /// Test: `a_rejected_value_is_named_in_the_refusal`.
    #[must_use]
    pub fn rejected_note(&self) -> String {
        match &self.rejected {
            Some(reason) => format!(" ({reason})"),
            None => String::new(),
        }
    }
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
                "the configured {MAX_USAGE_PCT_KEY} {pct} is outside 1..=100 and was ignored; \
                 the default {DEFAULT_MAX_USAGE_PCT}% applies"
            )),
        },
    }
}

/// The refusal a measured mount earns against `threshold`, if any.
///
/// Why: the FAIL-OPEN half — an unmeasured mount yields no refusal, which is
/// what the Bash guard needs and what the provisioning path deliberately does
/// not use.
/// What: `Some` when `usage_pct >= threshold.threshold_pct` (at the threshold
/// refuses, per #7497), `None` when below it or when `measured` is `None`.
/// Test: `over_threshold_refuses`, `at_threshold_refuses`,
/// `under_threshold_allows`, `an_unmeasured_mount_yields_no_refusal`.
#[must_use]
pub fn refusal_if_over(
    measured: Option<&MeasuredMount>,
    threshold: &ResolvedThreshold,
) -> Option<WorktreeDiskError> {
    let measured = measured?;
    if measured.usage_pct < f32::from(threshold.threshold_pct) {
        return None;
    }
    Some(WorktreeDiskError::OverThreshold {
        mount_point: measured.mount_point.clone(),
        usage_pct: measured.usage_pct,
        threshold_pct: threshold.threshold_pct,
        rejected_note: threshold.rejected_note(),
        config_path: config_path_display(),
    })
}

/// The FAIL-CLOSED decision: refuse when over threshold OR unmeasurable.
///
/// Why: ADR-0037 — a provisioning path that explicitly asked for a worktree
/// must not proceed on a measurement it could not take. "I could not check" is
/// not "it is fine". This arm is reachable because
/// [`trusty_common::host_metrics::mount_for_path`] returns `None` for a
/// filesystem the OS does not enumerate rather than guessing `/`.
/// What: `measured == None` is [`WorktreeDiskError::Unmeasurable`]; otherwise
/// [`refusal_if_over`] decides.
/// Test: `an_unmeasurable_mount_fails_closed`, `over_threshold_refuses`.
pub fn check_usage(
    measured: Option<&MeasuredMount>,
    threshold: &ResolvedThreshold,
    path: &Path,
) -> Result<(), WorktreeDiskError> {
    let Some(measured) = measured else {
        return Err(WorktreeDiskError::Unmeasurable {
            path: path.display().to_string(),
            threshold_pct: threshold.threshold_pct,
            rejected_note: threshold.rejected_note(),
            config_path: config_path_display(),
        });
    };
    match refusal_if_over(Some(measured), threshold) {
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
/// picks its mount by device id. `None` when the OS lists no mount on that
/// device — an unmeasurable result, never a substituted one.
/// Test: covered live by `tests/worktree_disk_usage_gate.rs`; the selection
/// rule itself is tested in `trusty_common::host_metrics`.
#[must_use]
pub fn measure(path: &Path) -> Option<MeasuredMount> {
    trusty_common::host_metrics::mount_for_path(path).map(|m| MeasuredMount {
        mount_point: m.mount_point,
        usage_pct: m.usage_pct,
    })
}

/// How a provisioning path obtains the measurement its disk gate applies.
///
/// Why (#7603): [`measure`] reads the REAL mount, and [`active_threshold`] reads
/// the operator's REAL config under ambient `$HOME` — so an in-process test of
/// any entry point above the gate decides its verdict from the developer's own
/// machine. The managed-workspace suite passed 12764/0 and went red forty
/// minutes later on identical code, once the host crossed the 90% the operator's
/// `~/.trusty-tools/trusty-mpm/config.yaml` names. `$HOME` cannot be redirected
/// out of that read from a `tm`-bin test — `env_isolation_tests.rs` bans writing
/// it — so determinism has to arrive as an argument. This is the same seam
/// [`check_measured`] and `create_session_worktree_measured` give the layer
/// below (#7497), lifted to the entry points a test actually calls.
/// What: [`Self::MeasureTarget`] is production — measure the mount the worktree
/// would land on. [`Self::Pinned`] applies an already-taken measurement instead.
/// It is NOT an off switch: the threshold still comes from the operator's
/// config, a pinned over-threshold value refuses exactly as a real one does, and
/// `Pinned(None)` is still the fail-closed unmeasurable case.
/// Test: `a_pinned_gate_measures_nothing`,
/// `the_default_gate_measures_the_real_mount`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DiskGate {
    /// Measure the mount the worktree would land on (production).
    #[default]
    MeasureTarget,
    /// Apply this already-taken measurement instead of measuring.
    Pinned(Option<MeasuredMount>),
}

impl DiskGate {
    /// The measurement this gate applies to a worktree landing at `path`.
    ///
    /// Why: one place decides "measure or take what I was given", so no caller
    /// can half-apply the injection.
    /// What: [`measure`] for [`Self::MeasureTarget`]; the pinned value otherwise.
    /// Test: `a_pinned_gate_measures_nothing`,
    /// `the_default_gate_measures_the_real_mount`.
    #[must_use]
    pub fn measurement_for(&self, path: &Path) -> Option<MeasuredMount> {
        match self {
            Self::MeasureTarget => measure(path),
            Self::Pinned(measured) => measured.clone(),
        }
    }
}

/// Gate the creation of a new worktree at `path` against an ALREADY-TAKEN
/// measurement (fail closed).
///
/// Why: the seam the end-to-end tests need. Threading the measurement through
/// is what lets `tests/worktree_disk_usage_gate.rs` pin a synthetic percentage
/// and assert the real entry point's behaviour deterministically, instead of
/// passing silently on a runner whose disk sits outside the expressible
/// range (#7497 review, MEDIUM 2).
/// What: resolves the operator's threshold and applies [`check_usage`].
/// Test: `tests/worktree_disk_usage_gate.rs`.
pub fn check_measured(
    path: &Path,
    measured: Option<&MeasuredMount>,
) -> Result<(), WorktreeDiskError> {
    check_usage(measured, &active_threshold(), path)
}

/// Gate the creation of a new worktree at `path` (fail closed).
///
/// Why: the single entry point every tm-owned provisioning path calls, so
/// `tm session new`, `tm launch`, the MCP `session_new` and the console all
/// refuse identically.
/// What: measures `path`'s mount and applies [`check_measured`].
/// Test: `tests/worktree_disk_usage_gate.rs`.
pub fn check_worktree_creation(path: &Path) -> Result<(), WorktreeDiskError> {
    check_measured(path, measure(path).as_ref())
}

/// The refusal reason a `git worktree add` targeting `path` earns (fail open).
///
/// Why: the PreToolUse Bash guard's half. It denies a command outright, so an
/// unmeasurable mount must ALLOW — every other classifier in that guard family
/// fails open, and a guard that denied what it could not read would block
/// ordinary work on any host it does not understand. The `warn!` is what keeps
/// that silence from being total.
/// What: `None` when the mount is unmeasurable or usage is below the threshold;
/// `Some(message)` otherwise.
/// Test: `an_unmeasured_mount_yields_no_refusal`, and the `pm_guard_*disk*`
/// cases in `tests/tm_hook_pm_guard.rs`.
#[must_use]
pub fn bash_refusal(path: &Path) -> Option<String> {
    let threshold = active_threshold();
    let measured = measure(path);
    if measured.is_none() {
        // #7497: fail OPEN here — see this function's doc for why the Bash
        // guard's posture differs from the provisioning path's.
        warn!(
            path = %path.display(),
            threshold_pct = threshold.threshold_pct,
            "disk-usage gate: could not measure the mount holding this worktree target — allowing"
        );
        return None;
    }
    refusal_if_over(measured.as_ref(), &threshold).map(|e| e.to_string())
}

/// The threshold in force for this process.
///
/// Why: named so the resolution is stated once rather than at each gate. There
/// is deliberately no "gate off" answer — see the module doc.
/// What: reads `disk.max_usage_pct` from the operator's config under
/// `dirs::home_dir()` and resolves it through [`resolve_max_usage_pct`]. An
/// unknown home is the documented absent case: the default applies.
/// Test: `active_threshold_at_reads_the_operators_value`,
/// `an_unreadable_config_leaves_the_default_in_force`.
#[must_use]
pub fn active_threshold() -> ResolvedThreshold {
    match dirs::home_dir() {
        Some(home) => active_threshold_at(&home),
        None => resolve_max_usage_pct(None),
    }
}

/// [`active_threshold`] against an explicit home directory (hermetic).
///
/// Why: the production reader resolves `dirs::home_dir()`, which a test must
/// not depend on. Pointing `base` at a `tempfile::TempDir` is what lets the
/// config→threshold path be asserted without touching the operator's file.
/// What: [`read_configured`] under `base`, then [`resolve_max_usage_pct`]. A
/// discarded value is `warn!`-logged here, once, in addition to travelling with
/// the returned threshold.
/// Test: `active_threshold_at_reads_the_operators_value`,
/// `an_unreadable_config_leaves_the_default_in_force`.
#[must_use]
pub fn active_threshold_at(base: &Path) -> ResolvedThreshold {
    let resolved = resolve_max_usage_pct(read_configured(base));
    if let Some(reason) = &resolved.rejected {
        warn!("disk-usage gate: {reason}");
    }
    resolved
}

/// Read `disk.max_usage_pct` from the config under `base`.
///
/// Why: the gate needs the operator's value, and must not let an unrelated typo
/// elsewhere in the file disable a PROTECTIVE gate (#6927's lesson).
/// What: `<base>/.trusty-tools/trusty-mpm/config.yaml`. An absent file is
/// `None`; an unreadable one is `None` plus a `warn!` — and `None` resolves to
/// the default 90, so an unreadable config gates at the default rather than not
/// at all.
/// Test: `an_absent_section_reads_as_no_configured_value`,
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
