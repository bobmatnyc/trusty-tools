//! `tm presence local`: Bob proves presence at the Studio (#8878 PR2a).
//!
//! Why: plan.md §3 — the pm-guard denies the bound Architect's trust-anchor
//! writes until presence is proven, and only a prompt Bob answers can prove
//! it. The verb must grant nothing to anyone else: the pm-guard denies it to
//! PMs and agents (`pm_guard_presence::evaluate_presence_verbs`), and the
//! verb itself refuses, before any prompt, a caller that is not the bound
//! Architect.
//! What: [`local`] checks [`session_binding`] (the same chain the hook runs,
//! over the `claude` above the Bash shell), asks the [`Authenticator`], and on
//! `Granted` mints `architect-launch/<pid>.presence` for that `claude` with
//! the Keychain key ([`mint_local`]). [`prompt`] and [`mint_local`] are shared
//! with `tm fleet bind`. Residual: a Claude Code subagent of the Architect
//! runs under the same `claude` with the same environment; only the hook,
//! which sees the payload's `agent_id`, refuses it.
//! Test: `presence_tests.rs` beside this file.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::presence::{self, METHOD_LOCAL};
use trusty_mpm::core::presence_key::KeySource;
use trusty_mpm::core::presence_local::{Authenticator, LocalAuthenticator, PresenceOutcome};
use trusty_mpm::core::twin_identity::ClaudeProcess;

use crate::cli::{DevicesAction, PresenceAction};
use crate::commands::fleet::status::this_session_env;
use crate::commands::pm_guard_architect_reason::session_binding;
use crate::commands::pm_guard_trust_anchor::{ANCHOR_ROOT, HookEnv};
use crate::commands::{presence_client, presence_devices, presence_request, presence_wait};

/// Run a `tm presence` action.
pub(crate) fn run(action: PresenceAction) -> anyhow::Result<()> {
    let home = dirs::home_dir()
        .filter(|h| h.is_absolute())
        .context("the home directory is unknown")?;
    let root = home.join(ANCHOR_ROOT);
    let env = HookEnv {
        home: Some(home),
        ..this_session_env()
    };
    let source = KeySource::ambient(&root);
    // #8878 PR2b-1b: the device verbs.
    let key = || source.load_or_create().context("the presence key");
    let call = presence_client::live(presence_client::CLI_TIMEOUT);
    match action {
        PresenceAction::Request { action } => {
            presence_request::request(&env, &action, &call).map(drop)
        }
        PresenceAction::Wait { timeout, request } => {
            let claude = this_claude(&env)?;
            let timeout = presence_wait::parse_timeout(&timeout)?;
            let wait_env = presence_wait::WaitEnv {
                root: &root,
                claude,
                key: &|| source.load().ok().flatten(),
                now: &|| presence::unix_secs(std::time::SystemTime::now()),
                sleep: &std::thread::sleep,
            };
            match presence_wait::wait(&wait_env, request.as_deref(), timeout) {
                0 => Ok(()),
                code => std::process::exit(code),
            }
        }
        PresenceAction::Devices { action } => match action {
            DevicesAction::Add { name } => {
                // #8878 PR2b-2: the enrol link needs `[presence] rp_origin`.
                let presence = MpmConfig::load_default().presence;
                let origin = presence.enrol_origin();
                presence_devices::add(&LocalAuthenticator, &key, &name, origin, &call).map(drop)
            }
            DevicesAction::List => presence_devices::list(&root, &key()?).map(drop),
            DevicesAction::Revoke { credential_id } => {
                presence_devices::revoke(&LocalAuthenticator, &key, &root, &credential_id).map(drop)
            }
        },
        PresenceAction::Local => {
            let path = local(
                &env,
                MpmConfig::load_default,
                &LocalAuthenticator,
                &source,
                &root,
            )?;
            println!(
                "Presence granted for the Architect: {} (valid while used at least every 120 minutes).",
                path.display()
            );
            Ok(())
        }
    }
}

/// Prove presence for the bound Architect calling this, and mint its grant.
///
/// Why/What: see the module doc. Every refusal returns `Err` before the
/// authenticator runs (identity) or before anything is written (the prompt).
/// Test: `a_pm_calling_presence_local_is_refused_before_any_prompt_8878`,
/// `a_declined_or_unavailable_prompt_writes_no_grant_8878`,
/// `presence_local_grants_the_bound_architect_8878`.
pub(crate) fn local(
    env: &HookEnv,
    config: impl FnOnce() -> MpmConfig,
    auth: &dyn Authenticator,
    source: &KeySource,
    root: &Path,
) -> anyhow::Result<PathBuf> {
    // #8878 PR2a: only the bound Architect; no prompt for anyone else.
    if let Err(why) = session_binding(env, config) {
        bail!(
            "presence unavailable: this session is not the bound Architect ({why}); no prompt \
             was shown and no grant was written"
        );
    }
    let claude = this_claude(env)?;
    prompt(
        auth,
        &format!(
            "approve trust-anchor writes by the Architect (claude pid {}) for up to 120 idle minutes",
            claude.pid
        ),
    )?;
    mint_local(root, &claude, source)
}

/// The `claude` above the calling shell.
pub(crate) fn this_claude(env: &HookEnv) -> anyhow::Result<ClaudeProcess> {
    env.claude
        .get()
        .map_err(anyhow::Error::msg)?
        .context("no `claude` process is an ancestor of this command")
}

/// Ask `auth`; `Ok` only when the user authenticated.
pub(crate) fn prompt(auth: &dyn Authenticator, reason: &str) -> anyhow::Result<()> {
    match auth.authenticate(reason) {
        PresenceOutcome::Granted => Ok(()),
        PresenceOutcome::Declined(why) => bail!("presence declined: {why}; no grant was written"),
        PresenceOutcome::Unavailable(why) => bail!(
            "presence unavailable: {why}; no grant was written. Run `tm presence local` from the \
             Architect session at the Studio's own screen"
        ),
    }
}

/// Mint a [`METHOD_LOCAL`] grant for `claude` under `root`, making the key
/// on first use.
pub(crate) fn mint_local(
    root: &Path,
    claude: &ClaudeProcess,
    source: &KeySource,
) -> anyhow::Result<PathBuf> {
    let key = source.load_or_create().context("the presence key")?;
    let now = presence::unix_secs(std::time::SystemTime::now());
    presence::mint(root, claude, METHOD_LOCAL, &key, now).map_err(anyhow::Error::msg)
}

#[cfg(test)]
#[path = "presence_tests.rs"]
pub(crate) mod tests;
