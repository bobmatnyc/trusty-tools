//! `tm presence devices add|list|revoke` (#8878 PR2b-1b).
//!
//! Why: plan-pr2-devices.md §4 — Bob enrols a passkey device and revokes a
//! lost one from the Studio. Enrol and revoke need fresh local presence:
//! the CLI asks Touch ID first and only then mints the sealed ticket the
//! store and the daemon demand, so a caller that skips the CLI has no ticket.
//! What: [`add`] refuses before any prompt while `[presence] browser_enrol`
//! is off (#8878 PR2b-2, Architect Q4); otherwise it prompts, mints an enrol
//! ticket, checks with the daemon that the registration opens, and prints
//! the one-time enrol link (single use, 10 minutes, Architect Q3). [`list`] reads
//! the sealed records. [`revoke`] prompts, mints a revoke ticket and revokes
//! locally (record first, then every grant the device approved).
//! Test: `presence_verbs_tests.rs`.

use std::path::Path;

use anyhow::Context as _;
use serde_json::json;
use trusty_mpm::core::presence;
use trusty_mpm::core::presence_device;
use trusty_mpm::core::presence_enrol::{TICKET_TTL, TicketPurpose, enrol_url, mint_ticket};
use trusty_mpm::core::presence_key::{GrantKey, UNTRUSTED_NOTICE};
use trusty_mpm::core::presence_local::Authenticator;

use crate::commands::presence::prompt;
use crate::commands::presence_client::Call;

/// What `add` says while the browser enrol page is switched off.
pub(crate) const ENROL_DISABLED: &str = "enrolment via browser is not enabled: \
     `[presence] browser_enrol` is off (owner decision D24) or `rp_origin` is unset; \
     no prompt was shown and no ticket was minted";

fn now() -> u64 {
    presence::unix_secs(std::time::SystemTime::now())
}

/// Prompt, mint an enrol ticket for `name`, print the one-time enrol link.
///
/// Why: #8878 PR2b-2 — the passkey is created in a browser on the device,
/// at `enrol_origin` (the console's tailnet origin, which WebAuthn binds).
/// What: `enrol_origin` `None` (switch off) fails before any prompt. Then
/// Touch ID, the ticket, a `register.start` that proves the daemon will open
/// the registration, and the link from `enrol_url`, which is returned.
/// Test: `devices_add_and_revoke_refuse_without_presence_8878`,
/// `devices_add_prints_the_enrol_link_after_presence_8878`,
/// `devices_add_fails_closed_while_browser_enrol_is_off_8878`.
pub(crate) fn add(
    auth: &dyn Authenticator,
    key: &dyn Fn() -> anyhow::Result<GrantKey>,
    name: &str,
    enrol_origin: Option<&str>,
    call: Call<'_>,
) -> anyhow::Result<String> {
    let origin = enrol_origin.context(ENROL_DISABLED)?;
    prompt(auth, &format!("enrol the presence device \"{name}\""))?;
    let ticket =
        mint_ticket(&key()?, TicketPurpose::Enrol, name, now()).map_err(anyhow::Error::msg)?;
    call("mpm.presence.register.start", json!({ "ticket": ticket }))
        .map_err(anyhow::Error::msg)
        .context("the daemon refused to open the registration")?;
    let url = enrol_url(origin, &ticket).map_err(anyhow::Error::msg)?;
    println!(
        "Open this link on the device to enrol \"{name}\". It works once and expires in {} \
         minutes:\n{url}\n{UNTRUSTED_NOTICE}",
        TICKET_TTL.as_secs() / 60
    );
    Ok(url)
}

/// Print every enrolled device.
///
/// Test: `devices_add_and_revoke_refuse_without_presence_8878`.
pub(crate) fn list(root: &Path, key: &GrantKey) -> anyhow::Result<usize> {
    let rows = presence_device::list(root, key).map_err(|e| anyhow::anyhow!("{e}"))?;
    for (id, found) in &rows {
        match found {
            Ok(d) => println!(
                "{id}  {}  created {}  last used {}  count {}  backup eligible {}  backed up {}",
                d.name, d.created, d.last_used, d.sign_count, d.backup_eligible, d.backup_state
            ),
            Err(e) => println!("{id}  UNUSABLE: {e}"),
        }
    }
    println!("{} device(s). {UNTRUSTED_NOTICE}", rows.len());
    Ok(rows.len())
}

/// Prompt, mint a revoke ticket, revoke `credential_id` under `root`.
///
/// Test: `devices_add_and_revoke_refuse_without_presence_8878`,
/// `devices_revoke_removes_the_device_and_its_grant_8878`.
pub(crate) fn revoke(
    auth: &dyn Authenticator,
    key: &dyn Fn() -> anyhow::Result<GrantKey>,
    root: &Path,
    credential_id: &str,
) -> anyhow::Result<usize> {
    prompt(auth, &format!("revoke the presence device {credential_id}"))?;
    let key = key()?;
    let ticket = mint_ticket(&key, TicketPurpose::Revoke, credential_id, now())
        .map_err(anyhow::Error::msg)?;
    let done = presence_device::revoke(root, &key, &ticket, credential_id, now())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "Revoked {credential_id}; removed {} grant(s).",
        done.grants_removed.len()
    );
    Ok(done.grants_removed.len())
}

#[cfg(test)]
#[path = "presence_verbs_tests.rs"]
mod tests;
