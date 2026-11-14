//! The caller variables a Linux-spawned trusty-console keeps (#11085).
//!
//! Why: `trusty_common::linux_lifecycle::start` clears the spawned daemon's
//! environment down to `DaemonDescriptor::BASE_ENV` plus `env_passthrough`
//! (#11098). A console started any other way (a shell, launchd) inherits the
//! whole environment. Its stdio MCP bridges inherit the console's environment,
//! and a bridge that auto-starts its daemon hands that environment on again, so
//! a name missing here is missing in every daemon a console bridge starts.
//! What: [`effective_passthrough`] is the descriptor's list: [`ENV_PASSTHROUGH`]
//! (the `serve` path's own non-credential reads, the token-file PATH and the
//! webhook secret), [`BRIDGE_DAEMONS`] (the analyze and mpm daemons the bridges
//! auto-start), and trusty-common's `passthrough::MEMORY_DAEMON` and
//! `passthrough::SEARCH_DAEMON`, the lists those daemons' own verbs pass.
//! [`EXCLUDED`] names what is deliberately left out.
//! Test: `env_passthrough_carries_security_and_data_names_and_no_credential`,
//! `env_passthrough_carries_the_token_file_path`,
//! `env_passthrough_carries_the_webhook_secret`,
//! `console_passthrough_carries_every_memory_and_search_name`.

use trusty_common::linux_lifecycle::passthrough;

/// Names the `serve` path itself reads, copied from the caller when set.
pub(crate) const ENV_PASSTHROUGH: &[&str] = &[
    // Logging (`init_tracing`, std backtraces, `tracing_init::maybe_disable_color`).
    "RUST_LOG",
    "RUST_BACKTRACE",
    "NO_COLOR",
    "TERM",
    // reqwest's proxy and CA lookups for the HTTP health probes.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    // `bin_resolve`: the install dir and the dev build dir it searches.
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    // `memory_pressure`: the container probe behind the host metrics.
    "container",
    // #11085: this crate, `bind::BindMode::from_env_and_flags` — tailnet exposure.
    "TRUSTY_CONSOLE_BIND",
    // #11085: trusty-common `console_token::load` — a PATH to the token file,
    // not the token. Dropping it makes the console serve every TCP route with
    // no token (#11074, #11089). A credential-named exception:
    // [`CREDENTIAL_NAMED_EXCEPTIONS`].
    "TRUSTY_CONSOLE_TOKEN_FILE",
    // #11085 (owner decision): `webhook::WebhookIngress::from_env` reads the
    // secret from the environment only, with no fallback, so a console started
    // without it refuses every webhook. The second credential-named exception.
    "GITHUB_WEBHOOK_SECRET",
    // #11085: this crate, `secrets_uds` (registry root, externally run vault).
    "TRUSTY_MPM_ROOT",
    "TRUSTY_SECRETS_EXTERNAL",
    // #11085: trusty-common `uds::on_demand`, the analyze supervisor.
    "TRUSTY_ANALYZE_EXTERNAL",
    "TRUSTY_ANALYZE_IDLE_TIMEOUT_SECS",
    // #11085: trusty-common `mpm_rpc`, `memory_rpc`, `search_rpc` — which
    // sockets the console dials. `TRUSTY_DATA_DIR` picks the search socket.
    "TRUSTY_MPM_SOCKET",
    "TRUSTY_MEMORY_SOCKET",
    "TRUSTY_SEARCH_SOCKET",
    "TRUSTY_DATA_DIR",
    // #11085: trusty-common `error_capture` and `shutdown`.
    "TRUSTY_NO_BUG_CAPTURE",
    "TRUSTY_TERMINATION_GRACE_SECS",
];

/// Non-credential names of the daemons a console bridge auto-starts that have
/// no Linux passthrough list of their own (#11085).
///
/// Why: `trusty-analyze mcp` starts the analyze daemon when its socket does not
/// answer (`commands::daemon_guard`), and so does the review bridge through its
/// analyze subprocess client; `trusty-mpm serve --stdio` starts the mpm daemon
/// (`serve_stdio::ensure_daemon_up`). Both spawns inherit the bridge's
/// environment, which is the console's.
/// What: the names traced from each daemon's `env::var` / `var_os` reads, by
/// literal and by constant. Credentials, caller-session identity and hook-time
/// policy toggles are in [`EXCLUDED`] instead.
pub(crate) const BRIDGE_DAEMONS: &[&str] = &[
    // trusty-analyze daemon: LLM region and model, build-tool and diagnostics
    // deadlines, MCP reply cap, LSP settings, its socket override.
    "AWS_REGION",
    "TRUSTY_AWS_REGION",
    "TRUSTY_LLM_MODEL",
    "TRUSTY_BUILD_TOOL_TIMEOUT_SECS",
    "TRUSTY_DIAGNOSTICS_DEADLINE_SECS",
    "TRUSTY_MCP_MAX_RESPONSE_BYTES",
    "ANALYZE_LSP",
    "ANALYZE_LSP_IDLE_SECS",
    "ANALYZE_LSP_RUST_ANALYZER",
    "TRUSTY_ANALYZE_SOCKET",
    // trusty-mpm daemon: the tmux server it drives, its URL, supervisor and
    // background services, catalog, workspace layout, session launch.
    "TMUX_TMPDIR",
    "TRUSTY_MPM_URL",
    "TRUSTY_MPM_AUTO_RESUME",
    "TRUSTY_MPM_ORPHAN_GC",
    "TRUSTY_MPM_ORPHAN_GC_INTERVAL_SECS",
    "TRUSTY_MPM_CATALOG_REF",
    "TRUSTY_MPM_CATALOG_REPO",
    "TRUSTY_MPM_CATALOG_TTL_HOURS",
    "TRUSTY_MPM_ALLOW_HOST_STATE",
    "TRUSTY_MPM_ALLOW_MCP_SPAWN",
    "TRUSTY_MPM_ALLOW_STALE_INSTALL",
    "TRUSTY_MPM_INPROJECT_HYGIENE",
    "TRUSTY_MPM_SLOT_POOL_EVICT_INTERVAL_SECS",
    "TRUSTY_MPM_DELEGATION_QUIET_STALE_SECS",
    "TRUSTY_MPM_EVENTS_MESSAGE_BODY",
    "TRUSTY_MPM_REPOS_ROOT",
    "TRUSTY_MPM_WORKSPACE_ROOT",
    "TRUSTY_MPM_WORKTREES_DIRNAME",
    "TRUSTY_MPM_SESSION_PROFILE",
    "TM_WORKSPACE_TMUX",
    "TM_DISABLE_SPAWN_DISCLAIM",
    "TM_GH_SHIM_DIR",
    "TRUSTY_CONTENT_OFFLINE",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_DISABLE_MOUSE",
    "GH_CONFIG_DIR",
    "GH_HOST",
    "TMP",
    "USERNAME",
    // trusty-mpm daemon: manager and divert models, the Anthropic endpoint,
    // bug-report app ids (not keys), the Telegram bot's public username.
    "TRUSTY_MANAGER_MODEL",
    "TRUSTY_DIVERT_WORKER_MODEL",
    "TRUSTY_DIVERT_MIN_LINES",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL",
    "TRUSTY_BUGREPORT_GH_APP_ID",
    "TRUSTY_BUGREPORT_GH_INSTALL_ID",
    "TELEGRAM_BOT_USERNAME",
];

/// The descriptor's passthrough: every list above, first occurrence kept.
///
/// Why: #11085 — a memory or search daemon auto-started by a console bridge
/// must see every name its own `start` would pass, or it runs with tenancy
/// enforcement off and another embedder model.
/// What: [`ENV_PASSTHROUGH`], then [`BRIDGE_DAEMONS`], then trusty-common's
/// `MEMORY_DAEMON` and `SEARCH_DAEMON`, with duplicates dropped.
/// Test: `console_passthrough_carries_every_memory_and_search_name`.
pub(crate) fn effective_passthrough() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let all = ENV_PASSTHROUGH
        .iter()
        .chain(BRIDGE_DAEMONS)
        .chain(passthrough::MEMORY_DAEMON)
        .chain(passthrough::SEARCH_DAEMON);
    for name in all {
        if !names.iter().any(|n| n == name) {
            names.push((*name).to_string());
        }
    }
    names
}

/// Passed names that look like credentials, each passed on purpose.
///
/// Why: #11085 — the passthrough refuses every KEY/TOKEN/SECRET/PASSWORD
/// name. The token-file PATH must reach the console, or a restarted console
/// drops its TCP token gate (#11074, #11089); the webhook secret must, or it
/// refuses every webhook (owner decision).
#[cfg(test)]
pub(crate) const CREDENTIAL_NAMED_EXCEPTIONS: &[&str] =
    &["TRUSTY_CONSOLE_TOKEN_FILE", "GITHUB_WEBHOOK_SECRET"];

/// Names the console or its bridges' daemons read that are deliberately NOT
/// passed, with why.
#[cfg(test)]
pub(crate) const EXCLUDED: &[(&str, &str)] = &[
    (
        "TRUSTY_EXIT_WITH_PARENT",
        "parent-death stamp; #11098 drops it",
    ),
    ("OPENROUTER_API_KEY", "credential (analyze, memory, search)"),
    ("GITHUB_TOKEN", "credential (analyze, mpm)"),
    ("GH_TOKEN", "credential (mpm)"),
    ("ANTHROPIC_API_KEY", "credential (mpm)"),
    ("CLAUDE_CODE_OAUTH_TOKEN", "credential (mpm)"),
    ("TRUSTY_BUGREPORT_GITHUB_TOKEN", "credential (mpm)"),
    (
        "TRUSTY_BUGREPORT_TOKEN_FILE",
        "credential-named; only the two exceptions pass",
    ),
    (
        "TRUSTY_BUGREPORT_GH_APP_KEY_FILE",
        "credential-named; only the two exceptions pass",
    ),
    ("TMUX", "the caller's pane, not the daemon's"),
    ("TMUX_PANE", "the caller's pane, not the daemon's"),
    ("TM_MANAGED_SESSION_ID", "the caller's managed session"),
    ("CLAUDE_CODE_SESSION_ID", "the caller's Claude session"),
    ("CLAUDE_PROJECT_DIR", "the caller's Claude project"),
    (
        "TRUSTY_MPM_PM_UNRESTRICTED",
        "hook-time guard toggle, read in the managed pane",
    ),
    (
        "TRUSTY_MPM_PM_DENY_BY_DEFAULT",
        "hook-time guard toggle, read in the managed pane",
    ),
    (
        "TRUSTY_MPM_DISABLE_HOOKS",
        "hook-time toggle, read in the managed pane",
    ),
];
