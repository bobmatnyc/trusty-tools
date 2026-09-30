//! `tm env` — edit a dotenv file without printing a value (#8939).
//!
//! Why: owner ruling item 44 — the Architect manages a project's env files,
//! and the pm-guard lets only these two shapes through (Architect rulings
//! Q1-Q4).
//! What: [`EnvAction`] — `set` and `keys`.
//! Test: `cli_parses_env_set_and_keys`.

use std::path::PathBuf;

use clap::Subcommand;

/// Actions for the `env` subcommand.
#[derive(Debug, Subcommand)]
pub(crate) enum EnvAction {
    /// Set one key in a dotenv file, never printing its value.
    ///
    /// The value is read from stdin (a pipe, not a terminal), or from the
    /// login Keychain with `--from-keychain <service> --account <account>`.
    /// A `KEY=value` argument is refused. Writes atomically with mode 0600
    /// and refuses a symlink. Only the bound Architect may run it.
    Set {
        /// The dotenv file.
        path: PathBuf,
        /// The key name.
        key: String,
        /// Read the value from this login-Keychain service.
        #[arg(long, value_name = "SERVICE", requires = "account")]
        from_keychain: Option<String>,
        /// The Keychain item's account.
        #[arg(long, value_name = "ACCOUNT", requires = "from_keychain")]
        account: Option<String>,
        /// Refused: the value never goes on the command line.
        #[arg(hide = true)]
        extra: Vec<String>,
    },
    /// Print a dotenv file's key names, never a value.
    ///
    /// Fails with no output when any line is not blank, a comment or a
    /// `KEY=value` assignment. Refuses a symlink or a non-regular file. Only
    /// the bound Architect may run it.
    Keys {
        /// The dotenv file.
        path: PathBuf,
    },
}
