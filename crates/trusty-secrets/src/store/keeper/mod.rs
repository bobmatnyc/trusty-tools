//! [`KeeperBackend`]: a [`SecretBackend`] over Keeper Commander, `keeper`
//! (#7519 P3, DOC-74 §8.2, §15.4).
//!
//! Why: a project may keep its values in Keeper (DOC-74 §6.1). Commander is
//! the only Keeper CLI with a write path that keeps the value out of argv
//! (Architect ruling 1, 2026-10-07): a batch of commands read from stdin.
//! `ksm` is not used. The shared runner in [`super::cli`] closes every other
//! leak; this module decides which `keeper` commands run, and in what
//! order.
//! What, per operation (`<v>` is the trusty vault name, which is also the
//! Keeper folder path, e.g. `trusty/acme/web`; `<uid>` a listed uid). Every
//! call is `keeper --config <file> --batch-mode ...`:
//! - every operation first confirms the folder: it lists `/`, then each
//!   prefix of `<v>`, and needs exactly one folder named exactly as the
//!   next segment. `ls` of a missing path may exit 0 with the parent's
//!   matching entries, so only this walk proves the folder exists. A
//!   segment a successful listing lacks makes the folder absent (ruling 3);
//!   any failed listing is an error, and no stderr text is ever a miss.
//!   Then it lists the folder, `ls --format json <v>`, and keeps the record
//!   rows titled with the key. A row of another type than `login` is
//!   refused, so no other record is read, edited or removed.
//! - `get`: no row is `Ok(None)`; one row is `get --format json -- <uid>`,
//!   whose record must be that uid, title and type; two are an error.
//! - `set`: an absent folder is an error. No row is a `record-add`, one row
//!   a `record-update`, each one line on stdin (`keeper ... -`) with the
//!   value as `$BASE64:`. Then the folder is listed again and the record
//!   read back; anything but exactly that record holding exactly the value
//!   is an error (ruling 3: fail closed).
//! - `delete`: `rm --force -- <uid>` for each row, then a listing must no
//!   longer show any of them. `rm` moves a record to Keeper's trash, which
//!   satisfies the delete sweep (ruling 5). No row is `Ok(false)`.
//! - `list_names` is not implemented: the names-only index lists keys.
//!
//! Headless use needs a person to approve the device and turn on persistent
//! login once (ruling 6); until then every call is
//! [`SecretsError::BackendLocked`], and stdin is never a terminal, so
//! `keeper` cannot prompt. Provisional and shims-only: no run against a real
//! account has pinned an output shape, an exit status, or a marker phrase.
//! Test: `keeper_tests.rs` beside this module, and `server/keeper_tests.rs`
//! for the server path.

mod markers;
mod record;
mod settings;
#[cfg(test)]
pub(crate) mod shim;

#[cfg(test)]
mod keeper_tests;

use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;

use super::cli::{CliCommand, CliRun, CliSpec, Verdict};
use super::config::{self, MachineSecretsConfig};
use super::{Capabilities, SecretBackend};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};
use record::{Entry, Listed};

pub use settings::{DEFAULT_TIMEOUT, KeeperSettings};

/// The runner's facts about `keeper`: id, program, hints and markers.
const SPEC: CliSpec = CliSpec::new(BackendId::KEEPER, "keeper", DEFAULT_TIMEOUT)
    .with_hints(
        "install Keeper Commander and set `secrets.keeper.program` in the machine config \
         to the absolute path of `keeper`, or select another secrets backend",
        // #7519: ruling 6 — the error names the one-time human step.
        "a person must first approve this device and turn on persistent login with Keeper \
         Commander (`this-device register`, `this-device persistent-login on`) using the \
         config file `secrets.keeper.config_path` names, then log in again after an idle \
         timeout",
    )
    .with_markers(markers::MISSING, markers::LOCKED);

const AMBIGUOUS: &str = "more than one Keeper record has this key's title; none was used";
const AMBIGUOUS_FOLDER: &str = "more than one Keeper folder has a name on this vault's path";
const FOREIGN: &str =
    "a Keeper record with this key's title is not a login record; it was left alone";
const NO_FOLDER: &str = "this Keeper account has no folder at this vault's path; create it first";
const UNCONFIRMED_SET: &str = "Keeper did not confirm the write: the record read back differs";
const UNCONFIRMED_DELETE: &str = "Keeper did not confirm the delete: the record is still listed";

/// The Keeper backend.
///
/// Why: see the module docs.
/// What: stateless apart from its settings; every call runs `keeper`
/// afresh, so no value is cached.
/// Test: `keeper_capabilities_are_read_write_and_list_names_spawns_nothing`.
#[derive(Debug)]
pub struct KeeperBackend {
    settings: KeeperSettings,
}

/// What the folder listing said about a key.
enum Lookup {
    /// A successful listing shows no folder at the vault's path.
    NoFolder,
    /// The `login` rows titled with the key.
    Rows(Vec<Listed>),
}

/// [`SecretsError::CliNotInstalled`] for `program`, with the pin hint.
fn not_installed(program: &OsStr) -> SecretsError {
    SecretsError::CliNotInstalled {
        program: program.to_string_lossy().into_owned(),
        hint: SPEC.install_hint,
    }
}

impl KeeperBackend {
    /// A backend invoking `keeper` as `settings` says.
    pub fn new(settings: KeeperSettings) -> Self {
        Self { settings }
    }

    /// One `keeper` invocation, global flags first.
    ///
    /// What: a `program` that is not absolute is
    /// [`SecretsError::CliNotInstalled`], and nothing is spawned.
    /// Test: `keeper_program_must_be_an_absolute_executable_machine_pin`.
    fn command(&self, vault: &VaultName, key: &SecretKey) -> Result<CliCommand, SecretsError> {
        let s = &self.settings;
        // #7519: a bare or relative name resolves through `PATH` or the
        // working directory at spawn, where a planted `keeper` answers.
        if !Path::new(&s.program).is_absolute() {
            return Err(not_installed(&s.program));
        }
        Ok(CliCommand::new(SPEC)
            .program(&s.program)
            .args(&s.leading_args)
            .timeout(s.timeout)
            .target(vault, key)
            .arg("--config")
            .arg(&s.config_path)
            // #7519: never a prompt; stdin is a batch or nothing.
            .arg("--batch-mode"))
    }

    fn failed(&self, vault: &VaultName, key: &SecretKey, reason: &'static str) -> SecretsError {
        SecretsError::Backend {
            backend: BackendId::KEEPER.to_string(),
            vault: vault.to_string(),
            key: key.to_string(),
            reason: reason.to_string(),
        }
    }

    /// The error a run that is not `Ok` maps to.
    fn run_error(&self, run: CliRun, vault: &VaultName, key: &SecretKey) -> SecretsError {
        match run.into_value() {
            Err(e) => e,
            Ok(_) => self.failed(vault, key, "the CLI's answer could not be interpreted"),
        }
    }

    /// `ls --format json <path>`.
    ///
    /// What: a locked run, and a run that exited 0 with a locked phrase on
    /// stdout instead of a listing, are [`SecretsError::BackendLocked`]. Any
    /// other failure, including exit 0 with text that is not a listing, is
    /// an error too: never a miss (ruling 3).
    fn list(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        path: &str,
    ) -> Result<Vec<Entry>, SecretsError> {
        let run = self
            .command(vault, key)?
            .args(["ls", "--format", "json", path])
            .run()?;
        match run.verdict {
            Verdict::Ok => match record::parse_listing(run.stdout.expose()) {
                Ok(entries) => Ok(entries),
                // #7519: ruling 3 — exit 0 with an error on stdout fails.
                Err(_) if markers_in(run.stdout.expose()) => Err(self.locked()),
                Err(reason) => Err(self.failed(vault, key, reason)),
            },
            _ => Err(self.run_error(run, vault, key)),
        }
    }

    fn locked(&self) -> SecretsError {
        SecretsError::BackendLocked {
            backend: BackendId::KEEPER.to_string(),
            hint: SPEC.locked_hint,
        }
    }

    /// The `login` rows titled `key` in the vault's folder, or
    /// [`Lookup::NoFolder`].
    ///
    /// What: the vault's folder is listed only after
    /// [`Self::folder_confirmed`]; a failed listing is an error.
    fn lookup(&self, vault: &VaultName, key: &SecretKey) -> Result<Lookup, SecretsError> {
        // #7519: `ls` of a missing path may exit 0 with the parent's entries
        // matching its last segment, so its success proves no folder.
        if !self.folder_confirmed(vault, key)? {
            return Ok(Lookup::NoFolder);
        }
        let rows = record::titled(self.list(vault, key, vault.as_str())?, key);
        if rows
            .iter()
            .any(|row| row.record_type != record::RECORD_TYPE)
        {
            return Err(self.failed(vault, key, FOREIGN));
        }
        Ok(Lookup::Rows(rows))
    }

    /// Whether listings from the root down show the folder at `vault`'s path.
    ///
    /// What: lists `/`, then each confirmed prefix of the path, and needs
    /// exactly one folder named exactly as the next segment — no case
    /// folding, no glob. A segment no successful listing shows is `false`
    /// (ruling 3); every segment shown once is `true`; two folders with one
    /// name, or a failed listing on the way, are an error.
    /// Test: `keeper_never_touches_the_parent_when_the_vault_folder_is_missing`.
    fn folder_confirmed(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        let mut parent = String::from("/");
        for segment in vault.as_str().split('/') {
            let entries = self.list(vault, key, &parent)?;
            match record::folders_named(&entries, segment) {
                0 => return Ok(false),
                1 => {}
                _ => return Err(self.failed(vault, key, AMBIGUOUS_FOLDER)),
            }
            parent = if parent == "/" {
                segment.to_string()
            } else {
                format!("{parent}/{segment}")
            };
        }
        Ok(true)
    }

    /// The single row: none is `None`, two or more an error.
    fn single(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        mut rows: Vec<Listed>,
    ) -> Result<Option<Listed>, SecretsError> {
        if rows.len() > 1 {
            return Err(self.failed(vault, key, AMBIGUOUS));
        }
        Ok(rows.pop())
    }

    /// `get --format json -- <uid>`, checked against the listed row.
    fn read(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        uid: &str,
    ) -> Result<SecretValue, SecretsError> {
        let uid = record::checked_uid(uid).map_err(|reason| self.failed(vault, key, reason))?;
        let run = self
            .command(vault, key)?
            .args(["get", "--format", "json", "--", uid])
            .run()?;
        match run.verdict {
            Verdict::Ok => record::password_from(run.stdout.expose(), uid, key)
                .map_err(|reason| self.failed(vault, key, reason)),
            // #7519: ruling 3 — a listed record that will not read is an
            // error, never a miss.
            _ => Err(self.run_error(run, vault, key)),
        }
    }

    /// `keeper ... -`, with `batch` on stdin; the value never in argv.
    fn write(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
        batch: &record::Batch,
    ) -> Result<(), SecretsError> {
        // #7519: ruling 1 — stdin only; the runner also refuses the raw
        // and the encoded value in argv and the environment.
        let run = self
            .command(vault, key)?
            .hide(value)
            .hide(&batch.encoded)
            .arg("-")
            .run_with_stdin(&batch.text)?;
        match run.verdict {
            Verdict::Ok => Ok(()),
            _ => Err(self.run_error(run, vault, key)),
        }
    }
}

/// Whether `text` carries a locked phrase.
fn markers_in(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    markers::LOCKED.iter().any(|marker| text.contains(marker))
}

impl SecretBackend for KeeperBackend {
    fn id(&self) -> BackendId {
        BackendId::keeper()
    }

    /// `READ | WRITE`: no `LIST_NAMES`, because the index lists keys (A6).
    fn capabilities(&self) -> Capabilities {
        Capabilities::READ | Capabilities::WRITE
    }

    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        let rows = match self.lookup(vault, key)? {
            Lookup::NoFolder => return Ok(None),
            Lookup::Rows(rows) => rows,
        };
        match self.single(vault, key, rows)? {
            None => Ok(None),
            Some(row) => self.read(vault, key, &row.uid).map(Some),
        }
    }

    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        let rows = match self.lookup(vault, key)? {
            Lookup::NoFolder => return Err(self.failed(vault, key, NO_FOLDER)),
            Lookup::Rows(rows) => rows,
        };
        let existing = self.single(vault, key, rows)?;
        let batch = match &existing {
            None => record::add_command(vault, key, value),
            Some(row) => record::update_command(&row.uid, value),
        }
        .map_err(|reason| self.failed(vault, key, reason))?;
        self.write(vault, key, value, &batch)?;
        // #7519: ruling 3 — exit 0 is not a confirmation; read it back.
        let unconfirmed = || self.failed(vault, key, UNCONFIRMED_SET);
        let Lookup::Rows(rows) = self.lookup(vault, key)? else {
            return Err(unconfirmed());
        };
        let Some(row) = self.single(vault, key, rows)? else {
            return Err(unconfirmed());
        };
        if existing.is_some_and(|before| before.uid != row.uid) {
            return Err(unconfirmed());
        }
        let stored = self.read(vault, key, &row.uid)?;
        if stored.expose() != value.expose() {
            return Err(unconfirmed());
        }
        Ok(())
    }

    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        let rows = match self.lookup(vault, key)? {
            Lookup::NoFolder => return Ok(false),
            Lookup::Rows(rows) => rows,
        };
        if rows.is_empty() {
            return Ok(false);
        }
        // Every copy titled with the key goes, as a delete clears every backend.
        for row in &rows {
            let uid =
                record::checked_uid(&row.uid).map_err(|reason| self.failed(vault, key, reason))?;
            let run = self
                .command(vault, key)?
                .args(["rm", "--force", "--", uid])
                .run()?;
            if run.verdict != Verdict::Ok {
                return Err(self.run_error(run, vault, key));
            }
        }
        // #7519: ruling 3 — confirmed only by a listing without the rows;
        // ruling 5 — a record in Keeper's trash is not listed.
        if let Lookup::Rows(after) = self.lookup(vault, key)?
            && after.iter().any(|a| rows.iter().any(|r| r.uid == a.uid))
        {
            return Err(self.failed(vault, key, UNCONFIRMED_DELETE));
        }
        Ok(true)
    }
}

/// Open the Keeper backend when the machine config enables it.
///
/// Why: #7519 P1 carry-over (a) and ruling 74 — a delete sweeps every
/// enabled CLI backend, so only an enabled one may be written to, and only
/// the machine config may name the program and the Commander config file.
/// What: reads `machine_config`; unless [`MachineSecretsConfig::enables`]
/// `keeper`, [`SecretsError::BackendNotEnabled`]. Then
/// [`KeeperSettings::from_machine`]. Spawns nothing, so `secrets.doctor`
/// can call it.
/// Test: `keeper_open_requires_machine_enablement`.
pub fn open(machine_config: &Path) -> Result<Arc<dyn SecretBackend>, SecretsError> {
    let id = BackendId::keeper();
    let machine: Option<MachineSecretsConfig> = config::load_machine_at(machine_config)?;
    let Some(machine) = machine.filter(|m| m.enables(&id)) else {
        return Err(SecretsError::BackendNotEnabled {
            backend: id.to_string(),
        });
    };
    let settings = KeeperSettings::from_machine(&machine, machine_config)?;
    Ok(Arc::new(KeeperBackend::new(settings)))
}
