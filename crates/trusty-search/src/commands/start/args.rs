//! The arguments of `trusty-search start`.
//!
//! Why (#9214): `main.rs` sits at its frozen `check_line_cap` budget, and
//! `--no-http` adds a field. The fields moved here verbatim; `main.rs` keeps
//! the subcommand's help text and flattens this struct into it.
//! What: one `clap::Args` struct, read by [`super::handle_start`].
//! Test: `bare_flag_still_means_true`, `no_http_flag_spellings_parse` and
//! `start_socket_flag_takes_an_absolute_path_and_refuses_a_relative_one`
//! parse `start` through the real `Cli`.

/// Every flag `trusty-search start` accepts.
#[derive(clap::Args, Debug, Clone)]
pub struct StartArgs {
    /// Port to listen on (default: 7878, auto-selects next if busy)
    #[arg(long, default_value_t = crate::service::DEFAULT_PORT)]
    pub(crate) port: u16,

    /// Run in the foreground instead of forking a background daemon.
    ///
    /// Default (`trusty-search start`): self-spawns a detached child with
    /// `--foreground` and returns immediately, so the daemon survives the
    /// caller's terminal closing (e.g. tmux pane SIGHUP). Use this flag
    /// when the process is managed by launchd, systemd, or Docker — those
    /// supervisors require the managed binary to stay in the foreground.
    #[arg(long, default_value_t = false)]
    pub(crate) foreground: bool,

    /// Embedding execution device: `auto` (default), `cpu`, or `gpu`.
    ///
    /// - `auto`: prefer CUDA on Linux/Windows (binary must be built with
    ///   `--features cuda`), then CoreML on Apple Silicon, otherwise CPU.
    /// - `cpu`: force CPU even when a GPU is available — useful for A/B
    ///   benchmarking or freeing the GPU for another workload.
    /// - `gpu`: require GPU acceleration; exit 1 if no GPU EP can be
    ///   initialised. Useful on a dedicated GPU indexing node where
    ///   silent CPU fallback would mean a 10× slower reindex.
    ///
    /// Implemented as the `TRUSTY_DEVICE` env var, which the embedder
    /// reads at session-init time. Set explicitly to override the daemon
    /// default.
    #[arg(long, value_parser = ["auto", "cpu", "gpu"], default_value = "auto")]
    pub(crate) device: String,

    /// Override the data directory used by the daemon (lockfile, port file,
    /// indexes.toml, per-index data).
    ///
    /// Equivalent to setting `TRUSTY_DATA_DIR` in the environment.
    /// This flag takes precedence over an inherited `TRUSTY_DATA_DIR` when
    /// both are set (#8149 — the old precedence was the reverse, so a
    /// second daemon bound the first daemon's RPC socket). The directory is
    /// created automatically if it does not exist.
    /// Must be an absolute path.
    ///
    /// Use this to run an isolated daemon (e.g. for cert/benchmark work)
    /// alongside the production daemon without lockfile conflicts:
    ///   trusty-search start --data-dir /tmp/ts-cert --port 7879
    #[arg(long, env = "TRUSTY_DATA_DIR")]
    pub(crate) data_dir: Option<std::path::PathBuf>,

    /// Suppress the auto-discovery scan at startup.
    ///
    /// By default the daemon walks `scan_paths` (from
    /// `~/.config/trusty-search/config.yaml`) after hydrating its registry
    /// from `indexes.toml` and indexes any project not yet registered.
    /// Pass this flag (or set `TRUSTY_NO_AUTO_DISCOVER=1`) to skip that
    /// scan entirely — the daemon will only serve indexes that are already
    /// present in `indexes.toml` or registered manually at runtime.
    ///
    /// Useful when the scan-paths tree is very large, when the daemon is
    /// started in a CI/CD environment that should not discover arbitrary
    /// repositories, or when reproducible startup behaviour is required.
    ///
    /// Precedence: CLI flag > `TRUSTY_NO_AUTO_DISCOVER` env var > default
    /// (auto-discover enabled on the default data directory, disabled on an
    /// explicit `--data-dir` / `TRUSTY_DATA_DIR` — #8176).
    ///
    /// Accepted env values: `1`/`true`/`yes`/`on` and `0`/`false`/`no`/`off`
    /// (case-insensitive). Before #4823 this was a bare `bool`, which under
    /// clap means the CLI flag is a presence flag but the env var goes
    /// through strict `FromStr<bool>` — so the `=1` spelling documented
    /// everywhere else was rejected and the daemon refused to boot.
    // #4823: accept the documented `=1` spelling instead of only
    // `true`/`false`, which aborted startup from a launchd unit.
    #[arg(long, env = "TRUSTY_NO_AUTO_DISCOVER", num_args = 0..=1, require_equals = true, default_value_t = false, default_missing_value = "true", value_parser = crate::commands::service_unit::parse_truthy_bool)]
    pub(crate) no_auto_discover: bool,

    /// Run the auto-discovery scan even on an explicit data directory.
    ///
    /// #8176: a daemon started against an explicit `--data-dir` (or
    /// `TRUSTY_DATA_DIR`) no longer auto-discovers, on any start. A throwaway
    /// instance used to walk `scan_paths` and force-reindex the colocated
    /// `.trusty-search/` stores of every unrelated repository it found,
    /// which is the opposite of what an isolated data directory asks for.
    /// Pass this flag to opt that scan back in; it has no effect on the
    /// machine's default data directory, which still auto-discovers, and
    /// `--no-auto-discover` still wins over it.
    #[arg(long, conflicts_with = "no_auto_discover")]
    pub(crate) auto_discover: bool,

    /// Cap on how many per-index searches run concurrently within a single
    /// cross-project (`search_all` / `POST /search`) fan-out (issue #2845).
    ///
    /// An unbounded fan-out over ~150+ indexes issued every per-index
    /// query near-simultaneously and overran the daemon's admission
    /// limiter (503 `server_busy` storm). This caps in-flight per-index
    /// work so a large fan-out degrades gracefully instead of tripping the
    /// limiter. Default 8. Clamped to `>= 1`.
    ///
    /// Implemented as the `TRUSTY_SEARCH_FANOUT_CONCURRENCY` env var, read
    /// by the daemon per request. A per-request `max_fanout_concurrency`
    /// body field overrides this for a single call. Ignored when
    /// `--serial` is set.
    #[arg(long, value_name = "N")]
    pub(crate) fanout_concurrency: Option<usize>,

    /// Force cross-project fan-out searches to run strictly one index at a
    /// time (issue #2845) — equivalent to `--fanout-concurrency 1`.
    ///
    /// A safety valve that trades fan-out latency for guaranteed
    /// non-overload; useful on memory/CPU-constrained hosts or when the
    /// concurrency limiter is being tripped. Takes precedence over
    /// `--fanout-concurrency`.
    #[arg(long, default_value_t = false)]
    pub(crate) serial: bool,

    /// Serve the RPC socket only: bind no HTTP listener (#9214, ADR-0032).
    ///
    /// The daemon binds no TCP port, writes no `daemon.port` or `http_addr`
    /// file (and removes any an earlier run left), registers no HTTP address
    /// for discovery, and reports `transport.http_addr: null` in
    /// `search.health`. Every route is reached over the socket instead; the
    /// HTTP-only routes (the `/ui` admin panel, `/upgrade`, `/metrics`) are
    /// unavailable. Auto-discovery is skipped, because it registers projects
    /// over HTTP. `--port` is ignored. Off by default: HTTP still binds.
    ///
    /// Also read from `TRUSTY_SEARCH_NO_HTTP` (`1`/`true`/`yes`/`on`;
    /// `0`/`false`/`no`/`off` leave HTTP on), which is how a launchd unit or
    /// a sandbox sets it. Any other value is rejected and the daemon does not
    /// start.
    #[arg(long, env = "TRUSTY_SEARCH_NO_HTTP", num_args = 0..=1, require_equals = true, default_value_t = false, default_missing_value = "true", value_parser = crate::commands::service_unit::parse_truthy_bool)]
    pub(crate) no_http: bool,

    /// Bind the RPC socket at exactly this path (#9214).
    ///
    /// Without it the daemon binds `<TRUSTY_DATA_DIR>/trusty-search.sock`, or
    /// the shared default socket when `TRUSTY_DATA_DIR` is unset. A client
    /// whose `TRUSTY_SEARCH_SOCKET` names another path passes it here when it
    /// auto-starts the daemon, so both sides use one socket. Must be an
    /// absolute path; the parent directory is created if it does not exist.
    /// The lockfile and the port file stay under the data directory.
    #[arg(long, value_name = "PATH", value_parser = parse_socket_path)]
    pub(crate) socket: Option<std::path::PathBuf>,
}

/// Parse `start --socket`: an absolute path, refused otherwise.
///
/// Why: #9214 — a relative socket path resolves against the daemon's cwd
/// (`/` under launchd), so the daemon would bind somewhere no client looks.
/// The data-dir resolver refuses a relative `TRUSTY_DATA_DIR` the same way.
/// What: returns the path unchanged when absolute; otherwise an error naming
/// the flag, which clap reports before the daemon starts.
/// Test: `start_socket_flag_takes_an_absolute_path_and_refuses_a_relative_one`.
pub(crate) fn parse_socket_path(raw: &str) -> Result<std::path::PathBuf, String> {
    let path = std::path::PathBuf::from(raw);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(format!("--socket must be an absolute path (got: {raw:?})"))
    }
}
