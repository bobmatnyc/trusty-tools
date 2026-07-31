//! Tests for the #4451 agent-reachability doctor probe.
//!
//! Why: split out of `doctor_agent_reachability.rs` so that file stays well
//! under the 500-SLOC production cap, matching the existing
//! `doctor_push_guard{,_tests}.rs` pattern.
//! What: covers both branches of the pure [`super::verdict`] decision plus the
//! two wrapper paths that resolve a real tier from a `FrameworkPaths`.
//! Test: this file.

use std::path::Path;

use super::{check_agent_reachability, verdict};
use crate::core::doctor::CheckStatus;
use crate::core::model_inject::relocated_setting_source_tiers;
use crate::core::paths::FrameworkPaths;

/// The happy path: the tier the roster deploys into is one the spawn loads.
#[test]
fn ok_when_the_deploy_tier_is_loaded() {
    let check = verdict(
        Path::new("/tm/claude-config/agents"),
        "user",
        &["user", "project", "local"],
    );
    assert_eq!(check.status, CheckStatus::Ok);
    assert_eq!(check.name, "agent_reachability");
}

/// The regression this check exists for: a perfectly deployed roster in a tier
/// the spawn flag does not name. This is precisely #4451 — presence-only checks
/// stayed green here while every delegation failed.
#[test]
fn fails_when_the_spawn_flag_drops_the_deploy_tier() {
    let check = verdict(
        Path::new("/tm/claude-config/agents"),
        "user",
        &["project", "local"],
    );
    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "a deploy tier the spawn never loads must FAIL, not warn: {}",
        check.message
    );
}

/// A failure operators cannot act on is worth little — pin the three facts the
/// message must carry.
#[test]
fn failure_names_the_directory_the_tier_and_the_flag() {
    let check = verdict(
        Path::new("/tm/claude-config/agents"),
        "user",
        &["project", "local"],
    );
    assert!(
        check.message.contains("/tm/claude-config/agents"),
        "message must name the deploy directory: {}",
        check.message
    );
    assert!(
        check.message.contains("`user` tier"),
        "message must name the unreachable tier: {}",
        check.message
    );
    assert!(
        check.message.contains("--setting-sources"),
        "message must name the spawn flag: {}",
        check.message
    );
    assert!(
        check.message.contains("#4451"),
        "message must cite the issue: {}",
        check.message
    );
}

/// Doctor must not soften this to a `Warn`: when it trips there are zero
/// specialists in the session, which is a broken stack, not a preference.
#[test]
fn failure_is_a_hard_fail_not_a_warn() {
    let check = verdict(Path::new("/somewhere/agents"), "user", &[]);
    assert_ne!(check.status, CheckStatus::Warn);
    assert_eq!(check.status, CheckStatus::Fail);
}

/// The live coupling: the production deploy destination must land in a tier the
/// production spawn flag loads. This is the assertion that would have caught
/// #4437's move — it reads BOTH sides from production code, so changing either
/// one alone trips it.
#[test]
fn production_deploy_tier_is_reachable() {
    let paths = FrameworkPaths::under("/base");
    let check = check_agent_reachability(&paths, Some(Path::new("/base/workspace")));
    assert_eq!(
        check.status,
        CheckStatus::Ok,
        "the bundled roster deploys into a tier managed sessions do not load \
         (loaded: {:?}): {}",
        relocated_setting_source_tiers(),
        check.message
    );
}

/// A project-tier deploy destination (deploy home == harness cwd) is also
/// reachable — the check gates on tier membership, not on one specific
/// directory, so a future move back to project-local deployment stays green
/// without editing this probe.
#[test]
fn ok_for_a_project_tier_deploy_destination() {
    let mut paths = FrameworkPaths::under("/base");
    let workspace = Path::new("/base/workspace");
    paths.agent_deploy = workspace.join("agents");
    let check = check_agent_reachability(&paths, Some(workspace));
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("`project` tier"),
        "expected the project tier to be named: {}",
        check.message
    );
}

/// With no project directory supplied the probe still resolves a tier rather
/// than panicking or silently passing on an unknown.
#[test]
fn resolves_a_tier_without_a_project_directory() {
    let paths = FrameworkPaths::under("/base");
    let check = check_agent_reachability(&paths, None);
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(check.message.contains("`user` tier"), "{}", check.message);
}
