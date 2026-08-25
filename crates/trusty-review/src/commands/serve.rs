//! Handler for the `serve` subcommand (UDS daemon and MCP stdio modes).
//!
//! Why: extracted from `main.rs` to keep that file under the 500-line cap (#610).
//! The `--stdio` flag runs the MCP stdio JSON-RPC loop; without it the daemon
//! binds its Unix socket. #6277 (ADR-0032) replaced the TCP loopback HTTP
//! listener with that socket; `--stdio` is untouched.
//!
//! What: for `--stdio`, spawns `build_app_state` in a background task and calls
//! `mcp::run_deferred` immediately so the MCP `initialize` handshake is answered
//! before any network or credential calls are made (issue #1739).  For daemon
//! mode, builds `AppState` synchronously then calls `service::serve`.
//!
//! Test: `cargo run -p trusty-review --features http-server -- serve --help`
//! must exit 0; wire tests live in `service::rpc`; MCP dispatch covered by
//! `mcp::tests`.

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
    service::{AppState, serve as serve_uds, socket_path},
    store::DedupNeed,
};

use crate::cli_verify;

// ─── serve args ──────────────────────────────────────────────────────────────

/// Arguments for the `serve` subcommand.
///
/// Why: collects the socket path and mode flag so the server can be configured
/// purely from CLI flags without env-var wrangling.
/// What: `--socket` overrides the socket path; `--stdio` activates the MCP
/// JSON-RPC stdio loop instead of binding it.
///
/// #6277: `--port` and `--bind` are gone with the TCP transport they configured.
/// A UDS daemon has no port, so keeping the flags to ignore them would leave an
/// operator believing they had moved a listener that never moved.
/// Test: `cargo run -p trusty-review --features http-server -- serve --help`.
#[derive(Debug, clap::Parser)]
pub struct ServeArgs {
    /// Unix socket to bind.
    ///
    /// Defaults to `<data dir>/trusty-review.sock` — the same path
    /// `trusty_common::daemon_socket_path("trusty-review")` hands every
    /// consumer. Override it and nothing will find this daemon; the flag exists
    /// for tests and for running a second instance deliberately.
    /// Ignored when --stdio is set.
    #[arg(long, value_name = "PATH")]
    pub socket: Option<PathBuf>,

    /// Retired with the TCP listener (#6277). Accepted and ignored.
    ///
    /// Why this is not simply deleted: the launchd plist on every machine that
    /// installed trusty-review before this change still passes
    /// `--port 7891`. Upgrading the BINARY does not rewrite the plist — only
    /// `trusty-review service install` does — so a bare `cargo install` would
    /// leave clap exiting 2 on an unexpected argument, and `KeepAlive::Always`
    /// with a 10-second throttle turns that into a permanent crash loop with
    /// nothing in the logs but a usage message. Accepting the flag and warning
    /// keeps the daemon serving through the upgrade window, which is the whole
    /// point of a deprecation shim.
    ///
    /// Hidden from `--help` so it is never presented as a live option.
    #[arg(long, value_name = "PORT", hide = true)]
    pub port: Option<u16>,

    /// Retired with the TCP listener (#6277). Accepted and ignored — see
    /// [`ServeArgs::port`].
    #[arg(long, value_name = "ADDR", hide = true)]
    pub bind: Option<String>,

    /// Run as a JSON-RPC 2.0 / MCP stdio service instead of binding the socket.
    ///
    /// In this mode stdout is the JSON-RPC transport; all logs go to stderr.
    /// Wire into Claude Code via .mcp.json:
    ///   { "mcpServers": { "trusty-review": { "command": "trusty-review",
    ///                                        "args": ["serve", "--stdio"] } } }
    #[cfg(feature = "mcp")]
    #[arg(long, default_value_t = false)]
    pub stdio: bool,
}

// ─── handler ─────────────────────────────────────────────────────────────────

/// Execute the `serve` subcommand.
///
/// Why: the daemon and MCP stdio modes share the same dependency-building
/// logic; only the final transport and startup sequencing differ.  For stdio
/// (issue #1739) `AppState` is built in the background so the MCP `initialize`
/// handshake is answered in <1 ms regardless of provider or credential state.
/// For the daemon, `AppState` is built synchronously (no strict deadline).
/// What: for `--stdio`, spawns `build_app_state` as a background tokio task,
/// then calls `mcp::run_deferred` with the watch receiver immediately.  For
/// daemon mode, builds `AppState` then binds the socket.  All logs go to
/// stderr; stdout stays clean.
/// Test: see module doc; MCP deferred path is smoke-tested with broken Bedrock
/// creds (initialize must respond in <1.5 s without the env-var workaround).
pub async fn cmd_serve(config: ReviewConfig, args: ServeArgs) -> Result<()> {
    #[cfg(feature = "mcp")]
    if args.stdio {
        // ── Deferred-startup path (issue #1739) ──────────────────────────────
        // Start the MCP stdio loop BEFORE building AppState so the `initialize`
        // handshake (sent immediately by Claude Code, ~1.5 s deadline) is
        // answered in <1 ms.  AppState construction — which may include a real
        // Bedrock Converse API call for liveness probing — runs in a background
        // task and feeds the watch channel when it finishes.
        info!("trusty-review MCP stdio service starting (deferred AppState build, issue #1739)");
        let (state_tx, state_rx) = tokio::sync::watch::channel::<DeferredStateValue>(None);
        let config_for_bg = config.clone();
        tokio::spawn(async move {
            // #5064: the MCP surface hardcodes `allow_posting: false`, so it
            // never reaches the dedup write path and must not open the file.
            let value: DeferredStateValue =
                match build_app_state(config_for_bg, DedupNeed::NotNeeded).await {
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
        return trusty_review::mcp::run_deferred(state_rx).await;
    }

    // ── Daemon mode: synchronous build (no strict startup deadline) ──────────
    // #5064: the webhook handler runs `allow_posting: true`, so this mode must
    // not start without the dedup store — a missing claim gate means duplicate
    // comments on a redelivered webhook.
    let state = build_app_state(config.clone(), DedupNeed::Required).await?;

    warn_about_retired_transport_flags(args.port, args.bind.as_deref());

    let socket = match args.socket {
        Some(p) => p,
        None => socket_path()?,
    };

    info!(
        socket = %socket.display(),
        reviewer_model = %config.role_models.reviewer.model,
        dry_run = config.dry_run,
        "trusty-review serve starting"
    );

    serve_uds(state, &socket).await
}

/// Tell an operator their launchd plist is stale, and how to fix it (#6277).
///
/// Why: the daemon serves correctly with these flags ignored, so nothing forces
/// the operator to notice that the unit is still describing a TCP listener that
/// no longer exists. A warning at every start is what turns a silent
/// compatibility shim into a migration that finishes — and it names the command
/// rather than describing it, because the operator reading a launchd log wants
/// something to paste.
///
/// What: one WARN per retired flag that was passed, to stderr (stdout stays
/// clean). Silent when neither is present, which is every correctly-installed
/// host.
///
/// Test: `retired_transport_flags_are_accepted_and_ignored`.
fn warn_about_retired_transport_flags(port: Option<u16>, bind: Option<&str>) {
    if port.is_none() && bind.is_none() {
        return;
    }
    if let Some(port) = port {
        tracing::warn!(
            port,
            "--port is retired and ignored: trusty-review serves a Unix socket \
             since #6277 (ADR-0032), not a TCP port"
        );
    }
    if let Some(bind) = bind {
        tracing::warn!(
            bind,
            "--bind is retired and ignored: trusty-review serves a Unix socket \
             since #6277 (ADR-0032)"
        );
    }
    eprintln!(
        "trusty-review: WARNING — this process was started with retired TCP \
         flags, so its launchd unit predates the Unix-socket migration (#6277). \
         The daemon is serving normally; rewrite the unit with:\n    \
         trusty-review service install"
    );
}

// ─── dep builder ─────────────────────────────────────────────────────────────

/// Build the shared `AppState` used by both HTTP and MCP stdio modes.
///
/// Why: both modes need the same set of deps; building them once avoids
/// repetition.  Async because `BedrockProvider::new` loads AWS credentials
/// asynchronously.  Also calls `resolve_index` so the correct trusty-search
/// index is selected when `TRUSTY_SEARCH_INDEX` is unset (issue #670 /
/// auto-derive #661).
/// What: builds the reviewer and verifier LLM providers, resolves the search
/// index from the daemon, constructs HTTP search/analyze clients, opens the
/// durable dedup store *if `dedup_need` says this mode can post* (#5064), and
/// wraps everything in `AppState`. A `Required` store that cannot be opened is
/// returned as `Err` — it is never downgraded to `dedup: None`.
/// Test: `open_for_not_needed_touches_nothing`, `open_for_required_opens`
/// (store side); handler behaviour covered transitively by unit tests that
/// inject fakes.
async fn build_app_state(mut config: ReviewConfig, dedup_need: DedupNeed) -> Result<AppState> {
    let reviewer_model = config.role_models.reviewer.model.clone();
    let default_provider = config.role_models.reviewer.provider.clone();
    let llm = build_provider(&reviewer_model, &default_provider, &config)
        .await
        .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;

    let verifier = cli_verify::build_verifier_for_serve(&config).await?;
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

    /// REGRESSION (#6277): the argv an un-reinstalled launchd plist passes must
    /// still PARSE.
    ///
    /// Why: `service install` writes `["serve", "--socket", <path>]`, but
    /// upgrading the binary does not rewrite the plist — only rerunning that
    /// command does. Every host installed before this change still passes
    /// `--port 7891`. Without the deprecation shim clap exits 2 on an unexpected
    /// argument, and `KeepAlive::Always` with a 10-second throttle turns that
    /// into a permanent crash loop whose only symptom is a usage message in the
    /// launchd log. This test is what keeps the shim from being tidied away by
    /// someone who sees two fields nothing reads.
    /// What: parses the pre-#6277 argv and asserts it succeeds, that the retired
    /// values land in their fields (so the warning can name them), and that a
    /// retired `--port` never stands in for a socket path.
    /// Test: this is the test.
    #[test]
    fn retired_transport_flags_are_accepted_and_ignored() {
        let args = ServeArgs::try_parse_from(["serve", "--port", "7891"])
            .expect("the pre-#6277 launchd argv must still parse, or the daemon crash-loops");
        assert_eq!(args.port, Some(7891));
        assert!(
            args.socket.is_none(),
            "a retired --port must not stand in for a socket path"
        );

        let both = ServeArgs::try_parse_from(["serve", "--port", "7891", "--bind", "127.0.0.1"])
            .expect("the full pre-#6277 argv must parse");
        assert_eq!(both.port, Some(7891));
        assert_eq!(both.bind.as_deref(), Some("127.0.0.1"));

        // The warning path must not panic on any combination it can be handed.
        warn_about_retired_transport_flags(both.port, both.bind.as_deref());
        warn_about_retired_transport_flags(None, None);
    }

    /// Why: the retired flags are a migration shim, not an option anyone should
    /// discover and start using. `hide = true` keeps them out of `--help`, and a
    /// future edit that drops the attribute would quietly re-advertise a TCP
    /// listener that does not exist.
    /// What: renders the subcommand help and asserts neither flag appears, while
    /// the live `--socket` does.
    /// Test: this is the test.
    #[test]
    fn retired_transport_flags_are_hidden_from_help() {
        let help = ServeArgs::command().render_help().to_string();
        assert!(
            help.contains("--socket"),
            "the live flag must be discoverable: {help}"
        );
        for retired in ["--port", "--bind"] {
            assert!(
                !help.contains(retired),
                "{retired} is accepted for compatibility only and must not be \
                 advertised: {help}"
            );
        }
    }
}
