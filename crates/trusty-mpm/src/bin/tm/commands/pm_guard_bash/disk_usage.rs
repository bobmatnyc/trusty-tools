//! The PreToolUse half of the `disk.max_usage_pct` worktree gate (#7497).
//!
//! Why: the daemon's provisioning path is not the only way a worktree appears —
//! a dispatched agent runs `git worktree add` through the `Bash` tool, and that
//! call never reaches `create_session_worktree`. Gating only the daemon would
//! leave the agent path free to fill the last of the disk. This rule is why an
//! `Agent(isolation: "worktree")` dispatch is covered too: the dispatch itself
//! creates nothing, the later `git worktree add` does.
//!
//! What: [`evaluate_worktree_add_disk_usage`] reuses
//! [`super::worktree_add_targets`] — the same `cd` / `git -C` / flag-skipping
//! resolution the temp-root rule uses — and asks
//! [`crate::core::disk_usage_guard`] whether the mount holding each target is at
//! or above the threshold. Only `worktree add` is examined;
//! `list`/`remove`/`prune` are never gated.
//!
//! Fails OPEN, like every other classifier in this module: a mount that cannot
//! be measured allows the command with a `warn!`. See
//! `disk_usage_guard::bash_refusal` for why this posture differs from the
//! provisioning path's.
//!
//! Test: `worktree_add_targets_resolves_cd_and_dash_c` and
//! `worktree_list_and_remove_produce_no_disk_targets` in
//! `pm_guard_bash/tests.rs` for the pure shape, and
//! `pm_guard_denies_worktree_add_over_the_disk_threshold` (plus its allow
//! sibling) in `tests/tm_hook_pm_guard.rs` for the end-to-end binary path.

use std::path::{Path, PathBuf};

use trusty_mpm::core::disk_usage_guard;

/// Deny `git worktree add` when its target's mount is at or above the
/// configured `disk.max_usage_pct`.
///
/// Why: see the module doc — this is the agent-facing half of the gate, and it
/// must be called from `pm_guard()` BEFORE the Guard 4 subagent exemption for
/// the same structural reason the temp-root rule is.
/// What: resolves the command's `worktree add` targets, then returns the first
/// refusal any of them earns. `None` when the command creates no worktree, when
/// the gate is off, when nothing could be measured, or when every target's
/// mount is below the threshold.
/// Test: `worktree_list_and_remove_produce_no_disk_targets`, and the end-to-end
/// hook cases named in the module doc.
pub(crate) fn evaluate_worktree_add_disk_usage(command: &str, cwd: &Path) -> Option<String> {
    refusal_for_targets(&super::worktree_add_targets(command, cwd), |path| {
        disk_usage_guard::bash_refusal(path)
    })
}

/// The first refusal `targets` earns from `refuse`.
///
/// Why: the measurement is injected so the rule's shape — "no targets means no
/// work, and the first refusal wins" — is testable without a real filesystem.
/// What: `None` for an empty target list (the fast path every non-worktree Bash
/// call takes, and the reason no disk is sampled for them).
/// Test: `the_first_refusing_target_wins`, `no_targets_means_no_measurement`.
pub(crate) fn refusal_for_targets(
    targets: &[PathBuf],
    refuse: impl Fn(&Path) -> Option<String>,
) -> Option<String> {
    targets.iter().find_map(|target| refuse(target))
}
