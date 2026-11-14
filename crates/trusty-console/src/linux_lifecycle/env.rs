//! The caller variables a Linux-spawned trusty-console keeps (#11085).
//!
//! Why: `trusty_common::linux_lifecycle::start` clears the spawned daemon's
//! environment down to `DaemonDescriptor::BASE_ENV` plus `env_passthrough`
//! (#11098). A console started any other way (a shell, launchd) inherits the
//! whole environment, so a variable the `serve` path reads that is missing
//! here makes a `start`/`restart` console behave differently.
//! What: [`ENV_PASSTHROUGH`], derived from every non-credential env read on
//! the `serve` path — this crate's `src/` and the trusty-common code it calls —
//! plus the token-file PATH, and [`EXCLUDED`], the names deliberately left out.
//! Test: `env_passthrough_carries_security_and_data_names_and_no_credential`,
//! `env_passthrough_carries_the_token_file_path`.

/// Names copied from the caller's environment, when set, into the console's.
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
    // no token (#11074, #11089). The one credential-named exception:
    // [`CREDENTIAL_NAMED_PATHS`].
    "TRUSTY_CONSOLE_TOKEN_FILE",
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

/// Passed names that look like credentials but hold a file PATH, not a value.
///
/// Why: #11085 — the passthrough refuses every KEY/TOKEN/SECRET/PASSWORD
/// name; the token-file path must still reach the console, or a restarted
/// console drops its TCP token gate (#11074, #11089).
#[cfg(test)]
pub(crate) const CREDENTIAL_NAMED_PATHS: &[&str] = &["TRUSTY_CONSOLE_TOKEN_FILE"];

/// Names the `serve` path reads that are deliberately NOT passed, with why.
#[cfg(test)]
pub(crate) const EXCLUDED: &[(&str, &str)] = &[
    (
        "GITHUB_WEBHOOK_SECRET",
        "secret; the webhook ingress refuses deliveries without it (fail-closed)",
    ),
    (
        "TRUSTY_EXIT_WITH_PARENT",
        "parent-death stamp; #11098 drops it",
    ),
];
