//! `trusty-review` CLI entry point.
//!
//! Why: provides the user-facing interface for running, comparing and
//! inspecting PR reviews, and running the calibration harness (#1422).
//!
//! What: parses flags via clap-derive, resolves config, and dispatches to the
//! appropriate subcommand handler.  All heavy logic lives in `commands/`.
//! STDOUT stays clean (only review output); all tracing goes to stderr.
//!
//! Test: `cargo run -p trusty-review -- --help` must succeed; each subcommand
//! is tested in its own module under `commands/`.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

#[cfg(feature = "report")]
mod cli_report;
mod cli_verify;
mod commands;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};

use trusty_review::config::ReviewConfig;

use commands::calibrate::{CalibrateArgs, cmd_calibrate};
use commands::compare::{CompareArgs, cmd_compare};
#[cfg(feature = "mcp")]
use commands::mcp_stdio::{McpArgs, cmd_mcp_stdio};
use commands::run::{RunArgs, cmd_run};

// ─── CLI top-level ────────────────────────────────────────────────────────────

/// trusty-review — fast local PR-review service
///
/// An LLM-backed code reviewer that fetches PR diffs, retrieves code context
/// from trusty-search, and produces structured review verdicts.
///
/// Reviews are dry-run by default (no comments posted to GitHub). `run` posts
/// live only when `--live` is passed explicitly — the ambient
/// `PR_INTELLIGENCE_DRY_RUN` env var alone can never enable posting for this
/// command (#4460).
///
/// #6290: there is no review daemon. Every review is one invocation of this
/// binary; `run --json` returns the same structured result the retired
/// `review.run` method did.
#[derive(Debug, Parser)]
#[command(
    name = "trusty-review",
    version = env!("CARGO_PKG_VERSION"),
    about = "Fast local PR-review service — LLM-backed code review",
    long_about = None,
)]
struct Cli {
    /// Path to the TOML configuration file.
    /// Default: $XDG_CONFIG_HOME/trusty-review/config.toml
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

// ─── Subcommands ──────────────────────────────────────────────────────────────

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run a single PR review with the default (or overridden) reviewer model.
    ///
    /// Fetches the PR diff from GitHub and runs the LLM review pipeline.
    /// Dry-run by default (no comment posted). Pass `--live` to post the
    /// review to the PR — the ambient `PR_INTELLIGENCE_DRY_RUN` env var alone
    /// can never enable posting (#4460); the resolved mode is printed before
    /// any work begins.
    ///
    /// Use --local-diff to review a local unified diff file without GitHub
    /// (pass `-` to read the diff from stdin instead of a file), or --base
    /// [--head] to review an arbitrary local git ref range (`git diff -M
    /// <base>...<head>`; --head defaults to HEAD). All three local sources
    /// are always dry-run — they can never post (#2993).
    Run(RunArgs),

    /// Compare the same PR across multiple models to evaluate speed/cost/quality.
    ///
    /// Runs the review pipeline once per model in the compare set (or --models
    /// override) and prints a comparison table.  Always dry-run.  Accepts the
    /// same --local-diff / --base [--head] local-diff flags as `run` (#2993).
    Compare(CompareArgs),

    /// Generate a deterministic technical due-diligence report from a manifest.
    ///
    /// Loads a TOML manifest naming one or more target repositories, enriches
    /// each local checkout with git provenance, consumes pre-produced
    /// trusty-analyze metrics JSON, and fills a bundled report template — writing
    /// a `{slug}.md` / `{slug}.json` pair to the output directory.
    ///
    /// Deterministic only (M1): no LLM synthesis.  Any placeholder with no source
    /// value renders as `not stated in source data` (never invented).
    ///
    /// Requires the `report` Cargo feature (enabled by default).
    #[cfg(feature = "report")]
    Report(cli_report::ReportArgs),

    /// Run the calibration harness against a human-reviewed PR corpus (#1422).
    ///
    /// Loads a JSONL corpus (one CorpusEntry JSON object per line), runs the
    /// review pipeline in dry-run mode for each PR, fuzzy-matches trusty findings
    /// against human findings by (file, kind), and emits a JSON report:
    ///
    ///   {recall, precision, per_pr:[{pr, recall, precision, false_positives:[...]}],
    ///    verdict_bars, rust_semantic_fp_rate}
    ///
    /// `rust_semantic_fp_rate` measures precision of logic-error/ownership findings
    /// on `.rs` files — the known Rust false-positive hotspot.
    ///
    /// `verdict_bars` reports #1897's three verdict-level acceptance bars —
    /// strict agreement, RC-recall, and the clean-PR over-flag rate — over the
    /// corpus entries carrying a `reference_verdict`. It is absent when no entry
    /// carries one, so an unlabelled corpus reads as "not measured" rather than
    /// as a pass (#2974).
    ///
    /// Always dry-run safe: never posts to GitHub.
    Calibrate(CalibrateArgs),

    /// Print the crate version, or (with `--json`) the DOC-1
    /// capability-discovery envelope `tctl doctor --self-check` reads.
    ///
    /// Why (#6913): `tctl doctor --self-check trusty-review` spawns this exact
    /// form and requires `contract_version` + a non-empty `verbs[]`;
    /// trusty-review had no `version` subcommand, so the self-check died on a
    /// clap usage error (exit 2) before it could parse anything.
    /// What: delegates to `commands::version::run`. Answers from the binary
    /// alone — no config, no tokio runtime, no network.
    /// Test: `commands::version::envelope_satisfies_the_doc1_self_check`,
    /// `tests/version_cli.rs::version_json_parses_and_carries_the_crate_version`.
    Version {
        /// Emit the DOC-1 capability-discovery envelope as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Manage inference provider configuration (API keys) — the universal
    /// `config keys set/list/test/unset` surface shared by every trusty-*
    /// binary (epic #2400 Wave 1, #2405).
    Config(trusty_common::inference::config::ConfigCommand),

    /// Serve the console webhook relay over a Unix socket, then exit (#5182).
    ///
    /// Binds `trusty-review-webhook.sock` in the hardened scratch directory —
    /// the socket `trusty-console` has been relaying to since #5089 step 3 —
    /// writes each verified delivery to a durable inbox, and acknowledges only
    /// after that write is fsync'd. The ack is what lets console delete its own
    /// copy, so acking earlier would lose the delivery outright.
    ///
    /// Not a resident daemon: console spawns this on demand and SIGTERMs it.
    /// Run it by hand only to inspect the socket.
    WebhookListen,

    /// Run the MCP JSON-RPC 2.0 stdio service.
    ///
    /// stdout is the JSON-RPC transport; every log line goes to stderr. Wire it
    /// into Claude Code via .mcp.json:
    ///   { "mcpServers": { "trusty-review": { "command": "trusty-review",
    ///                                        "args": ["mcp"] } } }
    ///
    /// Answers to `serve` as well, because that is what every `.mcp.json`
    /// written before #6290 spells. `serve` no longer starts a daemon: there is
    /// none. See `commands::mcp_stdio`.
    ///
    /// Requires the `mcp` Cargo feature (enabled by default).
    #[cfg(feature = "mcp")]
    #[command(alias = "serve")]
    Mcp(McpArgs),
}

// ─── Logging ──────────────────────────────────────────────────────────────────

/// Level applied when `RUST_LOG` is unset or does not parse.
const DEFAULT_LOG_DIRECTIVES: &str = "warn";

/// Target held at `warn` unless `RUST_LOG` names exactly it (#8948). A
/// directive target matches by string prefix, so `aws` covers every AWS crate
/// that can log a credential: `aws_config`, `aws_sigv4`, `aws_runtime`,
/// `aws_credential_types`, `aws_types`, `aws_sdk_*` and `aws_smithy_*`.
const AWS_LOG_GUARD: &str = "aws";

/// Build the stderr tracing filter from `RUST_LOG`.
///
/// Why: #8948 — the AWS credential provider logs the access key ID at INFO,
/// and stderr lands in agent transcripts. A `RUST_LOG=info` meant for
/// trusty-review's own events let that line through.
/// What: `RUST_LOG` when it parses, else `warn`; then `aws=warn` unless
/// `RUST_LOG` has a directive whose target is exactly `aws`. A more specific
/// operator directive such as `aws_config=info` still wins over the guard, and
/// covers only its own target. An unparsable `RUST_LOG` falls back to `warn`
/// with the guard.
/// Test: `aws_info_events_stay_off_stderr_at_the_default_level`,
/// `aws_info_events_stay_off_stderr_under_rust_log_info`,
/// `aws_sigv4_events_stay_off_stderr_under_rust_log_trace`,
/// `an_aws_sub_target_directive_keeps_the_guard_on_its_siblings`,
/// `an_unparsable_rust_log_falls_back_to_warn_with_the_aws_guards`,
/// `an_explicit_aws_directive_is_honoured`.
fn log_filter(rust_log: Option<&str>) -> tracing_subscriber::EnvFilter {
    use tracing_subscriber::EnvFilter;
    let base = rust_log
        .filter(|s| EnvFilter::try_new(s).is_ok())
        .unwrap_or(DEFAULT_LOG_DIRECTIVES);
    let mut directives = base.to_string();
    let named = base
        .split(',')
        .filter_map(|d| d.trim().split('=').next())
        .any(|target| target == AWS_LOG_GUARD);
    if !named {
        directives.push_str(&format!(",{AWS_LOG_GUARD}=warn"));
    }
    // `EnvFilter::new` drops a directive it cannot parse rather than failing;
    // every piece here was validated above, so nothing is dropped.
    EnvFilter::new(directives)
}

// ─── Entry point ──────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    // Tracing to stderr — never stdout (stdout is reserved for review output
    // and, in --stdio mode, for the MCP JSON-RPC transport).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(log_filter(std::env::var("RUST_LOG").ok().as_deref()))
        .init();

    let cli = Cli::parse();

    // #6913: `version` is answered from the binary alone, ahead of the tokio
    // runtime and of `ReviewConfig::from_env_and_file`. tctl's self-check
    // spawns this on hosts with no trusty-review config at all, so anything it
    // depends on is a way for capability discovery to fail.
    if let Commands::Version { json } = &cli.command {
        commands::version::run(*json);
        return Ok(());
    }

    // #6290: no synchronous launchd pre-dispatch any more — there is no
    // `service` subcommand, because there is no unit for it to manage.
    let rt = tokio::runtime::Runtime::new().context("build tokio runtime")?;
    rt.block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> Result<()> {
    let Cli {
        config: config_path,
        command,
    } = cli;

    // #6135: `report` builds its own config, because the manifest it is given is
    // itself a config layer — it declares the provider and models of the run
    // that produced it, and those must be resolved before the rest of the chain.
    #[cfg(feature = "report")]
    let command = match command {
        Commands::Report(args) => {
            return cli_report::cmd_report(config_path.as_deref(), args).await;
        }
        other => other,
    };

    let config = ReviewConfig::from_env_and_file(config_path.as_deref(), None);

    match command {
        Commands::Run(args) => cmd_run(config, config_path.as_deref(), args).await,
        Commands::Compare(args) => cmd_compare(config, args).await,
        #[cfg(feature = "mcp")]
        Commands::Mcp(args) => cmd_mcp_stdio(config, args).await,
        // `report` is dispatched above, before the config is built.
        #[cfg(feature = "report")]
        Commands::Report(_) => unreachable!("report dispatched before the config is built"),
        Commands::Calibrate(args) => cmd_calibrate(config_path.as_deref(), args).await,
        Commands::WebhookListen => trusty_review::webhook_listener::run(config).await,
        Commands::Config(cmd) => cmd.run().await,
        // #6913: dispatched in `main`, before the runtime and the config.
        Commands::Version { .. } => {
            unreachable!("version dispatched before the tokio runtime is built")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    // ── --config reaches every config rebuild (#8947) ───────────────────────

    /// The production text of a source file, before its test module.
    fn production(src: &str) -> &str {
        src.split("#[cfg(test)]").next().unwrap_or(src)
    }

    /// REGRESSION (#8947): `main` hands the `--config` path to `run` and
    /// `calibrate`, and neither rebuilds its config from no file.
    /// `run_config_honours_the_config_file_verification_settings` proves the
    /// rebuild honours the path it is given.
    #[test]
    fn run_and_calibrate_rebuild_their_config_from_the_config_path() {
        let main = production(include_str!("main.rs"));
        assert!(main.contains("cmd_run(config, config_path.as_deref(), args)"));
        assert!(main.contains("cmd_calibrate(config_path.as_deref(), args)"));
        for (name, src) in [
            ("run.rs", include_str!("commands/run.rs")),
            ("calibrate.rs", include_str!("commands/calibrate.rs")),
        ] {
            let code = production(src);
            assert!(
                code.contains("ReviewConfig::from_env_and_file(config_path,"),
                "{name} must rebuild its config from the --config path"
            );
            assert!(
                !code.contains("from_env_and_file(None"),
                "{name} rebuilds its config from no file"
            );
        }
        let run = production(include_str!("commands/run.rs"));
        assert!(run.contains("run_config(config_path, &args)"));
    }

    // ── stderr log filter (#8948) ───────────────────────────────────────────

    /// In-memory stand-in for stderr.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// What reaches "stderr" under `log_filter(rust_log)` for one INFO event
    /// from the credential provider, the SDK, and trusty-review itself.
    fn emitted(rust_log: Option<&str>) -> String {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_env_filter(log_filter(rust_log))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "aws_config::profile::credentials", "key AKIA8948");
            tracing::info!(target: "aws_sdk_bedrockruntime::client", "sdk-8948");
            tracing::info!(target: "trusty_review::pipeline", "own-8948");
        });
        let bytes = capture.0.lock().expect("capture lock").clone();
        String::from_utf8(bytes).expect("utf8 log output")
    }

    /// REGRESSION (#8948): no AWS INFO line at the default level.
    #[test]
    fn aws_info_events_stay_off_stderr_at_the_default_level() {
        let out = emitted(None);
        assert!(!out.contains("AKIA8948"), "credential line leaked: {out}");
        assert!(!out.contains("sdk-8948"), "SDK INFO line leaked: {out}");
    }

    /// REGRESSION (#8948): `RUST_LOG=info` opens trusty-review's own INFO
    /// events, not the credential provider's.
    #[test]
    fn aws_info_events_stay_off_stderr_under_rust_log_info() {
        let out = emitted(Some("info"));
        assert!(out.contains("own-8948"), "own INFO event must show: {out}");
        assert!(!out.contains("AKIA8948"), "credential line leaked: {out}");
        assert!(!out.contains("sdk-8948"), "SDK INFO line leaked: {out}");
    }

    /// REGRESSION (#8948): `RUST_LOG=trace` must not open the SigV4 signer,
    /// whose `params` and `canonical_request` events carry the key ID and the
    /// session token.
    #[test]
    fn aws_sigv4_events_stay_off_stderr_under_rust_log_trace() {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_env_filter(log_filter(Some("trace")))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: "aws_sigv4::http_request::sign", "sigv4-8948");
            tracing::trace!(target: "trusty_review::pipeline", "own-trace-8948");
        });
        let out = String::from_utf8(capture.0.lock().expect("capture lock").clone())
            .expect("utf8 log output");
        assert!(
            out.contains("own-trace-8948"),
            "own TRACE event must show: {out}"
        );
        assert!(!out.contains("sigv4-8948"), "SigV4 event leaked: {out}");
    }

    /// REGRESSION (#8948): a directive for one `aws_config` module does not
    /// lift the guard from the credential provider beside it.
    #[test]
    fn an_aws_sub_target_directive_keeps_the_guard_on_its_siblings() {
        let out = emitted(Some("info,aws_config::imds=debug"));
        assert!(!out.contains("AKIA8948"), "credential line leaked: {out}");
        assert!(!out.contains("sdk-8948"), "SDK INFO line leaked: {out}");
    }

    /// Error arm (#8948): an unparsable `RUST_LOG` keeps the guards.
    #[test]
    fn an_unparsable_rust_log_falls_back_to_warn_with_the_aws_guards() {
        let out = emitted(Some("info,aws_config=notalevel"));
        assert!(out.is_empty(), "fallback must be `warn` with guards: {out}");
    }

    /// An operator who names `aws_config` in `RUST_LOG` gets what they asked.
    #[test]
    fn an_explicit_aws_directive_is_honoured() {
        let out = emitted(Some("warn,aws_config=info"));
        assert!(
            out.contains("AKIA8948"),
            "explicit directive dropped: {out}"
        );
        assert!(
            !out.contains("sdk-8948"),
            "aws_sdk guard must still hold: {out}"
        );
    }

    /// REGRESSION (#6290): `serve` must keep parsing, and must land on `Mcp`.
    ///
    /// Why: every `.mcp.json` on every host that has ever installed
    /// trusty-review spells this `["serve", "--stdio"]`. clap exits 2 on an
    /// unknown subcommand, so renaming without the alias turns an upgrade into
    /// an MCP server that will not start, in a config file this repo does not
    /// own and cannot edit.
    /// What: both spellings parse to the same variant, and the daemon
    /// subcommands the alias replaced are gone.
    /// Test: this is the test.
    #[cfg(feature = "mcp")]
    #[test]
    fn serve_is_still_accepted_as_an_alias() {
        for argv in [
            vec!["trusty-review", "serve", "--stdio"],
            vec!["trusty-review", "mcp"],
        ] {
            let cli =
                Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
            assert!(
                matches!(cli.command, Commands::Mcp(_)),
                "{argv:?} must reach the MCP stdio handler"
            );
        }

        for retired in ["socket", "service"] {
            assert!(
                Cli::try_parse_from(["trusty-review", retired]).is_err(),
                "`{retired}` described the daemon and must be gone, not silently accepted"
            );
        }
    }
}
