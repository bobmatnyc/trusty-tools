//! `config keys <verb>` — the credential-management feature (issue #2404).
//!
//! Why: this is the credential half of the universal `config` module — the
//! verbs every binary needs to manage inference provider keys without ever
//! echoing a value. The clap types here are the parse surface; the actual work
//! lives in [`super::ops`] so it stays testable without a TTY or a live network.
//! What: [`KeysCommand`] (the `keys` feature), [`KeysVerb`] (`set`/`list`/
//! `test`/`unset` — deliberately NO `get`, per the redaction mandate), and
//! [`KeysCommand::run`], which resolves the production secure store +
//! configurator and dispatches to [`super::ops`]. Interactive/stdin value
//! sourcing for `set` lives in [`source_set_value`] so the hidden-prompt path
//! is isolated from the (fully testable) [`super::ops`] core.
//! Test: `crates/trusty-common/tests/config_keys_cli.rs` (argv grammar + the
//! set→list→test→unset flow via `super::ops`).

use std::io::IsTerminal;

use anyhow::Context;

use crate::inference::configurator::Configurator;
use crate::inference::credentials::{default_store, load_env_local_once};
use crate::inference::providers::register_default_factories;

use super::ops;

/// The `keys` feature: manage inference provider API keys.
///
/// Why: groups the four credential verbs under one `config keys …` namespace so
/// the grammar reads `<bin> config keys <verb> <provider>`.
/// What: a clap [`clap::Args`] wrapping the [`KeysVerb`] subcommand.
/// Test: `config_keys_argv_grammar_parses`.
#[derive(Debug, clap::Args)]
pub struct KeysCommand {
    #[command(subcommand)]
    verb: KeysVerb,
}

/// The credential verbs. There is intentionally no `get` — a verb that printed a
/// stored key would violate the epic's never-echo-a-value mandate.
///
/// Why: `set`/`list`/`test`/`unset` cover the full lifecycle a human or a script
/// needs while keeping the raw value write-only and probe-only — it is set, its
/// presence/tier is listed, its validity is tested live, and it is removed, but
/// it is never read back out.
/// What: each verb carries only the arguments it needs; `list` takes none.
/// Test: `config_keys_argv_grammar_parses`, `config_keys_rejects_get`.
#[derive(Debug, clap::Subcommand)]
enum KeysVerb {
    /// Store a provider's API key in the secure store (File-0600 or OS keyring).
    ///
    /// Omit VALUE to read it interactively without echo (or from a stdin pipe),
    /// so the key never lands in your shell history.
    Set(SetArgs),
    /// List which providers have a key and via which tier (names/tiers only).
    List,
    /// Cheap live auth check: probe the provider with the resolved key.
    Test(ProviderArg),
    /// Remove a provider's key from the secure store (never touches env/.env.local).
    Unset(ProviderArg),
}

/// Arguments for `config keys set`.
///
/// Why: `provider` names which credential to store; `value` is optional so the
/// key can be supplied non-interactively (arg or stdin pipe — scriptable) OR
/// read from a hidden prompt when omitted on a TTY.
/// What: a positional provider slug and an optional positional value.
/// Test: `config_keys_argv_grammar_parses`.
#[derive(Debug, clap::Args)]
struct SetArgs {
    /// Provider name (e.g. `openrouter`, `anthropic`, `openai`, `fireworks`, `together`).
    provider: String,
    /// The API key value. Omit to read it hidden from a TTY or piped via stdin.
    value: Option<String>,
}

/// Arguments for verbs that take just a provider (`test`, `unset`).
///
/// Why: `test` and `unset` each act on one named provider and nothing else.
/// What: a single positional provider slug.
/// Test: `config_keys_argv_grammar_parses`.
#[derive(Debug, clap::Args)]
struct ProviderArg {
    /// Provider name to act on.
    provider: String,
}

impl KeysCommand {
    /// Run the parsed `keys` verb against the production environment.
    ///
    /// Why: the production wiring the mount recipe relies on — it resolves the
    /// real secure [`crate::inference::credentials::KeyStore`] and, for `test`,
    /// the default provider [`Configurator`], so a mounting binary supplies
    /// nothing. Every operation is delegated to the injectable [`super::ops`]
    /// seam so this method stays a thin, side-effecting shell.
    ///
    /// `pub` (issue #2405): a binary that already owns a top-level `config`
    /// command in a different domain (e.g. trusty-search's daemon
    /// memory-limit `config get/set`, trusty-installer's stack-member
    /// `config <members>`) cannot mount the whole-grammar [`super::ConfigCommand`]
    /// without colliding with its existing verbs. Exposing `KeysCommand` (and
    /// this method) directly lets such a binary nest ONLY the credential `keys`
    /// feature under its own `config` enum instead — see the "mount `keys`
    /// under an existing `config`" recipe in the parent module's docs.
    /// What: builds `stdout` as the output sink and dispatches. `set` sources its
    /// value via [`source_set_value`] (hidden prompt / stdin pipe / arg); `test`
    /// first loads `.env.local` (so the env tier reflects it) and registers the
    /// default adapter factories before probing. `list` deliberately does NOT
    /// pre-load `.env.local`, so its tier report distinguishes the shell-env tier
    /// from the `.env.local` tier honestly.
    /// Test: exercised end-to-end through `super::ops` in
    /// `crates/trusty-common/tests/config_keys_cli.rs`.
    pub async fn run(self) -> anyhow::Result<()> {
        let store = default_store();
        let stdout = std::io::stdout();
        match self.verb {
            KeysVerb::Set(args) => {
                if args.value.is_some() {
                    // Best-effort nudge: an inline VALUE argument lands in shell
                    // history/process-list on most systems. Warn on stderr (never
                    // the value itself) without blocking the scriptable path.
                    eprintln!(
                        "warning: passing the key inline may leave it in your shell history; \
                         omit VALUE for a hidden prompt, or pipe it via stdin."
                    );
                }
                let value = source_set_value(args.value)?;
                ops::set(store.as_ref(), &args.provider, &value, &mut stdout.lock())
            }
            KeysVerb::List => ops::list(store.as_ref(), &mut stdout.lock()),
            KeysVerb::Unset(args) => ops::unset(store.as_ref(), &args.provider, &mut stdout.lock()),
            KeysVerb::Test(args) => {
                // Load `.env.local` so the resolver's env tier reflects it, then
                // wire the built-in adapter factories for the live probe.
                load_env_local_once();
                let mut cfg = Configurator::new();
                register_default_factories(&mut cfg);
                let outcome = ops::probe(store.as_ref(), &cfg, &args.provider).await?;
                ops::report_probe(&args.provider, &outcome, &mut stdout.lock())?;
                outcome.into_result()
            }
        }
    }
}

/// Resolve the value for `set`: explicit arg, piped stdin, or a hidden TTY prompt.
///
/// Why: keys must never land in shell history, so the default (no VALUE arg)
/// path reads the secret from stdin — hidden when stdin is a terminal, plain
/// line-read when it is a pipe (the scriptable path). Keeping this here, out of
/// [`super::ops`], means the `rpassword`/`is_terminal` interactive machinery is
/// never on a test's code path — tests drive [`super::ops::set`] with a value
/// directly, and the piped-line parsing via [`super::ops::read_key_line`].
/// What: returns `value` when present; otherwise, on a TTY, prompts with echo
/// suppressed via `rpassword`; on a pipe, reads one line from stdin via
/// [`super::ops::read_key_line`]. Never prints the value.
/// Test: the non-interactive branches are covered by
/// `config_keys_cli.rs::read_key_line_trims_piped_value` (stdin line) and the
/// `set` flow (explicit value); the hidden-prompt branch is interactive-only.
fn source_set_value(value: Option<String>) -> anyhow::Result<String> {
    if let Some(value) = value {
        return Ok(value);
    }
    if std::io::stdin().is_terminal() {
        return rpassword::prompt_password("API key (input hidden): ")
            .context("failed to read hidden key input from the terminal");
    }
    let mut stdin = std::io::stdin().lock();
    ops::read_key_line(&mut stdin).context("failed to read key from stdin")
}
