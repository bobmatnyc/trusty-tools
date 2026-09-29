//! The hard floor of `tm hook --pm-guard`, and the Architect exemption (#8878).
//!
//! Why: design of record D8 (#8878) makes the floor hold under
//! `TRUSTY_MPM_PM_UNRESTRICTED` and `TRUSTY_MPM_DISABLE_HOOKS`: no secret
//! values in the transcript, no `rm -rf` of `$HOME` or `/`, and the D4
//! additions. The Architect rulings for PR 2b (#8878, comment 5889258808)
//! exempt the process-bound Architect from the D4 remainder and from the D5
//! rules, and keep the existing floors universal, the Architect included.
//! What: [`deny_floors`] runs first in `pm_guard`, ahead of both bypasses, and
//! prints the first deny [`evaluate_floors`] returns. Universal: the
//! trust-anchor rule (with its own Architect exemption), a command the guard
//! cannot classify (`$'…'` quoting, #6660), a destructive delete
//! of a root-class target or an unresolvable one, and a secret-file read or
//! printed credential — evaluated here only under a bypass, because the
//! guarded path reaches the same rules at their own sites, in their original
//! order. Architect-exempt, on every path: the D4 remainder
//! (`pm_guard_bash::evaluate_d4_floor`). [`ArchitectGate`] is the one
//! identity answer for the D4 remainder and the D5 call sites in `pm_guard`;
//! it asks [`architect_main_thread`] at most once, and only when a rule
//! would deny. A D4 deny names the identity check that failed (#8878 PR-I).
//! FAIL-CLOSED: the exemption is granted only when
//! [`architect_main_thread`] establishes the identity; every failure
//! there is "not the Architect", so the rule denies.
//! Test: `pm_guard_floor_tests.rs`; end to end in
//! `tests/tm_hook_pm_guard_trust_anchor_8878.rs`.

use std::cell::OnceCell;
use std::path::Path;

use serde_json::Value;
use trusty_mpm::core::config::MpmConfig;

use crate::commands::pm_guard_architect_reason::{
    NotArchitect, architect_main_thread, with_identity,
};
use crate::commands::pm_guard_bash::{
    GitProbe, LiveGit, evaluate_d4_floor, evaluate_destructive_delete_command,
    unclassifiable_command,
};
use crate::commands::pm_guard_deny_log::{DenyContext, audit_denied_tool};
use crate::commands::pm_guard_response::build_pm_guard_deny_response;
use crate::commands::pm_guard_secret_env_files::evaluate_env_plist_read;
use crate::commands::pm_guard_secret_read::evaluate_secret_file_read;
use crate::commands::pm_guard_trust_anchor::{self, HookEnv, TRUST_ANCHOR_RULE};

/// The rule name recorded with a D4-remainder deny.
pub(crate) const D4_FLOOR_RULE: &str = "d4-floor";

/// Whether this call is the process-bound Architect's main thread, asked once.
///
/// Why: the D4 remainder and the D5 rules share one exemption, and the
/// identity walks the process table, so it is resolved lazily and cached.
/// What: [`Self::identity`] runs [`architect_main_thread`] over the payload,
/// the hook environment and the user config on first use, and caches the
/// verdict with its reason.
/// Test: `the_architect_is_exempt_from_the_d4_remainder`,
/// `every_identity_failure_denies_the_d4_remainder`.
pub(crate) struct ArchitectGate<'a> {
    payload: &'a Value,
    env: HookEnv,
    config: Box<dyn Fn() -> MpmConfig + 'a>,
    verdict: OnceCell<Result<(), NotArchitect>>,
}

impl<'a> ArchitectGate<'a> {
    /// The gate over this process's environment and the user config.
    pub(crate) fn ambient(payload: &'a Value) -> Self {
        Self::new(payload, HookEnv::ambient(), MpmConfig::load_default)
    }

    /// The gate over explicit inputs.
    pub(crate) fn new(
        payload: &'a Value,
        env: HookEnv,
        config: impl Fn() -> MpmConfig + 'a,
    ) -> Self {
        Self {
            payload,
            env,
            config: Box::new(config),
            verdict: OnceCell::new(),
        }
    }

    /// Whether the call is the Architect's main thread (#8878, ruling A).
    pub(crate) fn is_architect(&self) -> bool {
        self.identity().is_ok()
    }

    /// [`Self::is_architect`], naming the first failed check (#8878 PR-I).
    pub(crate) fn identity(&self) -> Result<(), NotArchitect> {
        *self
            .verdict
            .get_or_init(|| architect_main_thread(self.payload, &self.env, &*self.config))
    }
}

/// A floor deny: the rule name recorded with it, and the reason printed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FloorDeny {
    pub(crate) rule: &'static str,
    pub(crate) reason: String,
}

/// Print and audit the first floor deny; `true` when the call was denied.
///
/// Why: `pm_guard` calls this before `TRUSTY_MPM_DISABLE_HOOKS` and
/// `TRUSTY_MPM_PM_UNRESTRICTED` are read (#8878, D8).
/// What: [`evaluate_floors`] against the live repository.
/// Test: `the_floors_hold_under_each_bypass` (integration).
pub(crate) async fn deny_floors(
    url: &str,
    payload: &Value,
    hook_cwd: &Path,
    gate: &ArchitectGate<'_>,
    bypassed: bool,
) -> bool {
    let Some(deny) = evaluate_floors(payload, hook_cwd, gate, &LiveGit, bypassed) else {
        return false;
    };
    audit_denied_tool(
        &DenyContext::from_payload(url, payload),
        deny.rule,
        &deny.reason,
    )
    .await;
    println!("{}", build_pm_guard_deny_response(&deny.reason));
    true
}

/// The first floor a call hits, or `None`.
///
/// What: see the module doc; a payload with no tool name hits none (the
/// guarded path denies it later). `hook_cwd` places relative paths. The
/// universal floors are evaluated only when `bypassed`.
/// Test: `the_universal_floors_bind_the_architect`,
/// `the_architect_is_exempt_from_the_d4_remainder`,
/// `every_identity_failure_denies_the_d4_remainder`.
pub(crate) fn evaluate_floors(
    payload: &Value,
    hook_cwd: &Path,
    gate: &ArchitectGate<'_>,
    git: &dyn GitProbe,
    bypassed: bool,
) -> Option<FloorDeny> {
    let tool_name = payload.get("tool_name").and_then(Value::as_str)?;
    let command = (tool_name == "Bash").then(|| {
        payload
            .get("tool_input")
            .and_then(|v| v.get("command"))
            .and_then(Value::as_str)
            .unwrap_or_default()
    });
    if bypassed && let Some(deny) = universal_floor(payload, tool_name, command, hook_cwd, gate) {
        return Some(deny);
    }
    // #8878 D4 remainder: the process-bound Architect is exempt.
    if let Some(reason) = command.and_then(|c| evaluate_d4_floor(c, hook_cwd, git))
        && let Err(why) = gate.identity()
    {
        // #8878 PR-I: the deny names the identity check that failed.
        return Some(FloorDeny {
            rule: D4_FLOOR_RULE,
            reason: with_identity(reason, why),
        });
    }
    None
}

/// The existing floors (#8878 D8), universal: the Architect is not exempt,
/// bar the trust-anchor rule's own Architect exemption.
fn universal_floor(
    payload: &Value,
    tool_name: &str,
    command: Option<&str>,
    hook_cwd: &Path,
    gate: &ArchitectGate<'_>,
) -> Option<FloorDeny> {
    let deny = |rule, reason: String| Some(FloorDeny { rule, reason });
    let tool_input = payload.get("tool_input");
    // #8878 fix round: `$'\x2f'` decoding hides a path or program from every
    // rule below, so a command the guard cannot read denies here too, as it
    // does first on the guarded path (#6660).
    if let Some(reason) = command.and_then(unclassifiable_command) {
        return deny("unclassifiable-command", reason.to_string());
    }
    if let Some(reason) = pm_guard_trust_anchor::evaluate(payload, &gate.env, &*gate.config) {
        return deny(TRUST_ANCHOR_RULE, reason);
    }
    // A delete of `$HOME` or `/`, or one whose target cannot be resolved.
    if let Some(class) = command.and_then(|c| evaluate_destructive_delete_command(c, hook_cwd))
        && class.is_floor()
    {
        return deny("destructive-delete", class.reason().to_string());
    }
    // No secret value reaches the transcript.
    if let Some(reason) = evaluate_secret_file_read(tool_name, tool_input)
        .or_else(|| evaluate_env_plist_read(tool_name, tool_input, hook_cwd))
    {
        return deny("secret-file-read", reason);
    }
    None
}

#[cfg(test)]
#[path = "pm_guard_floor_tests.rs"]
mod tests;
