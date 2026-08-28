//! Long-form command handlers for the `trusty-analyze` CLI.
//!
//! Why: the `serve`, `deep`, and `review-pr` handler bodies are the largest
//! arms of the `main()` dispatcher; lifting them here keeps `main.rs` under the
//! 500-SLOC production cap (see #1195) while `main` stays a thin router.
//! What: `run_serve` boots the daemon (and, with `--mcp`, an MCP stdio loop),
//! `run_deep` proxies the deep-analysis method, and `run_review_pr` fetches +
//! reviews a GitHub PR diff.
//! Test: exercised end-to-end via the CLI (`trusty-analyze serve|deep|review-pr`);
//! error paths (daemon down, missing token) are covered by the existing manual
//! smoke tests documented on each `Cmd` variant, plus
//! `run_deep_reports_an_absent_socket` below.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::ValueEnum;

use trusty_analyze::core::FactStore;
use trusty_analyze::core::TrustySearchClient;
use trusty_analyze::core::{overlay_path_beside_facts, ScipOverlayStore};
use trusty_analyze::mcp::AnalyzerMcpServer;
use trusty_analyze::service::{serve, serve_on_demand, AnalyzerAppState};

/// Output format for the `review`, `deep`, and `review-pr` subcommands.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Machine-readable JSON.
    Json,
    /// Human-readable text report.
    Text,
}

/// Boot the sidecar daemon, optionally alongside an MCP stdio loop.
///
/// Why: `serve` is the process entry point for the daemon and the single most
/// complex CLI arm; keeping it in one focused function documents the startup
/// sequence (search dependency check → stores → socket).
/// What: probes trusty-search, opens the facts and SCIP overlay stores, warns
/// on a missing OpenRouter key, then binds the socket (with an inline MCP stdio
/// loop when `mcp` is set).
///
/// The two branches serve with DIFFERENT lifetimes and that is the point: the
/// bare `serve` path reclaims itself after an idle window (#6350), while the
/// `--mcp` path serves until it is signalled, because the socket is dialled by
/// the stdio loop this same process runs (#6355). See the branch comment below.
///
/// #6287: `--mcp-port`, and the axum MCP HTTP/SSE server it started, are gone.
/// ADR-0032 leaves `trusty-console` as the workspace's only HTTP surface, so a
/// remote MCP client reaches this dispatcher through the console rather than
/// through a second port this process binds.
///
/// Why (#5067): there is no embedder-load step here any more. It used to sit
/// between the store opens and the MCP servers, constructing a fastembed model
/// nothing selected; the hf-hub request it made had no timeout and blocked the
/// bind for as long as it ran (31m46s measured). The state's embedder is now
/// `BowEmbedder`, set by `AnalyzerAppState::new`, which touches neither disk
/// nor network — so boot performs no fallible initialization to report.
///
/// Test: run `trusty-analyze serve` without trusty-search running and verify
/// exit code 1; with it running the daemon binds and answers `analyze.health`.
/// The `--mcp` branch's lifetime is pinned by `tests/on_demand.rs`'
/// `an_mcp_session_outlives_the_idle_window`.
pub async fn run_serve(
    search: TrustySearchClient,
    facts_path: PathBuf,
    socket: Option<PathBuf>,
    mcp: bool,
) -> Result<()> {
    // Hard dependency: refuse to start if trusty-search is unreachable.
    // Why: there is no standalone/offline mode — every analysis operation
    // fetches chunk corpora from the search daemon at runtime.
    // What: one GET /health probe before we bind our own port or open redb.
    // Test: run `trusty-analyzer serve` without trusty-search running and
    // verify exit code 1 and the printed error message.
    if !search.health().await.unwrap_or(false) {
        eprintln!(
            "Error: trusty-search is not reachable at {}\n       Start it first: trusty-search daemon",
            search.base_url()
        );
        std::process::exit(1);
    }

    let facts = FactStore::open(&facts_path)
        .with_context(|| format!("open facts store at {}", facts_path.display()))?;

    // #5049: SCIP overlays live in their own redb file beside the facts store,
    // so one `--facts-path` still governs the whole data directory.
    let overlay_path = overlay_path_beside_facts(&facts_path);
    let scip_overlays = ScipOverlayStore::open(&overlay_path)
        .with_context(|| format!("open SCIP overlay store at {}", overlay_path.display()))?;

    // #5067: no embedder is constructed here. `AnalyzerAppState::new` installs
    // the infallible BOW embedder; nothing on this path loads a model.
    let state = AnalyzerAppState::new(search, facts, scip_overlays);

    // Warn at startup when OPENROUTER_API_KEY is absent so operators
    // notice the gap before any deep-analysis call returns a 400.
    // Why: the key is read once at startup (stored in state.api_key);
    // setting it after the daemon is running has no effect until the
    // next restart. A clear startup warning surfaces this constraint
    // early (issue #528).
    // What: logs a WARN to stderr when the resolved api_key is None.
    // Test: side-effect-only — verified by running the daemon without
    // the env var and observing the log line.
    if state.api_key.is_none() {
        tracing::warn!(
            "OPENROUTER_API_KEY is not set; deep (LLM) analysis will be \
             unavailable until the daemon is restarted with it set"
        );
    }

    // #6287: the socket path is DERIVED, not published. `--socket` exists for
    // tests and for deliberately running a second instance; a consumer resolves
    // the default through the same `daemon_socket_path` call the daemon uses,
    // so overriding it here means nothing will find this daemon.
    let socket = match socket {
        Some(p) => p,
        None => trusty_analyze::service::socket_path()?,
    };

    if mcp {
        // Run both: the daemon in a task, MCP stdio in the foreground. The
        // dispatcher dials the socket this process is about to bind, so it is
        // pointed at the same path rather than at a second transport.
        //
        // #6355: `serve`, not `serve_on_demand`. The idle window belongs to a
        // server nobody owns — one a client started and walked away from. This
        // process owns its own lifetime: its real job is the stdio loop below,
        // and `mcp::rpc_client::call` dials this socket once per tool call with
        // no respawn. An idle exit would unlink the socket while the stdio loop
        // is still connected to its client over stdin/stdout, and every later
        // tool call would answer `isError` with a transport failure for the rest
        // of the process's life. `serve` ends on SIGTERM/SIGINT, which is the
        // lifetime the loop below already has. `serve_with_idle`'s doc comment
        // in `service/rpc.rs` states the same invariant from the other side.
        let socket_for_daemon = socket.clone();
        let daemon = tokio::spawn(async move {
            if let Err(e) = serve(state, &socket_for_daemon).await {
                tracing::error!("trusty-analyze daemon exited: {e:#}");
            }
        });
        let result = trusty_analyze::mcp::stdio::run(AnalyzerMcpServer::new(socket)).await;
        daemon.abort();
        result
    } else {
        serve_on_demand(state, &socket).await
    }
}

/// Handle `trusty-analyze deep <index_id>`.
///
/// Why: deep analysis is a thin wrapper over the daemon's
/// `analyze.deep_analysis` method. Keeping it a client call (rather than
/// re-implementing in-process) means the CLI uses the same code path as MCP
/// clients and external tooling.
/// What: sends `{ "index_id": ..., "model": ... }` over the socket and prints
/// the [`trusty_analyze::core::DeepAnalysisReport`] as JSON or text.
///
/// #6287: this used to be a `reqwest` POST to `http://127.0.0.1:<port>`. The
/// timeout is now the shared MCP-client budget rather than reqwest's default,
/// which was unbounded — an LLM call that never returned held this CLI forever.
///
/// # Errors
///
/// When the socket cannot be dialled, when the daemon answers with a JSON-RPC
/// error, or when the report does not decode.
///
/// #6350: the socket is no longer assumed to be served. `ensure_running` starts
/// `trusty-analyze serve` when nothing is there and returns immediately when
/// something is, so `deep` works with no daemon installed and never starts a
/// second one beside a live server. Its failure is reported, not degraded —
/// there is no offline deep-analysis path to fall back to.
///
/// Test: `run_deep_reports_an_absent_socket`; the start itself is
/// `tests/on_demand.rs`' `two_concurrent_callers_share_one_server`.
pub async fn run_deep(
    index_id: String,
    model: Option<String>,
    format: OutputFormat,
    socket: &Path,
) -> Result<()> {
    // #6350: start the server before dialling it.
    trusty_common::uds::OnDemandAnalyze::at(socket)
        .ensure_running()
        .await
        .with_context(|| format!("start trusty-analyze on demand for {}", socket.display()))?;

    let mut params = serde_json::json!({ "index_id": index_id });
    if let Some(m) = model.as_deref() {
        params["model"] = serde_json::Value::String(m.to_string());
    }
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "analyze.deep_analysis",
        "params": params,
    });
    let response: trusty_common::uds::server::RpcResponse =
        trusty_common::uds::send_framed_request(
            socket,
            &request,
            trusty_analyze::core::mcp_client_timeout(),
        )
        .await
        .with_context(|| format!("analyze.deep_analysis over {}", socket.display()))?;
    if let Some(error) = response.error {
        anyhow::bail!("deep analysis failed ({}): {}", error.code, error.message);
    }
    let result = response
        .result
        .ok_or_else(|| anyhow::anyhow!("deep analysis answered with neither result nor error"))?;
    let report: trusty_analyze::core::DeepAnalysisReport =
        serde_json::from_value(result).context("decode the deep-analysis report")?;
    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).context("serialize deep report")?
            );
        }
        OutputFormat::Text => {
            print!(
                "{}",
                trusty_analyze::core::render_deep_analysis_text(&report)
            );
        }
    }
    Ok(())
}

/// Handle `trusty-analyze review-pr <owner/repo> <pr>`.
///
/// Why: fetches a GitHub PR diff and runs the analyzer's review pipeline,
/// optionally posting the result back as a PR comment.
/// What: parses `owner/repo`, reads `GITHUB_TOKEN`, requires trusty-search to
/// be reachable (review cross-references the index corpus), fetches the diff,
/// runs `analyze_diff_with_client`, prints the report, and posts a comment
/// when `--post-comment` is set.
/// Test: with no `GITHUB_TOKEN` set the function returns an error before any
/// network call.
pub async fn run_review_pr(
    repo: String,
    pr: u64,
    index_id: Option<String>,
    post_comment: bool,
    format: OutputFormat,
) -> Result<()> {
    let (owner, repo_name) = repo
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("repo must be in 'owner/repo' form, got '{repo}'"))?;
    let token = std::env::var(trusty_common::env_vars::ENV_GITHUB_TOKEN)
        .map_err(|_| anyhow::anyhow!("GITHUB_TOKEN is not set; required to fetch the PR diff"))?;
    let index_id = index_id
        .ok_or_else(|| anyhow::anyhow!("--index-id is required to cross-reference the diff"))?;

    // Review is backed by trusty-search; refuse to run if it's unreachable.
    let search = TrustySearchClient::new(
        std::env::var("TRUSTY_SEARCH_URL").unwrap_or_else(|_| "http://127.0.0.1:7878".to_string()),
    );
    if !search.health().await.unwrap_or(false) {
        eprintln!(
            "Error: trusty-search is unreachable at {}. review-pr requires trusty-search to be running.",
            search.base_url()
        );
        std::process::exit(1);
    }

    let client = reqwest::Client::new();
    let diff = trusty_analyze::core::fetch_pr_diff(&client, owner, repo_name, pr, &token)
        .await
        .with_context(|| format!("fetch diff for {owner}/{repo_name}#{pr}"))?;
    let report = trusty_analyze::core::analyze_diff_with_client(&diff, &search, &index_id)
        .await
        .context("analyze PR diff")?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).context("serialize report")?
            );
        }
        OutputFormat::Text => {
            print!("{}", trusty_analyze::core::render_review_text(&report));
        }
    }

    if post_comment {
        let markdown = trusty_analyze::core::format_review_as_markdown(&report);
        trusty_analyze::core::post_pr_comment(&client, owner, repo_name, pr, &markdown, &token)
            .await
            .with_context(|| format!("post review comment to {owner}/{repo_name}#{pr}"))?;
        println!("Posted review comment to {owner}/{repo_name}#{pr}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#6287): `trusty-analyze deep` dialled `http://127.0.0.1:<port>`, so
    /// after the daemon moved to a socket it would have failed with a
    /// connection-refused against a port nothing binds — or worse, reached an
    /// unrelated process that had taken 7879. This is what proves the CLI dials
    /// the socket: it fails naming the socket path, which the HTTP version
    /// could not do.
    /// What: points `run_deep` at a path in an empty temp dir and asserts the
    /// error names it.
    ///
    /// #6350: `TRUSTY_ANALYZE_EXTERNAL=1` is set for the duration, and it is
    /// load-bearing twice over. `run_deep` now starts the server before
    /// dialling, so without the opt-out this test would spawn a real
    /// `trusty-analyze` on any machine that has one installed, wait out the
    /// 20-second spawn budget, and then assert against a spawn error instead of
    /// the dial error it exists to prove. With the opt-out the client dials
    /// whatever is there and nothing is — which is exactly the arrangement an
    /// operator running their own server has. The START failure is covered
    /// separately, against a real binary, in `tests/on_demand.rs`.
    /// Test: this is the test.
    #[serial_test::serial]
    #[tokio::test]
    async fn run_deep_reports_an_absent_socket() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = tmp.path().join("absent.sock");
        // SAFETY: `#[serial]` keeps every other test in this binary off the
        // environment for the duration, which is the precondition `set_var` /
        // `remove_var` carry in edition 2024.
        unsafe { std::env::set_var(trusty_common::uds::ANALYZE_EXTERNAL_ENV, "1") };
        let result = run_deep("idx".into(), None, OutputFormat::Json, &socket).await;
        unsafe { std::env::remove_var(trusty_common::uds::ANALYZE_EXTERNAL_ENV) };
        let err = result.expect_err("an absent socket cannot answer");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains(&socket.display().to_string()),
            "the error must name the socket it could not reach: {rendered}"
        );
        assert!(
            rendered.contains("analyze.deep_analysis"),
            "the error must name the method: {rendered}"
        );
    }
}
