//! `trusty-analyze` CLI: sidecar daemon + ad-hoc analysis commands.
//!
//! Subcommands:
//! - `serve`        run HTTP daemon (and, with `--mcp`, an MCP stdio loop)
//! - `analyze`      one-shot complexity hotspot report for an index
//! - `facts list|add|delete`
//! - `health`       probe both daemons

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use trusty_analyze::core::{facts::new_fact, AnalyzerRegistry, FactStore, TrustySearchClient};
#[cfg(any(feature = "bundled-ort", feature = "load-dynamic", feature = "cuda"))]
use trusty_analyze::embedder::NeuralEmbedder;
use trusty_analyze::embedder::{BowEmbedder, Embedder};
use trusty_analyze::mcp::AnalyzerMcpServer;
use trusty_analyze::service::{serve, AnalyzerAppState, DEFAULT_PORT};

mod commands;
use commands::daemon as daemon_cmds;
use commands::service::{run_service_action, ServiceAction as ServiceActionEnum};
use commands::setup::{run_setup, SetupTarget};

/// Bundled declarative help config (issue #216). Loaded once per process.
///
/// Why: every binary in the workspace embeds its `help.yaml` via
/// `include_str!` so the workspace-shared `trusty_common::help::suggest`
/// helper can propose corrections for typos in unknown subcommands.
/// What: `LazyLock<HelpConfig>` parsed from `help.yaml` at first access.
/// Test: parse coverage lives in `trusty-common`; this site is exercised
/// manually via `trusty-analyze healh`.
static HELP: std::sync::LazyLock<trusty_common::help::HelpConfig> =
    std::sync::LazyLock::new(|| {
        trusty_common::help::load_help(include_str!("../help.yaml"))
            .expect("trusty-analyze help.yaml is bundled and valid")
    });

#[derive(Parser, Debug)]
#[command(
    name = "trusty-analyze",
    version,
    about = "Sidecar code-analysis daemon for trusty-search"
)]
struct Cli {
    /// Base URL of the trusty-search daemon. Defaults to http://127.0.0.1:7878.
    #[arg(
        long,
        default_value = "http://127.0.0.1:7878",
        env = "TRUSTY_SEARCH_URL"
    )]
    search_url: String,

    /// Path to the redb facts store (default: `~/.trusty-tools/analyze/facts.redb`).
    /// Override via `TRUSTY_ANALYZER_FACTS`. (#632: home-anchored to fix launchd cwd crash)
    #[arg(long, env = "TRUSTY_ANALYZER_FACTS")]
    facts_path: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the HTTP sidecar daemon.
    Serve {
        /// Run in foreground (used by launchd service).
        #[arg(long, help = "Run in foreground (used by launchd service)")]
        foreground: bool,
        /// Starting port (auto-detect upward if busy). Defaults to 7879.
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Also run an MCP stdio loop on this process. Useful only when invoked
        /// as a subprocess by an MCP client.
        #[arg(long)]
        mcp: bool,
        /// Start MCP HTTP/SSE server on this port (in addition to or instead of stdio).
        /// Exposes `POST /mcp` and `GET /mcp/sse`.
        #[arg(long)]
        mcp_port: Option<u16>,
        /// Path to the fastembed model cache. If the model is not present
        /// here, the neural embedder fails to load and the daemon falls
        /// back to BOW clustering.
        #[arg(
            long,
            default_value = ".fastembed_cache",
            env = "TRUSTY_FASTEMBED_CACHE"
        )]
        fastembed_cache: PathBuf,
    },
    /// One-shot complexity report for a registered index.
    Analyze {
        index_id: String,
        #[arg(long, default_value_t = 20)]
        top_k: usize,
    },
    /// Analyze a unified git diff and produce a quality review report.
    ///
    /// Why: PR review is the highest-leverage moment to catch complexity and
    /// smells — before code lands. Like every other analyzer command, review
    /// is backed by trusty-search: it pulls the named index's chunk corpus so
    /// the report reflects trusty-search's already-computed complexity for the
    /// files the diff touches. Requires trusty-search to be running.
    /// What: reads a unified diff from a file or stdin, parses changed hunks,
    /// fetches the index corpus from trusty-search, merges the two, and prints
    /// a JSON (or text) report.
    /// Test: `echo "$(git diff HEAD~1)" | trusty-analyze review --index-id my-proj -`
    /// prints JSON; with trusty-search down it exits 1 with a clear error.
    Review {
        /// Index ID to cross-reference against in trusty-search (required).
        #[arg(long)]
        index_id: String,
        /// Path to a unified diff file, or "-" to read from stdin.
        #[arg(default_value = "-")]
        diff: String,
        /// Output format: json (default, machine-readable) or text.
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
    /// Run an LLM-augmented deep analysis pass against an index.
    ///
    /// Why: deterministic `review` metrics tell you *what* is wrong; this
    /// subcommand adds an LLM prose narrative that explains *why it matters*
    /// and *what to do*, framework-aware. Separated from `review` so the
    /// deterministic path stays cheap and reproducible.
    /// What: hits `POST /analyze/deep` on the daemon (so trusty-search +
    /// trusty-analyze must both be running). Resolves the OpenRouter key from
    /// `OPENROUTER_API_KEY` on the daemon side and the model from
    /// `TRUSTY_LLM_MODEL` (override with `--model`). Prints the
    /// `DeepAnalysisReport` as JSON or text.
    /// Test: `trusty-analyze deep my-index` against a running daemon with a
    /// configured API key prints a narrative; without a key the daemon
    /// returns 400 and the CLI exits non-zero with a clear error.
    Deep {
        /// Index ID to analyse (required).
        index_id: String,
        /// Optional OpenRouter model id (e.g. `openai/gpt-4o-mini`).
        #[arg(long)]
        model: Option<String>,
        /// Output format: json (default) or text.
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        /// Port the analyzer daemon is bound to.
        #[arg(long, default_value_t = DEFAULT_PORT, env = "TRUSTY_ANALYZER_PORT")]
        port: u16,
    },
    /// Facts subcommands.
    Facts {
        #[command(subcommand)]
        op: FactsCmd,
    },
    /// Probe both daemons.
    Health,
    /// Run an MCP stdio server pointed at the analyzer daemon.
    Mcp {
        /// Base URL of the analyzer daemon. Defaults to http://127.0.0.1:7879.
        #[arg(long, default_value = "http://127.0.0.1:7879")]
        analyzer_url: String,
    },
    /// Open the analyzer dashboard UI in the default browser.
    ///
    /// Why: gives users a one-command path to the embedded UI without having
    /// to remember the port or URL. Probes the daemon first so we fail loudly
    /// with a useful message when the daemon isn't running.
    /// What: TCP-probes `127.0.0.1:<port>`, opens `http://127.0.0.1:<port>/ui`
    /// on success, prints a hint on failure.
    /// Test: run with the daemon down — should print the "not running" hint
    /// and exit non-zero. With the daemon up, should open the browser.
    #[command(alias = "dash")]
    Dashboard {
        /// Port the analyzer daemon is bound to.
        #[arg(long, default_value_t = DEFAULT_PORT, env = "TRUSTY_ANALYZER_PORT")]
        port: u16,
    },
    /// Start the daemon in the background.
    ///
    /// Why: gives users a one-command path to boot the daemon without having
    /// to wire up launchd/systemd. Spawns `trusty-analyze serve` as a detached
    /// child process and writes its PID to `~/.trusty-analyze/daemon.pid`.
    /// What: spawns the current exe with `serve --port <port>` and detaches
    /// stdio. Idempotent: a live PID + reachable port is treated as success.
    /// Test: `trusty-analyze start` followed by `trusty-analyze status` should
    /// report RUNNING; `trusty-analyze stop` should clean up.
    Start {
        /// Port to listen on (default: 7879).
        #[arg(long, default_value_t = DEFAULT_PORT, env = "TRUSTY_ANALYZER_PORT")]
        port: u16,
    },
    /// Stop the running daemon.
    ///
    /// Why: pairs with `start` — sends SIGTERM to the PID recorded at start
    /// time, then waits briefly for the port to close.
    /// What: reads `~/.trusty-analyze/daemon.pid`, invokes `kill -TERM`, polls
    /// the port for up to 5 s, and removes the PID file on success.
    /// Test: with a running daemon → exits 0 with "stopped" message.
    Stop {
        /// Port the daemon is bound to.
        #[arg(long, default_value_t = DEFAULT_PORT, env = "TRUSTY_ANALYZER_PORT")]
        port: u16,
    },
    /// Show daemon status (running/down, port, version).
    ///
    /// Why: more detailed than `health` — focuses on the analyzer daemon
    /// itself (PID, version) rather than the trusty-search pairing.
    /// What: TCP-probes the configured port, reads the PID file, and queries
    /// `/health` for a version string when the daemon answers.
    /// Test: with the daemon down → prints DOWN and exits 0.
    #[command(alias = "st")]
    Status {
        /// Port the daemon is bound to.
        #[arg(long, default_value_t = DEFAULT_PORT, env = "TRUSTY_ANALYZER_PORT")]
        port: u16,
    },
    /// Diagnose configuration and environment issues.
    ///
    /// Why: gives users a self-service "why isn't this working?" path with a
    /// ✓ / ✗ summary per check.
    /// What: verifies the daemon is reachable, the data dir is writable, and
    /// the facts-store path can be opened. Exits non-zero on any failure.
    /// Test: with the daemon down → ✗ for daemon, exits 1.
    Doctor {
        /// Port the daemon is bound to.
        #[arg(long, default_value_t = DEFAULT_PORT, env = "TRUSTY_ANALYZER_PORT")]
        port: u16,
    },
    /// Generate shell completion script.
    ///
    /// Why: shell completion massively improves discoverability for a CLI
    /// with this many subcommands and flags.
    /// What: emits a completion script for the chosen shell to stdout, using
    /// `clap_complete`. Supports bash, zsh, fish, elvish, powershell.
    /// Test: `trusty-analyze completions zsh > /tmp/_trusty-analyze` should
    /// produce a non-empty zsh completion script.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Manage the trusty-analyzer background service (macOS launchd).
    ///
    /// Installs a LaunchAgent plist at
    /// `~/Library/LaunchAgents/com.trusty.trusty-analyze.plist` that runs the
    /// daemon in the foreground under launchd supervision. Not supported on
    /// Linux / Windows — the subcommand exits 1 with a clear message.
    Service {
        #[command(subcommand)]
        action: ServiceSubcommand,
    },
    /// Configure integrations with Claude Code, Cursor, claude-mpm, and daemon.
    ///
    /// Why: wiring trusty-analyze into MCP hosts means writing config files in
    /// host-specific locations; `setup` automates that so users don't hand-edit
    /// JSON or remember plist paths.
    /// What: each subcommand writes (or merges) one configuration artifact;
    /// `setup all` runs every target.
    /// Test: `trusty-analyze setup claude-code --project /tmp/x` writes a
    /// `.mcp.json` containing the `trusty-analyzer` MCP server entry.
    Setup {
        #[command(subcommand)]
        target: SetupTarget,
    },
    /// Fetch a GitHub PR diff and run analysis.
    ///
    /// Why: PR review is the highest-leverage moment to catch complexity and
    /// smells. This pulls the diff straight from GitHub so users don't have to
    /// check out the branch.
    /// What: reads `GITHUB_TOKEN` from the environment, fetches the PR's
    /// unified diff, runs the review pipeline against the named index, and
    /// optionally posts the report back as a PR comment.
    /// Test: `trusty-analyze review-pr owner/repo 12 --index-id x` with no
    /// `GITHUB_TOKEN` set exits 1 with a clear error.
    ReviewPr {
        /// owner/repo (e.g. bobmatnyc/trusty-analyze).
        repo: String,
        /// PR number.
        pr: u64,
        /// trusty-search index ID to cross-reference.
        #[arg(long)]
        index_id: Option<String>,
        /// Post analysis as a GitHub PR comment (requires GITHUB_TOKEN).
        #[arg(long)]
        post_comment: bool,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
}

/// Subcommands for `trusty-analyzer service` (macOS launchd integration).
#[derive(Subcommand, Debug)]
enum ServiceSubcommand {
    /// Install and start as a launchd service
    Install,
    /// Stop and uninstall the service
    Uninstall,
    /// Show service status
    Status,
    /// Tail service logs
    Logs,
}

#[derive(Subcommand, Debug)]
enum FactsCmd {
    /// List all facts (optionally filtered).
    List {
        #[arg(long)]
        subject: Option<String>,
        #[arg(long)]
        predicate: Option<String>,
        #[arg(long)]
        object: Option<String>,
    },
    /// Add (upsert) a fact.
    Add {
        subject: String,
        predicate: String,
        object: String,
        index_id: String,
    },
    /// Delete a fact by its u64 id.
    Delete { id: u64 },
}

/// Output format for the `review` subcommand.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    /// Machine-readable JSON.
    Json,
    /// Human-readable text report.
    Text,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Why: when invoked as an MCP server (`serve --mcp` or `mcp`), stdin/stdout
    // carry JSON-RPC 2.0 framing. Any tracing line emitted on stdout corrupts
    // the protocol and breaks the client. trusty-common's `init_tracing`
    // routes the subscriber to stderr, keeping stdout clean.
    // What: install the shared trusty-common stderr subscriber with the
    // default `info` verbosity (overridable via `RUST_LOG`). Replaces the
    // ad-hoc `tracing_subscriber::fmt().init()` call, which defaulted to
    // stdout and caused the #66 MCP framing corruption.
    // Test: with the daemon running and a search-side error, `serve --mcp`
    // no longer emits a non-JSON line on stdout; covered indirectly by the
    // stdio MCP integration tests.
    trusty_common::init_tracing(1);

    // Why: parse via `try_parse` so we can attach the workspace-shared
    // "did you mean?" suggestion (issue #216) before exiting on a clap error.
    let argv: Vec<String> = std::env::args().collect();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            e.print().ok();
            if matches!(
                e.kind(),
                clap::error::ErrorKind::InvalidSubcommand | clap::error::ErrorKind::UnknownArgument
            ) {
                trusty_common::help::print_suggestion_hint(&argv, &HELP);
            }
            std::process::exit(e.exit_code());
        }
    };
    let search = TrustySearchClient::new(&cli.search_url);
    // Home-anchored default; see daemon_cmds::resolve_facts_path (#632).
    let facts_path = daemon_cmds::resolve_facts_path(cli.facts_path)?;

    // Update check: run only for human-facing commands, never when serving MCP
    // stdio. Two subcommands speak JSON-RPC 2.0 over stdio and must be excluded:
    // - `serve --mcp`: HTTP daemon + inline MCP stdio loop
    // - `mcp`: standalone MCP stdio server pointed at the analyzer daemon
    // In both cases stderr noise may disrupt clients that relay stderr. For
    // `serve` without `--mcp` (HTTP daemon only), the process is long-running
    // with no interactive user to read a banner, so we skip it there too. The
    // check is throttled to once per 24 h (on-disk cache) so it is a
    // sub-millisecond cache read on typical invocations.
    let is_mcp_path = matches!(cli.cmd, Cmd::Serve { .. } | Cmd::Mcp { .. });
    if !is_mcp_path {
        if let Some(info) = trusty_common::update::check_throttled(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        )
        .await
        {
            eprintln!("{}", trusty_common::update::notice(&info));
        }
    }

    match cli.cmd {
        Cmd::Serve {
            foreground: _,
            port,
            mcp,
            mcp_port,
            fastembed_cache,
        } => {
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
        Cmd::Analyze { index_id, top_k } => {
            let chunks = search
                .get_chunks(&index_id)
                .await
                .with_context(|| format!("fetch chunks for {index_id}"))?;
            let report = trusty_analyze::core::quality::aggregate_quality(&chunks);
            println!(
                "Index: {} | chunks: {} | avg cyclomatic: {:.2} | %A: {:.1}% | smells: {}",
                index_id,
                report.chunk_count,
                report.avg_cyclomatic,
                report.pct_grade_a * 100.0,
                report.smell_count
            );
            // Run the language registry for a per-language structural summary.
            let registry = AnalyzerRegistry::default_registry();
            let static_res = registry.analyze(&chunks);
            println!(
                "\nAnalyzed {} chunks across {} files",
                static_res.analyzed_chunks, static_res.analyzed_files
            );
            // Roll up nodes per language.
            use std::collections::BTreeMap;
            let mut per_lang: BTreeMap<String, (usize, usize)> = BTreeMap::new();
            for n in &static_res.graph.nodes {
                per_lang.entry(n.language.clone()).or_insert((0, 0)).0 += 1;
            }
            for e in &static_res.graph.edges {
                if let Some(n) = static_res.graph.nodes.iter().find(|n| n.id == e.from) {
                    per_lang.entry(n.language.clone()).or_insert((0, 0)).1 += 1;
                }
            }
            for (lang, (nodes, edges)) in &per_lang {
                println!("  {lang}: {nodes} nodes, {edges} edges");
            }

            let hotspots = trusty_analyze::core::quality::complexity_hotspots(&chunks, top_k);
            println!("\nTop {top_k} complexity hotspots:");
            for (i, c) in hotspots.iter().enumerate() {
                let cyclo =
                    trusty_analyze::core::complexity::compute_complexity(&c.content).cyclomatic;
                println!(
                    "  {:>3}. cyclo={:>3} {}:{}-{} ({})",
                    i + 1,
                    cyclo,
                    c.file,
                    c.start_line,
                    c.end_line,
                    c.function_name.as_deref().unwrap_or("-")
                );
            }
            Ok(())
        }
        Cmd::Review {
            diff,
            format,
            index_id,
        } => {
            // Hard dependency: review pulls the index corpus from trusty-search,
            // so refuse to run if the search daemon is unreachable.
            // Why: there is no offline mode — review cross-references the
            // already-indexed chunks for the files the diff touches.
            // What: one GET /health probe before reading the diff.
            // Test: run `trusty-analyze review --index-id x` with trusty-search
            // down and verify exit code 1 and the printed error message.
            if !search.health().await.unwrap_or(false) {
                eprintln!(
                    "Error: trusty-search is unreachable at {}. The review command requires trusty-search to be running.",
                    search.base_url()
                );
                std::process::exit(1);
            }
            let diff_text = if diff == "-" {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .context("read diff from stdin")?;
                buf
            } else {
                std::fs::read_to_string(&diff).with_context(|| format!("read diff file {diff}"))?
            };
            let report =
                trusty_analyze::core::analyze_diff_with_client(&diff_text, &search, &index_id)
                    .await
                    .context("analyze diff")?;
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
            Ok(())
        }
        Cmd::Deep {
            index_id,
            model,
            format,
            port,
        } => run_deep(index_id, model, format, port).await,
        Cmd::Facts { op } => {
            let facts = FactStore::open(&facts_path)?;
            match op {
                FactsCmd::List {
                    subject,
                    predicate,
                    object,
                } => {
                    let hits =
                        facts.query(subject.as_deref(), predicate.as_deref(), object.as_deref())?;
                    println!("{} fact(s)", hits.len());
                    for f in hits {
                        println!(
                            "  [{}] ({}) {} --{}--> {}  prov={:?}",
                            f.id, f.index_id, f.subject, f.predicate, f.object, f.provenance
                        );
                    }
                }
                FactsCmd::Add {
                    subject,
                    predicate,
                    object,
                    index_id,
                } => {
                    let f = new_fact(subject, predicate, object, index_id);
                    let id = f.id;
                    facts.upsert(f)?;
                    println!("upserted: {id}");
                }
                FactsCmd::Delete { id } => {
                    let removed = facts.delete(id)?;
                    println!("removed: {removed}");
                }
            }
            Ok(())
        }
        Cmd::Health => {
            let search_ok = search.health().await.unwrap_or(false);
            println!(
                "trusty-search ({}): {}",
                search.base_url(),
                if search_ok { "OK" } else { "DOWN" }
            );
            // The analyzer's own health is queried via HTTP if it's running.
            let analyzer_url = format!("http://127.0.0.1:{}", DEFAULT_PORT);
            let client = reqwest::Client::new();
            let analyzer_ok = client
                .get(format!("{analyzer_url}/health"))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            println!(
                "trusty-analyzer ({analyzer_url}): {}",
                if analyzer_ok { "OK" } else { "DOWN" }
            );
            Ok(())
        }
        Cmd::Mcp { analyzer_url } => {
            let server = AnalyzerMcpServer::new(analyzer_url);
            trusty_analyze::mcp::stdio::run(server).await
        }
        Cmd::Dashboard { port } => {
            use std::net::{SocketAddr, TcpStream};
            use std::time::Duration;
            let addr: SocketAddr = ([127, 0, 0, 1], port).into();
            let reachable = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok();
            if !reachable {
                eprintln!(
                    "Error: trusty-analyze is not running on port {port}.\n       Start it with: trusty-analyze serve"
                );
                std::process::exit(1);
            }
            let url = format!("http://127.0.0.1:{port}/ui");
            println!("Opening {url}");
            open::that(&url).with_context(|| format!("open {url} in browser"))?;
            Ok(())
        }
        Cmd::Service { action } => {
            let action = match action {
                ServiceSubcommand::Install => ServiceActionEnum::Install,
                ServiceSubcommand::Uninstall => ServiceActionEnum::Uninstall,
                ServiceSubcommand::Status => ServiceActionEnum::Status,
                ServiceSubcommand::Logs => ServiceActionEnum::Logs,
            };
            run_service_action(action)
        }
        Cmd::Start { port } => daemon_cmds::handle_start(port),
        Cmd::Stop { port } => daemon_cmds::handle_stop(port),
        Cmd::Status { port } => daemon_cmds::handle_status(port).await,
        Cmd::Doctor { port } => daemon_cmds::handle_doctor(port, &facts_path).await,
        Cmd::Completions { shell } => {
            // Why: clap_complete renders a script for the requested shell from
            // our derived `Cli` definition — keeps completion in sync with the
            // real argument parser.
            // What: build the clap `Command` via `CommandFactory`, then write
            // the completion script to stdout.
            // Test: `cargo run -- completions zsh | head` should print a
            // `#compdef trusty-analyze` line.
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
        Cmd::Setup { target } => run_setup(target).await,
        Cmd::ReviewPr {
            repo,
            pr,
            index_id,
            post_comment,
            format,
        } => run_review_pr(repo, pr, index_id, post_comment, format).await,
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
async fn run_deep(
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
async fn run_review_pr(
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
