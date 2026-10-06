//! `tm secrets` — the CLI over the trusty-secrets socket (#7521, DOC-74 §9).
//!
//! Why: owner rulings of 2026-09-17 make `set` the one upsert verb, with the
//! value read from the clipboard and never from argv; ruling 24 makes every
//! verb a client of the trusty-secrets on-demand socket.
//! What: [`SecretsAction`] and [`ArgText`], the redacting argument type.
//! Test: `cli_parses_every_secrets_verb`,
//! `parsed_set_arguments_never_show_in_debug`.

use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use clap::Subcommand;

/// A free-text argument of `set`, redacted in `Debug`.
///
/// Why: a caller who types the secret on the command line by mistake —
/// `set KEY:value`, `set KEY grp value`, `--value value` — must not see it
/// again in a `Debug` dump of the parsed command line.
/// What: the raw text, read with [`ArgText::text`]; parsing never fails, so
/// clap never quotes the argument in a usage error.
/// Test: `parsed_set_arguments_never_show_in_debug`.
#[derive(Clone)]
pub(crate) struct ArgText(String);

impl ArgText {
    /// The argument as typed.
    pub(crate) fn text(&self) -> &str {
        &self.0
    }
}

impl FromStr for ArgText {
    type Err = Infallible;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Ok(Self(raw.to_owned()))
    }
}

impl fmt::Debug for ArgText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ArgText(<redacted>)")
    }
}

/// Actions for the `secrets` subcommand.
///
/// Every verb resolves the project from the working directory's git
/// checkout and prints names only, never a value.
#[derive(Debug, Subcommand)]
pub(crate) enum SecretsAction {
    /// Add or replace one key of the project scope. The value comes from the
    /// clipboard.
    ///
    /// An empty clipboard is an error and stores nothing. `--value -` reads
    /// stdin instead; a value is never an argument. The confirmation shows
    /// the first 8 characters and the length, or only the length for a
    /// value of 8 characters or fewer.
    Set {
        /// The key name.
        key: ArgText,
        /// Optional label; the key is stored as `<group>.<KEY>`.
        group: Option<ArgText>,
        /// Only `-` (read stdin) is accepted.
        #[arg(long, value_name = "-")]
        value: Option<ArgText>,
        /// Refused: the value never goes on the command line.
        #[arg(hide = true)]
        extra: Vec<ArgText>,
    },
    /// Print the key names of the project and owner scopes, never a value.
    List,
    /// Report socket and backend reachability, never a value.
    Doctor,
}
