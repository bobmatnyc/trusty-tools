//! Opt-in automatic update CHECK that notifies — never auto-applies (#1316).
//!
//! Why: The #1316 added requirement asks for an opt-in periodic/background check
//! that *notifies* the operator that updates are available but **never** mutates
//! installed binaries without confirmation. This module is the notify-only half:
//! it is gated behind `[controller] auto_update_check` in the config and, when
//! enabled, surfaces a short "updates available — run `tctl upgrade`" notice. It
//! never calls any apply primitive; applying always goes through the explicit
//! `tctl upgrade` confirm-then-apply gate.
//!
//! What: [`auto_update_enabled`] reads the config flag (off by default — the
//! safe posture). [`notice_for`] formats the notification line from a candidate
//! count (a pure function). [`maybe_notify`] is the side-effecting entry point:
//! when enabled, it probes the stable set and, when updates exist, prints the
//! notice to stderr — and returns whether it notified.
//!
//! Test: `tests` covers the pure `notice_for` formatting and the disabled-by-
//! default `auto_update_enabled` path against a default config.

use super::runtime::block_on;
use super::stable_set::stable_set;
use super::up::config::{load_boot_config, BootConfig};
use super::update_engine::gather_candidates;

/// Whether the opt-in auto-update check is enabled in `config`.
///
/// Why: The default posture is OFF — tctl must not even *probe* crates.io in the
/// background unless the operator opted in via `[controller] auto_update_check`.
///
/// What: Returns `config.controller.auto_update_check`.
///
/// Test: `tests::disabled_by_default`.
pub fn auto_update_enabled(config: &BootConfig) -> bool {
    config.controller.auto_update_check
}

/// Format the notify-only notification line for `count` available updates.
///
/// Why: A pure formatter keeps the wording consistent and testable, and makes
/// explicit that the notice only *points at* the confirm-then-apply command —
/// it never implies anything was applied.
///
/// What: Returns `None` when `count == 0` (nothing to say); otherwise a line
/// like `"N update(s) available — run `tctl upgrade` to review and apply."`.
///
/// Test: `tests::notice_for_zero_is_none`, `tests::notice_for_some`.
pub fn notice_for(count: usize) -> Option<String> {
    if count == 0 {
        return None;
    }
    Some(format!(
        "{count} trusty update(s) available — run `trusty-installer upgrade` to review and apply (nothing is changed without your confirmation)."
    ))
}

/// Run the opt-in background update check, notifying when updates exist.
///
/// Why: The single side-effecting entry point a caller (e.g. `tctl up` or a
/// scheduled invocation) uses to surface available updates without ever applying
/// them.
///
/// What: Loads the controller config; if the check is disabled, returns `false`
/// immediately (no network). When enabled, probes the stable set via
/// `gather_candidates` (which is itself throttled to once/24h by
/// `trusty_common::update`), prints [`notice_for`] to stderr when there are
/// candidates, and returns whether it notified. Never mutates binaries.
///
/// Test: Side-effecting (network/config); the pure pieces are unit-tested.
pub fn maybe_notify() -> bool {
    let config = load_boot_config().unwrap_or_default();
    if !auto_update_enabled(&config) {
        return false;
    }
    let candidates = block_on(gather_candidates(&stable_set()));
    match notice_for(candidates.len()) {
        Some(line) => {
            eprintln!("{line}");
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The default posture must be OFF — no opt-in, no background probing.
    /// What: Asserts `auto_update_enabled` is false for a default config.
    /// Test: This is the test.
    #[test]
    fn disabled_by_default() {
        assert!(!auto_update_enabled(&BootConfig::default()));
    }

    /// Why: Zero updates must produce no notice (silence when current).
    /// What: Asserts `notice_for(0)` is `None`.
    /// Test: This is the test.
    #[test]
    fn notice_for_zero_is_none() {
        assert_eq!(notice_for(0), None);
    }

    /// Why: A non-zero count must point at the confirm-then-apply command and
    /// never claim anything was applied.
    /// What: Asserts the notice mentions the count and `tctl upgrade`.
    /// Test: This is the test.
    #[test]
    fn notice_for_some() {
        let n = notice_for(3).expect("some");
        assert!(n.contains('3'));
        assert!(n.contains("trusty-installer upgrade"));
        assert!(n.contains("confirmation"));
    }
}
