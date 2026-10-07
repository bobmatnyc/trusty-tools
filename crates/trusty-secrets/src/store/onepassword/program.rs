//! Where the 1Password backend finds `op` (#7519).
//!
//! Why: a bare `op` resolves through the `PATH` of whichever process spawned
//! the server. A planted `op` — `./node_modules/.bin/op`, or any `op` under
//! a relative or empty `PATH` entry, which names the working directory —
//! would receive item templates, values inside (Architect ruling, fix-bar).
//! What: [`OnePasswordSettings::resolve_program`] runs once, at open: a
//! machine pin is kept as given, otherwise [`find_on_path`] searches the
//! absolute entries of a `PATH` value the caller hands in (both now in
//! `store/cli/program.rs`). The binary reads
//! that value at start, beside the token; tests pass their own, so no test
//! reads or changes the process's `PATH`. [`is_executable_file`] is the bar
//! both a pin and a candidate pass, and [`not_installed`] the error when
//! nothing does.
//! Test: `path_tests.rs` beside this module, and
//! `onepassword_machine_program_pin_is_used_and_must_be_absolute`.

use std::ffi::OsStr;
use std::path::Path;

use super::{OnePasswordSettings, SPEC};
use crate::api::SecretsError;
// #7519 P3: the generic search moved to the shared CLI module.
pub(crate) use crate::store::cli::{find_on_path, is_executable_file};

/// The program name searched for on `PATH`.
pub(crate) const PROGRAM: &str = "op";

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
    /// result means no later `PATH` change can redirect a call.
    /// What: an absolute `program` — a machine pin
    /// [`Self::from_machine`] already checked, or one a test set — is kept
    /// as given. Otherwise `program` becomes the [`find_on_path`] hit for
    /// [`PROGRAM`] in `search_path`; no hit is [`not_installed`].
    /// Test: `onepassword_path_search_skips_relative_empty_and_dot_entries`,
    /// `onepassword_path_without_an_absolute_op_is_cli_not_installed`.
    pub(crate) fn resolve_program(
        mut self,
        search_path: Option<&OsStr>,
    ) -> Result<Self, SecretsError> {
        if Path::new(&self.program).is_absolute() {
            return Ok(self);
        }
        match find_on_path(PROGRAM, search_path) {
            Some(found) => {
                self.program = found.into_os_string();
                Ok(self)
            }
            None => Err(not_installed(OsStr::new(PROGRAM))),
        }
    }
}
