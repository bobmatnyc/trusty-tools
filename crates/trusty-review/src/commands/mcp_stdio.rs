//! Handler for the `mcp` subcommand — the MCP JSON-RPC stdio service.
//!
//! Why (#6290): this file used to be `serve`, and `serve` did two unrelated
//! things behind one flag. Without `--stdio` it bound a Unix socket and ran the
//! review daemon; with `--stdio` it spoke MCP over a pipe. ADR-0032's review
//! lane retires the daemon half outright — review runs per invocation now — and
//! what is left is the stdio service, which is not a daemon at all: it binds
//! nothing, is spawned by the MCP client that talks to it, and dies with the
//! pipe.
//!
//! What: `cmd_mcp_stdio` spawns `build_app_state` in a background task and
//! calls `mcp::run_deferred` immediately, so the MCP `initialize` handshake is
//! answered before any network or credential calls are made (issue #1739).
//!
//! 🔴 The subcommand answers to `serve` as well as `mcp`, and accepts the three
//! retired transport flags. Neither is decoration. Every `.mcp.json` on every
//! host that ever installed trusty-review spells this
//! `["serve", "--stdio"]`, and a launchd unit installed before #6290 spells it
//! `["serve", "--socket", "<path>"]` — clap exits 2 on an argument it does not
//! know, so dropping either spelling turns an upgrade into a client that cannot
//! start. [`retired_daemon_argv`] is what tells the two apart: a stale launchd
//! argv is answered with the migration instruction and exit 0, never with an
//! MCP loop reading a pipe that launchd never opened.
//!
//! Test: `stale_daemon_argv_still_parses`,
//! `retired_daemon_argv_detects_the_plist_argv`,
//! `retired_transport_flags_are_hidden_from_help`; the `serve` alias itself is
//! pinned by `serve_is_still_accepted_as_an_alias` in `main.rs`, and MCP
//! dispatch is covered by `mcp::tests`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tracing::{error, info};

use trusty_review::{
    config::ReviewConfig,
    integrations::{
        search_client::HttpSearchClient, subprocess_analyze_client::SubprocessAnalyzeClient,
    },
    llm::build_provider,
    mcp::DeferredStateValue,
    pipeline::enforce_verifier_liveness,
    service::AppState,
    store::DedupNeed,
};

use crate::cli_verify;

// ─── args ────────────────────────────────────────────────────────────────────

/// Arguments for the `mcp` subcommand.
///
/// Why: every field here is a compatibility shim; the live subcommand takes no
/// options at all. See the module docs for which argv each one keeps alive.
/// What: `--stdio` is the flag every existing MCP client passes and is now the
/// only mode, so it is accepted and ignored. `--socket` / `--port` / `--bind`
/// are the retired daemon flags — their PRESENCE is the signal
/// [`retired_daemon_argv`] reads.
/// Test: `stale_daemon_argv_still_parses`.
#[derive(Debug, clap::Parser)]
pub struct McpArgs {
    /// Accepted and ignored — stdio is the only mode (#6290).
    ///
    /// Kept because every `.mcp.json` written since #1739 passes it.
    #[arg(long, default_value_t = false, hide = true)]
    pub stdio: bool,

    /// Retired with the review daemon (#6290). Its presence means a stale
    /// launchd unit is starting this process — see [`retired_daemon_argv`].
    #[arg(long, value_name = "PATH", hide = true)]
    pub socket: Option<PathBuf>,

    /// Retired with the TCP listener (#6277) and then with the daemon (#6290).
    #[arg(long, value_name = "PORT", hide = true)]
    pub port: Option<u16>,

    /// Retired with the TCP listener (#6277) and then with the daemon (#6290).
    #[arg(long, value_name = "ADDR", hide = true)]
    pub bind: Option<String>,
}

/// Whether this argv came from a launchd unit that predates #6290.
///
/// Why: `["serve", "--stdio"]` and `["serve", "--socket", "<path>"]` reach the
/// same handler, and only one of them has a pipe on the other end. Running the
/// MCP loop for the launchd argv would read EOF immediately and exit, which
/// under `KeepAlive::Always` is a respawn loop whose only symptom is silence.
/// Telling them apart on the RETIRED flags rather than on `--stdio` is
/// deliberate: an operator who runs `trusty-review serve` by hand passes
/// neither, and gets the MCP loop, which is what that command now means.
///
/// What: `true` when any of the three daemon-transport flags was passed.
/// Test: `retired_daemon_argv_detects_the_plist_argv`.
fn retired_daemon_argv(args: &McpArgs) -> bool {
    args.socket.is_some() || args.port.is_some() || args.bind.is_some()
}

/// Tell an operator their launchd unit is stale, and how to clear it (#6290).
///
/// Why: the unit cannot work — there is no daemon to start — and the operator
/// reading a launchd log wants a command to paste rather than a description of
/// one. `tctl install` evicts the retired unit in its service-bootstrap pass
/// (`trusty_installer::commands::service_bootstrap`), so that is what this
/// names — NOT `tctl up`, which has no launchd pass at all (#6290 review).
/// What: one line to stderr; stdout stays clean.
/// Test: `stale_daemon_argv_still_parses` covers the parse half; the message
/// itself is a single `eprintln!` with no branching.
fn report_retired_daemon_argv() {
    eprintln!(
        "trusty-review: this process was started with the retired daemon flags, \
         so a launchd unit predating #6290 is still loaded. trusty-review has no \
         daemon — reviews run per invocation (`trusty-review run`). Clear the \
         stale unit with:\n    tctl install"
    );
}

// ─── handler ─────────────────────────────────────────────────────────────────

/// Execute the `mcp` subcommand.
///
/// Why: MCP clients send `initialize` immediately and give up after ~1.5 s, so
/// `AppState` — which may include a real Bedrock Converse liveness call — is
/// built in the background while the stdio loop answers the handshake (issue
/// #1739).
/// What: a stale launchd argv is answered with [`report_retired_daemon_argv`]
/// and `Ok(())`; otherwise `build_app_state` is spawned and `mcp::run_deferred`
/// takes over stdio. All logs go to stderr; stdout is the JSON-RPC transport.
///
/// # Errors
///
/// Propagates whatever the MCP stdio loop fails with. An `AppState` that cannot
/// be built is NOT an error here — it is reported through the watch channel so
/// the client sees a tool-call failure rather than a server that vanished.
///
/// Test: see the module docs; the deferred path is smoke-tested with broken
/// Bedrock credentials (initialize must answer in <1.5 s).
pub async fn cmd_mcp_stdio(config: ReviewConfig, args: McpArgs) -> Result<()> {
    if retired_daemon_argv(&args) {
        report_retired_daemon_argv();
        return Ok(());
    }

    // ── Deferred-startup path (issue #1739) ──────────────────────────────────
    info!("trusty-review MCP stdio service starting (deferred AppState build, issue #1739)");
    let (state_tx, state_rx) = tokio::sync::watch::channel::<DeferredStateValue>(None);
    tokio::spawn(async move {
        // #5064: the MCP surface hardcodes `allow_posting: false`, so it never
        // reaches the dedup write path and must not open the file.
        let value: DeferredStateValue = match build_app_state(config, DedupNeed::NotNeeded).await {
            Ok(state) => {
                info!("trusty-review AppState ready — tool calls now accepted");
                Some(Ok(Arc::new(state)))
            }
            Err(e) => {
                error!("trusty-review AppState build failed: {e:#}");
                Some(Err(format!("{e:#}")))
            }
        };
        // Ignore SendError: it means the stdio loop already exited (EOF).
        let _ = state_tx.send(value);
    });
    trusty_review::mcp::run_deferred(state_rx).await
}

// ─── dep builder ─────────────────────────────────────────────────────────────

/// Build the `AppState` the MCP tools run over.
///
/// Why: the MCP tool surface shares the review pipeline's dependency set with
/// every other caller; building it here keeps the stdio entry point's startup
/// sequencing in one place. Async because `BedrockProvider::new` loads AWS
/// credentials asynchronously.
/// What: builds the reviewer and verifier LLM providers, resolves the search
/// index from the daemon, constructs the search/analyze clients, and wraps them
/// in `AppState`. `dedup_need` is `NotNeeded` for this surface (#5064) — it
/// never posts, so it never opens the durable claim store.
/// Test: `open_for_not_needed_touches_nothing` (store side); handler behaviour
/// is covered transitively by unit tests that inject fakes.
async fn build_app_state(mut config: ReviewConfig, dedup_need: DedupNeed) -> Result<AppState> {
    let reviewer_model = config.role_models.reviewer.model.clone();
    let default_provider = config.role_models.reviewer.provider.clone();
    let llm = build_provider(&reviewer_model, &default_provider, &config)
        .await
        .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;

    let verifier = cli_verify::build_verifier_for_mcp_stdio(&config).await?;
    enforce_verifier_liveness(&config, verifier.as_ref())
        .await
        .map_err(|reason| anyhow::anyhow!(reason))?;

    // Resolve the search index before constructing AppState so the daemon's
    // registered root_path is matched against the current repo and the correct
    // index id is used even when TRUSTY_SEARCH_INDEX is not set.  The call is
    // a no-op when search_index_explicit is true (operator overrode it).
    let search = HttpSearchClient::from_config(&config)
        .map_err(|e| anyhow::anyhow!("failed to build search HTTP client: {e}"))?;
    config.resolve_index(&search).await;
    // Use the on-demand subprocess client instead of the HTTP daemon client.
    // Rationale: #632 — trusty-analyze is invoked on demand as a subprocess
    // (trusty-analyze review --index-id <id> -) rather than requiring a
    // long-running trusty-analyze serve daemon.
    let analyze = SubprocessAnalyzeClient::from_config(&config)
        .map_err(|e| anyhow::anyhow!("failed to build analyze HTTP client: {e}"))?;

    // #5064: one entry point decides open-vs-skip for every AppState builder.
    let dedup = trusty_review::store::open_dedup_for(&config.log_dir, dedup_need)?;

    Ok(AppState::with_verifier_and_dedup(
        config,
        llm,
        verifier,
        Arc::new(search),
        Some(Arc::new(analyze)),
        dedup,
    ))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory as _, Parser as _};

    /// REGRESSION (#6290): the argv an un-reinstalled launchd plist passes must
    /// still PARSE.
    ///
    /// Why: `service install` wrote `["serve", "--socket", <path>]` and, before
    /// #6277, `["serve", "--port", "7891"]`. Upgrading the binary does not
    /// rewrite the plist. Without these hidden fields clap exits 2 on an
    /// unexpected argument, and `KeepAlive::Always` with a 10-second throttle
    /// turns that into a permanent crash loop whose only symptom is a usage
    /// message in the launchd log.
    /// What: parses both stale spellings and asserts the values land in their
    /// fields, so the migration notice can be triggered off them.
    /// Test: this is the test.
    #[test]
    fn stale_daemon_argv_still_parses() {
        let uds = McpArgs::try_parse_from(["mcp", "--socket", "/tmp/x.sock"])
            .expect("the pre-#6290 launchd argv must still parse, or the unit crash-loops");
        assert_eq!(
            uds.socket.as_deref(),
            Some(std::path::Path::new("/tmp/x.sock"))
        );

        let tcp = McpArgs::try_parse_from(["mcp", "--port", "7891", "--bind", "127.0.0.1"])
            .expect("the pre-#6277 launchd argv must still parse");
        assert_eq!(tcp.port, Some(7891));
        assert_eq!(tcp.bind.as_deref(), Some("127.0.0.1"));
    }

    /// Why: the whole point of keeping the retired flags is to distinguish a
    /// launchd start from an MCP client start. A predicate that answered `true`
    /// for `--stdio` would send every MCP client the migration notice and exit
    /// before serving a single tool call.
    /// What: each retired flag alone is `true`; `--stdio` and a bare invocation
    /// are `false`.
    /// Test: this is the test.
    #[test]
    fn retired_daemon_argv_detects_the_plist_argv() {
        let parse = |argv: &[&str]| McpArgs::try_parse_from(argv).expect("parse");
        assert!(retired_daemon_argv(&parse(&["mcp", "--socket", "/tmp/x"])));
        assert!(retired_daemon_argv(&parse(&["mcp", "--port", "7891"])));
        assert!(retired_daemon_argv(&parse(&["mcp", "--bind", "127.0.0.1"])));
        assert!(!retired_daemon_argv(&parse(&["mcp", "--stdio"])));
        assert!(!retired_daemon_argv(&parse(&["mcp"])));
    }

    /// Why: these are compatibility shims, not options anyone should discover
    /// and start using. A future edit that drops `hide = true` would
    /// re-advertise a daemon that no longer exists.
    /// What: renders the subcommand help and asserts none of the four appears.
    /// Test: this is the test.
    #[test]
    fn retired_transport_flags_are_hidden_from_help() {
        let help = McpArgs::command().render_help().to_string();
        for retired in ["--socket", "--port", "--bind", "--stdio"] {
            assert!(
                !help.contains(retired),
                "{retired} is accepted for compatibility only and must not be \
                 advertised: {help}"
            );
        }
    }
}
