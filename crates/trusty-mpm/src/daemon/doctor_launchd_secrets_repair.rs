//! `tm doctor --fix` repair for credentials in a LaunchAgent plist (#8236).
//!
//! Why: the renderer guard only covers a unit that gets regenerated. A host
//! whose plist is hand-maintained — which is how #8236's `com.trusty.mpm.plist`
//! came to hold two credentials, since no code in this workspace writes that
//! file — is never reached by an install. This is.
//!
//! What: MIGRATE, then strip, and only what was migrated.
//!
//! 1. Read the plist's credential entries.
//! 2. For each key the credential REGISTRY maps to a provider, write the value
//!    into [`trusty_common::credentials::default_store`] and READ IT BACK. Only
//!    a byte-equal read-back counts as imported.
//! 3. Remove exactly the imported keys, with an ATOMIC write (temp file beside
//!    the target, then rename, mode preserved), so an interrupted repair cannot
//!    leave a truncated plist that launchd refuses to load.
//! 4. Anything not imported — an unregistered key, a store that refused, a
//!    read-back that disagreed — stays in the file, and the step says so.
//!
//! **No backup is taken**, unlike every other `--fix` repair. A backup of a
//! plist holding a credential is a second user-readable copy of that
//! credential, which is the defect, not a safety net. Atomicity is what makes
//! that safe: the original is byte-identical until the rename, and the rename
//! either happens completely or not at all.
//!
//! Rotation is NOT part of this and cannot be: the value was readable by
//! everything on the host for as long as it sat there, so removing it makes the
//! file safe and the credential still compromised. Every step's text says so.
//!
//! Test: `doctor_launchd_secrets_tests.rs`.

use std::path::Path;
use std::sync::Arc;

use trusty_common::atomic_file::write_atomic;
use trusty_common::credential_registry::provider_for_env_var;
use trusty_common::credentials::{KeyStore, default_store};
use trusty_common::launchd_secrets::{PlistCredentialEntry, credential_entries, scrub_plist_keys};

use super::doctor_launchd_secrets::{CHECK_NAME, PlistFinding, scan_launch_agents};
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};

/// Rewrite every trusty LaunchAgent that holds a migratable credential.
///
/// Why/What: see the module docs.
/// [`StepStatus::Planned`] under [`RepairMode::DryRun`];
/// [`StepStatus::Applied`] with NO backup once written;
/// [`StepStatus::Refused`] when there is nothing this repair may safely do;
/// [`StepStatus::Failed`] when the plist cannot be parsed, the import cannot be
/// confirmed, or the rewrite cannot be written — never a warning followed by a
/// pass. A clean host produces no steps at all.
/// Test: `repair_plans_without_writing`, `repair_migrates_then_removes`,
/// `repair_leaves_the_plist_untouched_when_the_import_fails`,
/// `repair_keeps_an_unmapped_key_and_says_so`,
/// `repair_refuses_a_binary_plist`,
/// `repair_refuses_a_symlinked_plist`,
/// `repair_fails_loudly_on_an_unparseable_plist`,
/// `repair_fails_loudly_when_the_plist_is_unwritable`,
/// `repair_fails_loudly_when_the_directory_cannot_be_listed`,
/// `repair_produces_no_steps_for_a_clean_host`,
/// `repair_is_idempotent`.
pub fn repair_launchd_plist_secrets(home: &Path, mode: RepairMode) -> Vec<RepairStep> {
    repair_with_store(home, mode, Arc::from(default_store()))
}

/// [`repair_launchd_plist_secrets`] against an injected store.
///
/// Why: every test here migrates into a `MemoryKeyStore`. No test may write to
/// a real Keychain or a real `~/.trusty`, and the read-back confirmation is
/// exactly the behaviour a test double has to be able to break.
/// Test: as [`repair_launchd_plist_secrets`].
pub(crate) fn repair_with_store(
    home: &Path,
    mode: RepairMode,
    store: Arc<dyn KeyStore>,
) -> Vec<RepairStep> {
    let findings = match scan_launch_agents(home) {
        Ok(findings) => findings,
        Err(e) => {
            return vec![RepairStep {
                check: CHECK_NAME,
                path: home.join("Library/LaunchAgents"),
                what: "remove plaintext credentials from the trusty LaunchAgent plists".to_string(),
                status: StepStatus::Failed(format!("could not list the directory: {}", e.kind())),
            }];
        }
    };

    findings
        .iter()
        .filter(|f| f.actionable())
        .map(|f| repair_one(f, mode, store.as_ref()))
        .collect()
}

/// One file's repair.
///
/// Why: the unreadable arm, the unmapped-only arm and the rewrite arm each have
/// to produce a step, so an operator running `--fix` sees the file that could
/// NOT be fixed beside the ones that were.
/// Test: as [`repair_launchd_plist_secrets`].
fn repair_one(finding: &PlistFinding, mode: RepairMode, store: &dyn KeyStore) -> RepairStep {
    let what = describe(finding);
    let step = |status| RepairStep {
        check: CHECK_NAME,
        path: finding.path.clone(),
        what: what.clone(),
        status,
    };

    if let Some(why) = &finding.unreadable {
        return step(StepStatus::Failed(why.clone()));
    }
    if finding.migratable.is_empty() {
        return step(StepStatus::Refused(format!(
            "no registered credential to migrate; {} has no provider mapping, so removing it \
             would disable the feature it configures — move it out by hand and rotate it",
            finding.unmapped.join(", ")
        )));
    }
    if mode == RepairMode::DryRun {
        return step(StepStatus::Planned);
    }

    // Re-read rather than trusting the scan's copy: `--fix` runs after the
    // report, and a reinstall in between would have changed the file.
    let xml = match std::fs::read_to_string(&finding.path) {
        Ok(xml) => xml,
        Err(e) => {
            return step(StepStatus::Failed(format!(
                "could not read it: {}",
                e.kind()
            )));
        }
    };
    let entries = match credential_entries(&xml) {
        Ok(entries) => entries,
        Err(e) => return step(StepStatus::Failed(e.reason)),
    };
    if entries.is_empty() {
        return step(StepStatus::Refused(
            "the plist no longer holds a credential — nothing to remove".to_string(),
        ));
    }

    let (imported, blocked) = import_all(&entries, store);
    if imported.is_empty() {
        return step(StepStatus::Failed(format!(
            "nothing was imported into the credential store, so the plist was left untouched: {}",
            blocked.join("; ")
        )));
    }

    let scrubbed = match scrub_plist_keys(&xml, &imported) {
        Ok(scrubbed) => scrubbed,
        Err(e) => return step(StepStatus::Failed(e.reason)),
    };
    match write_atomic(&finding.path, scrubbed.xml.as_bytes()) {
        // No backup, deliberately — see the module header.
        Ok(()) if blocked.is_empty() => step(StepStatus::Applied { backup: None }),
        Ok(()) => step(StepStatus::Failed(format!(
            "migrated and removed {}, but left {} in place",
            imported.join(", "),
            blocked.join("; ")
        ))),
        Err(e) => step(StepStatus::Failed(format!(
            "could not write the scrubbed plist ({}); the original is unchanged",
            e.kind()
        ))),
    }
}

/// Import every registry-mapped entry, confirming each by read-back.
///
/// Why (#8236 item 2): a plist entry may be stripped only once its value is
/// provably retrievable from the store. `set` returning `Ok` is not that proof
/// — a backend can accept a write it cannot serve back.
/// What: returns `(keys confirmed imported, one reason per key that was not)`.
/// Never logs, returns or formats a value: a read-back mismatch reports the KEY
/// and the word "mismatch", never either side of the comparison.
/// Test: `repair_migrates_then_removes`,
/// `repair_leaves_the_plist_untouched_when_the_import_fails`,
/// `repair_keeps_an_unmapped_key_and_says_so`.
fn import_all(
    entries: &[PlistCredentialEntry],
    store: &dyn KeyStore,
) -> (Vec<String>, Vec<String>) {
    let mut imported = Vec::new();
    let mut blocked = Vec::new();
    for entry in entries {
        let Some(provider) = provider_for_env_var(&entry.key) else {
            blocked.push(format!(
                "{}: no credential provider is registered for it",
                entry.key
            ));
            continue;
        };
        if entry.value.is_empty() {
            blocked.push(format!("{}: the plist entry is empty", entry.key));
            continue;
        }
        if let Err(e) = store.set(provider, entry.value.expose()) {
            blocked.push(format!(
                "{}: the store refused the write ({})",
                entry.key,
                kind_of(&e)
            ));
            continue;
        }
        match store.try_get(provider) {
            Ok(Some(back)) if back == entry.value.expose() => imported.push(entry.key.clone()),
            Ok(_) => blocked.push(format!(
                "{}: the store did not read back what was written (mismatch)",
                entry.key
            )),
            Err(e) => blocked.push(format!(
                "{}: the store could not be read back ({})",
                entry.key,
                kind_of(&e)
            )),
        }
    }
    (imported, blocked)
}

/// A value-free label for a store error.
///
/// Why: `KeyStoreError`'s own `Display` names a path and a backend message.
/// Neither is a credential, but the repair's contract is that it reports kinds,
/// and a kind cannot regress into a disclosure.
fn kind_of(e: &trusty_common::credentials::KeyStoreError) -> &'static str {
    trusty_common::credentials::StoreErrorKind::from(e).as_str()
}

/// The one-line description both modes print.
fn describe(finding: &PlistFinding) -> String {
    let keys = finding.keys();
    format!(
        "migrate {} into the credential store and remove {} from EnvironmentVariables \
         (mode {}) — no backup is taken (a backup would be a second readable copy); \
         ROTATE the credential, removing it here does not un-expose it",
        if finding.migratable.is_empty() {
            "nothing".to_string()
        } else {
            finding.migratable.join(", ")
        },
        if keys.is_empty() {
            "unknown".to_string()
        } else {
            keys.join(", ")
        },
        finding.mode_text(),
    )
}
