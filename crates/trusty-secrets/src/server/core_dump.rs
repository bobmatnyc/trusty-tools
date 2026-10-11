//! [`disable_core_dumps`]: the server's core-dump limit is zero (#7520 S2).
//!
//! Why: owner ruling Dd8 — a core file of the server would hold every value
//! its session cache and its in-flight requests hold.
//! What: one call at start, before any secret is read.
//! Test: `tests/core_dump_limit.rs`, and
//! `binary_runs_with_core_dumps_disabled` in `tests/on_demand_server.rs`.

use std::io;

/// Set this process's `RLIMIT_CORE` to 0, soft and hard.
///
/// Why: see the module docs.
/// What: stub; does nothing yet.
/// Test: `disable_core_dumps_sets_soft_and_hard_limit_to_zero`.
pub fn disable_core_dumps() -> io::Result<()> {
    Ok(())
}
