//! #10940: the startup `$HOME` provisioning step and its off-switch. The
//! hermetic tests pass steps that target a temp home; the one env test sets
//! `HOME` to a temp dir under `HOME_LOCK` and never reads the real one.

use std::cell::Cell;
use std::ffi::OsStr;
use std::path::Path;

use super::{
    NO_PROVISION_ENV, ProvisionSkip, provision_home_tier, provision_home_tier_with, provision_skip,
};
use crate::agents::bundled::content::test_support::repo_tree;
use crate::agents::bundled::{BundledTreeKind, ensure_bundled_agents_deployed_in};

fn argv(rest: &[&str]) -> Vec<String> {
    std::iter::once("tagent")
        .chain(rest.iter().copied())
        .map(str::to_owned)
        .collect()
}

/// Every entry under `dir`, for a readable failure message.
fn entries(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("read temp home")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// Runs the core with steps that deploy the checkout's bundled trees into
/// `home`, the way production deploys into `$HOME`. Returns whether each step
/// ran.
fn run_against(home: &Path, skip: Option<ProvisionSkip>) -> (super::HomeProvisioning, [bool; 3]) {
    let ran = [Cell::new(false), Cell::new(false), Cell::new(false)];
    let agents_dir = home.join(".trusty-agents").join("agents");
    let workflows_dir = home.join(".trusty-agents").join("workflows");
    let result = provision_home_tier_with(
        skip,
        || {
            ran[0].set(true);
            ensure_bundled_agents_deployed_in(repo_tree(BundledTreeKind::Agents), &agents_dir)
        },
        || {
            ran[1].set(true);
            ensure_bundled_agents_deployed_in(repo_tree(BundledTreeKind::Workflows), &workflows_dir)
        },
        || {
            ran[2].set(true);
            let _ = crate::assistants::provision::provision_startup_homes_in(
                &home.join("trusty-agents"),
                std::slice::from_ref(&agents_dir),
            );
        },
    );
    (result, [ran[0].get(), ran[1].get(), ran[2].get()])
}

/// Why: the decision is the whole off-switch; `0` must override the sandbox
/// implication so a sandbox can still provision into its own home.
/// What: one row per input shape, with the expected skip reason.
/// Test: itself.
#[test]
fn skip_decision_table() {
    use ProvisionSkip::*;
    let one = Some(OsStr::new("1"));
    let zero = Some(OsStr::new("0"));
    let other = Some(OsStr::new("true"));
    type Row<'a> = (
        Option<&'a OsStr>,
        Option<&'a OsStr>,
        &'a [&'a str],
        Option<ProvisionSkip>,
    );
    let rows: &[Row] = &[
        (None, None, &["--api"], None),
        (None, None, &[], None),
        (one, None, &["--api"], Some(NoProvisionEnv)),
        (one, one, &[], Some(NoProvisionEnv)),
        (None, one, &["--api"], Some(Sandbox)),
        (zero, one, &["--api"], None),
        (other, None, &[], None),
        (other, one, &[], Some(Sandbox)),
        (None, other, &[], None),
        (None, None, &["--help"], Some(Help)),
        (None, None, &["agents", "-h"], Some(Help)),
        (zero, None, &["--help"], Some(Help)),
        (None, None, &["say --help please"], None),
    ];
    for (np, sb, rest, want) in rows {
        assert_eq!(
            provision_skip(*np, *sb, &argv(rest)),
            *want,
            "{NO_PROVISION_ENV}={np:?} TRUSTY_SANDBOX={sb:?} args={rest:?}"
        );
    }
}

/// Why (#10940): with the off-switch set, startup must write nothing to the
/// home, for every skip reason.
/// What: runs the core against an empty temp home with each reason; no step
/// runs and the home stays empty.
/// Test: itself.
#[test]
fn a_skip_leaves_the_home_empty() {
    for reason in [
        ProvisionSkip::NoProvisionEnv,
        ProvisionSkip::Sandbox,
        ProvisionSkip::Help,
    ] {
        let home = tempfile::tempdir().expect("temp home");
        let (result, ran) = run_against(home.path(), Some(reason));
        assert_eq!(ran, [false; 3], "{reason:?}: a step ran");
        assert_eq!(result.skipped, Some(reason));
        assert!(
            entries(home.path()).is_empty(),
            "{reason:?}: home holds {:?}",
            entries(home.path())
        );
    }
}

/// Why: without the switch the pre-#10940 behaviour holds — the roster and
/// workflows are deployed and the homes step runs after them.
/// What: runs the core with no skip; every step runs and both trees land.
/// Test: itself.
#[test]
fn no_skip_provisions_the_home() {
    let home = tempfile::tempdir().expect("temp home");
    let (result, ran) = run_against(home.path(), None);
    assert_eq!(ran, [true; 3]);
    assert!(result.skipped.is_none());
    assert!(result.agents_err.is_none(), "{:?}", result.agents_err);
    assert!(result.workflows_err.is_none(), "{:?}", result.workflows_err);
    assert!(result.agents_report.total_touched() > 0);
    let dot = home.path().join(".trusty-agents");
    assert!(!entries(&dot.join("agents")).is_empty());
    assert!(!entries(&dot.join("workflows")).is_empty());
}

/// Why (#10940): the production entry must read the env switch itself.
/// What: under `HOME_LOCK`, points `HOME` at an empty temp dir, sets
/// `TAGENT_NO_PROVISION=1`, calls [`provision_home_tier`], and checks the home
/// is still empty; both variables are restored before the lock drops.
/// Test: itself.
#[test]
fn the_env_switch_keeps_a_temp_home_empty() {
    let _home_lock = crate::test_env::lock_home();
    let home = tempfile::tempdir().expect("temp home");
    let prev_home = std::env::var_os("HOME");
    let prev_switch = std::env::var_os(NO_PROVISION_ENV);
    // SAFETY: HOME_LOCK serializes every test that mutates HOME; the switch is
    // read only by this module.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var(NO_PROVISION_ENV, "1");
    }
    let result = provision_home_tier(&argv(&["--api"]));
    let left = entries(home.path());
    // SAFETY: as above.
    unsafe {
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_switch {
            Some(v) => std::env::set_var(NO_PROVISION_ENV, v),
            None => std::env::remove_var(NO_PROVISION_ENV),
        }
    }
    assert_eq!(result.skipped, Some(ProvisionSkip::NoProvisionEnv));
    assert!(left.is_empty(), "temp HOME holds {left:?}");
}
