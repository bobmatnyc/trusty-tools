//! `tm secrets` command group — slice 1, keychain only (issue #7521, DOC-74).
//!
//! Why: an operator needs one place to put a project's API keys that is not a
//! `.env` file the agent's own guard refuses to read (#7266). Slice 1 ships
//! the smallest surface that is useful on its own: configure a group, add a
//! key, list names, remove one, and check reachability.
//! What: [`SecretsAction`] — `configure`/`add`/`list`/`remove`/`doctor`. The
//! remaining verbs of #7521's brief (`import`, `copy`) and `exec` (#7525) are
//! deliberately absent rather than stubbed, so nothing here can claim a
//! capability the binary does not have.
//! Test: `cli_parses_secrets_configure`, `cli_parses_secrets_add`,
//! `cli_parses_secrets_add_stdin`, `cli_parses_secrets_list`,
//! `cli_parses_secrets_remove`, `cli_parses_secrets_doctor`.

use clap::Subcommand;

/// Actions for the `secrets` subcommand (issue #7521).
///
/// Why: one command group per storage concern, mirroring `tm auth`'s shape.
/// What: see each variant. A value is never accepted as an argument on any of
/// them — `ps` shows argv to every user on the machine (DOC-74 §4 T-3).
/// Test: the six `cli_parses_secrets_*` tests.
#[derive(Debug, Subcommand)]
pub(crate) enum SecretsAction {
    /// Record which backend and group this machine's `tm secrets` uses.
    ///
    /// Why: `add`/`list`/`remove` need a vault namespace, and deriving it
    /// from the git remote on every call fails outside a checkout. Writing it
    /// once makes every later call deterministic.
    /// What: validates `--provider` (only `keychain` exists in slice 1;
    /// 1Password and Keeper land with #7519), resolves `--group` — explicit,
    /// else the git-remote-derived `<owner>/<repo>` of the working directory
    /// — and writes both to `~/.trusty-tools/trusty-mpm/config.yaml`.
    /// Test: `cli_parses_secrets_configure`.
    Configure {
        /// Backend id. Slice 1 accepts only `keychain`.
        #[arg(long, default_value = "keychain")]
        provider: String,
        /// Vault namespace, conventionally `<owner>/<repo>`.
        #[arg(long)]
        group: Option<String>,
    },
    /// Add or replace one key's value in the configured vault.
    ///
    /// Why: the one write path. The value comes from stdin or a masked
    /// prompt, never from argv, so it reaches neither `ps` nor shell history.
    /// What: with `--value -`, reads stdin to EOF and trims it; without it,
    /// prompts on the TTY with echo off. A non-TTY stdin and no `--value -`
    /// is an error naming how to supply the value, never a silent read.
    /// Test: `cli_parses_secrets_add`, `cli_parses_secrets_add_stdin`,
    /// `secrets_add_reads_value_from_stdin_when_dash`,
    /// `secrets_add_refuses_when_no_tty_and_no_stdin_flag`.
    Add {
        /// Key name (the keychain account).
        key: String,
        /// Only `-` is accepted: read the value from stdin. Omit it to be
        /// prompted. A literal value is deliberately not accepted.
        #[arg(long)]
        value: Option<String>,
    },
    /// List the key NAMES stored for the configured group — never values.
    ///
    /// Test: `cli_parses_secrets_list`, `secrets_list_prints_names_only`.
    List,
    /// Remove one key from the configured vault.
    ///
    /// Test: `cli_parses_secrets_remove`.
    Remove {
        /// Key name to delete. Absent is not an error.
        key: String,
    },
    /// Report backend, group, keychain reachability, and indexed-name count.
    ///
    /// Why: the fastest way to tell "no keys yet" from "the keychain is not
    /// answering" before an `add` fails.
    /// What: probes with the sentinel entry `KeyringStore` already uses —
    /// never creating, reading, or unlocking a real secret — and exits 1 when
    /// anything is unhealthy. Prints no values.
    /// Test: `cli_parses_secrets_doctor`,
    /// `secrets_doctor_reports_probe_result_without_prompting`.
    Doctor,
}
