//! `tm presence devices add|list|revoke` (#8878 PR2b-1b).
//!
//! Why: plan-pr2-devices.md §4 — Bob enrols a passkey device and revokes a
//! lost one from the Studio. Enrol and revoke need fresh local presence:
//! the CLI asks Touch ID first and only then mints the sealed ticket the
//! store and the daemon demand, so a caller that skips the CLI has no ticket.
//! What: [`add`] prompts, mints an enrol ticket and asks the daemon to open
//! the registration; the options it prints need a browser, which arrives
//! with PR2b-2 (Architect Q4: shipped labelled incomplete). [`list`] reads
//! the sealed records. [`revoke`] prompts, mints a revoke ticket and revokes
//! locally (record first, then every grant the device approved).
//! Test: `presence_verbs_tests.rs`.

use std::path::Path;

use anyhow::Context as _;
use serde_json::json;
use trusty_mpm::core::presence;
use trusty_mpm::core::presence_device;
use trusty_mpm::core::presence_enrol::{TicketPurpose, mint_ticket};
use trusty_mpm::core::presence_key::{GrantKey, UNTRUSTED_NOTICE};
use trusty_mpm::core::presence_local::Authenticator;

use crate::commands::presence::prompt;
use crate::commands::presence_client::Call;

/// The label `add` prints until PR2b-2 ships the browser leg.
pub(crate) const ADD_INCOMPLETE: &str = "INCOMPLETE (#8878 PR2b-2): no browser path finishes \
     this enrolment yet; the options below are what the console page will answer";

fn now() -> u64 {
    presence::unix_secs(std::time::SystemTime::now())
}

/// Prompt, mint an enrol ticket for `name`, open the registration.
///
/// Test: `devices_add_and_revoke_refuse_without_presence_8878`,
/// `devices_add_opens_a_registration_after_presence_8878`.
pub(crate) fn add(
    auth: &dyn Authenticator,
    key: &dyn Fn() -> anyhow::Result<GrantKey>,
    name: &str,
    call: Call<'_>,
) -> anyhow::Result<String> {
    prompt(auth, &format!("enrol the presence device \"{name}\""))?;
    let ticket =
        mint_ticket(&key()?, TicketPurpose::Enrol, name, now()).map_err(anyhow::Error::msg)?;
    let reply = call("mpm.presence.register.start", json!({ "ticket": ticket }))
        .map_err(anyhow::Error::msg)
        .context("the daemon refused to open the registration")?;
    let options = serde_json::to_string_pretty(&reply["options"])?;
    println!("{ADD_INCOMPLETE}\n{options}\n{UNTRUSTED_NOTICE}");
    Ok(options)
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
