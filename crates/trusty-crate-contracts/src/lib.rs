//! Where a test that spans two production crates lives (#8341).
//!
//! Why: the point of separate crates is an efficient compilation process
//! (owner ruling, 2026-09-21). A test that drives crate A and crate B has to
//! live somewhere, and parking it in A buys the privilege with a
//! `[dev-dependencies]` edge on B. That edge is paid by every
//! `cargo test -p A` and every `cargo clippy -p A --all-targets`, in every
//! worktree, whether or not the runner cares about the cross-crate claim —
//! eight extra crates for trusty-mpm, roughly 135 for trusty-analyze, thirteen
//! for trusty-memory. Hosting the test here instead, over NORMAL dependencies
//! on both sides, moves that cost onto whoever runs THIS crate and leaves the
//! two production crates' compile graphs unwelded.
//!
//! What: a library target with no surface at all. Every test is an integration
//! test under `tests/`, one file per contract:
//!
//! - `tests/conformance_cross_gate.rs` — the AC-18 cross-gate golden test
//!   (DOC-15 C5, #1362): trusty-mpm's FRONT gate and trusty-review's BACK gate
//!   must never disagree about one shared `ResolvedIntent`.
//! - `tests/analyze_uds_consumers.rs` — the #6287 combined-PR proof: one live
//!   trusty-analyze socket, four consumers agreeing about what they see.
//! - `tests/memory_uds_consumer.rs` — the #6555 arm: tctl's own probe must read
//!   a live trusty-memory daemon as `Serving`.
//!
//! A new test belongs here when, and only when, it needs the real entry points
//! of two crates that do not already depend on one another. A test that needs
//! only its own crate stays in that crate, where it is cheaper.
//!
//! `scripts/check_dev_dep_edges.sh` is the gate that keeps this arrangement
//! from decaying back into dev-dependency edges.
//!
//! Test: `ac18_m5_divergence_front_escalate_iff_back_would_flag`,
//! `every_consumer_sees_a_live_uds_daemon_as_healthy`,
//! `tctl_probe_sees_a_live_uds_daemon_as_serving`.
