//! `tm secrets unlock`, `lock` and `migrate`: the 8 h unlock grant and the
//! vault blob it reads once (#10524).
//!
//! Why: owner ruling Dd5 — one approval, then no Keychain or 1Password
//! prompt for every secret read. The grant itself lives in the trusty-secrets
//! server; tm is its client.
//! What: [`unlock`] asks the person at this Mac to prove presence (Touch ID,
//! else the login password) and only then calls `secrets.unlock`; a declined
//! or unavailable prompt sends nothing. The server refuses an agent caller
//! on its own. [`lock`] calls `secrets.lock`. [`migrate`] calls
//! `secrets.migrate` for one step and prints counts and key names. None
//! prints a value.
//! Test: `unlock_declined_presence_sends_nothing`,
//! `unlock_granted_presence_opens_the_grant_or_reports_the_server`,
//! `lock_with_no_grant_says_so`, `migrate_without_a_blob_store_is_refused`.

use std::io::Write;

use anyhow::bail;
use serde_json::Value;
use trusty_mpm::core::presence_local::{Authenticator, PresenceOutcome};
use trusty_secrets::api::methods::{
    LockResponse, MigrateRequest, MigrateResponse, MigrateStep, UnlockResponse, method,
};

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
    // #10524: in the per-item layout each Keychain item still prompts once.
    if !answer.blob_held {
        writeln!(
            out,
            "note: the Keychain is not migrated; each item prompts once. \
             Run `tm secrets migrate` for one prompt in total"
        )?;
    }
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

/// The migration step the `--finish` / `--rollback` flags name (#10524).
pub(super) fn step_of(finish: bool, rollback: bool) -> MigrateStep {
    match (finish, rollback) {
        (true, _) => MigrateStep::Finish,
        (_, true) => MigrateStep::Rollback,
        _ => MigrateStep::Migrate,
    }
}

/// `migrate`: one `secrets.migrate` call; prints counts and key names.
pub(super) async fn migrate(
    ctx: &Ctx<'_>,
    step: MigrateStep,
    dry_run: bool,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let request = serde_json::to_value(MigrateRequest::new(step, dry_run))?;
    let answer: MigrateResponse = ctx.call(method::MIGRATE, request).await.map_err(|e| {
        anyhow::anyhow!(
            "{e}; the `secrets.migrate` record in the trusty-secrets audit log names the \
             failed step"
        )
    })?;
    let verb = match (step, dry_run) {
        (MigrateStep::Finish, false) => "deleted old items",
        (MigrateStep::Finish, true) => "would delete up to old items",
        (_, false) => "moved items",
        (_, true) => "would move items",
    };
    if answer.already {
        let state = match step {
            MigrateStep::Rollback => "already per-item",
            _ => "already migrated",
        };
        writeln!(out, "{state}: nothing changed ({} items)", answer.items)?;
    } else {
        let source = answer
            .source
            .map(|s| format!(" from the {s}"))
            .unwrap_or_default();
        writeln!(out, "{verb}{source}: {}", answer.items)?;
    }
    for key in &answer.keys {
        writeln!(out, "  {key}")?;
    }
    Ok(())
}
