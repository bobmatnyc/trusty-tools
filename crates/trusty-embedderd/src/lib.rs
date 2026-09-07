//! `trusty-embedderd` library entry point.
//!
//! Why: exposes the daemon's startup logic as a library function so that
//! downstream consumers (notably the `trusty-embedderd` shim binary bundled
//! inside `trusty-search`) can call into it without duplicating any logic.
//! The binary in this crate and the bundled binary in `trusty-search` both
//! call `trusty_embedderd::run()` — one command, two install paths, zero
//! divergence.
//!
//! What: re-exports the internal submodules (for integration tests that import
//! by module path) and provides `run()`, which parses `std::env::args()` via
//! clap, resolves the transport, initialises tracing, loads the ONNX model, and
//! serves. Returns `Ok(())` on clean shutdown; propagates any startup error.
//!
//! #6289: this daemon binds no TCP socket. The `--http` listener it used to
//! offer at `127.0.0.1:7890` is retired under
//! [ADR-0032](https://github.com/bobmatnyc/trusty-tools/blob/main/docs/adr/0032-no-service-owns-http-console-is-the-only-http-surface.md);
//! `--stdio` (the auto-spawn transport) and `--socket` (a hardened Unix socket)
//! are the two transports, and passing `--http` is refused at parse time.
//!
//! Issue #1633: the ONNX model load is bounded by `readiness::run_bounded`
//! (default 180 s, `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS`). A provider-init
//! deadlock (observed on AL2023/glibc 2.34 — the CPU(no-arena) execution
//! provider blocks in `futex_wait_queue` indefinitely) now fails loudly with
//! a nonzero exit and an actionable stderr message instead of hanging the
//! process forever with no stdio/UDS listener ever bound. Because the listener
//! is only started *after* this call returns `Ok`, the daemon structurally
//! cannot report readiness (a stdio response, a UDS accept) while init is still
//! outstanding — there is no code path that answers anything until model load
//! has actually succeeded.
//!
//! Test: `cargo test -p trusty-embedderd` (unit + integration), specifically
//! `tests/no_tcp_listener.rs` for the retired listener and
//! `tests/concurrent_embed.rs` for the UDS transport. The `run()` path is
//! exercised indirectly by `embedder_supervisor_e2e` integration tests in
//! `trusty-search` (which spawn the binary). The bounded model-init mechanics
//! are unit-tested in `readiness` against synthetic futures (no ONNX runtime
//! required).

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

pub mod batch_queue;
pub mod protocol;
pub mod stdio_server;
pub mod uds_server;

// Why (issue #1633): bounds the model-init call below so an ORT
// provider-init deadlock (observed on AL2023/glibc 2.34) fails loudly within
// a fixed ceiling instead of hanging the process forever. Private — no
// public API surface change, gated behind `daemon` since that is the
// only place `FastEmbedder::new()` is called from this crate.
#[cfg(feature = "daemon")]
mod readiness;

// Why (issue #2222): a pre-init glibc probe that fails immediately (instead
// of waiting out the full `readiness::run_bounded` timeout) when the host
// glibc is too old for the bundled ONNX Runtime. Private, same `daemon`
// gate as `readiness` — see `glibc_probe`'s module doc for the full
// rationale and why only the Linux/glibc + `bundled-ort` call site actually
// invokes the check. Also compiled under `cfg(test)` regardless of target so
// the pure version-parsing/comparison unit tests run on every dev/CI
// platform (e.g. this workspace's macOS dev machines), not just Linux/gnu —
// on a non-Linux/gnu *non-test* build the module would otherwise be
// unreachable dead code, since the only call site is behind the same
// Linux/gnu `cfg`.
#[cfg(all(
    feature = "daemon",
    any(all(target_os = "linux", target_env = "gnu"), test)
))]
mod glibc_probe;

// Why (issue #250, narrowed by #6289): the daemon's startup sequence (`run`,
// `run_with_args`, `resolve_transport`, and the `Args` clap struct that drives
// them) only compiles under the `daemon` feature. The `protocol`,
// `batch_queue`, `stdio_server`, and `uds_server` modules stay unconditional —
// a library consumer that only needs the wire protocol pays for none of the
// daemon's startup machinery. The feature was called `http-server` until
// #6289 retired the HTTP listener it named; `http-server` survives as a
// deprecated alias so an out-of-workspace consumer keeps building.
#[cfg(feature = "daemon")]
use std::path::PathBuf;
#[cfg(feature = "daemon")]
use std::sync::Arc;
#[cfg(feature = "daemon")]
use std::time::Duration;

#[cfg(feature = "daemon")]
use anyhow::{bail, Context, Result};
#[cfg(feature = "daemon")]
use clap::Parser;
#[cfg(feature = "daemon")]
use tokio::signal::unix::{signal, SignalKind};
#[cfg(feature = "daemon")]
use tracing::info;
#[cfg(feature = "daemon")]
use trusty_common::embedder::{Embedder as _, FastEmbedder};

#[cfg(feature = "daemon")]
use batch_queue::{BatchConfig, BatchQueue};

// ── CLI ──────────────────────────────────────────────────────────────────────

/// CLI arguments for `trusty-embedderd`.
///
/// Why: clap derive is the workspace standard for all trusty-* binaries.
///
/// What: `--stdio` for the sidecar transport (piped stdin/stdout) and
/// `--socket` for the hardened Unix-socket listener; exactly one is required.
/// `--batch-size` and `--batch-window-ms` configure the `BatchQueue`
/// coalescing window. `--http` is accepted only so that
/// [`resolve_transport`] can refuse it by name — see that field's docs.
///
/// Test: `bare_invocation_configures_no_transport` and
/// `http_flag_is_refused_naming_the_adr` in `tests/no_tcp_listener.rs`.
#[cfg(feature = "daemon")]
#[derive(Parser, Debug)]
#[command(
    name = "trusty-embedderd",
    version,
    about = "Unified ONNX embedding daemon for trusty-tools (issue #164 consolidation)."
)]
pub struct Args {
    /// Run in stdio sidecar mode: read JSON-RPC requests from stdin,
    /// write responses to stdout. Mutually exclusive with --socket.
    /// This is the transport used when trusty-search auto-spawns
    /// trusty-embedderd as a child process (issue #110 Phase 2 default).
    ///
    /// Why: avoids socket-file management — the parent owns the pipe handles
    /// and the child exits automatically when the parent closes its end.
    #[arg(long, conflicts_with = "socket")]
    pub stdio: bool,

    /// Retired. Present only so the daemon can refuse it by name.
    ///
    /// Why (#6289): silently ignoring a flag that used to open a TCP listener
    /// would leave an operator believing the daemon is reachable on
    /// `127.0.0.1:7890`. Clap's own "unexpected argument" error would not say
    /// why it went away, so the flag stays parseable and
    /// [`resolve_transport`] turns it into an error naming ADR-0032.
    ///
    /// `num_args = 0..=1` so a bare `--http` lands in the same refusal as
    /// `--http 127.0.0.1:7890`. Hidden from `--help`: it is not an option, it
    /// is a gravestone.
    #[arg(long = "http", value_name = "ADDR", num_args = 0..=1,
          default_missing_value = "", hide = true)]
    pub http: Option<String>,

    /// Path for the Unix domain socket.
    ///
    /// Why: the in-host transport for consumers that manage the daemon
    /// themselves rather than letting trusty-search auto-spawn it. Bound
    /// through [`uds_server::bind_uds_listener`], which holds the containing
    /// directory at `0700` and the socket at `0600`, and every accepted
    /// connection is checked against this process's own uid.
    #[arg(long, env = "TRUSTY_EMBEDDERD_SOCKET")]
    pub socket: Option<PathBuf>,

    /// Maximum number of texts in one ONNX batch.
    ///
    /// Why: caps the tensor size so the ONNX session doesn't run out of
    /// arena memory on hosts with constrained RAM.
    #[arg(
        long,
        default_value_t = batch_queue::DEFAULT_BATCH_SIZE,
        env = "TRUSTY_EMBED_BATCH_SIZE"
    )]
    pub batch_size: usize,

    /// Batching coalescing window in milliseconds.
    ///
    /// Why: the window lets concurrent callers pile up before the worker
    /// flushes, maximising ONNX throughput. 10 ms is imperceptible to users.
    #[arg(
        long,
        default_value_t = batch_queue::DEFAULT_BATCH_WINDOW_MS,
        env = "TRUSTY_EMBED_BATCH_WINDOW_MS"
    )]
    pub batch_window_ms: u64,
}

// ── Transport selection ──────────────────────────────────────────────────────

/// The transport this daemon will serve on.
///
/// Why (#6289): making the choice a value rather than a pair of booleans is
/// what lets a test assert that no configuration — the default one included —
/// can reach a TCP listener. There is no TCP variant to construct.
///
/// What: `Stdio` reads JSON-RPC frames from stdin; `Uds` serves a hardened
/// Unix socket at the given path. Exactly one is selected per run.
///
/// Test: `bare_invocation_configures_no_transport`,
/// `socket_flag_selects_the_uds_transport` in `tests/no_tcp_listener.rs`.
#[cfg(feature = "daemon")]
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// Newline-framed JSON-RPC 2.0 over piped stdin/stdout.
    Stdio,
    /// Newline-framed JSON-RPC 2.0 over a hardened Unix socket.
    Uds(PathBuf),
}

/// Resolve parsed arguments into the one transport this run will serve.
///
/// Why (#6289): the daemon used to default to binding `127.0.0.1:7890` when
/// neither `--stdio` nor `--socket` was given, which is exactly the surface
/// ADR-0032 removes. Resolving before the model loads also means a bad
/// invocation fails in milliseconds instead of after a 22 MB ONNX download.
///
/// What: refuses `--http` with a message naming ADR-0032; otherwise returns
/// [`Transport::Stdio`] for `--stdio`, [`Transport::Uds`] for `--socket`, and
/// an error naming both flags when neither is given.
///
/// # Errors
///
/// When `--http` is present, or when no transport flag is given.
///
/// Test: `bare_invocation_configures_no_transport`,
/// `http_flag_is_refused_naming_the_adr`,
/// `socket_flag_selects_the_uds_transport`,
/// `stdio_flag_selects_the_stdio_transport` in `tests/no_tcp_listener.rs`.
#[cfg(feature = "daemon")]
pub fn resolve_transport(args: &Args) -> Result<Transport> {
    // #6289: refused, not ignored — see the `Args::http` field docs.
    if args.http.is_some() {
        bail!(
            "--http was removed: trusty-embedderd no longer binds a TCP listener (ADR-0032 — \
             no trusty-* service owns HTTP; trusty-console is the only HTTP surface). \
             Use --socket <path> for a hardened Unix socket, or --stdio for the sidecar \
             transport."
        );
    }

    if args.stdio {
        return Ok(Transport::Stdio);
    }

    match &args.socket {
        Some(path) => Ok(Transport::Uds(path.clone())),
        None => bail!(
            "no transport configured: pass --stdio (sidecar) or --socket <path> \
             (hardened Unix socket). trusty-embedderd binds no TCP port (ADR-0032)."
        ),
    }
}

// ── Library entry point ──────────────────────────────────────────────────────

/// Parse `std::env::args()`, init tracing, load the model, and run the daemon.
///
/// Why: extracted from `main` so the same startup sequence can be invoked from
/// both the standalone `trusty-embedderd` binary and the bundled shim inside
/// `trusty-search`. Both binaries call `trusty_embedderd::run().await` and
/// rely on clap's standard argv parsing — no change to user-visible CLI
/// surface beyond the retired `--http` (#6289).
///
/// What: parse CLI args → [`resolve_transport`] → init tracing to stderr →
/// load `FastEmbedder` → spawn `BatchQueue` → serve the selected transport.
///
/// Note: in `--stdio` mode stdout is reserved for JSON-RPC frames. All
/// tracing goes to stderr in every mode (MCP policy).
///
/// Test: `cargo run -p trusty-search --bin trusty-embedderd -- --socket
/// /tmp/trusty-<uid>/trusty-embedderd.sock`, then embed through
/// `trusty_common::embedder_client::UdsEmbedderClient`. For stdio mode:
/// `cargo run -p trusty-search --bin trusty-embedderd -- --stdio` (parent
/// drives via pipes). Covered indirectly by `embedder_supervisor_e2e` in
/// `trusty-search`.
#[cfg(feature = "daemon")]
pub async fn run() -> Result<()> {
    let args = Args::parse();
    run_with_args(args).await
}

/// Inner entry point that accepts a pre-parsed `Args`.
///
/// Why: allows callers (including tests) to supply a specific `Args` struct
/// rather than relying on `std::env::args()`, which is process-global.
/// What: performs the full daemon startup sequence using the provided args.
/// Test: the public `run()` is tested indirectly via the supervisor e2e tests;
/// the transport decision it makes is unit-tested through
/// [`resolve_transport`] in `tests/no_tcp_listener.rs`.
#[cfg(feature = "daemon")]
pub async fn run_with_args(args: Args) -> Result<()> {
    // #6289: decide the transport before anything expensive happens, so a
    // retired `--http` or a missing flag fails in milliseconds.
    let transport = resolve_transport(&args)?;

    // Init tracing to stderr — never stdout (MCP policy; stdout is used for
    // JSON-RPC frames in --stdio mode).
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("trusty_embedderd=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let config = BatchConfig {
        batch_size: args.batch_size.max(1),
        batch_window: Duration::from_millis(args.batch_window_ms),
    };

    info!(
        "trusty-embedderd starting (transport={:?}, batch_size={}, batch_window_ms={})",
        transport,
        config.batch_size,
        config.batch_window.as_millis(),
    );

    // Why (issue #2222): fail immediately if this host's glibc is too old
    // for the bundled (statically-linked) ONNX Runtime, instead of letting
    // `FastEmbedder::new()` below run into the AL2023/glibc-2.34
    // provider-init deadlock and wait out the full `run_bounded` timeout —
    // a host with a stale glibc can never succeed, so there is no reason to
    // wait. Only applies to Linux/glibc builds compiled with the
    // `bundled-ort` feature; a `load-dynamic` build dlopens a
    // host-supplied `libonnxruntime.so` at runtime, so the bundled-ORT
    // glibc floor does not apply to it. See `glibc_probe` for the full
    // rationale.
    #[cfg(all(target_os = "linux", target_env = "gnu", feature = "bundled-ort"))]
    glibc_probe::check_bundled_ort_glibc_compat()?;

    // Load the ONNX model (expensive one-time init), bounded so a
    // provider-init deadlock (issue #1633 — AL2023/glibc 2.34) fails loudly
    // instead of hanging forever with no listener ever bound.
    info!("loading embedding model...");
    let init_timeout = readiness::model_init_timeout();
    let embedder = readiness::run_bounded(
        "FastEmbedder::new (model load)",
        init_timeout,
        FastEmbedder::new(),
    )
    .await?;
    let dim = embedder.dimension();
    // Issue #3530: report the RESOLVED model name (fp32 default vs the
    // explicit int8 opt-in), not a hardcoded string that goes stale the
    // moment the default changes.
    let model_name = embedder.model_name();
    info!("model loaded: model={model_name} dim={dim}");

    // Spawn the BatchQueue — it owns the embedder exclusively.
    let embedder: Arc<dyn trusty_common::embedder::Embedder> = Arc::new(embedder);
    let queue = Arc::new(BatchQueue::new(embedder, config));
    info!(
        "BatchQueue started (batch_size={}, window_ms={})",
        config.batch_size,
        config.batch_window.as_millis()
    );

    match transport {
        Transport::Stdio => serve_stdio(queue).await,
        Transport::Uds(path) => serve_uds(&path, queue).await,
    }
}

/// Run the stdio sidecar loop until stdin EOF or SIGTERM.
///
/// Why: in stdio mode we own stdout exclusively for JSON-RPC frames, and the
/// OS delivers EOF on stdin when the parent exits — that is the clean
/// termination signal. SIGTERM is still handled so `kill` works from a shell.
/// What: races `stdio_server::run_stdio_server` against SIGTERM.
/// Test: `stdio_flag_selects_the_stdio_transport` proves this arm is the one
/// `--stdio` reaches; the loop itself is `stdio_eof_terminates_cleanly`, and
/// `trusty-search`'s supervisor e2e suite drives it end to end.
#[cfg(feature = "daemon")]
async fn serve_stdio(queue: Arc<BatchQueue>) -> Result<()> {
    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    tokio::select! {
        result = stdio_server::run_stdio_server(Arc::clone(&queue)) => {
            if let Err(e) = result {
                tracing::error!("stdio server error: {e:#}");
                std::process::exit(1);
            }
        }
        _ = sigterm.recv() => {
            info!("received SIGTERM — shutting down");
        }
    }
    Ok(())
}

/// Bind the hardened Unix socket and serve it until SIGTERM/SIGINT.
///
/// Why (#6289): the only listener this daemon opens. `bind_uds_listener` holds
/// the containing directory at `0700` and the socket at `0600`, and the accept
/// loop refuses any peer whose uid is not this process's own (#5099).
/// What: creates the parent directory, binds, spawns the accept loop, waits for
/// a termination signal, then unlinks the socket so the next run starts fresh.
/// Test: `daemon_serves_a_hardened_socket_and_no_tcp_port` in
/// `tests/no_tcp_listener.rs`; concurrency in `tests/concurrent_embed.rs`.
#[cfg(feature = "daemon")]
async fn serve_uds(socket_path: &std::path::Path, queue: Arc<BatchQueue>) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket directory {}", parent.display()))?;
    }
    let listener = uds_server::bind_uds_listener(socket_path)
        .with_context(|| format!("bind UDS socket at {}", socket_path.display()))?;
    info!(
        "trusty-embedderd UDS listening at {}",
        socket_path.display()
    );
    tokio::spawn(uds_server::run_uds_accept_loop(listener, queue));

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
    tokio::select! {
        _ = sigterm.recv() => info!("received SIGTERM — shutting down"),
        _ = sigint.recv() => info!("received SIGINT — shutting down"),
    }

    // Remove the socket file on clean exit so the next run starts fresh.
    if let Err(e) = std::fs::remove_file(socket_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("failed to remove UDS socket on shutdown: {e}");
        }
    }
    Ok(())
}
