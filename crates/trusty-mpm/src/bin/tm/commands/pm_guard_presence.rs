//! The pm-guard's presence rules (#8878 PR2a).
//!
//! Why: plan.md §3 and plan-pr2-devices.md §1 — a trust-anchor write by the
//! bound Architect needs Bob's proven presence, and the hook cannot show a
//! prompt, so it denies with the remedy (never `ask`) and `tm presence local`
//! leaves a grant the next call reads. The verbs that ask for presence must
//! not be a PM's or an agent's to run.
//! What: [`presence_gate`] turns an Architect anchor write with no valid
//! [`presence::check`] grant into a deny naming `tm presence local`; a valid
//! grant is refreshed and the write passes. [`evaluate_presence_verbs`] finds
//! `tm presence …` and `tm fleet bind` in a Bash command with the walk the
//! `tm fleet relaunch` floor uses; `pm_guard_floor` denies them for every
//! caller but the Architect's main thread, bound or not. [`rc_hint`] notes
//! Remote Control in the deny text; it grants nothing (Bob Q9b).
//! FAIL-CLOSED: a missing home, process, key or grant, and every
//! [`PresenceRefusal`], deny.
//! Test: `pm_guard_presence_tests.rs`.

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;
use trusty_mpm::core::presence::{self, PresenceRefusal};
use trusty_mpm::core::presence_key::{GrantKey, KeySource, is_account_root};

use crate::commands::pm_guard_bash::fleet_relaunch::{VerbHit, Verbs, find_tm_verb};
use crate::commands::pm_guard_trust_anchor::{ANCHOR_ROOT, HookEnv};
use crate::commands::presence_client;
use crate::commands::presence_request::fire_from_hook;

/// The verbs that ask for presence or bind the Architect.
pub(crate) const PRESENCE_VERBS: Verbs = &[&["presence"], &["fleet", "bind"]];

/// The environment variable Claude Code sets in a Remote Control session.
pub(crate) const BRIDGE_ENV: &str = "CLAUDE_CODE_BRIDGE_SESSION_ID";

/// The remedy every presence deny carries.
const REMEDY: &str = "Run `tm presence local` from the Architect session at the Studio (Touch \
     ID, or the login password), then retry; the grant lasts while a guarded call uses it at \
     least every 120 minutes. A caller with no one at the Studio's screen (SSH, no GUI session, \
     a subagent) cannot approve, and the guard never asks in the pane.";

/// The deny for an Architect anchor write with no valid grant, or `None`.
///
/// Why/What: see the module doc. `anchor` is the deny the write would get
/// from a non-Architect caller; it stays the first sentence.
/// Test: `an_architect_anchor_write_without_a_grant_is_denied_8878`,
/// `an_architect_anchor_write_with_a_grant_passes_and_refreshes_8878`,
/// `a_grant_idle_121_minutes_denies_the_write_8878`,
/// `hook_request_failure_does_not_change_the_deny_8878`.
pub(crate) fn presence_gate(anchor: String, payload: &Value, env: &HookEnv) -> Option<String> {
    let refusal = live_grant(env).err()?;
    let hint = rc_hint(payload, env);
    // #8878 PR2b-1b: ask enrolled devices; a failure leaves the text as is.
    let asked = env
        .presence_key
        .request(env)
        .map(|id| {
            format!(
                " A device approval was requested ({id}; its action text is labelled \
                 unverified origin); once approved, run `tm presence wait --request {id}`."
            )
        })
        .unwrap_or_default();
    Some(format!(
        "{anchor} Presence required (#8878): this is the Architect's main thread, but {refusal}. \
         {REMEDY}{asked}{hint}"
    ))
}

/// Check the calling `claude`'s grant now, and refresh it when it passes.
fn live_grant(env: &HookEnv) -> Result<(), PresenceRefusal> {
    let home = env.home.as_deref().ok_or(PresenceRefusal::Unreadable)?;
    let root = home.join(ANCHOR_ROOT);
    let claude = env
        .claude
        .get()
        .ok()
        .flatten()
        .ok_or(PresenceRefusal::Unreadable)?;
    let now = presence::unix_secs(std::time::SystemTime::now());
    let mut key = None;
    let grant = presence::check(
        &root,
        &claude,
        || {
            key = env.presence_key.get().ok().flatten();
            key.clone()
        },
        now,
    )?;
    if let Some(key) = key
        && let Err(why) = presence::refresh(&root, &grant, &key, now)
    {
        tracing::warn!("#8878: the presence grant was not refreshed: {why}");
    }
    Ok(())
}

/// " Remote Control …" when the session looks like a Remote Control one.
///
/// Why: Bob Q9b — an RC turn is a user turn, but nothing proves it, so the
/// hint is logged and grants nothing.
/// What: `env.bridge` (the [`BRIDGE_ENV`] variable is set) or a non-null
/// `bridge_status` in the payload. No value is printed.
/// Test: `the_deny_notes_remote_control_without_granting_8878`.
pub(crate) fn rc_hint(payload: &Value, env: &HookEnv) -> &'static str {
    let status = payload.get("bridge_status").is_some_and(|v| !v.is_null());
    if env.bridge || status {
        " Note: Remote Control appears active for this session; that is not presence and grants \
         nothing."
    } else {
        ""
    }
}

/// The deny for a command that runs a presence verb, before the identity is
/// asked; `None` when it runs none.
///
/// Test: `every_spelling_of_a_presence_verb_is_found_8878`.
pub(crate) fn evaluate_presence_verbs(command: &str) -> Option<String> {
    let what = match find_tm_verb(command, PRESENCE_VERBS)? {
        VerbHit::Runs => "runs `tm presence` or `tm fleet bind`".to_owned(),
        VerbHit::Unresolved(word) => {
            format!("runs `tm` with `{word}` where the subcommand is chosen")
        }
        VerbHit::Opaque(why) => format!("names `tm`, and {why}"),
        VerbHit::Unreadable(_) => "names `tm` but cannot be parsed".to_owned(),
    };
    Some(format!(
        "Trust-anchor write denied (#8878): this command {what}. `tm presence` and `tm fleet \
         bind` mint presence grants and launch records in `~/.trusty-mpm/architect-launch/`, so \
         only the Architect session's main thread may run them — no PM, agent or subagent."
    ))
}

/// The presence lookups a [`HookEnv`] carries: the grant key, and (#8878
/// PR2b-1b) the device-approval request. [`Default`] fails closed and
/// requests nothing.
#[derive(Clone)]
pub(crate) struct KeyLookup(Arc<KeyLookupFn>, Arc<RequestFn>);

/// The closure a [`KeyLookup`] runs.
type KeyLookupFn = dyn Fn() -> Result<Option<GrantKey>, String> + Send + Sync;

/// The device-approval request: the pending id, or `None`.
type RequestFn = dyn Fn(&HookEnv) -> Option<String> + Send + Sync;

impl KeyLookup {
    /// A lookup running `f`.
    pub(crate) fn new(
        f: impl Fn() -> Result<Option<GrantKey>, String> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(f), Arc::new(|_| None))
    }

    /// This lookup, asking for device approvals with `request`.
    pub(crate) fn with_request(
        self,
        request: impl Fn(&HookEnv) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self(self.0, Arc::new(request))
    }

    /// Ask for a device approval for the `claude` above `env`.
    pub(crate) fn request(&self, env: &HookEnv) -> Option<String> {
        (self.1)(env)
    }

    /// The process's key source under `home` ([`KeySource::ambient`]).
    pub(crate) fn live(home: Option<&Path>) -> Self {
        let Some(root) = home.map(|h| h.join(ANCHOR_ROOT)) else {
            return Self::default();
        };
        let account = is_account_root(&root);
        let source = KeySource::ambient(&root);
        let lookup = Self::new(move || source.load().map_err(|e| e.to_string()));
        if !account {
            return lookup;
        }
        // #8878 PR2b-1b: only the real home reaches the operator's daemon.
        lookup.with_request(|env| {
            fire_from_hook(env, &presence_client::live(presence_client::HOOK_TIMEOUT))
        })
    }

    /// Run the lookup.
    pub(crate) fn get(&self) -> Result<Option<GrantKey>, String> {
        (self.0)()
    }
}

impl Default for KeyLookup {
    fn default() -> Self {
        Self::new(|| Err("no presence key source was supplied".to_owned()))
    }
}

impl std::fmt::Debug for KeyLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeyLookup")
    }
}

#[cfg(test)]
#[path = "pm_guard_presence_tests.rs"]
mod tests;
