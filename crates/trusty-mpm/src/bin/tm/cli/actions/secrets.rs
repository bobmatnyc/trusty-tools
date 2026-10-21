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
use std::path::PathBuf;
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
    // #7521: slice 2 — remove, import and copy, project scope only.
    /// Remove one key of the project scope.
    ///
    /// A key the project scope does not hold is an error (exit 1).
    Remove {
        /// The key name.
        key: ArgText,
        /// The label the key was set with.
        group: Option<ArgText>,
    },
    /// Load every `KEY=value` of a dotenv file into the project scope.
    ///
    /// Prints key names only. `secret://` references are skipped and named.
    /// A syntax error names the line number, never its text, and stores
    /// nothing. Any key that fails makes the exit status 1. The file is left
    /// in place.
    Import {
        /// The dotenv file.
        path: PathBuf,
        /// Optional label applied to every key (`<group>.<KEY>`).
        group: Option<ArgText>,
    },
    /// Copy this project's keys from one backend to another.
    ///
    /// Copies the named keys, or every key of the project scope when none is
    /// named. Stays inside the project and prints names only. Any key not
    /// copied makes the exit status 1.
    Copy {
        /// Source backend id, e.g. `keychain`.
        #[arg(long)]
        from: String,
        /// Destination backend id.
        #[arg(long)]
        to: String,
        /// Stored key names (`<group>.<KEY>` for a grouped key); none means all.
        keys: Vec<ArgText>,
    },
    /// Report socket and backend reachability, never a value.
    Doctor,
    // #9072: tier 3 of DOC-74 §15.8 for any language, on the command line.
    /// Print one key's value inside a `tm secrets exec` grant.
    ///
    /// Reads the grant token from the fd that `TRUSTY_SECRETS_TOKEN_FD`
    /// names and asks the running trusty-secrets server for `KEY`. On
    /// success stdout holds the value and nothing else, with no trailing
    /// newline. No grant, a bad token, a refused key or a server that is not
    /// running exits 1 with a fixed message on stderr and nothing on stdout.
    /// Never starts a server.
    Resolve {
        /// The key name, as granted by `tm secrets exec`.
        key: ArgText,
    },
    // #9621: tier 1 of DOC-74 §15.8 — `--env` and `--stdin`; no `--dotenv`.
    /// Run a command with project keys in its environment or on its stdin.
    ///
    /// Each `--env NAME=KEY` sets `NAME` to the value of `KEY`; `--stdin KEY`
    /// writes the value of `KEY` to the command's stdin and closes it. A value
    /// never goes on any command line. Any key that does not resolve runs
    /// nothing and exits 1. Exit status is the command's, or 128 + signal.
    ///
    /// When stdout is not a terminal, stdout and stderr are scrubbed of every
    /// injected value of 8 characters or more. On a terminal nothing is
    /// scrubbed. Known limits: any process of the same user can read the
    /// command's environment (`ps eww`, `/proc/<pid>/environ`); the grant
    /// lasts at most 3600 s and is not renewed; when tm is killed with
    /// SIGKILL the grant lives until the server prunes it or it expires.
    Exec {
        /// `NAME=KEY`: set variable `NAME` to the value of `KEY`. Repeatable.
        #[arg(long = "env", value_name = "NAME=KEY")]
        env: Vec<ArgText>,
        /// Write the value of `KEY` to the command's stdin.
        #[arg(long, value_name = "KEY")]
        stdin: Option<ArgText>,
        /// The command and its arguments, after `--`.
        #[arg(last = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// The helper `exec` runs as its child; never run by hand.
    ///
    /// It reads the grant token from `--token-fd`, resolves each key, reports
    /// the values to tm on `--report-fd`, and execs the command in place.
    #[command(name = "__exec-stub", hide = true)]
    ExecStub {
        /// The trusty-secrets socket tm granted on.
        #[arg(long)]
        socket: PathBuf,
        /// The inherited pipe that carries the token.
        #[arg(long)]
        token_fd: i32,
        /// The inherited pipe the values go back to tm on.
        #[arg(long)]
        report_fd: i32,
        /// `NAME=KEY`, as given to `exec`.
        #[arg(long = "env", value_name = "NAME=KEY")]
        env: Vec<ArgText>,
        /// The `--stdin` key, as given to `exec`.
        #[arg(long, value_name = "KEY")]
        stdin: Option<ArgText>,
        /// The command and its arguments.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
}
