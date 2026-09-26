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

/// #8545: a `$CLAUDE_CONFIG_DIR` outside every home is fenced, together with
/// the home roots, and a sibling of it stays writable. The value is injected,
/// so no test mutates the process environment.
#[test]
fn a_claude_config_dir_outside_home_is_fenced() {
    let home = Path::new("/fence-test-home");
    let config_dir = Path::new("/elsewhere/claude-config");
    let from_env = claude_config_dir_root(Some(config_dir.into()));
    let roots = fence_roots(&[home], from_env.as_deref());
    for fenced in [
        config_dir.join("settings.json"),
        config_dir.join("agents/engineer.md"),
        home.join(".claude/settings.json"),
    ] {
        let outcome = std::panic::catch_unwind(|| check_against(&fenced, &roots));
        assert!(outcome.is_err(), "{} must be fenced", fenced.display());
    }
    check_against(Path::new("/elsewhere/claude-config-other/x"), &roots);
    check_against(Path::new("/elsewhere/project/.claude"), &roots);
}

/// #8545: when this process has a `$CLAUDE_CONFIG_DIR`, the pre-`main` arming
/// fenced it. Reads the environment only; vacuous where the variable is unset.
#[test]
fn a_set_claude_config_dir_is_an_armed_root_of_this_process() {
    if let Some(dir) = claude_config_dir_root(std::env::var_os("CLAUDE_CONFIG_DIR")) {
        assert!(
            armed_roots().contains(&dir),
            "{} is not fenced; armed roots: {:?}",
            dir.display(),
            armed_roots()
        );
    }
}

/// An unset or empty `$CLAUDE_CONFIG_DIR` adds no root.
#[test]
fn an_empty_claude_config_dir_fences_nothing() {
    assert_eq!(claude_config_dir_root(None), None);
    assert_eq!(claude_config_dir_root(Some("".into())), None);
    let home = Path::new("/fence-test-home");
    assert_eq!(fence_roots(&[home], None), fenced_roots_under(home));
    // `$HOME` and the passwd home are usually the same path: one set of roots.
    assert_eq!(fence_roots(&[home, home], None), fenced_roots_under(home));
}
