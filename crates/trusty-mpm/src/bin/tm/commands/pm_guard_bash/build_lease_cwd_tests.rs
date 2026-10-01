//! `build_lease_cwd` — the directory a leased build is meant to run in (#8969).

use super::*;
use crate::commands::pm_guard_bash::build_lease_rewrite::{LeaseRewrite, rewrite_for_lease_with};

const BASE: &str = "/base";
const HOME: &str = "/home/u";

/// The directory for the LAST `cargo` (or `bash`) word in `command`.
fn dir(command: &str) -> BuildDir {
    let at = command
        .rfind("cargo")
        .or_else(|| command.rfind("bash"))
        .expect("a build word");
    build_dir(command, at, Some(Path::new(BASE)), Some(Path::new(HOME)))
}

fn pinned(path: &str) -> BuildDir {
    BuildDir::Pinned(PathBuf::from(path))
}

fn expected(path: &str) -> BuildDir {
    BuildDir::Expected(PathBuf::from(path))
}

/// The issue's shape: a literal absolute `cd` chain is carried into the lease
/// as `--chdir`, so a dropped `cd` cannot move the build.
#[test]
fn a_pure_absolute_cd_chain_pins_the_directory() {
    for (command, want) in [
        ("cd /wt && cargo test -p x", pinned("/wt")),
        (
            "cd /a/../wt && RUSTC_WRAPPER=sccache CARGO_BUILD_JOBS=2 cargo test",
            pinned("/wt"),
        ),
        ("cd ~ && cargo build", pinned(HOME)),
        ("cd ~/src && cargo build", pinned("/home/u/src")),
        ("cd /a && cd /wt && cargo build", pinned("/wt")),
        ("cd '/my wt' && cargo check", pinned("/my wt")),
    ] {
        assert_eq!(dir(command), want, "{command}");
    }
    let prefix = lease_prefix("tm build-lease", "cd '/my wt' && cargo check", 15, None);
    assert_eq!(prefix.as_deref(), Ok("tm build-lease --chdir '/my wt' --"));
}

/// Any other resolvable shape is checked, never relocated: the lease refuses
/// unless the shell really is there.
#[test]
fn a_resolvable_cd_is_expected_not_pinned() {
    for (command, want) in [
        ("cd crates/x && cargo test", expected("/base/crates/x")),
        (
            "cd /wt && cd crates/x && cargo test",
            expected("/wt/crates/x"),
        ),
        ("cd -- /wt && cargo test", expected("/wt")),
        ("(cd /wt && cargo test)", expected("/wt")),
        ("cd /wt; cargo test", expected("/wt")),
        ("cd /wt || exit 1; cargo test", expected("/wt")),
        ("pushd /wt && cargo test", expected("/wt")),
        ("pushd /wt && popd && cargo test", expected(BASE)),
        (
            "cd /wt && cargo test && cd .. && cargo build",
            expected("/"),
        ),
        ("echo $(cd /wt; cargo build)", expected("/wt")),
        ("if cd /wt; then cargo test; fi", expected("/wt")),
    ] {
        assert_eq!(dir(command), want, "{command}");
    }
    let prefix = lease_prefix(
        "tm build-lease",
        "cd crates/x && cargo test",
        15,
        Some(Path::new(BASE)),
    );
    assert_eq!(
        prefix.as_deref(),
        Ok("tm build-lease --expect-cwd /base/crates/x --")
    );
}

/// A `cd` whose subshell closed, or that is only data, does not move the build.
#[test]
fn a_scoped_or_quoted_cd_leaves_the_directory_unchanged() {
    for command in [
        "cargo test",
        "(cd /wt) && cargo test",
        "bash -c 'cd /wt && cargo build'",
        "git commit -m 'cd x' && cargo test",
        "echo cd && cargo test",
    ] {
        assert_eq!(dir(command), BuildDir::Unchanged, "{command}");
    }
    assert_eq!(
        lease_prefix("tm build-lease", "cargo test", 0, None).as_deref(),
        Ok("tm build-lease --")
    );
}

/// The Fail-Open Check's error arm: a directory change the hook cannot resolve
/// refuses the build — it is never rewritten without its directory, and never
/// left to run unleased.
#[test]
fn an_unresolvable_cd_refuses_the_build() {
    for command in [
        "cd \"$WT\" && cargo test",
        "cd $WT && cargo test",
        "cd - && cargo test",
        "cd ~bob && cargo test",
        "cd a b && cargo test",
        "pushd && cargo test",
        "popd && cargo test",
        "cd $(git rev-parse --show-toplevel) && cargo test",
        "case x in a) cd /wt;; esac; cargo test",
    ] {
        assert!(
            matches!(dir(command), BuildDir::Unresolvable(_)),
            "{command}: {:?}",
            dir(command)
        );
    }
    assert!(matches!(
        build_dir("cd x && cargo test", 8, None, None),
        BuildDir::Unresolvable(_)
    ));
    let heavy = trusty_mpm::core::build_lease::config::BuildLeaseConfig::default()
        .effective_heavy_build_commands();
    let command = "cd \"$WT\" && cargo test";
    let verdict = rewrite_for_lease_with(command, &heavy, &|at| {
        lease_prefix("tm build-lease", command, at, Some(Path::new(BASE)))
    });
    let LeaseRewrite::Refuse(reason) = verdict else {
        panic!("an unresolvable cd must refuse, got {verdict:?}");
    };
    assert!(reason.contains("#8969"), "{reason}");
    // An already-leased build is left alone: the escape the refusal names.
    let wrapped = "cd \"$WT\" && tm build-lease -- cargo test";
    let verdict = rewrite_for_lease_with(wrapped, &heavy, &|at| {
        lease_prefix("tm build-lease", wrapped, at, Some(Path::new(BASE)))
    });
    assert_eq!(verdict, LeaseRewrite::None);
}
