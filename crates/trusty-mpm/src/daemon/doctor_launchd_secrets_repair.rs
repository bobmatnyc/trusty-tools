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
//! 2. For each key the credential REGISTRY maps to a provider, READ the store
//!    first. An absent value is written and READ BACK; only a byte-equal
//!    read-back counts as imported. An equal value counts as imported with no
//!    write. A DIFFERENT value is never overwritten (#8563): it may be a key
//!    the operator already rotated, and the plist copy is the old one. A store
//!    that cannot be read fails closed — no write, no strip.
//! 3. Remove exactly the imported keys, with an ATOMIC write (temp file beside
//!    the target, then rename, mode preserved), so an interrupted repair cannot
//!    leave a truncated plist that launchd refuses to load.
//! 4. Anything not imported — an unregistered key, a conflicting store value, a
//!    store that refused, a read-back that disagreed — stays in the file, and
//!    the step says so. The dry run runs the same store pre-check read-only, so
//!    its plan names the same keys.
//!
//! **No backup is taken**, unlike every other `--fix` repair. A backup of a
//! plist holding a credential is a second user-readable copy of that
//! credential, which is the defect, not a safety net. Atomicity is what makes
//! that safe: the original is byte-identical until the rename, and the rename
//! either happens completely or not at all.
//!
//! Rotation is NOT part of this and cannot be: the value was readable by
//! everything on the host for as long as it sat there, so removing it makes the
//! file safe and the credential still compromised. Every step's text says so,
//! and says the running daemon keeps the old environment until launchd reloads
//! the unit.
//!
//! Test: `doctor_launchd_secrets_tests.rs`, `doctor_launchd_secrets_repair_tests.rs`.

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
/// `repair_is_idempotent`, `a_different_store_value_is_never_overwritten`,
/// `an_equal_store_value_is_imported_without_a_write`,
/// `a_store_read_error_before_the_write_fails_closed`,
/// `the_applied_step_names_the_reload_commands`.
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
        Err(e) => return vec![listing_failed(home, &e)],
    };

    findings
        .iter()
        .filter(|f| f.actionable())
        .map(|f| repair_one(f, mode, store.as_ref()))
        .collect()
}

/// The one step reported when `~/Library/LaunchAgents` cannot be listed.
pub(crate) fn listing_failed(home: &Path, e: &std::io::Error) -> RepairStep {
    RepairStep {
        check: CHECK_NAME,
        path: home.join("Library/LaunchAgents"),
        what: "remove plaintext credentials from the trusty LaunchAgent plists".to_string(),
        status: StepStatus::Failed(format!("could not list the directory: {}", e.kind())),
    }
}

/// One file's repair.
///
/// Why: the unreadable arm, the unmapped-only arm and the rewrite arm each have
/// to produce a step, so an operator running `--fix` sees the file that could
/// NOT be fixed beside the ones that were.
/// What: the dry run reads the plist and runs the store pre-check read-only, so
/// a key the apply would leave in place is named in the plan too. A step whose
/// plist is (or would be) rewritten carries the reload commands.
/// Test: as [`repair_launchd_plist_secrets`].
pub(crate) fn repair_one(
    finding: &PlistFinding,
    mode: RepairMode,
    store: &dyn KeyStore,
) -> RepairStep {
    let mut what = describe(finding);
    let step = |what: String, status| RepairStep {
        check: CHECK_NAME,
        path: finding.path.clone(),
        what,
        status,
    };

    if let Some(why) = &finding.unreadable {
        return step(what, StepStatus::Failed(why.clone()));
    }
    if finding.migratable.is_empty() {
        return step(
            what,
            StepStatus::Refused(format!(
                "no registered credential to migrate; {} has no provider mapping, so removing \
                 it would disable the feature it configures — move it out by hand and rotate it",
                finding.unmapped.join(", ")
            )),
        );
    }

    // Re-read rather than trusting the scan's copy: `--fix` runs after the
    // report, and a reinstall in between would have changed the file.
    let xml = match std::fs::read_to_string(&finding.path) {
        Ok(xml) => xml,
        Err(e) => {
            let why = format!("could not read it: {}", e.kind());
            return step(what, StepStatus::Failed(why));
        }
    };
    let entries = match credential_entries(&xml) {
        Ok(entries) => entries,
        Err(e) => return step(what, StepStatus::Failed(e.reason)),
    };
    if entries.is_empty() {
        return step(
            what,
            StepStatus::Refused(
                "the plist no longer holds a credential — nothing to remove".to_string(),
            ),
        );
    }

    if mode == RepairMode::DryRun {
        // #8563: the plan runs the same pre-check the apply does, read-only.
        let (importable, blocked) = plan_all(&entries, store);
        if importable == 0 {
            return step(
                what,
                StepStatus::Refused(format!(
                    "nothing would be imported into the credential store, so the plist would \
                     be left untouched: {}",
                    blocked.join("; ")
                )),
            );
        }
        if !blocked.is_empty() {
            what.push_str(&format!("; leaves in place: {}", blocked.join("; ")));
        }
        what.push_str(&reload_note(&finding.path));
        return step(what, StepStatus::Planned);
    }

    let (imported, blocked) = import_all(&entries, store);
    if imported.is_empty() {
        return step(
            what,
            StepStatus::Failed(format!(
                "nothing was imported into the credential store, so the plist was left \
                 untouched: {}",
                blocked.join("; ")
            )),
        );
    }

    let scrubbed = match scrub_plist_keys(&xml, &imported) {
        Ok(scrubbed) => scrubbed,
        Err(e) => return step(what, StepStatus::Failed(e.reason)),
    };
    if let Err(e) = write_atomic(&finding.path, scrubbed.xml.as_bytes()) {
        return step(
            what,
            StepStatus::Failed(format!(
                "could not write the scrubbed plist ({}); the original is unchanged",
                e.kind()
            )),
        );
    }
    what.push_str(&reload_note(&finding.path));
    if blocked.is_empty() {
        // No backup, deliberately — see the module header.
        step(what, StepStatus::Applied { backup: None })
    } else {
        step(
            what,
            StepStatus::Failed(format!(
                "migrated and removed {}, but left {} in place",
                imported.join(", "),
                blocked.join("; ")
            )),
        )
    }
}

/// What the store pre-check decided for one plist entry.
enum Precheck {
    /// The store holds nothing for `provider`: write, then read back.
    Write(&'static str),
    /// The store already holds this exact value: imported, no write.
    AlreadyStored,
    /// The entry stays in the plist, for this value-free reason.
    Blocked(String),
}

/// Decide one entry's fate by READING the store, never writing it.
///
/// Why (#8563): `set` overwrites. An operator who already stored a rotated key
/// would lose it to the old plist value, and the read-back would then match
/// and report success. Reading first is the only way to know.
/// What: an unregistered key or an empty entry is blocked; an absent (or empty)
/// store value means write; an equal one means already stored; a different one
/// is blocked and left in place; a read error is blocked (fail-closed). Every
/// reason names the key, the provider and an error kind — never a value.
/// Test: `a_different_store_value_is_never_overwritten`,
/// `an_equal_store_value_is_imported_without_a_write`,
/// `a_store_read_error_before_the_write_fails_closed`.
fn precheck(entry: &PlistCredentialEntry, store: &dyn KeyStore) -> Precheck {
    let key = &entry.key;
    let Some(provider) = provider_for_env_var(key) else {
        return Precheck::Blocked(format!(
            "{key}: no credential provider is registered for it"
        ));
    };
    if entry.value.is_empty() {
        return Precheck::Blocked(format!("{key}: the plist entry is empty"));
    }
    match store.try_get(provider) {
        Ok(None) => Precheck::Write(provider),
        Ok(Some(existing)) if existing.is_empty() => Precheck::Write(provider),
        Ok(Some(existing)) if existing == entry.value.expose() => Precheck::AlreadyStored,
        Ok(Some(_)) => Precheck::Blocked(format!(
            "{key}: the store already holds a different {provider} credential; left in place"
        )),
        Err(e) => Precheck::Blocked(format!(
            "{key}: the store could not be read before the write ({}); nothing was written",
            kind_of(&e)
        )),
    }
}

/// The dry run's view: how many entries would be imported, and why the rest
/// would stay.
fn plan_all(entries: &[PlistCredentialEntry], store: &dyn KeyStore) -> (usize, Vec<String>) {
    let mut importable = 0;
    let mut blocked = Vec::new();
    for entry in entries {
        match precheck(entry, store) {
            Precheck::Write(_) | Precheck::AlreadyStored => importable += 1,
            Precheck::Blocked(why) => blocked.push(why),
        }
    }
    (importable, blocked)
}

/// Import every registry-mapped entry the pre-check allows, confirming each
/// write by read-back.
///
/// Why (#8236 item 2): a plist entry may be stripped only once its value is
/// provably retrievable from the store. `set` returning `Ok` is not that proof
/// — a backend can accept a write it cannot serve back. #8563: and `set` runs
/// only when the store held nothing — see [`precheck`].
/// What: returns `(keys confirmed imported, one reason per key that was not)`.
/// Never logs, returns or formats a value: a read-back mismatch reports the KEY
/// and the word "mismatch", never either side of the comparison.
/// Test: `repair_migrates_then_removes`,
/// `repair_leaves_the_plist_untouched_when_the_import_fails`,
/// `repair_keeps_an_unmapped_key_and_says_so`,
/// `a_different_store_value_is_never_overwritten`.
fn import_all(
    entries: &[PlistCredentialEntry],
    store: &dyn KeyStore,
) -> (Vec<String>, Vec<String>) {
    let mut imported = Vec::new();
    let mut blocked = Vec::new();
    for entry in entries {
        let provider = match precheck(entry, store) {
            Precheck::Write(provider) => provider,
            Precheck::AlreadyStored => {
                imported.push(entry.key.clone());
                continue;
            }
            Precheck::Blocked(why) => {
                blocked.push(why);
                continue;
            }
        };
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
    format!(
        "migrate {} into the credential store and remove each key confirmed there from \
         EnvironmentVariables (mode {}) — no backup is taken (a backup would be a second \
         readable copy); ROTATE the credential, removing it here does not un-expose it",
        if finding.migratable.is_empty() {
            "nothing".to_string()
        } else {
            finding.migratable.join(", ")
        },
        finding.mode_text(),
    )
}

/// How to make the running daemon drop the removed credential.
///
/// Why (#8563): launchd hands a job the environment it loaded, so the daemon
/// keeps the stripped value until the unit is unloaded and loaded again, and
/// `kickstart -k` restarts the process from the SAME loaded definition. An
/// operator told only "removed" would restart it and believe it done.
/// What: `bootout` + `bootstrap` for the plist's label — its file stem, which
/// is the label every trusty installer writes.
/// Test: `the_applied_step_names_the_reload_commands`.
fn reload_note(path: &Path) -> String {
    let label = path.file_stem().map_or_else(
        || "<label>".to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    format!(
        "; the running daemon keeps its old environment, credential included, until launchd \
         reloads the unit: `launchctl bootout gui/$(id -u)/{label}` then `launchctl bootstrap \
         gui/$(id -u) {}` — `launchctl kickstart -k` does NOT reload the environment",
        path.display()
    )
}

#[cfg(test)]
#[path = "doctor_launchd_secrets_repair_tests.rs"]
mod tests;
