//! [`OnePasswordBackend`]: a [`SecretBackend`] over the 1Password CLI, `op`
//! (#7519 P2, DOC-74 §8.2, §15.4).
//!
//! Why: a project may keep its values in 1Password (DOC-74 §6.1). `op` is
//! a child process, so every way a value can leak from one — argv, the
//! environment, stderr copied into an error, a template file left on disk —
//! is closed by the shared runner in [`super::cli`]; this module decides
//! only which `op` commands run, and in what order.
//! What, per operation (`<v>` is the trusty vault name, which is also the
//! 1Password vault's name; `<id>` is an id a listing returned):
//! - every operation first runs `op item list --vault <v> --format json`,
//!   which returns titles and ids, and keeps the rows titled with the key.
//!   `op` says "isn't a vault" both for a vault the account lacks and for
//!   one this identity cannot see. A row of another category than
//!   `PASSWORD` is refused, so no other item is read, edited or deleted.
//! - `get`: no vault or no row is `Ok(None)`. One row is
//!   `op read --no-newline op://<vault id>/<id>/password`, the value on
//!   stdout. Two rows are an error.
//! - `set`: no row is `op item create --vault <v> -`, the JSON item template
//!   with the value on stdin. One row is `op item edit <id> --vault <v>
//!   --template <file>`, the template in a 0600 file in a 0700 directory
//!   that is removed on drop (owner ruling 2026-10-07). Never
//!   delete-then-create or create-then-archive. Two rows are an error.
//! - `delete`: `op item delete <id> --vault <v>` for each row; no row is
//!   `Ok(false)`. No vault is [`SecretsError::VaultNotVisible`], never a
//!   miss (#7524 P2-M3): the item may sit in a vault this identity cannot
//!   see, so the delete sweep must keep the index row. The index records no
//!   holding backend, so this also refuses a key that was never in
//!   1Password; the error names the escapes (DOC-74 §8.2).
//! - `list_names` is not implemented: listing goes through the names-only
//!   index, so the capabilities are `READ | WRITE` only (A6).
//!
//! `op` runs by absolute path only: a machine `program` pin as given, or
//! the first `op` in a fixed list of system directories, found once at open
//! (`program.rs`, #7524 P2-M2). No `PATH` is read, and nothing is spawned
//! by bare name.
//!
//! The key never reaches argv: it is the item's title, compared with listed
//! rows and written inside the template. Only the validated vault name,
//! listed ids that pass `checked_id`, and fixed flags do. `--account`
//! and `--config` come from the machine config only; the service-account
//! token only from the runner's environment overlay. Headless with no token
//! and no session, `op` fails and the backend reports
//! [`SecretsError::BackendLocked`]; stdin is never a terminal, so `op`
//! cannot prompt, and nothing falls back to the Keychain or to files.
//!
//! Live check, not yet run: `op` may start a background daemon that caches
//! sessions. If that daemon inherits the runner's stdout or stderr pipe, the
//! runner waits for the pipe to close until the timeout and then kills the
//! process group, so every call would fail slowly. The 1Password live check
//! on a real account verifies that a call returns promptly and that no `op`
//! daemon is left holding a pipe; the stderr phrases in `markers` are pinned
//! by the same check.
//! Test: `onepassword_tests.rs` beside this module, and
//! `server/onepassword_tests.rs` for the server path.

mod item;
mod markers;
mod program;
mod settings;
#[cfg(test)]
pub(crate) mod shim;

#[cfg(test)]
mod onepassword_tests;
#[cfg(test)]
mod path_tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::cli::{CliCommand, CliRun, CliSpec, TemplateFile, Verdict};
use super::config::{self, MachineSecretsConfig};
use super::{Capabilities, SecretBackend};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};
use item::Listed;

pub use settings::{
    DEFAULT_TIMEOUT, OnePasswordSettings, SERVICE_ACCOUNT_TOKEN_ENV, inherited_op_vars, token_from,
};

/// The runner's facts about `op`: id, program, hints and markers.
const SPEC: CliSpec = CliSpec::new(BackendId::ONEPASSWORD, "op", DEFAULT_TIMEOUT)
    .with_hints(
        // #7524 P2-M2: names the system directories searched and the pin.
        program::INSTALL_HINT,
        "unlock the 1Password app with its CLI integration on, run `op signin`, or set \
         OP_SERVICE_ACCOUNT_TOKEN for a headless run",
    )
    .with_markers(markers::MISSING, markers::LOCKED);

const AMBIGUOUS: &str = "more than one 1Password item has this key's title; none was used";
const FOREIGN: &str =
    "a 1Password item with this key's title is not a Password item; it was left alone";
const NO_VAULT: &str = "this account has no 1Password vault with this name; create it first";
const VANISHED: &str = "the 1Password item was removed while it was being updated";

/// The 1Password backend.
///
/// Why: see the module docs.
/// What: stateless apart from its settings; every call runs `op` afresh,
/// so no value is cached. `Debug` shows the settings, never a token.
/// Test: `onepassword_capabilities_are_read_write_and_list_names_spawns_nothing`.
#[derive(Debug)]
pub struct OnePasswordBackend {
    settings: OnePasswordSettings,
}

/// What `op item list` said about a key.
enum Lookup {
    /// `op` shows no vault with this name: it is missing, or this identity
    /// cannot see it.
    NoVault,
    /// The `PASSWORD` rows titled with the key.
    Rows(Vec<Listed>),
}

impl OnePasswordBackend {
    /// A backend invoking `op` as `settings` says.
    pub fn new(settings: OnePasswordSettings) -> Self {
        Self { settings }
    }

    /// One `op` invocation for `vault` and `key`, global flags first.
    ///
    /// What: a `program` that is not absolute is
    /// [`SecretsError::CliNotInstalled`], and nothing is spawned.
    /// Test: `onepassword_without_op_in_the_system_dirs_names_the_program_pin`.
    fn command(&self, vault: &VaultName, key: &SecretKey) -> Result<CliCommand, SecretsError> {
        let s = &self.settings;
        // #7519: a bare or relative name would resolve at spawn, through
        // `PATH` or the working directory, where a planted `op` answers.
        if !Path::new(&s.program).is_absolute() {
            return Err(program::not_installed(&s.program));
        }
        let mut command = CliCommand::new(SPEC)
            .program(&s.program)
            .args(&s.leading_args)
            .timeout(s.timeout)
            .target(vault, key);
        // #7519: non-secret settings stay in argv, never in the env overlay.
        if let Some(account) = &s.account {
            command = command.arg("--account").arg(account);
        }
        if let Some(dir) = &s.config_dir {
            command = command.arg("--config").arg(dir);
        }
        // #7519: A10 — the token reaches the child through the overlay only.
        if let Some(token) = &s.token {
            command = command.env(SERVICE_ACCOUNT_TOKEN_ENV, token.expose());
        }
        Ok(command)
    }

    fn failed(&self, vault: &VaultName, key: &SecretKey, reason: &'static str) -> SecretsError {
        SecretsError::Backend {
            backend: BackendId::ONEPASSWORD.to_string(),
            vault: vault.to_string(),
            key: key.to_string(),
            reason: reason.to_string(),
        }
    }

    /// The error a `Locked` or `Other` run maps to.
    fn run_error(&self, run: CliRun, vault: &VaultName, key: &SecretKey) -> SecretsError {
        match run.into_value() {
            Err(e) => e,
            Ok(_) => self.failed(vault, key, "the CLI's answer could not be interpreted"),
        }
    }

    /// The `PASSWORD` rows titled `key` in `vault`, or [`Lookup::NoVault`].
    fn lookup(&self, vault: &VaultName, key: &SecretKey) -> Result<Lookup, SecretsError> {
        let run = self
            .command(vault, key)?
            .args([
                "item",
                "list",
                "--vault",
                vault.as_str(),
                "--format",
                "json",
            ])
            .run()?;
        let verdict = run.verdict;
        match verdict {
            Verdict::Ok => {
                let rows = item::matching(run.stdout.expose(), key)
                    .map_err(|reason| self.failed(vault, key, reason))?;
                if rows.iter().any(|row| row.category != item::CATEGORY) {
                    return Err(self.failed(vault, key, FOREIGN));
                }
                Ok(Lookup::Rows(rows))
            }
            Verdict::Missing => Ok(Lookup::NoVault),
            _ => Err(self.run_error(run, vault, key)),
        }
    }

    /// The single row titled `key`: none is `None`, two or more an error.
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

    /// `op item create --vault <v> -`, the template on stdin.
    fn create(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        template: &SecretValue,
    ) -> Result<(), SecretsError> {
        // #7519: owner ruling — the value reaches `op` on stdin only.
        let run = self
            .command(vault, key)?
            .args(["item", "create", "--vault", vault.as_str(), "-"])
            .run_with_stdin(template)?;
        let verdict = run.verdict;
        match verdict {
            Verdict::Ok => Ok(()),
            _ => Err(self.run_error(run, vault, key)),
        }
    }

    /// `op item edit <id> --vault <v> --template <file>`.
    fn edit(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        row: &Listed,
        template: &SecretValue,
    ) -> Result<(), SecretsError> {
        let id = item::checked_id(&row.id).map_err(|reason| self.failed(vault, key, reason))?;
        // #7519: the command first, so a refused program writes no file.
        let command = self.command(vault, key)?;
        // #7519: owner ruling — `op item edit` reads a template only from a
        // file; the guard removes it on every return path.
        let file = TemplateFile::create(&self.settings.template_root, template)?;
        let run = command
            .args(["item", "edit", id, "--vault", vault.as_str(), "--template"])
            .arg(file.path())
            .run();
        drop(file);
        let run = run?;
        let verdict = run.verdict;
        match verdict {
            Verdict::Ok => Ok(()),
            Verdict::Missing => Err(self.failed(vault, key, VANISHED)),
            _ => Err(self.run_error(run, vault, key)),
        }
    }
}

impl SecretBackend for OnePasswordBackend {
    fn id(&self) -> BackendId {
        BackendId::onepassword()
    }

    /// `READ | WRITE`: no `LIST_NAMES`, because the index lists keys (A6).
    fn capabilities(&self) -> Capabilities {
        Capabilities::READ | Capabilities::WRITE
    }

    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        let rows = match self.lookup(vault, key)? {
            Lookup::NoVault => return Ok(None),
            Lookup::Rows(rows) => rows,
        };
        let Some(row) = self.single(vault, key, rows)? else {
            return Ok(None);
        };
        let reference = item::op_reference(&row.vault.id, &row.id)
            .map_err(|reason| self.failed(vault, key, reason))?;
        // #7519: A3 — a missing item is `Ok(None)`; locked or unknown is `Err`.
        self.command(vault, key)?
            .args(["read", "--no-newline", reference.as_str()])
            .run()?
            .into_value()
    }

    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        let template =
            item::template(key, value).map_err(|reason| self.failed(vault, key, reason))?;
        let rows = match self.lookup(vault, key)? {
            Lookup::NoVault => return Err(self.failed(vault, key, NO_VAULT)),
            Lookup::Rows(rows) => rows,
        };
        // #7519: owner ruling — find, then create or edit; never replace.
        match self.single(vault, key, rows)? {
            None => self.create(vault, key, &template),
            Some(row) => self.edit(vault, key, &row, &template),
        }
    }

    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        let rows = match self.lookup(vault, key)? {
            // #7524 P2-M3: "isn't a vault" is also what `op` says for a vault
            // this identity cannot see; a miss here let the sweep drop the row.
            // Fix round: the error names both escapes (create the vault, or
            // stop enabling 1Password); the index records no holding backend.
            Lookup::NoVault => {
                return Err(SecretsError::VaultNotVisible {
                    backend: BackendId::ONEPASSWORD.to_string(),
                    vault: vault.to_string(),
                    key: key.to_string(),
                });
            }
            Lookup::Rows(rows) => rows,
        };
        let mut removed = false;
        // Every copy titled with the key goes, as a delete clears every backend.
        for row in rows {
            let id = item::checked_id(&row.id).map_err(|reason| self.failed(vault, key, reason))?;
            let run = self
                .command(vault, key)?
                .args(["item", "delete", id, "--vault", vault.as_str()])
                .run()?;
            let verdict = run.verdict;
            match verdict {
                Verdict::Ok => removed = true,
                // Gone between the listing and the delete: nothing to clear.
                Verdict::Missing => {}
                _ => return Err(self.run_error(run, vault, key)),
            }
        }
        Ok(removed)
    }
}

/// Open the 1Password backend when the machine config enables it.
///
/// Why: #7519 P1 carry-over (a) — a delete sweeps every enabled CLI
/// backend, so only an enabled one may be written to. A tracked project
/// file alone cannot point the server at a 1Password account.
/// What: reads `machine_config`; unless [`MachineSecretsConfig::enables`]
/// the backend, [`SecretsError::BackendNotEnabled`]. Then
/// [`OnePasswordSettings::from_machine`] with `template_root` and `token`,
/// then `op` from the machine `program` pin or the first system directory
/// in `crate::store::program::ONEPASSWORD_DIRS` that holds one — once,
/// here, never per call. No `PATH` is read (#7524 P2-M2). No `op` is
/// [`SecretsError::CliNotInstalled`]. Spawns nothing, so `secrets.doctor`
/// can call it (A9).
/// Test: `onepassword_open_requires_machine_enablement`,
/// `onepassword_op_on_the_spawner_path_is_never_chosen`.
pub fn open(
    machine_config: &Path,
    template_root: &Path,
    token: Option<SecretValue>,
) -> Result<Arc<dyn SecretBackend>, SecretsError> {
    open_in(
        machine_config,
        template_root,
        token,
        &crate::store::program::onepassword_dirs(),
    )
}

/// [`open`], searching `dirs` for `op` instead of the production list.
///
/// Why: a test cannot rely on the host having, or lacking, a real `op` in a
/// system directory, and must never run one.
/// Test: `onepassword_resolves_op_from_the_system_dirs_in_order`.
pub(crate) fn open_in(
    machine_config: &Path,
    template_root: &Path,
    token: Option<SecretValue>,
    dirs: &[PathBuf],
) -> Result<Arc<dyn SecretBackend>, SecretsError> {
    let id = BackendId::onepassword();
    let machine: Option<MachineSecretsConfig> = config::load_machine_at(machine_config)?;
    let Some(machine) = machine.filter(|m| m.enables(&id)) else {
        return Err(SecretsError::BackendNotEnabled {
            backend: id.to_string(),
        });
    };
    let settings = OnePasswordSettings::from_machine(
        &machine,
        machine_config,
        template_root.to_path_buf(),
        token,
    )?
    .resolve_program(dirs)?;
    Ok(Arc::new(OnePasswordBackend::new(settings)))
}
