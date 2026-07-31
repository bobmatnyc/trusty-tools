//! Doctor probe: can a managed session's harness actually REACH the deployed
//! bundled agents? (issue #4451)
//!
//! Why: `check_agents` (`doctor_fs_checks`) and `check_deployment_completeness`
//! (`doctor_deploy_validate`) are both presence-only — they count `.md` files
//! and diff the payload against the canonical roster. Neither can fail when the
//! files are perfectly deployed into a directory the harness never scans, which
//! is exactly what #4451 was: 42 well-formed agents on disk, both checks green,
//! and every delegation failing with `Agent type '<name>' not found`. A check
//! that cannot fail when the thing it checks is broken has no value.
//!
//! What this probe asserts is the one invariant those two cannot: the SETTINGS
//! TIER the roster deploys into must be a tier the managed spawn's
//! `--setting-sources` flag actually loads. Both sides are read from
//! production code — the deploy destination from
//! [`FrameworkPaths::agent_deploy_dir`], the tier list from
//! [`crate::core::model_inject::relocated_setting_source_tiers`] — so this is
//! not a restatement of a constant: move the deploy destination without
//! updating the flag (what #4437 did) or narrow the flag without moving the
//! roster, and this check fails.
//!
//! Deliberately STATIC rather than spawning a probe `claude -p`: a live spawn
//! costs tens of seconds, needs network and auth, and would make `tm doctor`
//! flaky and slow on the one code path operators reach for when things are
//! already broken. The static form catches the entire failure CLASS (#4203 and
//! #4451 are the same bug at two different deploy destinations) at zero cost.
//!
//! Test: `crates/trusty-mpm/src/daemon/doctor_agent_reachability_tests.rs`.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::model_inject::{
    SETTING_SOURCES_FLAG_RELOCATED, relocated_setting_source_tiers, settings_tier_of,
};
use crate::core::paths::FrameworkPaths;

/// Name of this check as it appears in `tm doctor` output.
const CHECK_NAME: &str = "agent_reachability";

/// Probe whether the bundled-agent deploy tier is one a managed session loads.
///
/// Why: see the module doc — this is the resolvability half of #4451, distinct
/// from the presence-only `agents`/`deployment` checks. It is a hard `Fail`,
/// not a `Warn`: when it trips, the session has NO specialists at all and every
/// delegation silently degrades to `general-purpose`, which is a broken stack
/// rather than a configuration preference.
/// What: resolves the settings tier the roster deploys into via
/// [`settings_tier_of`] (`harness_cwd` is the managed session's working
/// directory — the cwd `claude` is spawned with, which is what makes a
/// destination "project" rather than "user"), then asserts that tier appears in
/// [`relocated_setting_source_tiers`]. `Fail` when it does not, naming the
/// deploy directory, the tier, and the flag so the message is actionable
/// without reading the source. `Ok` otherwise.
/// Test: `production_deploy_tier_is_reachable`,
/// `ok_for_a_project_tier_deploy_destination`.
pub(super) fn check_agent_reachability(
    paths: &FrameworkPaths,
    harness_cwd: Option<&Path>,
) -> DoctorCheck {
    let dir = paths.agent_deploy_dir();
    // The tier is decided by the deploy destination's `.claude`-equivalent
    // HOME, not by the `agents/` leaf — `settings_tier_of` compares that home
    // against the harness cwd. For the tm-managed destination the home is
    // `$CLAUDE_CONFIG_DIR`; falling back to `dir` itself keeps a malformed
    // (parentless) path from panicking.
    let deploy_home = dir.parent().unwrap_or(&dir);
    // With no project directory supplied there is no managed cwd to compare
    // against; an empty path can never equal a real deploy home, so the tier
    // resolves to `user` — which is the correct answer for the tm-managed
    // destination and the conservative one for any other.
    let cwd = harness_cwd.unwrap_or_else(|| Path::new(""));
    let tier = settings_tier_of(deploy_home, cwd);
    verdict(&dir, tier, &relocated_setting_source_tiers())
}

/// Pure verdict: does `tier` appear in `loaded`? (issue #4451)
///
/// Why: separated from [`check_agent_reachability`] so the FAIL branch is
/// directly testable. The real tier list comes from a compile-time constant, so
/// a test that could only call the wrapper would be unable to exercise the very
/// branch this check exists for — it would assert the happy path and silently
/// lose the regression guard, which is the same shape of blind spot #4451 is
/// about.
/// What: `Ok` when `loaded` contains `tier`; `Fail` otherwise, with a message
/// naming the deploy directory, the tier, the spawn flag, and the tiers that
/// flag actually loads.
/// Test: `ok_when_the_deploy_tier_is_loaded`,
/// `fails_when_the_spawn_flag_drops_the_deploy_tier`,
/// `failure_names_the_directory_the_tier_and_the_flag`,
/// `failure_is_a_hard_fail_not_a_warn`.
fn verdict(dir: &Path, tier: &str, loaded: &[&str]) -> DoctorCheck {
    if loaded.contains(&tier) {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "bundled agents deploy into the `{tier}` tier ({}), which a managed \
                 session loads via `{SETTING_SOURCES_FLAG_RELOCATED}`",
                dir.display()
            ),
        );
    }
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Fail,
        format!(
            "bundled agents deploy into the `{tier}` tier ({}) but a managed session \
             launches with `{SETTING_SOURCES_FLAG_RELOCATED}`, which loads only [{}] — \
             the harness never scans that directory, so EVERY delegation degrades to \
             `general-purpose` with `Agent type '<name>' not found`, no matter how \
             complete the deploy is (issue #4451). Fix the spawn flag or the deploy \
             destination so the two name the same tier.",
            dir.display(),
            loaded.join(", ")
        ),
    )
}

#[cfg(test)]
#[path = "doctor_agent_reachability_tests.rs"]
mod tests;
