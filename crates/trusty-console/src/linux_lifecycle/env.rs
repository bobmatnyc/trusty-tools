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
//! [`EXCLUDED`] and [`EXCLUDED_PREFIXES`] name what is deliberately left out.
//! Limit: the verbs spawn the console with cwd `/`, so a daemon its bridges
//! start loads only the user-level `~/.env.local` tier, never the project tier
//! a shell-started console finds from its own cwd.
//! Test: `env_passthrough_carries_security_and_data_names_and_no_credential`,
//! `env_passthrough_carries_the_token_file_path`,
//! `env_passthrough_carries_the_webhook_secret`,
//! `env_passthrough_applies_the_owner_decisions_and_final_review_lists`,
//! `console_passthrough_carries_every_memory_and_search_name`,
//! `every_clap_env_name_a_bridge_daemon_crate_reads_is_passed_or_excluded`.

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
/// What: every non-credential name the `serve` (analyze) and `tm daemon` (mpm)
/// runtime paths read, and the trusty-common modules they call: literal and
/// constant `env::var` / `var_os` reads, helper lookups over the process
/// environment (`resolve_store_root(&process_env, ..)`, `from_env_with`), clap
/// `env = "NAME"` attributes, the non-credential AWS SDK settings behind the
/// Bedrock provider (profile, region, config file, endpoints, FIPS and
/// dual-stack, IMDS, retries), the `trusty-secrets serve` tunables, and the
/// ssh agent socket. Credentials, every other credential pointer,
/// caller-session identity and hook-time toggles are in [`EXCLUDED`] instead;
/// the passed credential pointers are in [`CREDENTIAL_NAMED_EXCEPTIONS`].
pub(crate) const BRIDGE_DAEMONS: &[&str] = &[
    // #11085: trusty-analyze `Cli::facts_path`, a clap `env` attribute. The
    // auto-start spawns plain `serve`, so this is the only route to the store.
    "TRUSTY_ANALYZER_FACTS",
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
    // #11085: trusty-mpm `architect_store::resolve_store_root(&process_env, ..)`,
    // the daemon's dashboard store root (`daemon::rpc::architect_dashboard`).
    "DASHBOARD_ROOT",
    "ARCHITECT_PROJECT_DIR",
    // #11085: trusty-mpm daemon services, read through `ENV_*` constants:
    // merged-PR reclaim, slot-pool eviction, orphan GC logging, the resume
    // breaker, the auto-resume loop's `SupervisorConfig::from_env`, and the
    // doctor's lock-stall and LaunchAgents checks.
    "TRUSTY_MPM_WORKTREE_RECLAIM",
    "TRUSTY_MPM_WORKTREE_RECLAIM_INTERVAL_SECS",
    "TRUSTY_MPM_SLOT_POOL_EVICT",
    "TRUSTY_MPM_ORPHAN_GC_SKIP_LOG_EVERY",
    "TRUSTY_MPM_RESUME_FLAP_WINDOW_SECS",
    "TRUSTY_MPM_RESUME_FLAP_THRESHOLD",
    "TRUSTY_MPM_RESUME_BOOT_GRACE_SECS",
    "TRUSTY_MPM_SUPERVISOR_INTERVAL",
    "TRUSTY_MPM_SUPERVISOR_CLASSIFY",
    "TRUSTY_MPM_PR_CLEANUP_INTERVAL",
    "TRUSTY_DOCTOR_LOCK_STALL_SECS",
    "TRUSTY_MPM_LAUNCH_AGENTS_DIR",
    // #11085: `core::alt_screen::MANAGED_DEFAULTS`, beside
    // `CLAUDE_CODE_DISABLE_MOUSE`: an operator value wins over the default.
    "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN",
    // #11085: tmux's `default-shell` for a tmux server the daemon starts.
    "SHELL",
    // #11085: the Bedrock provider. `core::sm::providers::resolve` probes the
    // profile flag and the AWS-hosted markers; the aws-config default chain
    // (trusty-common `chat::bedrock_impl`) picks the profile, region, config
    // file, endpoint, FIPS and dual-stack mode, IMDS use and retries from these.
    // `AWS_CONFIG_FILE` is a documented credential-pointer exception.
    "AWS_PROFILE",
    "AWS_DEFAULT_REGION",
    "AWS_CONFIG_FILE",
    "AWS_EXECUTION_ENV",
    "ECS_CONTAINER_METADATA_URI",
    "ECS_CONTAINER_METADATA_URI_V4",
    "AWS_ENDPOINT_URL",
    "AWS_ENDPOINT_URL_BEDROCK_RUNTIME",
    "AWS_USE_FIPS_ENDPOINT",
    "AWS_USE_DUALSTACK_ENDPOINT",
    "AWS_EC2_METADATA_DISABLED",
    "AWS_MAX_ATTEMPTS",
    "AWS_RETRY_MODE",
    // #11085: the mpm daemon spawns `trusty-secrets serve` with its own
    // environment (`mcp_backend`); that server's cache and idle tunables.
    "TRUSTY_SECRETS_CACHE_MAX_AGE_SECS",
    "TRUSTY_SECRETS_IDLE_TIMEOUT_SECS",
    // #11085 (owner decision): the mpm daemon's ssh git fetches keep the ssh
    // agent, as under a shell-started console. A documented exception.
    "SSH_AUTH_SOCK",
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

/// Passed credentials and credential pointers, each passed on purpose.
///
/// Why: #11085 — the passthrough refuses every KEY/TOKEN/SECRET/PASSWORD
/// name and every other credential pointer. These four pass:
/// - `TRUSTY_CONSOLE_TOKEN_FILE`: the token-file PATH, or a restarted console
///   drops its TCP token gate (#11074, #11089).
/// - `GITHUB_WEBHOOK_SECRET`: or the console refuses every webhook (owner).
/// - `SSH_AUTH_SOCK`: bridge-started daemons keep ssh agent access (owner).
/// - `AWS_CONFIG_FILE`: the Bedrock provider finds its profile (owner).
#[cfg(test)]
pub(crate) const CREDENTIAL_NAMED_EXCEPTIONS: &[&str] = &[
    "TRUSTY_CONSOLE_TOKEN_FILE",
    "GITHUB_WEBHOOK_SECRET",
    "SSH_AUTH_SOCK",
    "AWS_CONFIG_FILE",
];

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
        "credential-named; only the documented exceptions pass",
    ),
    (
        "TRUSTY_BUGREPORT_GH_APP_KEY_FILE",
        "credential-named; only the documented exceptions pass",
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
    // #11085: clap `env = "NAME"` attributes no bridge-started daemon acts on.
    (
        "TRUSTY_INDEX",
        "search: pins one `serve` MCP session to a project; `start` ignores it, \
         and the console's own search bridge must stay unpinned across projects",
    ),
    (
        "TRUSTY_FASTEMBED_CACHE",
        "analyze: deprecated `serve` flag, parsed and ignored (#5067)",
    ),
    ("TM_ARCHITECT_STORE", "mpm: read by `tm tui` only"),
    (
        "TRUSTY_MPM_ADDR",
        "mpm: deprecated `daemon --addr`, ignored with a log line (#6288)",
    ),
    (
        "TRUSTY_MPM_TAILSCALE",
        "mpm: deprecated; when set, `daemon` refuses to start (#3330)",
    ),
    // #11085: the full sweep of the mpm and analyze daemon paths.
    (
        "AWS_ACCESS_KEY_ID",
        "credential (mpm provider probe, AWS SDK)",
    ),
    ("AWS_SECRET_ACCESS_KEY", "credential (AWS SDK)"),
    ("AWS_SESSION_TOKEN", "credential (AWS SDK)"),
    (
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "credential pointer; only the documented exceptions pass",
    ),
    (
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
        "credential pointer; only the documented exceptions pass",
    ),
    (
        "AWS_SHARED_CREDENTIALS_FILE",
        "credential pointer; only the documented exceptions pass",
    ),
    (
        "AWS_ROLE_ARN",
        "useless without the excluded AWS_WEB_IDENTITY_TOKEN_FILE, yet \
         `ProviderRegistry::from_env` counts it as AWS credentials, so \
         auto_precedence would pick Bedrock and fail instead of OpenRouter",
    ),
    (
        "TRUSTY_SECRETS_SOCKET",
        "secrets: the console's secrets client dials the fixed socket",
    ),
    (
        "TRUSTY_SECRETS_INDEX_DIR",
        "secrets: the console's secrets client dials the fixed socket",
    ),
    ("GH_ENTERPRISE_TOKEN", "credential (mpm)"),
    ("GITHUB_ENTERPRISE_TOKEN", "credential (mpm)"),
    ("TELEGRAM_BOT_TOKEN", "credential (mpm)"),
    ("SLACK_BOT_TOKEN", "credential (mpm)"),
    ("SLACK_APP_TOKEN", "credential (mpm)"),
    (
        "TRUSTY_LOCAL_API_KEY",
        "credential (trusty-common inference)",
    ),
    (
        "GH_USER",
        "mpm: the caller's gh account; removed from bound children",
    ),
    (
        "DBATG_SESSION",
        "mpm: the caller's PM session; `tm dbatg` only",
    ),
    (
        "SSH_CONNECTION",
        "mpm: presence prompt; `tm presence` CLI only",
    ),
    ("SSH_CLIENT", "mpm: presence prompt; `tm presence` CLI only"),
    ("SSH_TTY", "mpm: presence prompt; `tm presence` CLI only"),
    (
        "TRUSTY_MPM_NOTIFY_INBOX",
        "mpm: hook-time, read by `tm hook` in the managed pane",
    ),
    (
        "CLAUDE_MPM_SUB_AGENT",
        "hook-time guard marker, read in the managed pane",
    ),
    (
        "CLAUDE_PID",
        "the caller's Claude session; scrubbed from children",
    ),
    (
        "CLAUDE_EFFORT",
        "the caller's Claude session; scrubbed from children",
    ),
    (
        "CLAUDE_CODE_EXECPATH",
        "the caller's Claude session; scrubbed from children",
    ),
    (
        "RUSTUP_TOOLCHAIN",
        "ignored: mpm strips it from every child it spawns",
    ),
    (
        "TRUSTY_MEMORY_PALACE",
        "per-caller palace pin; the console's memory bridge stays unpinned",
    ),
    (
        "TRUSTY_RESIDENCY",
        "ignored: trusty-common `residency` defines it; no daemon reads it",
    ),
    (
        "TRUSTY_RESIDENCY_PULL_SECS",
        "ignored: trusty-common `residency` defines it; no daemon reads it",
    ),
    (
        "TRUSTY_RESIDENCY_STALE_SECS",
        "ignored: trusty-common `residency` defines it; no daemon reads it",
    ),
    (
        "TRUSTY_RESIDENCY_GRACE_SECS",
        "ignored: trusty-common `residency` defines it; no daemon reads it",
    ),
    ("TMUX_SOCKET", "mpm: `tm architect` CLI only"),
    ("DASHBOARD_FORMAT", "mpm: `tm architect dashboard` CLI only"),
    ("DASHBOARD_ACTOR", "mpm: `tm architect dashboard` CLI only"),
    (
        "DASHBOARD_SCHEMA_DIR",
        "mpm: `tm architect dashboard` CLI only",
    ),
    (
        "DASHBOARD_LOCAL_URL",
        "mpm: `tm architect dashboard` CLI only",
    ),
    (
        "DASHBOARD_TAILNET_URL",
        "mpm: `tm architect dashboard` CLI only",
    ),
    ("TM_TUI_MOUSE", "mpm: `tm tui` only"),
    ("TM_TUI_TRACE", "mpm: `tm tui` only"),
    ("TERM_PROGRAM", "mpm: `tm tui` only"),
    ("ITERM_SESSION_ID", "mpm: `tm tui` only"),
];

/// Name prefixes deliberately NOT passed, with why.
///
/// Why: #11085 — the console runs the review bridge only for its
/// `console_metrics` poll; review starts no daemon of its own, and each of its
/// settings falls back to its strict default, so a verb-started console
/// changes nothing by dropping them.
#[cfg(test)]
pub(crate) const EXCLUDED_PREFIXES: &[(&str, &str)] = &[
    (
        "PR_INTELLIGENCE_",
        "review: console polls console_metrics only; strict defaults",
    ),
    (
        "TRUSTY_REVIEW_",
        "review: console polls console_metrics only; strict defaults",
    ),
];
