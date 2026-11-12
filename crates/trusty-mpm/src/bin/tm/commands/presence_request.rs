//! `tm presence request`, and the hook's fire-and-forget request (#8878
//! PR2b-1b).
//!
//! Why: plan-pr2-devices.md §3 step 1 — when the guard denies an Architect
//! anchor write, an enrolled device should be asked to approve it. Asking
//! grants nothing: the daemon records a sealed pending request for this
//! `claude`, and only a verified passkey assertion turns it into a grant.
//! What: [`new_request`] builds the params for the `claude` above the caller;
//! [`request`] sends them through a [`Call`]; [`fire_from_hook`] does the
//! same within [`HOOK_TIMEOUT`](crate::commands::presence_client::HOOK_TIMEOUT)
//! and turns any failure into `None`, so the deny text stays as it was.
//! Test: `presence_verbs_tests.rs`.

use serde_json::{Value, json};
use trusty_mpm::core::presence_key::UNTRUSTED_NOTICE;

use crate::commands::pm_guard_trust_anchor::HookEnv;
use crate::commands::presence::this_claude;
use crate::commands::presence_client::Call;

/// The `mpm.presence.request` params for the `claude` above `env`.
pub(crate) fn new_request(env: &HookEnv, action: &str) -> anyhow::Result<Value> {
    let claude = this_claude(env)?;
    let project = env
        .project_dir
        .as_deref()
        .and_then(|d| std::path::Path::new(d).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(json!({
        "pid": claude.pid,
        "start_time": claude.start_time,
        "project": project,
        "session": "",
        "action": action,
    }))
}

/// Ask the daemon for a device approval of `action` for this `claude`.
///
/// Why/What: see the module doc. Prints the request id, the action as the
/// approver sees it, and the #10865 notice.
/// Test: `presence_request_sends_this_claude_and_prints_the_notice_8878`.
pub(crate) fn request(env: &HookEnv, action: &str, call: Call<'_>) -> anyhow::Result<String> {
    let params = new_request(env, action)?;
    let reply = call("mpm.presence.request", params).map_err(anyhow::Error::msg)?;
    let id = reply["id"].as_str().unwrap_or_default().to_owned();
    println!(
        "Presence request {id}: {} — approve it on an enrolled device (console page, #8878 \
         PR2b-2), then run `tm presence wait --request {id}`.\n{UNTRUSTED_NOTICE}",
        reply["action"].as_str().unwrap_or_default()
    );
    Ok(id)
}

/// The hook's request: the pending id, or `None` on any failure (no
/// `claude`, daemon down, timeout, rate limit). Never changes the verdict.
///
/// Test: `hook_request_failure_does_not_change_the_deny_8878`.
pub(crate) fn fire_from_hook(env: &HookEnv, call: Call<'_>) -> Option<String> {
    let params = new_request(env, "trust-anchor write by the Architect").ok()?;
    match call("mpm.presence.request", params) {
        Ok(reply) => reply["id"].as_str().map(str::to_owned),
        Err(why) => {
            tracing::debug!("#8878: no presence request was made: {why}");
            None
        }
    }
}
