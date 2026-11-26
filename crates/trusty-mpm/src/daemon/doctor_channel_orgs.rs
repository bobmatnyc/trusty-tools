//! `tm doctor` rows for org channel bindings (#10423, S1b).
//!
//! Why: #10423 acceptance 4: an operator must see, per org, which channel
//! it binds to and whether each credential ref resolves, before a send
//! fails on it. Names only, never a value.
//! What: [`channel_org_rows`] reads `channels/orgs.toml`. No file is one
//! `channel_orgs` `Ok` row (not configured); a file that does not load is one
//! `Fail` row. Otherwise one `channel_org:<org>` row per org: its kind and
//! workspace or space, then `role=present|missing|invalid(code)|store_error`
//! per credential ref, judged by [`RefHealth`] from the names-only index.
//! `Fail` on a missing or invalid ref, `Unknown` on a store fault, else `Ok`.
//! Test: `doctor_channel_orgs_tests.rs`.

use std::path::Path;

use crate::channels::orgs::{ChannelKind, OrgsConfig};
use crate::channels::secret_ref::{RefHealth, StoreResolver};
use crate::channels::send_store::orgs_path;
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The row name for the config as a whole.
pub(crate) const NAME: &str = "channel_orgs";

/// The rows for the process home, with the production resolver.
pub(crate) fn check_channel_orgs() -> Vec<DoctorCheck> {
    let Some(home) = dirs::home_dir() else {
        return vec![DoctorCheck::new(
            NAME,
            CheckStatus::Unknown,
            "the home directory is unknown",
        )];
    };
    match StoreResolver::production() {
        Ok(resolver) => channel_org_rows(&home, &|scope, r| resolver.health(scope, r)),
        Err(e) => channel_org_rows(&home, &|_, _| RefHealth::StoreError(e.clone())),
    }
}

/// The rows for `home`, judging each ref with `health`.
///
/// Test: `doctor_reports_each_org_binding_and_ref_health_by_name`,
/// `doctor_without_orgs_toml_is_ok_and_a_bad_file_fails`.
pub(crate) fn channel_org_rows(
    home: &Path,
    health: &dyn Fn(Option<&str>, &str) -> RefHealth,
) -> Vec<DoctorCheck> {
    let path = orgs_path(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return vec![DoctorCheck::new(
                NAME,
                CheckStatus::Ok,
                format!("no {}: channel sends are not configured", path.display()),
            )];
        }
        Err(e) => {
            return vec![DoctorCheck::new(
                NAME,
                CheckStatus::Fail,
                format!("{} unreadable: {}", path.display(), e.kind()),
            )];
        }
    };
    let cfg = match OrgsConfig::parse_toml(&text) {
        Ok(cfg) => cfg,
        Err(e) => {
            return vec![DoctorCheck::new(
                NAME,
                CheckStatus::Fail,
                format!("{}: {e}", path.display()),
            )];
        }
    };
    cfg.orgs
        .iter()
        .map(|(org, entry)| {
            let binding = match entry.kind {
                ChannelKind::Slack => {
                    format!(
                        "slack workspace {}",
                        entry.workspace.as_deref().unwrap_or("?")
                    )
                }
                ChannelKind::Gchat => {
                    format!("gchat space {}", entry.space.as_deref().unwrap_or("?"))
                }
            };
            let mut status = CheckStatus::Ok;
            let mut refs = Vec::new();
            for (role, r) in &entry.credentials {
                let word = match health(entry.secrets.as_deref(), r) {
                    RefHealth::Present => "present".to_owned(),
                    RefHealth::Missing => {
                        status = CheckStatus::Fail;
                        "missing".to_owned()
                    }
                    RefHealth::Invalid(code) => {
                        status = CheckStatus::Fail;
                        format!("invalid({code})")
                    }
                    RefHealth::StoreError(_) => {
                        if status == CheckStatus::Ok {
                            status = CheckStatus::Unknown;
                        }
                        "store_error".to_owned()
                    }
                };
                refs.push(format!("{role}={word}"));
            }
            if refs.is_empty() {
                status = CheckStatus::Fail;
                refs.push("no credential refs".to_owned());
            }
            DoctorCheck::new(
                format!("channel_org:{org}"),
                status,
                format!("{binding}; {}", refs.join(", ")),
            )
        })
        .collect()
}
