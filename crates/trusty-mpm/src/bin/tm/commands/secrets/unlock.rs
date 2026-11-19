//! `tm secrets unlock` and `tm secrets lock`: the 8 h unlock grant (#10524).
//!
//! Why: owner ruling Dd5 — one approval, then no Keychain or 1Password
//! prompt for every secret read. The grant itself lives in the trusty-secrets
//! server; tm is its client.
//! What: [`unlock`] asks the person at this Mac to prove presence (Touch ID,
//! else the login password) and only then calls `secrets.unlock`; a declined
//! or unavailable prompt sends nothing. The server refuses an agent caller
//! on its own. [`lock`] calls `secrets.lock`. Neither prints a value.
//! Test: `unlock_declined_presence_sends_nothing`,
//! `unlock_granted_presence_opens_the_grant_or_reports_the_server`,
//! `lock_with_no_grant_says_so`.

use std::io::Write;

use anyhow::bail;
use serde_json::Value;
use trusty_mpm::core::presence_local::{Authenticator, PresenceOutcome};
use trusty_secrets::api::methods::{LockResponse, UnlockResponse, method};

use super::Ctx;

/// The reason the presence prompt shows.
const REASON: &str = "unlock trusty-secrets for 8 hours";

/// `unlock`: presence through `auth`, then one `secrets.unlock` call.
pub(super) async fn unlock(
    ctx: &Ctx<'_>,
    auth: &dyn Authenticator,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    match auth.authenticate(REASON) {
        PresenceOutcome::Granted => {}
        PresenceOutcome::Declined(why) => {
            bail!("tm secrets unlock: presence declined: {why}; nothing was unlocked")
        }
        PresenceOutcome::Unavailable(why) => {
            bail!("tm secrets unlock: presence unavailable: {why}; nothing was unlocked")
        }
    }
    let answer: UnlockResponse = ctx.call(method::UNLOCK, Value::Null).await?;
    let hours = answer.expires_in_secs / 3600;
    let minutes = answer.expires_in_secs % 3600 / 60;
    writeln!(
        out,
        "unlocked for {hours}h{minutes:02}m: secret reads raise no prompt until then, \
         system sleep, logout, `tm secrets lock`, or a trusty-secrets restart"
    )?;
    Ok(())
}

/// `lock`: one `secrets.lock` call.
pub(super) async fn lock(ctx: &Ctx<'_>, out: &mut dyn Write) -> anyhow::Result<()> {
    let answer: LockResponse = ctx.call(method::LOCK, Value::Null).await?;
    if answer.locked {
        writeln!(out, "locked: the next secret read prompts again")?;
    } else {
        writeln!(out, "no unlock grant was live")?;
    }
    Ok(())
}
