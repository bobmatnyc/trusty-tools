//! The `$HOME` provisioning step of tagent startup and its off-switch (#10940).
//!
//! Why: every `tagent` start (but `--version`, `config` and `mcp-serve`)
//! deployed the bundled agents and workflows into `$HOME/.trusty-agents` and
//! created `~/trusty-agents/<instance>`. An isolated run had no way to stop
//! that short of overriding `HOME`, and `--help` provisioned too.
//! What: [`provision_skip`] decides whether to skip, [`provision_home_tier_with`]
//! runs or skips the three steps, and [`provision_home_tier`] wires both to the
//! process env and the production deploy functions.
//! Test: `skip_decision_table`, `a_skip_leaves_the_home_empty`,
//! `no_skip_provisions_the_home`, `the_env_switch_keeps_a_temp_home_empty`.

use std::ffi::OsStr;

use trusty_common::credentials::{SANDBOX_ENV_VAR, sandbox_flag_set};

use crate::agents::bundled::{self, ReprovisionReport};
use crate::assistants;

/// The off-switch: `1` skips the `$HOME` provisioning step, `0` forces it on
/// even under `TRUSTY_SANDBOX=1` (#10940).
/// Test: `startup_provision_tests::skip_decision_table`.
pub(super) const NO_PROVISION_ENV: &str = "TAGENT_NO_PROVISION";

/// Why the provisioning step did not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProvisionSkip {
    /// `TAGENT_NO_PROVISION=1`.
    NoProvisionEnv,
    /// `TRUSTY_SANDBOX=1` with `TAGENT_NO_PROVISION` not set to `0`.
    Sandbox,
    /// `--help` / `-h` on the command line: clap prints and exits afterwards.
    Help,
}

/// Whether startup skips the `$HOME` provisioning step.
///
/// Why (#10940): an isolated run must be able to start without writing under
/// the user's home, and help output must never provision.
/// What: `Help` when any argument after argv\[0\] is exactly `--help` or `-h`.
/// Otherwise `no_provision` decides when it is exactly `1` (skip) or exactly
/// `0` (provision); any other value, or none, falls through to the sandbox
/// flag, read with trusty-common's exactly-`1` rule ([`sandbox_flag_set`]).
/// Test: `startup_provision_tests::skip_decision_table`.
pub(super) fn provision_skip(
    no_provision: Option<&OsStr>,
    sandbox: Option<&OsStr>,
    args: &[String],
) -> Option<ProvisionSkip> {
    if args.iter().skip(1).any(|a| a == "--help" || a == "-h") {
        return Some(ProvisionSkip::Help);
    }
    match no_provision.and_then(OsStr::to_str) {
        Some("1") => return Some(ProvisionSkip::NoProvisionEnv),
        Some("0") => return None,
        _ => {}
    }
    sandbox_flag_set(sandbox).then_some(ProvisionSkip::Sandbox)
}

/// What the provisioning step did, for the startup banner and logs.
#[derive(Debug, Default)]
pub(super) struct HomeProvisioning {
    /// The bundled-agent deploy's counts; zero when skipped or failed.
    pub(super) agents_report: ReprovisionReport,
    /// The bundled-agent deploy's error, held until tracing is up (#9014).
    pub(super) agents_err: Option<anyhow::Error>,
    /// The bundled-workflow deploy's error (#5227).
    pub(super) workflows_err: Option<anyhow::Error>,
    /// `Some` when the step was skipped, naming why.
    pub(super) skipped: Option<ProvisionSkip>,
}

/// Hermetic core of [`provision_home_tier`]: run the three deploy steps unless
/// `skip` is set.
///
/// Why (#10940): the steps are parameters, so a test proves the skip writes
/// nothing by passing steps that target a temp home, with no process-env
/// mutation.
/// What: `Some(skip)` returns at once and calls no step. `None` calls
/// `deploy_agents`, `deploy_workflows`, then `provision_homes`, in that order
/// (the instance discovery reads the deployed roster), keeping each deploy
/// error; the homes step is infallible by contract (#4325).
/// Test: `startup_provision_tests::a_skip_leaves_the_home_empty`,
/// `startup_provision_tests::no_skip_provisions_the_home`.
pub(super) fn provision_home_tier_with(
    skip: Option<ProvisionSkip>,
    deploy_agents: impl FnOnce() -> anyhow::Result<ReprovisionReport>,
    deploy_workflows: impl FnOnce() -> anyhow::Result<ReprovisionReport>,
    provision_homes: impl FnOnce(),
) -> HomeProvisioning {
    // #10940: the off-switch; nothing below runs, so nothing is written.
    if skip.is_some() {
        return HomeProvisioning {
            skipped: skip,
            ..HomeProvisioning::default()
        };
    }
    let (agents_report, agents_err) = match deploy_agents() {
        Ok(report) => (report, None),
        Err(e) => (ReprovisionReport::default(), Some(e)),
    };
    let workflows_err = deploy_workflows().err();
    provision_homes();
    HomeProvisioning {
        agents_report,
        agents_err,
        workflows_err,
        skipped: None,
    }
}

/// The startup provisioning step: deploy the bundled agents (#3405, #3556)
/// and workflows (#5227) under `$HOME/.trusty-agents`, then create each
/// assistant instance's home (#4325), unless [`provision_skip`] says skip.
///
/// Why: one call from `run_startup_init`, before the `--api` early dispatch,
/// so every start that reaches it honours the same off-switch (#10940).
/// What: reads `TAGENT_NO_PROVISION` and `TRUSTY_SANDBOX` once each, then
/// [`provision_home_tier_with`] over the production deploy functions. Every
/// failure is held in the result, never returned: provisioning stays
/// best-effort.
/// Test: `startup_provision_tests::the_env_switch_keeps_a_temp_home_empty`.
pub(super) fn provision_home_tier(args: &[String]) -> HomeProvisioning {
    let skip = provision_skip(
        std::env::var_os(NO_PROVISION_ENV).as_deref(),
        std::env::var_os(SANDBOX_ENV_VAR).as_deref(),
        args,
    );
    provision_home_tier_with(
        skip,
        bundled::ensure_bundled_agents_deployed,
        bundled::ensure_bundled_workflows_deployed,
        || {
            let _ = assistants::provision_startup_homes();
        },
    )
}

#[cfg(test)]
#[path = "startup_provision_tests.rs"]
mod tests;
