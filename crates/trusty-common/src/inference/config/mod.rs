//! Universal `config` clap module (issue #2404, epic #2400 Wave 1).
//!
//! Why: every trusty-* binary that talks to an inference provider needs the
//! SAME credential-management CLI — set a provider's key, list which providers
//! are configured (and via which tier), probe a key with a cheap live auth
//! check, and remove a key. Before this module each binary would grow its own
//! divergent `config`/`keys` handling. This is the one implementation ALL ten
//! clap binaries mount identically (#2405 does the mounting); adding it to a new
//! binary is two lines and never diverges.
//!
//! What: [`ConfigCommand`] is a clap [`clap::Args`] a binary embeds as a single
//! variant of its own top-level subcommand enum. It nests a `<feature>`
//! subcommand ([`ConfigFeature`]) so the grammar is
//! `<bin> config <feature> <verb> …`; Wave 1 ships the `keys` feature
//! ([`keys::KeysCommand`]) with `set` / `list` / `test` / `unset`. The
//! operations themselves live in the injectable [`ops`] seam so they are 100%
//! testable without a TTY or a live provider. [`ConfigCommand::run`] resolves its
//! own secure [`crate::inference::credentials::KeyStore`] and
//! [`crate::inference::Configurator`], so a mounting binary needs no wiring.
//!
//! # Mount recipe for #2405 (the entire integration — two lines)
//!
//! Add the variant to your binary's top-level clap enum:
//!
//! ```ignore
//! use trusty_common::inference::config::ConfigCommand;
//!
//! #[derive(clap::Subcommand)]
//! enum Commands {
//!     // … your existing subcommands …
//!     /// Manage inference provider configuration (API keys).
//!     Config(ConfigCommand),          // line 1: mount
//! }
//! ```
//!
//! Then dispatch it (inside your `#[tokio::main]` async entry point):
//!
//! ```ignore
//! match cli.command {
//!     // … your existing arms …
//!     Commands::Config(cmd) => cmd.run().await?,   // line 2: dispatch
//! }
//! ```
//!
//! Adding a future `config <feature>` (e.g. `config model …`) is a new
//! [`ConfigFeature`] variant here — mount sites do not change (the
//! extensibility contract in the acceptance criteria).
//!
//! Enable the `config-cli` feature in the mounting binary's dependency on
//! `trusty-common`. It implies `inference-client`; a default (no-features)
//! `trusty-common` build compiles neither this module nor `clap`.
//!
//! Test: `ops` inline tests + `crates/trusty-common/tests/config_keys_cli.rs`
//! (argv parsing, set→list→test→unset flow, tier reporting, and the
//! never-print-a-value guarantee).

pub mod keys;
pub mod ops;

use keys::KeysCommand;

/// The universal `config` subcommand a binary mounts as one enum variant.
///
/// Why: one embeddable type is the whole public mount surface — a binary adds
/// `Config(ConfigCommand)` and calls [`Self::run`]; everything else (feature
/// routing, store/configurator resolution, secret redaction) is internal so the
/// mount is identical across binaries and cannot drift.
/// What: a clap [`clap::Args`] wrapping the `<feature>` subcommand. `run`
/// dispatches to the selected feature's own runner.
/// Test: `crates/trusty-common/tests/config_keys_cli.rs::config_command_mounts_and_parses`.
#[derive(Debug, clap::Args)]
pub struct ConfigCommand {
    #[command(subcommand)]
    feature: ConfigFeature,
}

/// The `<feature>` layer of the `config` grammar.
///
/// Why: `config` is a namespace, not a single command — keys today, potentially
/// model/provider defaults later. Modelling the feature as its own subcommand
/// enum means a new feature is one variant added HERE with zero mount-site
/// churn.
/// What: Wave 1 exposes only [`Self::Keys`]. Each variant owns a feature command
/// type that implements its own verbs.
/// Test: `config_keys_argv_grammar_parses`.
#[derive(Debug, clap::Subcommand)]
enum ConfigFeature {
    /// Manage inference provider API keys (set / list / test / unset).
    Keys(KeysCommand),
}

impl ConfigCommand {
    /// Execute the parsed `config` command.
    ///
    /// Why: the single dispatch entry a mounting binary calls — one `.await`
    /// after the one-line mount. Async because `config keys test` performs a
    /// live provider auth probe.
    /// What: routes to the selected [`ConfigFeature`]'s runner, which resolves
    /// its own secure store + configurator. Surfaces `anyhow::Result` at the
    /// boundary (matching every binary's top-level error printer); never returns
    /// or logs a key value.
    /// Test: `crates/trusty-common/tests/config_keys_cli.rs` drives the verbs
    /// through the [`ops`] seam this delegates to.
    pub async fn run(self) -> anyhow::Result<()> {
        match self.feature {
            ConfigFeature::Keys(cmd) => cmd.run().await,
        }
    }
}
