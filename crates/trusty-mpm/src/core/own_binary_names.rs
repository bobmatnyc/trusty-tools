//! Canonical binary names this crate ships — the single source of truth for
//! the three consumers that don't need a different search order.
//!
//! Why (#4058): `Cargo.toml`'s `[[bin]]` targets `tm` and `trusty-mpm` build
//! from the identical `src/bin/tm/main.rs` entry point, so any code that
//! recognises "a trusty-mpm process" or "the trusty-mpm binary" must
//! recognise BOTH names — they are the same artifact under two names, not two
//! artifacts. Before this module existed, four call sites each hand-maintained
//! their own copy of this two-name list (`daemon::discovery::CLAUDE_COMMANDS`,
//! `core::standalone::hooks::MPM_BIN_NAMES`,
//! `core::session_launch::settings::STATUSLINE_BIN_NAMES`, and an inline
//! `name == "trusty-mpm" || name == "tm"` check in the `tm stop` daemon-PID
//! scan) and had already drifted once: `CLAUDE_COMMANDS` was missing
//! `"trusty-mpm"`, so a session launched via the `trusty-mpm` binary was
//! silently invisible to auto-discovery (`GET /sessions` and friends) even
//! though an identical session launched via `tm` was discovered correctly.
//! This module exists purely because that drift produced a real defect, not
//! because of any general cross-crate consolidation policy — all four lists
//! are private to this one crate.
//! What: [`OWN_BINARY_NAMES`] is the two names, ORDERED `"tm"` then
//! `"trusty-mpm"` — that order IS load-bearing, not incidental.
//! `core::session_launch::settings::STATUSLINE_BIN_NAMES` aliases this
//! constant directly, so it inherits (and needs) the `tm`-first order for its
//! first-PATH-hit-wins lookup. `core::standalone::hooks::MPM_BIN_NAMES` needs
//! the OPPOSITE preference for its own first-hit-wins lookup, so it keeps a
//! separate literal array with the same two-name SET rather than aliasing
//! this constant — see that module's doc comment, and
//! `hooks::tests::test_mpm_bin_names_matches_own_binary_names_set`, which pins
//! the two to the same set. `daemon::discovery::is_claude_command` and
//! `bin::tm::commands::daemon::find_daemon_pids` compare by exact equality, so
//! order is immaterial to them.
//! Test: `own_binary_names_is_tm_then_trusty_mpm` (below) pins the exact
//! contents and order. `daemon::discovery`'s `is_claude_command_matches_trusty_mpm`
//! / `is_claude_command_rejects_others` and
//! `core::session_launch::settings::tests::resolve_statusline_binary_with_*`
//! exercise consumers of this constant. `bin::tm::commands::daemon::find_daemon_pids`
//! has no test of its own (pre- or post-#4058) — its predicate is a
//! straightforward `.any(|n| name == *n)` filter over this constant.

/// The `[[bin]]` names this crate's `Cargo.toml` produces, in the order
/// `core::session_launch::settings::STATUSLINE_BIN_NAMES` needs (see module
/// doc — order is load-bearing).
pub const OWN_BINARY_NAMES: &[&str] = &["tm", "trusty-mpm"];

#[cfg(test)]
mod tests {
    use super::OWN_BINARY_NAMES;

    /// Why (#4058 review round 1 MEDIUM finding 3): pins the exact contents
    /// AND order so a future edit here is a deliberate, reviewed change
    /// rather than a silent drift — `STATUSLINE_BIN_NAMES` aliases this
    /// constant directly and inherits whatever order it has.
    /// What: asserts the constant is exactly `["tm", "trusty-mpm"]`.
    #[test]
    fn own_binary_names_is_tm_then_trusty_mpm() {
        assert_eq!(OWN_BINARY_NAMES, &["tm", "trusty-mpm"]);
    }
}
