//! Parent-death watchdog for the `tagent --api` sidecar (#3734).
//!
//! Why: When the trusty-agents desktop GUI spawns `tagent --api` as a sidecar,
//! the sidecar must not outlive its parent. PR #3728 reaps it on the Tauri
//! quit event, but that path does not fire for every GUI death: an external
//! SIGTERM/SIGKILL, a crash, or macOS's synchronous Cmd+Q teardown can all
//! exit the GUI without the reap running, leaving `tagent --api` orphaned
//! (reparented to launchd, pid 1) and still holding its fixed port. A watchdog
//! INSIDE the sidecar closes the whole orphan class: whatever kills the parent,
//! the sidecar notices and self-exits, releasing the port.
//!
//! What: [`arm_parent_death_watchdog`] delegates to
//! `trusty_common::parent_death`, which owns the detection loop and its tests.
//! #7085 needed the same watchdog for test-spawned `trusty-memory` daemons, and
//! a second copy of a cross-crate capability is a defect, so the implementation
//! moved to `trusty-common` and this stayed as the sidecar's named entry point.
//!
//! Test: the detection loop and both death signals are covered by
//! trusty-common's `parent_death` module tests
//! (`cargo test -p trusty-common --features unconditional-only parent_death`).

/// Arm the parent-death watchdog: self-exit once `parent_pid` is gone.
///
/// Why: #3734 — no GUI failure mode (Cmd+Q, crash, external SIGTERM/SIGKILL)
/// may leave the `--api` sidecar orphaned on its fixed port. This is the
/// guaranteed backstop that runs regardless of whether the GUI's own Tauri-side
/// reap fires.
/// What: hands the GUI's pid to
/// [`trusty_common::parent_death::arm_for_named_parent`], the variant for a
/// parent named by flag rather than by environment — the GUI passes
/// `--parent-pid`, and that pid is not required to be this process's direct
/// spawner. Call once, only when running as a sidecar. A `parent_pid` of 0 or 1
/// is not watchable and arms nothing.
/// Test: covered by trusty-common's `parent_death` module tests
/// (`cargo test -p trusty-common --features unconditional-only parent_death`).
pub fn arm_parent_death_watchdog(parent_pid: u32) {
    trusty_common::parent_death::arm_for_named_parent(parent_pid, "tagent");
}
