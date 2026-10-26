//! `tm doctor --fix` repair for the managed `.claude.json` (#10041).
//!
//! Why: `tm mcp add` stopped writing credentials into the env block, but an
//! install registered before that still has the value in the file — the exact
//! state #10041 reports. This migrates it out without breaking the server.
//! What: the same MIGRATE-then-strip contract as the LaunchAgent repair
//! (`doctor_launchd_secrets_repair`, whose `import_all` it reuses entry by
//! entry): a registry-mapped value is stored, read back byte-equal, and only
//! then removed from `mcpServers.<server>.env`. The server keeps working
//! because it resolves the same credential from the store at launch. The file
//! is tightened to 0600 BEFORE the rewrite, then replaced by one atomic rename
//! (`write_atomic`), so a failed repair leaves the original bytes. No backup is
//! taken: a backup of this file is a second copy of the credential. Wide
//! `.claude.json*` siblings are tightened to 0600, never deleted.
//! Test: `doctor_claude_json_secrets_tests.rs`.

use std::path::Path;

use serde_json::Value;
use trusty_common::atomic_file::write_atomic;
use trusty_common::credentials::{KeyStore, default_store};
use trusty_common::launchd_secrets::{PlistCredentialEntry, PlistSecret};

use super::doctor_claude_json_secrets::{
    CHECK_NAME, ClaudeJsonFinding, EnvCredential, credential_entries, is_wide, mode_of,
    scan_claude_json,
};
use super::doctor_launchd_secrets_repair::import_all;
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};

/// Repair `<config_dir>/.claude.json` against the default credential store.
///
/// Why/What: see the module doc. A clean dir produces no steps.
/// Test: as [`repair_with_store`].
pub fn repair_claude_json_secrets(config_dir: &Path, mode: RepairMode) -> Vec<RepairStep> {
    repair_with_store(config_dir, mode, default_store().as_ref())
}

/// [`repair_claude_json_secrets`] against an injected store.
///
/// Test: `fix_migrates_the_token_and_keeps_the_server_working`,
/// `fix_leaves_the_file_untouched_when_the_store_refuses`,
/// `fix_leaves_the_original_bytes_when_the_rewrite_fails`,
/// `fix_dry_run_writes_nothing`, `fix_tightens_a_wide_file_and_its_backups`,
/// `fix_is_silent_on_a_clean_dir`.
pub(crate) fn repair_with_store(
    config_dir: &Path,
    mode: RepairMode,
    store: &dyn KeyStore,
) -> Vec<RepairStep> {
    let finding = scan_claude_json(config_dir);
    let mut steps = Vec::new();
    if let Some(why) = &finding.unreadable {
        steps.push(step(
            &finding.path,
            "judge the file for plaintext credentials".to_string(),
            StepStatus::Failed(why.clone()),
        ));
    } else if !finding.migratable.is_empty() {
        steps.push(migrate(&finding, mode, store));
    }
    if !finding.unmapped.is_empty() {
        let keys: Vec<String> = finding.unmapped.iter().map(EnvCredential::label).collect();
        steps.push(step(
            &finding.path,
            format!("move {} out of the MCP env block", keys.join(", ")),
            StepStatus::Refused(
                "no credential provider is registered for it, so there is no store slot to move \
                 it to and removing it would break the server — move it to `.env.local` by hand \
                 and rotate it"
                    .to_string(),
            ),
        ));
    }
    // Re-read: the migration tightens the file itself.
    if mode_of(&finding.path).is_some_and(is_wide) {
        steps.push(tighten(&finding.path, mode, ""));
    }
    for (sibling, _) in &finding.wide_siblings {
        steps.push(tighten(
            sibling,
            mode,
            "; it may hold an older plaintext copy — delete it by hand once the credential is \
             rotated",
        ));
    }
    steps
}

/// One step against `path`.
fn step(path: &Path, what: String, status: StepStatus) -> RepairStep {
    RepairStep {
        check: CHECK_NAME,
        path: path.to_path_buf(),
        what,
        status,
    }
}

/// The one-line description both modes print.
fn describe(f: &ClaudeJsonFinding) -> String {
    let keys: Vec<String> = f.migratable.iter().map(EnvCredential::label).collect();
    format!(
        "move {} into the credential store and remove each one confirmed there from the MCP env \
         block (mode {}); no backup is taken (it would be a second copy) — ROTATE the credential, \
         removing it here does not un-expose it",
        keys.join(", "),
        f.mode_text()
    )
}

/// Migrate every mapped credential, then rewrite the file once.
///
/// Why: a credential may leave the file only once the store provably serves
/// it, or the server loses its token. A failed store write or a failed rewrite
/// must leave the original file as it was.
/// What: re-reads the file under the `.claude.json` mutex, imports each entry
/// separately through `import_all` (so two servers sharing a key are judged
/// one at a time), removes only the imported ones, drops an env object left
/// empty, tightens the original to 0600, and publishes with `write_atomic`.
fn migrate(f: &ClaudeJsonFinding, mode: RepairMode, store: &dyn KeyStore) -> RepairStep {
    let what = describe(f);
    if mode == RepairMode::DryRun {
        return step(&f.path, what, StepStatus::Planned);
    }
    let _guard = crate::core::claude_json_guard::lock();
    let fail = |why: String| step(&f.path, what.clone(), StepStatus::Failed(why));

    let text = match std::fs::read_to_string(&f.path) {
        Ok(t) => t,
        Err(e) => return fail(format!("could not read it: {}", e.kind())),
    };
    let mut root: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return fail("it is no longer valid JSON; left untouched".to_string()),
    };

    let mut moved = Vec::new();
    let mut blocked = Vec::new();
    for (cred, value, mapped) in credential_entries(&root) {
        if !mapped {
            continue;
        }
        let entry = PlistCredentialEntry {
            key: cred.key.clone(),
            value: PlistSecret::new(value),
        };
        let (imported, why) = import_all(std::slice::from_ref(&entry), store);
        if imported.is_empty() {
            blocked.extend(why.into_iter().map(|w| format!("{}: {w}", cred.server)));
        } else {
            moved.push(cred);
        }
    }
    if moved.is_empty() {
        return fail(format!(
            "nothing was imported into the credential store, so the file was left untouched: {}",
            blocked.join("; ")
        ));
    }
    for cred in &moved {
        remove_env_key(&mut root, cred);
    }
    let bytes = match serde_json::to_string_pretty(&root) {
        Ok(s) => s,
        Err(e) => {
            return fail(format!(
                "could not serialise the result ({e}); left untouched"
            ));
        }
    };
    // #10041: tighten first, so the atomic rename publishes a 0600 file. A
    // failure here is not swallowed: the caller re-reads the mode after this
    // step and emits a `tighten` step, which reports the chmod failure.
    let _ = set_owner_only(&f.path);
    if let Err(e) = write_atomic(&f.path, bytes.as_bytes()) {
        return fail(format!(
            "could not write the rewritten file ({}); the original is unchanged",
            e.kind()
        ));
    }
    let labels: Vec<String> = moved.iter().map(EnvCredential::label).collect();
    if blocked.is_empty() {
        step(&f.path, what.clone(), StepStatus::Applied { backup: None })
    } else {
        fail(format!(
            "moved {}, but left {} in place",
            labels.join(", "),
            blocked.join("; ")
        ))
    }
}

/// Remove `mcpServers.<server>.env.<key>`, and the env object if left empty.
fn remove_env_key(root: &mut Value, cred: &EnvCredential) {
    let Some(entry) = root
        .get_mut("mcpServers")
        .and_then(|s| s.get_mut(&cred.server))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let now_empty = entry
        .get_mut("env")
        .and_then(Value::as_object_mut)
        .is_some_and(|env| {
            env.remove(&cred.key);
            env.is_empty()
        });
    if now_empty {
        entry.remove("env");
    }
}

/// `chmod 600`.
fn set_owner_only(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Plan or apply `chmod 600` on one file.
fn tighten(path: &Path, mode: RepairMode, note: &str) -> RepairStep {
    let what = format!("tighten the file to 0600{note}");
    if mode == RepairMode::DryRun {
        return step(path, what, StepStatus::Planned);
    }
    match set_owner_only(path) {
        Ok(()) => step(path, what, StepStatus::Applied { backup: None }),
        Err(e) => step(
            path,
            what,
            StepStatus::Failed(format!("chmod failed: {}", e.kind())),
        ),
    }
}
