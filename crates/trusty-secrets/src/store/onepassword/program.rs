//! Where the 1Password backend finds `op` (#7519, #7524 P2-M2).
//!
//! Why: a bare `op` resolves through the `PATH` of whichever process spawned
//! the server, and any directory on that `PATH` is the spawner's choice. A
//! planted `op` ahead of the real one would receive item templates, values
//! inside (Architect Decision A, 2026-10-07).
//! What: [`OnePasswordSettings::resolve_program`] runs once, at open: a
//! machine `program` pin is kept as given and overrides everything;
//! otherwise the first executable `op` in a fixed list of system
//! directories — [`crate::store::program::ONEPASSWORD_DIRS`] in production,
//! a test's own list in tests — becomes the program. No `PATH` is read.
//! [`not_installed`] is the error when neither finds one; its hint names the
//! pin and the directories.
//! Test: `path_tests.rs` beside this module, and
//! `onepassword_machine_program_pin_is_used_and_must_be_absolute`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{OnePasswordSettings, SPEC};
use crate::api::SecretsError;
pub(crate) use crate::store::cli::is_executable_file;
use crate::store::program::find_in_dirs;

/// The program name searched for.
pub(crate) const PROGRAM: &str = "op";

/// The install-or-pin hint for a missing `op`, naming the directories
/// searched (DOC-74 §6.2).
// #7524 P2-M2: one per OS, so the hint lists exactly what is searched.
#[cfg(target_os = "macos")]
pub(crate) const INSTALL_HINT: &str = "install the 1Password CLI \
     (https://developer.1password.com/docs/cli/get-started/) in /opt/homebrew/bin, \
     /usr/local/bin or /usr/bin, or set `secrets.onepassword.program` in the machine \
     config ~/.trusty-tools/trusty-common/config.yaml to its absolute path; the \
     server's PATH is not searched";

/// See the macOS hint above (#7524 P2-M2).
#[cfg(not(target_os = "macos"))]
pub(crate) const INSTALL_HINT: &str = "install the 1Password CLI \
     (https://developer.1password.com/docs/cli/get-started/) in /usr/local/bin or \
     /usr/bin, or set `secrets.onepassword.program` in the machine config \
     ~/.trusty-tools/trusty-common/config.yaml to its absolute path; the server's PATH \
     is not searched";

/// [`SecretsError::CliNotInstalled`] for `program`, with the install-or-pin
/// hint.
pub(crate) fn not_installed(program: &OsStr) -> SecretsError {
    SecretsError::CliNotInstalled {
        program: program.to_string_lossy().into_owned(),
        hint: SPEC.install_hint,
    }
}

impl OnePasswordSettings {
    /// These settings with `program` resolved to an absolute path.
    ///
    /// Why: see the module docs. Resolving once, at open, and spawning the
    /// result means nothing later can redirect a call.
    /// What: an absolute `program` — a machine pin
    /// [`Self::from_machine`] already checked, or one a test set — is kept
    /// as given. Otherwise `program` becomes the first executable `op` in
    /// an absolute directory of `dirs`, in order; none is [`not_installed`].
    /// Test: `onepassword_resolves_op_from_the_system_dirs_in_order`,
    /// `onepassword_machine_pin_overrides_the_system_dirs`,
    /// `onepassword_without_op_in_the_system_dirs_names_the_program_pin`.
    pub(crate) fn resolve_program(mut self, dirs: &[PathBuf]) -> Result<Self, SecretsError> {
        if Path::new(&self.program).is_absolute() {
            return Ok(self);
        }
        // #7524 P2-M2: the fixed list only; the spawner's `PATH` is never read.
        match find_in_dirs(PROGRAM, dirs) {
            Some(found) => {
                self.program = found.into_os_string();
                Ok(self)
            }
            None => Err(not_installed(OsStr::new(PROGRAM))),
        }
    }
}
