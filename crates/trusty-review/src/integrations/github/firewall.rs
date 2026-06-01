//! Push firewall — hard-coded non-configurable barrier against write operations.
//!
//! Why: the bot was historically caught force-pushing to an infra repo
//! (spec lesson §12.11).  This firewall is intentionally non-configurable:
//! no env var, no per-repo config, no feature flag can flip it.  The guard
//! must be called on every code path that would perform a git-write operation
//! (branch create, file commit, PR create, force-push).
//!
//! What: exposes a `const GH_ALLOW_PUSH: bool = false` constant and an
//! `assert_no_push_operation` function that returns `Err(GithubError::PushFirewall)`
//! whenever called.  All write paths in this crate MUST call this guard before
//! any network request.
//!
//! Test: `push_firewall_constant_is_false` asserts the constant is `false`;
//! `push_firewall_guard_always_errors` asserts the guard always returns an error.

use crate::integrations::github::GithubError;

// ─── Firewall constant ────────────────────────────────────────────────────────

/// Hard-coded push firewall.
///
/// Why: non-configurable barrier that prevents any git-write operation.
/// The value is `false` and MUST NOT be changed.  There is no runtime path
/// that can flip this constant.  (spec REV-403, lesson §12.11)
/// What: a compile-time `false` constant checked by `assert_no_push_operation`.
/// Test: `push_firewall_constant_is_false` asserts `GH_ALLOW_PUSH == false`.
pub const GH_ALLOW_PUSH: bool = false;

// ─── Guard function ───────────────────────────────────────────────────────────

/// Assert that no push operation is being attempted.
///
/// Why: all write paths (branch create, file commit, PR create, force-push)
/// must call this guard so the firewall is enforced at runtime, not just by
/// code review.  (spec REV-403)
/// What: always returns `Err(GithubError::PushFirewall)` — the firewall
/// constant `GH_ALLOW_PUSH` is `false` and the guard is unconditional.
/// There is no way to make this function return `Ok`.
/// Test: `push_firewall_guard_always_errors` asserts the guard always errors.
pub fn assert_no_push_operation() -> Result<(), GithubError> {
    // This guard is intentionally unconditional.  GH_ALLOW_PUSH is false and
    // must never be changed.  If you are reading this because you want to add
    // a write operation: do not.  See spec REV-403 and lesson §12.11.
    Err(GithubError::PushFirewall)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn push_firewall_constant_is_false() {
        // GH_ALLOW_PUSH must always be false; this test guards against any
        // accidental change to the constant value.
        // The compile-time assertion (const _: () = assert!(!GH_ALLOW_PUSH))
        // also enforces this; the named runtime test makes the invariant visible
        // in test output and CI.
        assert!(
            !GH_ALLOW_PUSH,
            "GH_ALLOW_PUSH must be false — the push firewall is non-configurable (spec REV-403)"
        );
    }

    #[test]
    fn push_firewall_guard_always_errors() {
        // assert_no_push_operation must always return Err, never Ok.
        let result = assert_no_push_operation();
        assert!(
            result.is_err(),
            "assert_no_push_operation must always return Err — push operations are forbidden"
        );
        // Verify the specific variant so callers can distinguish this from
        // transport/API errors.
        match result.unwrap_err() {
            GithubError::PushFirewall => {}
            other => panic!("expected PushFirewall, got {other:?}"),
        }
    }

    /// Compile-time assertion: GH_ALLOW_PUSH can never be true.
    ///
    /// Why: ensures the constant cannot silently drift to `true` through a
    /// future edit.  This `const` block fails to compile if `GH_ALLOW_PUSH`
    /// is ever set to `true`.
    /// What: a `const { assert!(!GH_ALLOW_PUSH) }` expression evaluated at
    /// compile time.
    const _: () = assert!(!GH_ALLOW_PUSH, "GH_ALLOW_PUSH must be false");
}
