//! Tests for [`super`] — the #8545 home-config write fence.

use super::*;

/// The fence fires under every fenced entry and nowhere else, including a
/// sibling whose name merely shares a prefix.
#[test]
fn check_panics_only_under_a_fenced_root() {
    let home = Path::new("/fence-test-home");
    let roots = fenced_roots_under(home);
    for fenced in [
        ".trusty-tools/trusty-mpm/claude-config/skills",
        ".trusty-mpm/framework/agents",
        ".claude/settings.json",
        ".claude.json",
    ] {
        let dest = home.join(fenced);
        let outcome = std::panic::catch_unwind(|| check_against(&dest, &roots));
        assert!(outcome.is_err(), "{} must be fenced", dest.display());
    }
    for open in [".claude-other/x", "project/.claude", "tmp/.trusty-mpm"] {
        let dest = home.join(open);
        check_against(&dest, &roots);
    }
    check_against(Path::new("/elsewhere/.trusty-mpm"), &roots);
}

/// Unarmed, `check` never panics — the production state.
#[test]
fn an_unarmed_fence_allows_everything() {
    check_against(Path::new("/any/.trusty-mpm/framework"), &[]);
}

/// The first `arm` wins; a later call neither re-arms nor widens the fence.
///
/// Arms THIS lib test binary over a fresh temp home no other test writes, so
/// the process-global it sets is inert for every sibling.
#[test]
fn arm_is_first_writer_wins() {
    let home = crate::test_support::hermetic_temp_dir();
    let first = arm(&[home.path()]);
    let second = arm(&[Path::new("/another-home")]);
    assert!(!second, "a second arm must be a no-op");
    if first {
        assert_eq!(armed_roots(), fenced_roots_under(home.path()).as_slice());
    }
    assert!(
        !armed_roots().iter().any(|r| r.starts_with("/another-home")),
        "a second arm must not widen the fence: {:?}",
        armed_roots()
    );
}
