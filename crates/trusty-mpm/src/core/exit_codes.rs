//! Shared CLI process exit codes (issue #1737).
//!
//! Why: two independent "the requested target is unavailable, not a hard
//! failure" conditions want the exact same exit code so scripts/skills can
//! branch on one stable value regardless of which condition produced it:
//! the Session Manager being off / unreachable (`tm session prune-idle`,
//! issue #1313, `commands::prune::EXIT_SM_UNAVAILABLE`) and an explicitly
//! configured daemon URL that fails its reachability probe (issue #1737,
//! [`crate::core::discovery::EXIT_DAEMON_URL_UNREACHABLE`]). Both used to
//! define their own `75` literal independently; two identical literals in
//! two crates/modules drift silently the moment either is revisited without
//! remembering the other exists. This module is the single source of truth
//! both reference.
//! What: [`EXIT_UNAVAILABLE`] — the value `75` (`EX_TEMPFAIL`-adjacent, per
//! `sysexits.h` conventions, chosen for "temporary/graceful unavailability"
//! rather than a hard failure).
//! Test: `exit_code_is_stable` (value); `commands::prune::tests::
//! unavailable_exit_code_is_stable` and `core::discovery::tests::
//! exit_code_matches_sm_unavailable_convention` both assert their
//! re-exported constant equals this one, keeping all three in lockstep.

/// Process exit code for a graceful "target unavailable" condition.
///
/// Why/What/Test: see the module doc above.
pub const EXIT_UNAVAILABLE: i32 = 75;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_is_stable() {
        assert_eq!(EXIT_UNAVAILABLE, 75);
    }
}
