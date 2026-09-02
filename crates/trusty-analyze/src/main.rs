//! `trusty-analyze` CLI: sidecar daemon + ad-hoc analysis commands.
//!
//! Subcommands:
//! - `serve`        bind the daemon socket (and, with `--mcp`, an MCP stdio loop)
//! - `analyze`      one-shot complexity hotspot report for an index
//! - `facts list|add|delete`
//! - `health`       probe both daemons
//! - `socket`       print the daemon's socket path and whether it is live

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use trusty_analyze::core::{facts::new_fact, AnalyzerRegistry, FactStore, TrustySearchClient};
use trusty_analyze::mcp::AnalyzerMcpServer;

mod commands;

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;
use commands::daemon as daemon_cmds;
use commands::daemon_guard::ensure_daemon_running;
use commands::run::{run_deep, run_review_pr, run_serve, OutputFormat};
use commands::service::{run_service_action, ServiceAction as ServiceActionEnum};
use commands::setup::{run_setup, SetupTarget};
use commands::socket::{handle_socket, SocketFormat};

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
    /// Run the sidecar daemon on its Unix socket.
    Serve {
        /// Run in foreground (used by launchd service).
        #[arg(long, help = "Run in foreground (used by launchd service)")]
        foreground: bool,
        /// Unix socket to bind.
        ///
        /// Defaults to `<data dir>/trusty-analyze.sock` — the same path
        /// `trusty_common::daemon_socket_path("trusty-analyze")` hands every
        /// consumer. Override it and nothing will find this daemon; the flag
        /// exists for tests and for running a second instance deliberately.
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
        /// Retired with the TCP listener (#6287). Accepted and ignored.
        ///
        /// Why this is not simply deleted: the launchd plist on every machine
        /// that installed trusty-analyze before this change still passes
        /// `--port 7879`. Upgrading the BINARY does not rewrite the plist —
        /// only `trusty-analyze service install` does — so a bare `cargo
        /// install` would leave clap exiting 2 on an unexpected argument, and
        /// `KeepAlive::Always` turns that into a permanent crash loop with
        /// nothing in the logs but a usage message.
        #[arg(long, value_name = "PORT", hide = true)]
        port: Option<u16>,
        /// Also run an MCP stdio loop on this process. Useful only when invoked
        /// as a subprocess by an MCP client.
        #[arg(long)]
        mcp: bool,
        /// Retired with the MCP HTTP/SSE transport (#6287). Accepted and
        /// ignored, for the reason [`Cmd::Serve::port`] records.
        #[arg(long, value_name = "PORT", hide = true)]
        mcp_port: Option<u16>,
        /// Deprecated no-op, kept so existing launchd plists and shell
        /// wrappers that pass it keep starting.
        ///
        /// Why (#5067): this pointed at the cache for the neural clustering
        /// embedder. That embedder is gone — it was loaded at every boot,
        /// selected by nobody, and its untimed hf-hub request blocked the
        /// daemon's bind for as long as the request took. Removing the flag
        /// outright would turn an unnecessary startup stall into an
        /// unnecessary startup failure for anyone still passing it.
        /// What: accepted and ignored; hidden from `--help`.
        /// Test: `serve_accepts_deprecated_fastembed_cache_flag`.
        #[arg(long, hide = true, env = "TRUSTY_FASTEMBED_CACHE")]
        fastembed_cache: Option<PathBuf>,
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
    },
    /// Facts subcommands.
    Facts {
        #[command(subcommand)]
        op: FactsCmd,
    },
    /// Probe both daemons.
    Health,
    /// Report the Unix socket the daemon serves on.
    ///
    /// #6287 replaced `trusty-analyze port` and its `http_addr` discovery file:
    /// the daemon binds a socket whose path is derived from the data directory,
    /// so there is no port to report and no file that can go stale.
    ///
    ///   trusty-analyze socket          → bare path: /…/trusty-analyze.sock
    ///   trusty-analyze socket --json   → JSON:      {"socket":"…","serving":true}
    ///
    /// Exits non-zero when nothing is answering on the socket, so shell
    /// substitution (`$(trusty-analyze socket)`) fails safely.
    Socket {
        /// Print a JSON object `{"socket":"…","serving":…}`.
        #[arg(long)]
        json: bool,
    },
    /// Run an MCP stdio server pointed at the analyzer daemon.
    Mcp {
        /// Unix socket the analyzer daemon serves on. Defaults to the derived
        /// path — see `trusty-analyze socket`.
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
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
    Start,
    /// Stop the running daemon.
    ///
    /// Why: pairs with `start` — sends SIGTERM to the PID recorded at start
    /// time, then waits briefly for the socket to stop answering.
    /// What: reads `~/.trusty-analyze/daemon.pid`, invokes `kill -TERM`, polls
    /// the socket for up to 5 s, and removes the PID file on success.
    /// Test: with a running daemon → exits 0 with "stopped" message.
    Stop,
    /// Show daemon status (running/down, socket, version).
    ///
    /// Why: more detailed than `health` — focuses on the analyzer daemon
    /// itself (PID, version) rather than the trusty-search pairing.
    /// What: probes the socket, reads the PID file, and calls `analyze.health`
    /// for a version string when the daemon answers.
    /// Test: with the daemon down → prints DOWN and exits 0.
    #[command(alias = "st")]
    Status,
    /// Diagnose configuration and environment issues.
    ///
    /// Why: gives users a self-service "why isn't this working?" path with a
    /// ✓ / ✗ summary per check.
    /// What: verifies the daemon is serving, the data dir is writable, and
    /// the facts-store path can be opened. Exits non-zero on any failure.
    /// Test: with the daemon down → ✗ for daemon, exits 1.
    Doctor,
    /// Print the crate version, or (with `--json`) the DOC-1
    /// capability-discovery envelope `tctl doctor --self-check` reads.
    ///
    /// Why (#6631): `tctl doctor trusty-analyze --self-check` spawns this
    /// exact form and requires `contract_version` + a non-empty `verbs[]`;
    /// trusty-analyze had no `version` subcommand at all, so the self-check
    /// failed on a clap usage error before it reached the daemon.
    /// What: delegates to `commands::version::run`. Needs no daemon and no
    /// data-directory access — it answers from the binary alone.
    /// Test: `commands::version::envelope_satisfies_the_doc1_self_check`,
    /// `main_tests::version_json_parses_and_carries_the_crate_version`.
    Version {
        /// Emit the DOC-1 capability-discovery envelope as JSON.
        #[arg(long)]
        json: bool,
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
    /// Remove the launchd LaunchAgent an older version installed (macOS).
    ///
    /// #6350: trusty-analyze installs no LaunchAgent. It starts on demand when a
    /// client needs it and exits after an idle window, so a `KeepAlive: Always`
    /// unit would restart it the moment it reclaimed itself. This subcommand is
    /// the migration off one: `service uninstall` unloads `com.trusty.analyze`
    /// and deletes its plist. Not supported on Linux / Windows — the subcommand
    /// exits 1 with a clear message.
    Service {
        #[command(subcommand)]
        action: ServiceSubcommand,
    },
    /// Serve the console webhook relay over a Unix socket, then exit (#5182).
    ///
    /// Why: `trusty-console` has been relaying verified GitHub deliveries to
    /// `trusty-analyze-webhook.sock` since #5089 step 3 and nothing bound it,
    /// so every delivery stayed pending forever.
    /// What: binds that socket in the hardened scratch directory, writes each
    /// delivery to a durable inbox, and acknowledges only after that write is
    /// fsync'd — the ack is what lets console delete its own copy. Not a
    /// resident daemon: console spawns it on demand and SIGTERMs it.
    /// Test: `trusty_analyze::webhook_listener` unit tests, plus the shared
    /// listener suite in trusty-common.
    WebhookListen,

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

    /// Manage inference provider configuration (API keys) — the universal
    /// `config keys set/list/test/unset` surface shared by every trusty-*
    /// binary (epic #2400 Wave 1, #2405).
    Config(trusty_common::inference::config::ConfigCommand),
}

/// Subcommands for `trusty-analyze service` (macOS launchd migration).
///
/// #6350: `install`, `status` and `logs` are gone — see `commands::service`.
/// trusty-analyze installs no unit; `uninstall` removes one a previous version
/// left behind.
#[derive(Subcommand, Debug)]
enum ServiceSubcommand {
    /// Unload and remove the LaunchAgent an older version installed
    Uninstall,
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
    // with no interactive user to read a banner, so we skip it there too.
    // `config` (#2405, LOW fix from PR #2528 review) is also excluded — the
    // universal credential CLI must be genuinely offline, with no network
    // update-check call, so `config keys list` never touches the network. The
    // check is throttled to once per 24 h (on-disk cache) so it is a
    // sub-millisecond cache read on typical invocations.
    let skip_update_check = matches!(
        cli.cmd,
        Cmd::Serve { .. } | Cmd::Mcp { .. } | Cmd::Config(_)
    );
    if !skip_update_check {
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
            socket,
            port,
            mcp,
            mcp_port,
            // #5067: accepted for backward compatibility, deliberately unused.
            fastembed_cache: _,
        } => {
            // #6287: warn rather than fail so a stale launchd plist still starts
            // the daemon. Silence would leave an operator believing they had
            // moved a listener that no longer exists.
            if port.is_some() || mcp_port.is_some() {
                // #6350: `service install` no longer exists — pointing an
                // operator at it turned a warning into a second dead end.
                tracing::warn!(
                    "--port / --mcp-port are retired (#6287): trusty-analyze serves a Unix \
                     socket. Run `trusty-analyze service uninstall` to evict the stale launchd \
                     plist that is still passing them, and `trusty-analyze socket` to see the \
                     path."
                );
            }
            run_serve(search, facts_path, socket, mcp).await
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
            for (i, h) in hotspots.iter().enumerate() {
                let c = &h.chunk;
                println!(
                    "  {:>3}. cyclo={:>3} {}:{}-{} ({})",
                    i + 1,
                    h.cyclomatic,
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
        } => {
            let socket = trusty_analyze::service::socket_path()?;
            run_deep(index_id, model, format, &socket).await
        }
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
            // #6287: the analyzer's own health comes off its socket. The #4392
            // HTTP_PROXY hazard is gone with the HTTP client — a Unix socket
            // has no proxy to be routed through.
            let socket = trusty_analyze::service::socket_path()?;
            let analyzer_ok =
                trusty_common::uds::socket_is_serving(&socket, std::time::Duration::from_secs(2))
                    .await;
            println!(
                "trusty-analyzer ({}): {}",
                socket.display(),
                if analyzer_ok { "OK" } else { "DOWN" }
            );
            Ok(())
        }
        Cmd::Socket { json } => {
            handle_socket(if json {
                SocketFormat::Json
            } else {
                SocketFormat::Path
            })
            .await
        }
        Cmd::Mcp { socket } => {
            // Auto-start the daemon when nothing is serving. Why: the `mcp`
            // subcommand is a stdio bridge that forwards every tool call to the
            // daemon; with the daemon down, every tool call fails with a
            // transport error. Auto-starting matches trusty-memory and
            // trusty-search (issue #1078).
            let socket = match socket {
                Some(p) => p,
                None => trusty_analyze::service::socket_path()?,
            };
            ensure_daemon_running(&socket).await?;
            trusty_analyze::mcp::stdio::run(AnalyzerMcpServer::new(socket)).await
        }
        Cmd::Service { action } => {
            let action = match action {
                ServiceSubcommand::Uninstall => ServiceActionEnum::Uninstall,
            };
            run_service_action(action)
        }
        // #6287: no socket argument — `handle_start` resolves the one path it
        // both probes and spawns for. See its doc comment.
        Cmd::Start => daemon_cmds::handle_start(),
        Cmd::Stop => daemon_cmds::handle_stop(&trusty_analyze::service::socket_path()?),
        Cmd::Status => daemon_cmds::handle_status(&trusty_analyze::service::socket_path()?).await,
        Cmd::Doctor => {
            daemon_cmds::handle_doctor(&trusty_analyze::service::socket_path()?, &facts_path).await
        }
        Cmd::Version { json } => {
            commands::version::run(json);
            Ok(())
        }
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
        Cmd::WebhookListen => trusty_analyze::webhook_listener::run(search).await,
        Cmd::ReviewPr {
            repo,
            pr,
            index_id,
            post_comment,
            format,
        } => run_review_pr(repo, pr, index_id, post_comment, format).await,
        Cmd::Config(cmd) => cmd.run().await,
    }
}
