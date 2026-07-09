//! Long-form command handlers for the `trusty-analyze` CLI.
//!
//! Why: the `serve`, `deep`, and `review-pr` handler bodies are the largest
//! arms of the `main()` dispatcher; lifting them here keeps `main.rs` under the
//! 500-SLOC production cap (see #1195) while `main` stays a thin router.
//! What: `run_serve` boots the HTTP/MCP daemon, `run_deep` proxies the deep
//! analysis endpoint, and `run_review_pr` fetches + reviews a GitHub PR diff.
//! Test: exercised end-to-end via the CLI (`trusty-analyze serve|deep|review-pr`);
//! error paths (daemon down, missing token) are covered by the existing manual
//! smoke tests documented on each `Cmd` variant.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::ValueEnum;

use trusty_analyze::core::FactStore;
use trusty_analyze::core::TrustySearchClient;
#[cfg(any(feature = "bundled-ort", feature = "load-dynamic", feature = "cuda"))]
use trusty_analyze::embedder::NeuralEmbedder;
use trusty_analyze::embedder::{BowEmbedder, Embedder};
use trusty_analyze::mcp::AnalyzerMcpServer;
use trusty_analyze::service::{serve, AnalyzerAppState};

/// Output format for the `review`, `deep`, and `review-pr` subcommands.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Machine-readable JSON.
    Json,
    /// Human-readable text report.
    Text,
}

/// Boot the HTTP sidecar daemon, optionally alongside MCP stdio / SSE loops.
///
/// Why: `serve` is the process entry point for the daemon and the single most
/// complex CLI arm; keeping it in one focused function documents the startup
/// sequence (search dependency check → embedder load → optional MCP servers).
/// What: probes trusty-search, opens the facts store, loads the neural (or BOW
/// fallback) embedder, warns on a missing OpenRouter key, optionally starts the
/// MCP HTTP/SSE server, then serves the HTTP daemon (with an inline MCP stdio
/// loop when `mcp` is set).
/// Test: run `trusty-analyze serve` without trusty-search running and verify
/// exit code 1; with it running the daemon binds and answers `/health`.
pub async fn run_serve(
    search: TrustySearchClient,
    facts_path: PathBuf,
    port: u16,
    mcp: bool,
    mcp_port: Option<u16>,
    fastembed_cache: PathBuf,
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

    // Try to load the neural embedder. Failure is non-fatal: we fall
    // back to BOW so the daemon still serves clustering requests.
    // Why: keeping the daemon resilient when the ONNX model is
    // missing (CI, fresh machines, offline) is more valuable than
    // hard-failing on startup.
    //
    // Why (issue #536): when no ORT backend feature is compiled in
    // (e.g. --no-default-features --features http-server with no
    // bundled-ort / load-dynamic / cuda), NeuralEmbedder is not
    // available at all. The cfg block below falls through to BOW
    // without requiring any runtime check.
    #[cfg(any(feature = "bundled-ort", feature = "load-dynamic", feature = "cuda"))]
    let embedder: Arc<dyn Embedder> = match NeuralEmbedder::new(Some(&fastembed_cache)) {
        Ok(e) => {
            tracing::info!("neural embedder loaded from {}", fastembed_cache.display());
            Arc::new(e)
        }
        Err(e) => {
            tracing::warn!(
                "neural embedder failed to load from {} ({e:#}); using BOW",
                fastembed_cache.display()
            );
            Arc::new(BowEmbedder::default())
        }
    };
    // When no ORT backend feature is compiled in, always use BOW.
    // The fastembed_cache path is unused in this build variant; the
    // let _ suppresses the dead-variable lint without removing the
    // CLI argument (operators still pass --fastembed-cache even if it
    // has no effect, so we keep the option for forward compatibility).
    #[cfg(not(any(feature = "bundled-ort", feature = "load-dynamic", feature = "cuda")))]
    let embedder: Arc<dyn Embedder> = {
        let _ = &fastembed_cache;
        tracing::info!(
            "no ORT backend compiled in; using BOW embedder \
             (build with bundled-ort, load-dynamic, or cuda for neural embeddings)"
        );
        Arc::new(BowEmbedder::default())
    };
    let state = AnalyzerAppState::new(search, facts).with_embedder(embedder);

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

    // Optionally start the MCP HTTP/SSE server on a separate port.
    // Why: some MCP clients (and remote integrations) prefer HTTP/SSE
    // over stdio. Spawned independently of the analyzer's own HTTP
    // daemon so the two ports stay decoupled.
    // What: binds `--mcp-port` and serves `POST /mcp` + `GET /mcp/sse`
    // pointing the dispatcher at `http://127.0.0.1:<port>`.
    // Test: pass `--mcp-port 7880`, then `curl -X POST
    // http://127.0.0.1:7880/mcp -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'`.
    if let Some(mcp_port) = mcp_port {
        let mcp_srv = AnalyzerMcpServer::new(format!("http://127.0.0.1:{port}"));
        let mcp_listener = tokio::net::TcpListener::bind(("127.0.0.1", mcp_port)).await?;
        tracing::info!("MCP HTTP/SSE server listening on port {mcp_port}");
        tokio::spawn(async move {
            axum::serve(mcp_listener, trusty_analyze::mcp::sse::router(mcp_srv))
                .await
                .ok();
        });
    }

    if mcp {
        // Run both: HTTP daemon in a task, MCP stdio in the foreground.
        let port_for_url = port;
        let http = tokio::spawn(async move {
            if let Err(e) = serve(state, port).await {
                tracing::error!("HTTP daemon exited: {e:#}");
            }
        });
        let mcp_server = AnalyzerMcpServer::new(format!("http://127.0.0.1:{port_for_url}"));
        trusty_analyze::mcp::stdio::run(mcp_server).await?;
        http.abort();
        Ok(())
    } else {
        serve(state, port).await
    }
}

/// Handle `trusty-analyze deep <index_id>`.
///
/// Why: deep analysis is a thin wrapper over `POST /analyze/deep`. Keeping it
/// HTTP-only (rather than re-implementing in-process) means the CLI uses the
/// same code path as MCP clients and external tooling.
/// What: POSTs `{ "index_id": ..., "model": ... }` to the daemon and prints
/// the [`DeepAnalysisReport`] as JSON or text.
/// Test: with the daemon down → exits non-zero with a clear error.
pub async fn run_deep(
    index_id: String,
    model: Option<String>,
    format: OutputFormat,
    port: u16,
) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/analyze/deep");
    let mut body = serde_json::json!({ "index_id": index_id });
    if let Some(m) = model.as_deref() {
        body["model"] = serde_json::Value::String(m.to_string());
    }
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("deep analysis request failed: HTTP {status}: {body}");
    }
    let report: trusty_analyze::core::DeepAnalysisReport = resp
        .json()
        .await
        .with_context(|| format!("decode response from {url}"))?;
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
    let token = std::env::var("GITHUB_TOKEN")
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
