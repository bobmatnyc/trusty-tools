//! trusty-console library entry point.
//!
//! Why: Expose the console daemon's startup sequence as a public `run()`
//! function so bundled shim binaries inside host crates (trusty-search,
//! trusty-memory, trusty-analyze, trusty-review, trusty-mpm) can call
//! `trusty_console::run()` without duplicating any logic. This mirrors the
//! exact pattern used by trusty-embedderd (bundled into trusty-search via
//! issue #187) and trusty-bm25-daemon (bundled into trusty-memory via PR #190).
//! What: Re-exports all public submodules and provides `run_from(argv)` as the
//! canonical library entry point that parses an explicit argv vector and
//! dispatches to subcommands; `run()` is a thin wrapper passing the process's
//! global argv.
//! Test: `cargo test -p trusty-console` exercises the CLI parsing tests defined
//! in the submodules.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing::info;
use trusty_common::{init_tracing, shutdown_signal, write_daemon_addr};

/// Default console HTTP bind address (used for both serve and `port` reporting).
///
/// Why: A single constant keeps the serve default and the `port` verb's
/// fallback in lock-step so `trusty-installer` (`tctl`) never discovers a port
/// the console would not actually bind to.
/// What: `127.0.0.1:7788` — the canonical localhost console address.
/// Test: `test_resolve_reported_addr_default` asserts the `port` verb falls
/// back to this value's host/port when no discovery file is present.
pub const DEFAULT_HTTP: &str = "127.0.0.1:7788";

/// Default console port, parsed once from [`DEFAULT_HTTP`].
///
/// Why: The `port` verb reports this when no running console has written a
/// discovery file yet.
/// What: `7788`.
/// Test: covered by the `port` verb tests below.
pub const DEFAULT_PORT: u16 = 7788;

pub mod bind;
pub mod connector;
pub mod detect;
pub mod mcp_handle;
pub mod metrics_poller;
pub mod poller;
pub mod proxy;
pub mod routes;
pub mod server;

// ─── CLI ─────────────────────────────────────────────────────────────────────

/// trusty-console: web dashboard for trusty services.
///
/// Why: Provides a single entry point for all console subcommands so future
/// phases (status, doctor, open) can be added without breaking existing usage.
/// What: Parses top-level arguments and delegates to subcommand handlers.
/// Test: `cargo run -p trusty-console -- serve --help` must succeed.
#[derive(Debug, Parser)]
#[command(
    name = "trusty-console",
    version,
    about = "Web dashboard for trusty services"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// Available subcommands.
///
/// Why: `serve` runs the dashboard; `port` is a non-serving contract verb that
/// reports the console's bound (or default) port so orchestrators like
/// `trusty-installer` (`tctl`) can discover/launch the console without parsing logs.
/// What: Clap enum; each variant carries its own args.
/// Test: Subcommand selection tested via `Cli::parse_from`.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Start the HTTP server and serve the console dashboard.
    Serve(ServeArgs),
    /// Report the console's bound (or default) HTTP port and exit.
    Port(PortArgs),
}

/// Arguments for `trusty-console port`.
///
/// Why: `trusty-installer` (`tctl`) (the orchestrator, issue #1316) discovers
/// the console URL by spawning `trusty-console port --json` and parsing the
/// `{addr,port}` envelope (see trusty-installer `os_env.rs`). The verb must
/// exist and be machine-readable for that discovery to work after the console is
/// de-bundled from the host crates (#1318).
/// What: A single `--json` flag selecting JSON output (default is a single
/// human-readable port line).
/// Test: `test_port_args_json_flag` / `test_port_args_default` below.
#[derive(Debug, Parser)]
pub struct PortArgs {
    /// Emit a JSON envelope `{"addr":"<host>","port":<u16>}` instead of a bare
    /// port number. Consumed by `trusty-installer` (`tctl`) console discovery.
    #[arg(long, default_value_t = false)]
    pub json: bool,
}

/// Arguments for `trusty-console serve`.
///
/// Why: The bind address must be configurable so users can change the port when
/// 7788 is taken; `--open` is a convenience for developers; `--poll-interval`
/// lets operators tune the background health-poll frequency; `--tailscale`
/// enables durable tailnet exposure without requiring `--http 0.0.0.0`.
/// What: Optional `--http` (default `127.0.0.1:7788`), `--open`,
/// `--poll-interval`, and `--tailscale` flags.
/// Env overrides: `TRUSTY_CONSOLE_BIND` sets the default bind mode so a
/// supervised/relaunched daemon stays tailnet-reachable without extra flags.
/// Test: Default address tested in `test_serve_args_defaults` below.
#[derive(Debug, Parser)]
pub struct ServeArgs {
    /// Address to listen on (default: 127.0.0.1:7788).
    ///
    /// Takes precedence over --tailscale and TRUSTY_CONSOLE_BIND when set to a
    /// non-default value.
    #[arg(long, default_value = "127.0.0.1:7788")]
    pub http: String,

    /// Expose the console on both 127.0.0.1 and the machine's Tailscale IPv4,
    /// enabling tailnet clients to reach the console without LAN exposure.
    ///
    /// The Tailscale IP is detected via `tailscale ip -4`. If Tailscale is not
    /// running, prints a warning and falls back to localhost-only.
    ///
    /// Can also be set persistently via the TRUSTY_CONSOLE_BIND=tailscale env
    /// var so a supervised/relaunched console stays tailnet-reachable without
    /// manually passing this flag.
    #[arg(long, default_value_t = false)]
    pub tailscale: bool,

    /// Open the console in the default browser after starting.
    #[arg(long, default_value_t = false)]
    pub open: bool,

    /// Background poll interval in seconds (default: 15).
    ///
    /// Controls BOTH the health poller (`poller::start`) AND the metrics
    /// poller (`metrics_poller::start`). Increasing this value reduces the
    /// frequency of both the HTTP health checks against each connector AND
    /// the stdio MCP `console_metrics` tool calls against trusty-analyze.
    #[arg(long, default_value_t = 15u64)]
    pub poll_interval: u64,
}

// ─── public entry point ────────────────────────────────────────────────────

/// Library entry point for the trusty-console daemon, using the process argv.
///
/// Why: The standalone `trusty-console` crate is now the SOLE producer of the
/// `trusty-console` binary (#1318 — de-bundled from the 5 host crates). The
/// thin `main.rs` calls this, which simply forwards the process's global argv
/// to `run_from`.
/// What: Collects `std::env::args()` and delegates to [`run_from`].
/// Test: Indirectly via the `run_from` tests below and the binary smoke test.
pub async fn run() -> Result<()> {
    run_from(std::env::args().collect()).await
}

/// Library entry point parameterised on an explicit argv vector.
///
/// Why: Decoupling argument parsing from the process's global argv (#1318)
/// lets callers (tests, future embedders) drive the console deterministically
/// without mutating `std::env`. Previously `run()` called `Cli::parse()`,
/// which read global argv and could not be exercised in isolation.
/// What: Initialises tracing, parses `argv` via `Cli::parse_from`, and
/// dispatches to the matching subcommand handler. `argv[0]` is the program
/// name (clap convention). Returns `Ok(())` after clean shutdown.
/// Test: `test_run_from_port_json_outputs_envelope` drives this directly with
/// a synthetic argv; integration via `cargo test -p trusty-console`.
pub async fn run_from(argv: Vec<String>) -> Result<()> {
    init_tracing(1);

    let cli = Cli::parse_from(argv);

    match cli.command {
        Commands::Serve(args) => run_serve(args).await,
        Commands::Port(args) => run_port(args),
    }
}

/// Resolve the console's reportable HTTP address (host, port).
///
/// Why: The `port` verb must report the LIVE port of a running console when
/// one exists, falling back to the default otherwise — so `trusty-installer`
/// (`tctl`) discovery (issue #1316) points at the real dashboard, not a guess.
/// What: Reads the `trusty-console` discovery file via
/// `trusty_common::read_daemon_addr`; on a parseable `host:port` returns that
/// pair, else falls back to ([`DEFAULT_HTTP`] host, [`DEFAULT_PORT`]). Never
/// errors — discovery failures degrade to the default.
/// Test: `test_resolve_reported_addr_default` (no file → default).
pub fn resolve_reported_addr() -> (String, u16) {
    if let Ok(Some(recorded)) = trusty_common::read_daemon_addr("trusty-console")
        && let Ok(sa) = recorded.parse::<std::net::SocketAddr>()
    {
        return (sa.ip().to_string(), sa.port());
    }
    let default_host = DEFAULT_HTTP
        .rsplit_once(':')
        .map(|(h, _)| h.to_owned())
        .unwrap_or_else(|| "127.0.0.1".to_owned());
    (default_host, DEFAULT_PORT)
}

/// Run the `port` subcommand: print the console's bound/default port and exit.
///
/// Why: `trusty-installer` (`tctl`) console discovery spawns
/// `trusty-console port --json` and parses a `{addr,port}` envelope
/// (trusty-installer `os_env.rs`). This verb is the contract that makes that
/// discovery work; without it the call exits non-zero and console discovery is
/// silently broken (the latent bug fixed by #1318).
/// What: Resolves the reportable address; with `--json` prints
/// `{"addr":"<host>","port":<u16>}` to stdout, otherwise prints the bare port.
/// Returns `Ok(())`.
/// Test: `test_run_from_port_json_outputs_envelope`,
/// `test_port_envelope_is_valid_json`.
pub fn run_port(args: PortArgs) -> Result<()> {
    let (addr, port) = resolve_reported_addr();
    if args.json {
        let envelope = serde_json::json!({ "addr": addr, "port": port });
        println!("{envelope}");
    } else {
        println!("{port}");
    }
    Ok(())
}

/// Run the `serve` subcommand.
///
/// Why: Separating the serve logic from `run()` keeps `run()` thin and allows
/// this function to be called from integration tests.
/// What: Resolves bind addresses (respecting `--tailscale`, `--http`, and
/// `TRUSTY_CONSOLE_BIND`), builds the router, binds TCP listener(s), writes
/// the discovery file, starts the background health-poll task, optionally opens
/// a browser, then serves until SIGTERM/SIGINT with graceful shutdown.
/// Additional addresses beyond the primary get their own spawned `axum::serve`
/// task that runs concurrently until the shared shutdown signal fires.
/// Test: Server integration tests in `server.rs` cover the router directly
/// without exercising this function (to avoid real TCP binding in unit tests).
pub async fn run_serve(args: ServeArgs) -> Result<()> {
    // ── resolve bind mode ───────────────────────────────────────────────────
    let mode = bind::BindMode::from_env_and_flags(&args.http, DEFAULT_HTTP, args.tailscale);
    let port = bind::port_from_addr(&args.http, DEFAULT_PORT);
    let addrs = bind::resolve_bind_addrs(&mode, port, bind::detect_tailscale_ipv4);

    // ── service setup ───────────────────────────────────────────────────────
    let connectors = detect::all_connectors();
    let state = server::AppState::new(connectors);

    // Kick off an eager first poll so the cache is warm before the first
    // HTTP request arrives.
    {
        let cache = state.poller_cache().clone();
        let c = state.connectors();
        cache.poll_once(c).await;
    }

    // Start the background poller that refreshes the cache on the configured
    // interval.
    poller::start(
        state.poller_cache().clone(),
        state.connectors(),
        Duration::from_secs(args.poll_interval),
    );

    // ── metrics MCP poll (trusty-analyze) ───────────────────────────────────
    // The analyze handle is stored in AppState so on-demand routes
    // (/api/console/metrics/analyze/indexes, /api/console/metrics/analyze/visualize)
    // share the same child process. Here we hand a clone of that Arc to the
    // background metrics poller so both paths reuse one stdio connection.
    //
    // Why "mcp" not "serve --mcp":
    // `serve --mcp` starts BOTH the HTTP daemon and an MCP stdio loop; it
    // requires trusty-search to be reachable at startup and tries to open the
    // redb facts store (which may already be locked by the running daemon).
    // `mcp` only runs a pure stdio bridge pointing at the running HTTP daemon;
    // if the HTTP daemon is not yet up, `ensure_mcp_daemon_up` in analyze's
    // `mcp` subcommand starts it automatically. This is the correct invocation
    // for a lightweight stdio-only console_metrics child.
    metrics_poller::start(
        state.analyze_handle(),
        state.metrics_cache().clone(),
        Duration::from_secs(args.poll_interval),
    );

    // ── metrics MCP poll (trusty-memory) ────────────────────────────────────
    // trusty-memory's stdio MCP mode is `serve --stdio` (see main.rs).
    // The bridge forwards all JSON-RPC calls to the running HTTP daemon and
    // auto-starts it if absent. On machines without trusty-memory the handle
    // marks it Absent immediately; the cache stays None;
    // /api/console/metrics/memory returns 503 (graceful degradation).
    //
    // The handle comes from AppState::mcp_handles so the services route and
    // the metrics poller share the same McpServiceHandle (and thus the same
    // tools/list probe result — once the probe marks the handle Degraded,
    // that state is visible to both paths without a second probe).
    {
        let handles = state.mcp_handles();
        if let Some(h) = handles.get("trusty-memory") {
            metrics_poller::start(
                Arc::clone(h),
                state.memory_metrics_cache().clone(),
                Duration::from_secs(args.poll_interval),
            );
        } else {
            tracing::warn!(
                service = "trusty-memory",
                "run_serve: no MCP handle registered for trusty-memory — \
                 metrics poller will not start for this service"
            );
        }
    }

    // ── metrics MCP poll (trusty-search) ────────────────────────────────────
    // trusty-search's stdio MCP mode is `serve` (see serve_stdio in main.rs).
    // On machines without trusty-search the handle marks it Absent immediately;
    // the cache stays None; /api/console/metrics/search returns 503.
    //
    // Same shared-handle pattern as trusty-memory above.
    {
        let handles = state.mcp_handles();
        if let Some(h) = handles.get("trusty-search") {
            metrics_poller::start(
                Arc::clone(h),
                state.search_metrics_cache().clone(),
                Duration::from_secs(args.poll_interval),
            );
        } else {
            tracing::warn!(
                service = "trusty-search",
                "run_serve: no MCP handle registered for trusty-search — \
                 metrics poller will not start for this service"
            );
        }
    }

    // ── metrics MCP poll (trusty-review) ────────────────────────────────────
    // trusty-review's stdio MCP mode is `serve --stdio` (see commands/serve.rs).
    // When in stdio mode, trusty-review does NOT start an HTTP daemon — it runs
    // a pure MCP JSON-RPC loop over stdin/stdout, connected to the LLM directly.
    // This is the correct invocation for the console's lightweight metrics poll.
    // On machines without trusty-review the handle marks it Absent immediately;
    // the cache stays None; /api/console/metrics/review returns 503.
    //
    // Same shared-handle pattern as trusty-memory and trusty-search above.
    {
        let handles = state.mcp_handles();
        if let Some(h) = handles.get("trusty-review") {
            metrics_poller::start(
                Arc::clone(h),
                state.review_metrics_cache().clone(),
                Duration::from_secs(args.poll_interval),
            );
        } else {
            tracing::warn!(
                service = "trusty-review",
                "run_serve: no MCP handle registered for trusty-review — \
                 metrics poller will not start for this service"
            );
        }
    }

    // ── metrics MCP poll (trusty-mpm) ───────────────────────────────────────
    // trusty-mpm's stdio MCP mode is `serve --stdio` (the #1221 bridge that
    // auto-starts the durable daemon and forwards JSON-RPC to its loopback
    // POST /rpc). The console_metrics poll keeps the coarse session-fleet +
    // supervisor health cache warm for /api/console/metrics/mpm; the Sessions
    // tab itself polls /api/console/sessions live at a faster cadence (#1222).
    // On machines without trusty-mpm the handle marks it Absent immediately; the
    // cache stays None; /api/console/metrics/mpm returns 503 (graceful).
    {
        let handles = state.mcp_handles();
        if let Some(h) = handles.get("trusty-mpm") {
            metrics_poller::start(
                Arc::clone(h),
                state.mpm_metrics_cache().clone(),
                Duration::from_secs(args.poll_interval),
            );
        } else {
            tracing::warn!(
                service = "trusty-mpm",
                "run_serve: no MCP handle registered for trusty-mpm — \
                 metrics poller will not start for this service"
            );
        }
    }

    let router = server::build_router(state.clone());

    // ── bind primary listener ───────────────────────────────────────────────
    let primary_addr = *addrs.first().context("bind address list is empty")?;
    let primary_listener = bind::bind_listener(primary_addr).await?;
    let primary_local = primary_listener.local_addr().context("get local addr")?;
    let addr_string = primary_local.to_string();
    info!("trusty-console listening on http://{primary_local}");

    // ── bind additional listeners (Tailscale mode: secondary addr) ──────────
    for &extra_addr in addrs.get(1..).unwrap_or(&[]) {
        let extra_listener = bind::bind_listener(extra_addr).await?;
        let extra_local = extra_listener
            .local_addr()
            .context("get extra local addr")?;
        info!("trusty-console also listening on http://{extra_local}");
        eprintln!("trusty-console (tailnet): http://{extra_local}");
        let r = router.clone();
        tokio::spawn(async move {
            if let Err(e) = axum::serve(extra_listener, r)
                .with_graceful_shutdown(trusty_common::shutdown_signal())
                .await
            {
                tracing::warn!("extra listener {extra_local} exited: {e}");
            }
        });
    }

    // ── write discovery file (primary address) ──────────────────────────────
    // Best-effort: log a warning on failure but do not abort the serve.
    if let Err(e) = write_daemon_addr("trusty-console", &addr_string) {
        tracing::warn!("could not write trusty-console discovery file: {e}");
    }

    let console_url = format!("http://{primary_local}");
    eprintln!("trusty-console: {console_url}");

    if args.open {
        // Best-effort browser open; ignore errors.
        let _ = open::that(&console_url);
    }

    axum::serve(primary_listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    // Best-effort removal of the discovery file on clean shutdown.
    // Only remove the file if it still points to our address; another
    // instance may have already written a new one.
    //
    // RESIDUAL RACE: the read → compare → delete sequence is not atomic. A
    // second instance could write a new address between our read and our
    // remove_file, causing us to delete a file we should not. The window is
    // tiny (milliseconds) and the consequence is cosmetic (a stale `port`
    // invocation returns the default rather than the live address). No
    // behavior change is required — this comment documents the known race.
    if let Ok(Some(recorded)) = trusty_common::read_daemon_addr("trusty-console")
        && recorded == addr_string
        && let Ok(dir) = trusty_common::resolve_data_dir("trusty-console")
    {
        let _ = std::fs::remove_file(dir.join("http_addr"));
    }

    Ok(())
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises tests that mutate the `TRUSTY_DATA_DIR_OVERRIDE` env var.
    ///
    /// Why: `std::env::set_var`/`remove_var` are process-global; parallel test
    /// threads racing on them cause flaky failures. A module-level mutex makes
    /// the override-set / call / override-clear sequence atomic per test.
    /// What: a `()` mutex acquired at the top of each env-mutating test.
    /// Test: used by the `port` verb tests below.
    static DATA_DIR_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Why: default http address must be 127.0.0.1:7788 and tailscale off.
    /// What: parses `serve` with no flags and checks all defaults.
    /// Test: this test itself.
    #[test]
    fn test_serve_args_defaults() {
        let cli = Cli::parse_from(["trusty-console", "serve"]);
        match cli.command {
            Commands::Serve(args) => {
                assert_eq!(args.http, "127.0.0.1:7788");
                assert!(!args.open);
                assert!(!args.tailscale);
                assert_eq!(args.poll_interval, 15);
            }
            other => panic!("expected Serve, got {other:?}"),
        }
    }

    /// Why: --tailscale flag must be parsed correctly.
    /// What: parses `serve --tailscale`; asserts tailscale=true.
    /// Test: this test itself.
    #[test]
    fn test_serve_args_tailscale_flag() {
        let cli = Cli::parse_from(["trusty-console", "serve", "--tailscale"]);
        match cli.command {
            Commands::Serve(args) => {
                assert!(args.tailscale);
                assert_eq!(args.http, "127.0.0.1:7788");
            }
            other => panic!("expected Serve, got {other:?}"),
        }
    }

    /// Why: custom --http flag must override the default.
    /// What: parses `serve --http 0.0.0.0:9000`.
    /// Test: this test itself.
    #[test]
    fn test_serve_args_custom_http() {
        let cli = Cli::parse_from(["trusty-console", "serve", "--http", "0.0.0.0:9000"]);
        match cli.command {
            Commands::Serve(args) => {
                assert_eq!(args.http, "0.0.0.0:9000");
            }
            other => panic!("expected Serve, got {other:?}"),
        }
    }

    /// Why: --poll-interval must override the default.
    /// What: parses `serve --poll-interval 30`.
    /// Test: this test itself.
    #[test]
    fn test_serve_args_custom_poll_interval() {
        let cli = Cli::parse_from(["trusty-console", "serve", "--poll-interval", "30"]);
        match cli.command {
            Commands::Serve(args) => {
                assert_eq!(args.poll_interval, 30);
            }
            other => panic!("expected Serve, got {other:?}"),
        }
    }

    /// Why: the `port` subcommand must parse with a default (non-JSON) form so
    /// the bare-port output path is reachable.
    /// What: parses `port` and asserts `--json` defaults to false.
    /// Test: this test itself.
    #[test]
    fn test_port_args_default() {
        let cli = Cli::parse_from(["trusty-console", "port"]);
        match cli.command {
            Commands::Port(args) => assert!(!args.json),
            other => panic!("expected Port, got {other:?}"),
        }
    }

    /// Why: `trusty-installer` (`tctl`) invokes `trusty-console port --json`; the flag must parse.
    /// What: parses `port --json` and asserts `json == true`.
    /// Test: this test itself.
    #[test]
    fn test_port_args_json_flag() {
        let cli = Cli::parse_from(["trusty-console", "port", "--json"]);
        match cli.command {
            Commands::Port(args) => assert!(args.json),
            other => panic!("expected Port, got {other:?}"),
        }
    }

    /// Why: when no console has written a discovery file, the reported port
    /// must fall back to the canonical default so `trusty-installer` (`tctl`)
    /// still gets a usable address.
    /// What: calls `resolve_reported_addr` under an isolated data dir (no
    /// discovery file present) and asserts the default host/port.
    /// Test: this test itself; uses the data-dir override env to avoid reading
    /// a real running console's file.
    #[test]
    fn test_resolve_reported_addr_default() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "trusty-console-port-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).expect("create temp data dir");
        // SAFETY: guarded by data_dir_test_lock to serialise env mutation.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let (addr, port) = resolve_reported_addr();
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
        assert_eq!(addr, "127.0.0.1");
        assert_eq!(port, DEFAULT_PORT);
    }

    /// Why: the JSON envelope emitted by `run_port` must be valid JSON with the
    /// `addr` and `port` keys that `trusty-installer` (`tctl`) `parse_console_port` consumes.
    /// What: builds the same envelope `run_port` prints and round-trips it
    /// through serde to assert structure.
    /// Test: this test itself.
    #[test]
    fn test_port_envelope_is_valid_json() {
        let envelope = serde_json::json!({ "addr": "127.0.0.1", "port": DEFAULT_PORT });
        let s = envelope.to_string();
        let v: serde_json::Value = serde_json::from_str(&s).expect("valid json");
        assert_eq!(v.get("addr").and_then(|a| a.as_str()), Some("127.0.0.1"));
        assert_eq!(
            v.get("port").and_then(|p| p.as_u64()),
            Some(DEFAULT_PORT as u64)
        );
    }

    /// Why: the #1318 decoupling requires that an explicit argv parses to the
    /// `Port` command and that the `port` handler runs without touching the
    /// process's global argv. This exercises that parse → dispatch path.
    /// What: parses `["trusty-console","port","--json"]` via `Cli::parse_from`
    /// (the same call `run_from` makes) and runs `run_port` synchronously under
    /// an isolated data dir; asserts the dispatch matches `Port` and the
    /// handler returns Ok. Kept synchronous so the env-override mutex is never
    /// held across an `await` (clippy::await_holding_lock).
    /// Test: this test itself.
    #[test]
    fn test_run_from_port_json_outputs_envelope() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "trusty-console-runfrom-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).expect("create temp data dir");
        // SAFETY: guarded by DATA_DIR_ENV_LOCK to serialise env mutation.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let argv = [
            "trusty-console".to_owned(),
            "port".to_owned(),
            "--json".to_owned(),
        ];
        let cli = Cli::parse_from(argv);
        let result = match cli.command {
            Commands::Port(args) => {
                assert!(args.json, "argv --json should parse to json=true");
                run_port(args)
            }
            other => panic!("expected Port, got {other:?}"),
        };
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
        assert!(result.is_ok(), "run_port(port --json) should succeed");
    }
}
