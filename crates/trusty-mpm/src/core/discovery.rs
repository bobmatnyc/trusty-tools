//! Where a `tm` client reaches the daemon: its unix socket, never TCP (#6288).
//!
//! Why: ADR-0032 makes the daemon's unix socket the only same-host transport,
//! and the 2026-10-16 Architect ruling on #6288 (df-D46) moves every in-repo
//! client off `http://127.0.0.1:7880`. Before it, this module resolved an HTTP
//! base URL — explicit flag, then the trusty-console gateway, then the lock
//! file, then `127.0.0.1:7880` — and a stale address could name an unrelated
//! process (#6284). The daemon still binds TCP until #6288 PR 2 retires it,
//! but no client resolves that address any more.
//! What: [`try_resolve_daemon_url`] returns a socket *locator*,
//! `unix:<path>` ([`socket_locator`]), which `DaemonClient::with_client`
//! turns into a socket-only client. An explicit `--url` / `TRUSTY_MPM_URL`
//! is judged by [`explicit_url_notice`] (Q3): a loopback one is ignored with a
//! notice, a remote one is refused. [`resolve_daemon_url`] is the infallible
//! form for fail-open callers (`tm hook`): a refusal becomes a sentinel URL
//! that no transport will send to. The lock-file reader, the isolation test
//! and the console-address helpers stay, for the banner, the statusline and
//! `tm daemon`.
//! Test: the unit tests below; end to end in `tests/tm_cli_socket.rs` and
//! `tests/tm_client_socket_cutover_6288.rs`.

use std::path::{Path, PathBuf};

use crate::core::paths::FRAMEWORK_DIR_NAME;

/// Canonical loopback bind address the daemon listens on by default.
///
/// Why (issue #1268): the daemon's `--addr` default, the thin CLI's `--url`
/// default, and [`DEFAULT_DAEMON_URL`] previously each hard-coded the
/// `127.0.0.1:7880` literal independently. Any one of them drifting (or an
/// operator carrying a stale `TRUSTY_MPM_URL=…:7881` in their environment)
/// produced "daemon: unreachable" because the client probed a port the daemon
/// never bound. Hoisting the port into a single constant — and deriving every
/// other default address string from it — makes the bind side and the client
/// side provably agree from one source of truth.
/// What: the literal `"127.0.0.1:7880"`, parsed into a `SocketAddr` by
/// [`default_daemon_addr`] and embedded into [`DEFAULT_DAEMON_URL`].
/// Test: `default_url_matches_addr`, `default_addr_parses`.
pub const DEFAULT_DAEMON_ADDR: &str = "127.0.0.1:7880";

/// Default daemon URL when no override and no lock file is found.
///
/// Why: derived from [`DEFAULT_DAEMON_ADDR`] so the client default and the
/// daemon bind default can never drift (issue #1268).
/// What: `"http://127.0.0.1:7880"` — the `http://` scheme prepended to
/// [`DEFAULT_DAEMON_ADDR`]. Kept as a `&'static str` literal (rather than a
/// runtime `format!`) so it remains usable in `const`/`default_value` contexts;
/// the `default_url_matches_addr` test guarantees the two stay in lockstep.
/// Test: `default_url_matches_addr`.
pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:7880";

/// Default trusty-console HTTP address (host:port) used when the console's
/// discovery file is absent or unreadable.
///
/// Why: trusty-console defaults to 127.0.0.1:7788; mirroring that here lets
/// the banner and the statusline name the console without importing
/// trusty-console (which would create a circular dependency between crates).
/// What: the literal `"127.0.0.1:7788"`, matching `trusty_console::DEFAULT_HTTP`.
/// Test: `console_addr_from_falls_back_when_absent_or_blank`.
pub const DEFAULT_CONSOLE_ADDR: &str = "127.0.0.1:7788";

/// Parse [`DEFAULT_DAEMON_ADDR`] into a `SocketAddr`.
///
/// Why: the daemon's clap `--addr` argument is typed as `SocketAddr`; this
/// helper lets that default be derived from the shared [`DEFAULT_DAEMON_ADDR`]
/// constant instead of repeating the literal (issue #1268).
/// What: parses the constant; the parse is infallible for a well-formed
/// literal, so a malformed constant is a programmer error caught by
/// `default_addr_parses` at test time.
/// Test: `default_addr_parses`.
pub fn default_daemon_addr() -> std::net::SocketAddr {
    DEFAULT_DAEMON_ADDR
        .parse()
        .expect("DEFAULT_DAEMON_ADDR is a valid SocketAddr literal")
}

/// Path to the daemon lock file.
///
/// Why: the lock file MUST live in the same `~/.trusty-mpm` root as every
/// other framework artifact (logs, sessions, framework dir). It previously
/// resolved under `dirs::config_dir()` (`~/.config/trusty-mpm`), so the daemon
/// wrote the lock to one directory while the rest of the app — and any user
/// inspecting the install — looked in another. That mismatch meant clients
/// that resolved the URL from a differently-configured environment (or simply
/// expected the documented `~/.trusty-mpm` location) never found the lock and
/// fell back to the default port, reporting "daemon unreachable".
/// What: `~/.trusty-mpm/daemon.lock`, derived from the same `home_dir` +
/// [`FRAMEWORK_DIR_NAME`] as `FrameworkPaths`.
/// Test: `lock_file_path_is_under_framework_root`.
pub fn lock_file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(FRAMEWORK_DIR_NAME)
        .join("daemon.lock")
}

/// Read `TRUSTY_MPM_URL` directly from the process environment.
///
/// Why (#2487): clap's `env = "TRUSTY_MPM_URL"` attribute on an
/// `Option<String>` field is the normal way a `tm` subcommand picks up the
/// override, but a couple of call sites resolve the daemon URL BEFORE
/// `Cli::try_parse()` runs — namely `tm projects` bare-TUI interception
/// (`commands::projects::launch_bare_tui`), which must fire ahead of clap so
/// the exact clap "requires a subcommand" usage error is preserved for a
/// non-interactive bare invocation (see that function's doc). Those sites
/// need the identical "did the operator actually set this" semantics as
/// every clap-parsed field, so they call this helper instead of
/// hand-rolling their own `std::env::var` read (and risking a different
/// empty-string or trim rule than the rest of the resolvers).
/// What: `std::env::var("TRUSTY_MPM_URL")`, trimmed, with an empty result
/// treated the same as "unset" (`None`) — matching every `resolve_daemon_url*`
/// empty-string check.
/// Test: `explicit_url_from_env_reads_set_var`,
/// `explicit_url_from_env_treats_empty_as_unset`.
pub fn explicit_url_from_env() -> Option<String> {
    std::env::var("TRUSTY_MPM_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// The prefix of a daemon socket locator, `unix:<path>` (#6288).
///
/// Why: every `tm` command and library client takes the daemon address as a
/// string. A locator in that same string slot moves them all to the socket at
/// once, and `DaemonClient::with_client` is the one place that reads it.
/// What: the literal `unix:`; the rest of the string is the socket path,
/// spaces included (macOS's `Application Support`).
/// Test: `resolved_url_is_the_socket_locator`.
pub const SOCKET_LOCATOR_PREFIX: &str = "unix:";

/// The locator string for the daemon socket at `path`.
pub fn socket_locator(path: &Path) -> String {
    format!("{SOCKET_LOCATOR_PREFIX}{}", path.display())
}

/// The socket path a locator names, or `None` for any other string.
pub fn socket_from_locator(locator: &str) -> Option<PathBuf> {
    locator
        .strip_prefix(SOCKET_LOCATOR_PREFIX)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// Whether `url`'s host is this machine: `localhost` or a loopback IP.
///
/// Why: Q3 of the #6288 ruling turns on it — a loopback `--url` names the
/// same daemon the socket reaches; any other host does not.
/// What: parses `url`; `true` for `localhost` (any case) or an IP whose
/// `is_loopback` holds, bracketed IPv6 included. Unparseable → `false`.
/// Test: `an_explicit_url_is_refused_unless_it_is_loopback`.
pub fn is_loopback_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// What an explicit `--url` / `TRUSTY_MPM_URL` means after the cutover.
///
/// Why (#6288 Q3, Architect ruling df-D46): every client reaches only the
/// LOCAL daemon. A URL naming another host (a `--tailscale` bind) used to
/// target that host; dropping it silently would let a destructive verb act on
/// the local daemon instead. A loopback URL names the daemon the socket
/// reaches, so it is ignored with one stderr line.
/// What: `Ok(None)` for no URL (or a blank one), `Ok(Some(notice))` for a
/// loopback URL, `Err(DaemonUrlError::RemoteUrlRefused)` for anything else,
/// including a URL that does not parse.
/// Test: `an_explicit_url_is_refused_unless_it_is_loopback`,
/// `a_remote_url_is_refused_and_a_loopback_url_is_ignored`.
pub fn explicit_url_notice(url: Option<&str>) -> Result<Option<String>, DaemonUrlError> {
    let Some(url) = url.map(str::trim).filter(|u| !u.is_empty()) else {
        return Ok(None);
    };
    if !is_loopback_url(url) {
        return Err(DaemonUrlError::RemoteUrlRefused {
            url: url.to_string(),
        });
    }
    Ok(Some(format!(
        "tm: ignoring --url/TRUSTY_MPM_URL {url}: tm reaches the local daemon over its unix \
         socket only (#6288)"
    )))
}

/// Resolve the daemon socket locator for a client.
///
/// Why: the single resolution every `tm` command, hook and library client
/// shares (#6288). An isolated process (#9556) that names no socket of its own
/// would otherwise resolve the host's data-dir socket — the live daemon.
/// What: applies [`explicit_url_notice`] (printing the notice to stderr), then
/// returns [`socket_locator`] of `TRUSTY_MPM_SOCKET`, else of the data-dir
/// socket. Under `TRUSTY_SANDBOX=1` with neither `TRUSTY_MPM_SOCKET` nor
/// `TRUSTY_DATA_DIR_OVERRIDE` set, it refuses with
/// [`DaemonUrlError::NoSandboxDaemon`]. Never dials anything.
///
/// # Errors
///
/// [`DaemonUrlError::RemoteUrlRefused`], [`DaemonUrlError::NoSandboxDaemon`],
/// or [`DaemonUrlError::NoSocketPath`] when the data directory cannot be
/// resolved.
///
/// Test: `resolved_url_is_the_socket_locator`,
/// `resolve_refuses_the_host_socket_under_a_bare_sandbox_flag`,
/// `an_explicit_url_is_refused_unless_it_is_loopback`.
pub fn try_resolve_daemon_url(explicit: Option<&str>) -> Result<String, DaemonUrlError> {
    if let Some(notice) = explicit_url_notice(explicit)? {
        eprintln!("{notice}");
    }
    let pinned = std::env::var_os(trusty_common::mpm_rpc::TRUSTY_MPM_SOCKET_ENV)
        .is_some_and(|v| !v.to_string_lossy().trim().is_empty());
    let own_data_dir =
        std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).is_some_and(|v| !v.is_empty());
    // #9556: a bare sandbox flag shares the host's data dir, so its socket is
    // the live daemon's.
    if isolated_environment() && !pinned && !own_data_dir {
        return Err(DaemonUrlError::NoSandboxDaemon);
    }
    let socket = crate::client::http_client::resolve_daemon_socket().map_err(|e| {
        DaemonUrlError::NoSocketPath {
            reason: format!("{e:#}"),
        }
    })?;
    Ok(socket_locator(&socket))
}

/// [`try_resolve_daemon_url`] for a fail-open caller.
///
/// Why: `tm hook` and the long-lived bridges must keep running when the
/// address is refused; they treat the daemon as absent instead.
/// What: the locator, or — after printing the refusal to stderr — the
/// matching sentinel ([`ISOLATED_NO_DAEMON_URL`] or [`REFUSED_DAEMON_URL`]),
/// which `reqwest` and `DaemonClient` refuse before any I/O.
/// Test: `a_refused_resolution_is_a_sentinel_no_transport_sends_to`.
pub fn resolve_daemon_url(explicit: Option<&str>) -> String {
    try_resolve_daemon_url(explicit).unwrap_or_else(|err| refusal_url(&err))
}

/// The resolution `tm`'s top-level dispatch runs once, before any command.
///
/// Why (#6288 Q3): a remote `--url` must stop a command before it acts on the
/// local daemon, but `tm hook` must never fail the user's turn, and a command
/// that needs no daemon must not fail in a sandbox (#9556).
/// What: the locator; a remote-URL refusal is an error unless `fail_open`;
/// every other refusal (and a remote one when `fail_open`) is printed or
/// logged and becomes its sentinel.
///
/// # Errors
///
/// [`DaemonUrlError::RemoteUrlRefused`] when not `fail_open`.
///
/// Test: `an_explicit_url_is_refused_unless_it_is_loopback`.
pub fn resolve_daemon_url_for_dispatch(
    explicit: Option<&str>,
    fail_open: bool,
) -> Result<String, DaemonUrlError> {
    match try_resolve_daemon_url(explicit) {
        Ok(url) => Ok(url),
        Err(err @ DaemonUrlError::RemoteUrlRefused { .. }) if !fail_open => Err(err),
        Err(err) => Ok(refusal_url(&err)),
    }
}

/// The sentinel a fail-open caller runs with after `err` refused resolution.
///
/// The #9556 sandbox refusal is logged, as before; any other refusal is
/// printed, because the operator supplied the address it refuses.
pub fn refusal_url(err: &DaemonUrlError) -> String {
    match err {
        DaemonUrlError::NoSandboxDaemon => {
            tracing::warn!("{err}");
            ISOLATED_NO_DAEMON_URL.to_string()
        }
        _ => {
            eprintln!("tm: {err}");
            REFUSED_DAEMON_URL.to_string()
        }
    }
}

/// The daemon lock file's URL as this process may use it (#9556).
///
/// Why: every reader of the lock — the resolver, the `tm` banner and the
/// statusline — must agree that an isolated process never sees the host
/// daemon's lock, or the banner shows the live 127.0.0.1:7880 as "online".
/// What: the lock's address (product magic and a live pid, as
/// `daemon_identity::read_lock` checks), dropped when the process is in an
/// [`isolated_environment`] and the lock belongs to the host by
/// [`lock_is_hosts`]. Outside isolation it is exactly `read_lock`'s address,
/// and no home lookup runs.
/// Test: `client_lock_url_ignores_a_host_lock_under_isolation`,
/// `client_lock_url_is_read_lock_outside_isolation`.
pub fn client_lock_url() -> Option<String> {
    let url = read_lock_file_url()?;
    // #9556: the home lookups run only when isolated.
    if isolated_environment() {
        let home = dirs::home_dir();
        let passwd_home = crate::core::host_state_gate::passwd_home_dir();
        if lock_is_hosts(&url, home.as_deref(), passwd_home.as_deref()) {
            return None;
        }
    }
    Some(url)
}

/// Whether a daemon lock read from `home` belongs to the host (#9556).
///
/// Why: an isolated process that inherited the operator's `HOME` reads the
/// host daemon's lock, which names the live 127.0.0.1:7880.
/// What: `true` when the lock names [`DEFAULT_DAEMON_URL`], when either home
/// is unknown, or when `home` and the password-database home resolve to the
/// same directory — the comparison `tm daemon --sandbox` makes before it
/// starts.
/// Test: `host_lock_is_judged_by_address_and_home`.
fn lock_is_hosts(
    lock_url: &str,
    home: Option<&std::path::Path>,
    passwd_home: Option<&std::path::Path>,
) -> bool {
    if lock_url.trim_end_matches('/') == DEFAULT_DAEMON_URL {
        return true;
    }
    let (Some(home), Some(account)) = (home, passwd_home) else {
        return true;
    };
    let resolve =
        |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    resolve(home) == resolve(account)
}

/// The base URL an infallible resolver hands out when an isolated process has
/// no daemon of its own (#9556).
///
/// Why: [`resolve_daemon_url`] returns a `String`, and its callers include
/// fail-open paths (`tm hook`). They must still never reach the host daemon.
/// What: a non-HTTP, non-socket scheme. `reqwest` rejects it before any DNS or
/// TCP work ("builder error … URL scheme is not allowed"), and the error names
/// this URL, which states the remedy.
/// Test: `isolated_no_daemon_url_is_refused_before_any_io`.
pub const ISOLATED_NO_DAEMON_URL: &str = "tm-sandbox-no-daemon://set-TRUSTY_MPM_SOCKET";

/// The base URL a fail-open caller runs with after a refused `--url` (#6288).
///
/// Why: as [`ISOLATED_NO_DAEMON_URL`], for a remote `--url`/`TRUSTY_MPM_URL`
/// or an unresolvable socket path.
/// Test: `a_refused_resolution_is_a_sentinel_no_transport_sends_to`.
pub const REFUSED_DAEMON_URL: &str = "tm-daemon-refused://see-stderr";

/// Turn a refusal sentinel back into the error it stands for.
///
/// Why: a caller holding an infallible resolver's answer (bare `tm`'s guided
/// default, the pm-guard dispatch) must stop with the refusal, not misread the
/// refused request as a slow daemon.
/// What: `Err(NoSandboxDaemon)` for [`ISOLATED_NO_DAEMON_URL`],
/// `Err(Refused)` for [`REFUSED_DAEMON_URL`], else `Ok(())`.
/// Test: `isolated_no_daemon_url_maps_back_to_the_refusal`.
pub fn refuse_isolated_no_daemon(url: &str) -> Result<(), DaemonUrlError> {
    match url {
        ISOLATED_NO_DAEMON_URL => Err(DaemonUrlError::NoSandboxDaemon),
        REFUSED_DAEMON_URL => Err(DaemonUrlError::Refused),
        _ => Ok(()),
    }
}

/// Refuse a client-side daemon spawn in an [`isolated_environment`] (#9556).
///
/// Why: `tm start` and the bare-`tm` autostart spawn a plain `tm daemon`, or
/// kickstart the host's launchd job, when nothing answers. In a sandbox that
/// daemon would bind the default 127.0.0.1:7880 (or fall back to an ephemeral
/// port) and write its lock under the inherited `HOME`; the launchd job is the
/// live daemon itself. The only sanctioned sandbox daemon is
/// `scripts/sandbox_daemon.sh` (#9121).
/// What: `Err(DaemonUrlError::NoSandboxDaemon)` when isolated, else `Ok(())`.
/// Test: `daemon_spawn_refused_only_under_isolation`.
pub fn refuse_daemon_spawn_when_isolated() -> Result<(), DaemonUrlError> {
    if isolated_environment() {
        Err(DaemonUrlError::NoSandboxDaemon)
    } else {
        Ok(())
    }
}

/// Why a client could not resolve where the daemon is.
///
/// Why: a refused address must stop a command rather than act on some other
/// daemon (#1737, #6288 Q3), and the caller needs to say which rule refused.
/// What: one variant per refusal; each message names the remedy.
/// Test: `an_explicit_url_is_refused_unless_it_is_loopback`,
/// `resolve_refuses_the_host_socket_under_a_bare_sandbox_flag`.
// #9556 (owner ruling 2026-10-09 17:49Z): non-exhaustive, so a later refusal
// kind is not another breaking change.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum DaemonUrlError {
    /// #6288 Q3: `--url`/`TRUSTY_MPM_URL` names a host other than this one.
    #[error(
        "--url/TRUSTY_MPM_URL {url} names a daemon on another host, but tm reaches only the \
         local daemon, over its unix socket (#6288). Refusing rather than acting on the local \
         daemon; unset --url/TRUSTY_MPM_URL to act locally."
    )]
    RemoteUrlRefused {
        /// The URL that was refused.
        url: String,
    },
    /// #9556: an isolated process names no socket of its own and refuses the
    /// host's.
    #[error(
        "no daemon for this sandbox: TRUSTY_SANDBOX=1 is set, but neither TRUSTY_MPM_SOCKET nor \
         TRUSTY_DATA_DIR_OVERRIDE names this sandbox's daemon socket; refusing the host daemon's \
         socket — start one with scripts/sandbox_daemon.sh and set TRUSTY_MPM_SOCKET to the \
         socket it prints"
    )]
    NoSandboxDaemon,
    /// The data directory, and with it the socket path, could not be resolved.
    #[error("cannot resolve the trusty-mpm daemon socket path: {reason}; set TRUSTY_MPM_SOCKET")]
    NoSocketPath {
        /// Why the data directory did not resolve.
        reason: String,
    },
    /// A fail-open caller ran on [`REFUSED_DAEMON_URL`]; the refusal itself
    /// was printed when it happened.
    #[error("the daemon address was refused (see the message above); no daemon was contacted")]
    Refused,
}

/// Process exit code for a daemon that is unavailable at the CLI boundary.
///
/// Why (issue #1737): shares the exact same value as the pre-existing
/// `commands::prune::EXIT_SM_UNAVAILABLE` (#1313) — both mean "the thing you
/// asked for is unavailable, not a generic failure" and want callers/skills
/// to branch on one stable, non-1 code instead of parsing stderr. Sourced
/// from [`crate::core::exit_codes::EXIT_UNAVAILABLE`], the single shared
/// constant, rather than redefining the literal `75` a second time —
/// `commands::prune::EXIT_SM_UNAVAILABLE` sources from the same constant, so
/// the two can never drift apart.
/// What: `75`.
/// Test: `exit_code_matches_sm_unavailable_convention`.
pub const EXIT_DAEMON_URL_UNREACHABLE: i32 = crate::core::exit_codes::EXIT_UNAVAILABLE;

/// Whether this process runs isolated from the host's trusty-* services.
///
/// Why (#9556): `scripts/sandbox_daemon.sh` sets both variables; either one
/// alone means the operator asked to stay off the host's default endpoints.
/// What: `true` when `TRUSTY_SANDBOX` is exactly `1` (the rule
/// `trusty_common::credentials::sandbox_flag_set` owns) or
/// `TRUSTY_DATA_DIR_OVERRIDE` is set and non-empty.
/// Test: `resolve_refuses_the_host_socket_under_a_bare_sandbox_flag`,
/// `daemon_spawn_refused_only_under_isolation`.
pub fn isolated_environment() -> bool {
    use trusty_common::credentials::{SANDBOX_ENV_VAR, sandbox_flag_set};
    sandbox_flag_set(std::env::var_os(SANDBOX_ENV_VAR).as_deref())
        || std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).is_some_and(|v| !v.is_empty())
}

/// The console address a discovery-file read recorded, trimmed; `None` when
/// absent or blank.
fn recorded_console_addr(recorded: Option<String>) -> Option<String> {
    recorded
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
}

/// Resolve the `host:port` the trusty-console is selected to serve on.
///
/// Why: the `tm statusline` version segment links to the dashboard, and the
/// banner probes the console (#9556). This repo's common-entry-point rule makes
/// a second copy of the discovery-file read a defect, so the read lives here
/// once and every caller routes through it (#6288: the daemon-URL gateway
/// resolver that was the other caller is gone).
/// What: reads the `trusty-console` discovery file via
/// [`trusty_common::read_daemon_addr`]; an absent, unreadable, or blank file
/// falls back to [`DEFAULT_CONSOLE_ADDR`]. Never errors, never probes the
/// network, and blocks on nothing but one small file read — the statusline
/// render path tolerates none of those.
/// Test: `console_addr_from_prefers_the_recorded_addr`,
/// `console_addr_from_falls_back_when_absent_or_blank`.
pub fn console_addr() -> String {
    console_addr_from(
        trusty_common::read_daemon_addr("trusty-console")
            .ok()
            .flatten(),
    )
}

/// Pure core of [`console_addr`] with the discovery-file read injected.
///
/// Why: the fallback rule is the part worth testing, and testing it should not
/// require stubbing `$HOME`.
/// What: returns `recorded` trimmed when it is present and non-blank, else
/// [`DEFAULT_CONSOLE_ADDR`].
/// Test: `console_addr_from_prefers_the_recorded_addr`,
/// `console_addr_from_falls_back_when_absent_or_blank`.
fn console_addr_from(recorded: Option<String>) -> String {
    recorded_console_addr(recorded).unwrap_or_else(|| DEFAULT_CONSOLE_ADDR.to_string())
}

/// The console address a client may contact, or `None` when it must not
/// contact one.
///
/// Why (#9556): the `tm` banner TCP-probes the console; under isolation with
/// no recorded console, the default is the host's live console.
/// What: [`console_addr_to_probe_from`] over the discovery-file read and
/// [`isolated_environment`].
/// Test: `console_probe_target_is_none_under_isolation`.
pub fn console_addr_to_probe() -> Option<String> {
    console_addr_to_probe_from(
        trusty_common::read_daemon_addr("trusty-console")
            .ok()
            .flatten(),
        isolated_environment(),
    )
}

/// Pure core of [`console_addr_to_probe`]: the recorded address, else the
/// default — except under isolation, where there is none (#9556).
/// Test: `console_probe_target_is_none_under_isolation`.
fn console_addr_to_probe_from(recorded: Option<String>, isolated: bool) -> Option<String> {
    recorded_console_addr(recorded)
        .or_else(|| (!isolated).then(|| DEFAULT_CONSOLE_ADDR.to_string()))
}

/// Build the trusty-console's base URL on its selected port.
///
/// Why: the `tm statusline` version segment is a clickable link to the
/// dashboard, and the link target must name the port the console is actually
/// configured for.
/// What: `http://{console_addr()}`. Plain HTTP because the console serves
/// loopback by default (ADR-0018); a console bound elsewhere records that
/// address in the same discovery file, so the host comes from there too.
/// Test: `console_base_url_wraps_the_resolved_addr`.
pub fn console_base_url() -> String {
    format!("http://{}", console_addr())
}

///
/// #1731: the parse, the product-magic check, and the stale-record cleanup all
/// live in [`crate::core::daemon_identity`] — this hand-rolled a second copy of
/// them, and trusted any TOML at the path with an `addr` line.
fn read_lock_file_url() -> Option<String> {
    super::daemon_identity::read_lock().map(|lock| lock.addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_url_matches_addr() {
        // Why (issue #1268): the client default URL and the daemon bind default
        // must agree. `DEFAULT_DAEMON_URL` is `http://` + `DEFAULT_DAEMON_ADDR`.
        assert_eq!(
            DEFAULT_DAEMON_URL,
            format!("http://{DEFAULT_DAEMON_ADDR}"),
            "client default URL and daemon bind addr drifted (issue #1268)"
        );
    }

    #[test]
    fn default_addr_parses() {
        // The shared bind constant must parse into a SocketAddr so the daemon's
        // clap `--addr` default can be derived from it.
        let addr = default_daemon_addr();
        assert_eq!(addr.to_string(), DEFAULT_DAEMON_ADDR);
        assert_eq!(addr.port(), 7880);
    }

    #[test]
    fn lock_file_path_is_under_framework_root() {
        // Why: the lock file must share the `~/.trusty-mpm` root with every
        // other framework artifact. A path under `~/.config` (the previous
        // behaviour) meant the daemon and its clients could disagree on the
        // location and the TUI would report "daemon unreachable".
        let path = lock_file_path();
        assert!(
            path.ends_with(format!("{FRAMEWORK_DIR_NAME}/daemon.lock")),
            "lock file path {path:?} is not under the {FRAMEWORK_DIR_NAME} root"
        );
        // The parent directory is the framework root itself.
        assert_eq!(
            path.parent().and_then(|p| p.file_name()),
            Some(std::ffi::OsStr::new(FRAMEWORK_DIR_NAME))
        );
    }

    /// The exit code for an unavailable daemon matches the
    /// established `EX_TEMPFAIL`-adjacent convention (75) already used by
    /// `commands::prune::EXIT_SM_UNAVAILABLE` (#1737).
    #[test]
    fn exit_code_matches_sm_unavailable_convention() {
        assert_eq!(EXIT_DAEMON_URL_UNREACHABLE, 75);
    }

    // ── explicit_url_from_env tests (#2487) ───────────────────────────────

    /// Serialises the `explicit_url_from_env` tests' `TRUSTY_MPM_URL`
    /// mutation against every other test in the crate that reads/writes
    /// process env vars — `std::env::set_var`/`remove_var` are not
    /// thread-safe across concurrently running tests (the known env-race
    /// flake class in this workspace). Delegates to the crate-wide
    /// [`crate::core::trusty_tools_config::env_test_lock`] so these tests are
    /// serialised against env tests in OTHER modules too, not just each
    /// other.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::core::trusty_tools_config::env_test_lock()
    }

    #[test]
    fn explicit_url_from_env_reads_set_var() {
        let _g = env_lock();
        // SAFETY: guarded by env_lock; restored to the prior value below.
        let prior = std::env::var("TRUSTY_MPM_URL").ok();
        unsafe { std::env::set_var("TRUSTY_MPM_URL", "http://127.0.0.1:7880") };
        let result = explicit_url_from_env();
        match prior {
            Some(v) => unsafe { std::env::set_var("TRUSTY_MPM_URL", v) },
            None => unsafe { std::env::remove_var("TRUSTY_MPM_URL") },
        }
        assert_eq!(
            result.as_deref(),
            Some("http://127.0.0.1:7880"),
            "must read TRUSTY_MPM_URL verbatim, even when it equals the default"
        );
    }

    #[test]
    fn explicit_url_from_env_treats_empty_as_unset() {
        let _g = env_lock();
        let prior = std::env::var("TRUSTY_MPM_URL").ok();
        // SAFETY: guarded by env_lock; restored to the prior value below.
        unsafe { std::env::set_var("TRUSTY_MPM_URL", "   ") };
        let result = explicit_url_from_env();
        match prior {
            Some(v) => unsafe { std::env::set_var("TRUSTY_MPM_URL", v) },
            None => unsafe { std::env::remove_var("TRUSTY_MPM_URL") },
        }
        assert_eq!(
            result, None,
            "a whitespace-only TRUSTY_MPM_URL must resolve to None, same as unset"
        );
    }

    #[test]
    fn explicit_url_from_env_unset_is_none() {
        let _g = env_lock();
        let prior = std::env::var("TRUSTY_MPM_URL").ok();
        // SAFETY: guarded by env_lock; restored to the prior value below.
        unsafe { std::env::remove_var("TRUSTY_MPM_URL") };
        let result = explicit_url_from_env();
        if let Some(v) = prior {
            unsafe { std::env::set_var("TRUSTY_MPM_URL", v) };
        }
        assert_eq!(result, None, "an unset TRUSTY_MPM_URL must resolve to None");
    }

    /// Why: a console configured on a non-default port is the whole reason this
    /// resolver exists — a fallback that ignored the recorded address would
    /// still pass a default-port-only test.
    /// Test: itself.
    #[test]
    fn console_addr_from_prefers_the_recorded_addr() {
        assert_eq!(
            console_addr_from(Some("127.0.0.1:9911".to_string())),
            "127.0.0.1:9911"
        );
        // Surrounding whitespace in the discovery file must not leak into a URL.
        assert_eq!(
            console_addr_from(Some("  127.0.0.1:9911\n".to_string())),
            "127.0.0.1:9911"
        );
    }

    /// Why: the console is often not running, and no caller may fail or block
    /// because of that — every absent/blank case degrades to the default.
    /// Test: itself.
    #[test]
    fn console_addr_from_falls_back_when_absent_or_blank() {
        assert_eq!(console_addr_from(None), DEFAULT_CONSOLE_ADDR);
        assert_eq!(console_addr_from(Some(String::new())), DEFAULT_CONSOLE_ADDR);
        assert_eq!(
            console_addr_from(Some("   \n".to_string())),
            DEFAULT_CONSOLE_ADDR
        );
    }

    /// Why: the statusline link target is this string, so the `http://` prefix
    /// and the selected port must both be pinned.
    /// Test: itself.
    #[test]
    fn console_base_url_wraps_the_resolved_addr() {
        let url = console_base_url();
        assert!(
            url.starts_with("http://"),
            "console base URL must be an http:// URL: {url}"
        );
        // With no discovery file this is the default; with one it is whatever
        // the console recorded. Either way the port must survive into the URL.
        assert_eq!(url, format!("http://{}", console_addr()));
    }

    use crate::secret_source::test_env::EnvVarGuard;
    use trusty_common::DATA_DIR_OVERRIDE_ENV;
    use trusty_common::credentials::SANDBOX_ENV_VAR;

    // ── #6288: the socket locator and the explicit-URL rule ─────────────────

    /// Why (#6288): every client resolves the socket, never an HTTP URL —
    /// `TRUSTY_MPM_SOCKET` when set, else the data-dir socket — and never
    /// consults the lock file's `http_addr`.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn resolved_url_is_the_socket_locator() {
        let home = crate::test_support::hermetic_temp_dir();
        let _home = EnvVarGuard::set("HOME", home.path());
        let _sandbox = EnvVarGuard::unset(SANDBOX_ENV_VAR);
        let _data = EnvVarGuard::unset(DATA_DIR_OVERRIDE_ENV);
        // A live lock naming the HTTP default must not be what resolves.
        crate::core::daemon_identity::write_lock_at(&lock_file_path(), DEFAULT_DAEMON_URL, "");
        let socket = home.path().join("App Support").join("trusty-mpm.sock");
        let _socket = EnvVarGuard::set(trusty_common::mpm_rpc::TRUSTY_MPM_SOCKET_ENV, &socket);

        let url = resolve_daemon_url(None);
        assert_eq!(url, socket_locator(&socket));
        assert_eq!(socket_from_locator(&url).as_deref(), Some(socket.as_path()));
        assert!(!url.contains("7880"), "{url}");

        let _unpinned = EnvVarGuard::unset(trusty_common::mpm_rpc::TRUSTY_MPM_SOCKET_ENV);
        let default = try_resolve_daemon_url(None).expect("data-dir socket");
        assert!(default.starts_with(SOCKET_LOCATOR_PREFIX), "{default}");
        assert!(default.ends_with("trusty-mpm.sock"), "{default}");
    }

    /// Why (#6288 Q3): a loopback URL names the daemon the socket reaches and
    /// is ignored with a notice; any other host — or a URL that does not
    /// parse — is refused rather than retargeted to the local daemon.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn an_explicit_url_is_refused_unless_it_is_loopback() {
        assert_eq!(explicit_url_notice(None), Ok(None));
        assert_eq!(explicit_url_notice(Some("  ")), Ok(None));
        for url in [
            "http://127.0.0.1:7880",
            "http://localhost:7880",
            "http://[::1]:7880",
            "http://127.0.0.1:1",
        ] {
            let notice = explicit_url_notice(Some(url)).expect(url).expect(url);
            assert!(
                notice.contains("ignoring") && notice.contains("#6288"),
                "{notice}"
            );
        }
        for url in [
            "http://100.64.0.1:7880",
            "http://mac.tailnet.ts.net:7880",
            "not a url",
        ] {
            let err = explicit_url_notice(Some(url)).expect_err(url);
            assert_eq!(err, DaemonUrlError::RemoteUrlRefused { url: url.into() });
            assert!(err.to_string().contains("unix socket"), "{err}");
        }
        // The fallible resolver refuses a remote URL before resolving a socket.
        let home = crate::test_support::hermetic_temp_dir();
        let _home = EnvVarGuard::set("HOME", home.path());
        let _sandbox = EnvVarGuard::unset(SANDBOX_ENV_VAR);
        assert!(matches!(
            try_resolve_daemon_url(Some("http://100.64.0.1:7880")),
            Err(DaemonUrlError::RemoteUrlRefused { .. })
        ));
        assert_eq!(
            resolve_daemon_url_for_dispatch(Some("http://100.64.0.1:7880"), true),
            Ok(REFUSED_DAEMON_URL.to_string()),
            "a hook runs with no daemon rather than failing"
        );
        assert!(resolve_daemon_url_for_dispatch(Some("http://100.64.0.1:7880"), false).is_err());
        let local = try_resolve_daemon_url(Some("http://127.0.0.1:7880")).expect("ignored");
        assert!(local.starts_with(SOCKET_LOCATOR_PREFIX), "{local}");
    }

    /// Why (#9556): `TRUSTY_SANDBOX=1` alone shares the host's data dir, whose
    /// socket is the live daemon's; the resolver refuses it. A sandbox that
    /// names its own socket, or its own data dir, resolves normally.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn resolve_refuses_the_host_socket_under_a_bare_sandbox_flag() {
        let home = crate::test_support::hermetic_temp_dir();
        let _home = EnvVarGuard::set("HOME", home.path());
        let _socket = EnvVarGuard::unset(trusty_common::mpm_rpc::TRUSTY_MPM_SOCKET_ENV);
        let _data = EnvVarGuard::unset(DATA_DIR_OVERRIDE_ENV);
        let _sandbox = EnvVarGuard::set(SANDBOX_ENV_VAR, "1");
        assert_eq!(
            try_resolve_daemon_url(None),
            Err(DaemonUrlError::NoSandboxDaemon)
        );
        assert_eq!(resolve_daemon_url(None), ISOLATED_NO_DAEMON_URL);

        let data = crate::test_support::hermetic_temp_dir();
        let _own = EnvVarGuard::set(DATA_DIR_OVERRIDE_ENV, data.path());
        let url = try_resolve_daemon_url(None).expect("own data dir");
        let path = socket_from_locator(&url).expect("a locator");
        assert!(path.starts_with(data.path()), "{url}");
    }

    /// Why (#6288): a fail-open caller handed a refusal runs with a sentinel
    /// that maps back to the refusal and that no transport sends to.
    /// Test: itself.
    #[tokio::test]
    async fn a_refused_resolution_is_a_sentinel_no_transport_sends_to() {
        let refused = refusal_url(&DaemonUrlError::RemoteUrlRefused {
            url: "http://100.64.0.1:7880".into(),
        });
        assert_eq!(refused, REFUSED_DAEMON_URL);
        assert_eq!(
            refuse_isolated_no_daemon(&refused),
            Err(DaemonUrlError::Refused)
        );
        let err = crate::client::DaemonClient::new(refused.clone())
            .get("/health")
            .send()
            .await
            .expect_err("a refused address is never sent to");
        assert!(!err.is_connect(), "refused before any dial: {err}");
        assert!(err.to_string().contains(REFUSED_DAEMON_URL), "{err}");
    }

    /// Why (#9556 delta critic): the banner and statusline read the lock
    /// through [`client_lock_url`]; with `TRUSTY_DATA_DIR_OVERRIDE` set and a
    /// host lock in `HOME`, they showed the live daemon online. The temp
    /// `HOME` stands in for the inherited home; its lock names the default and
    /// a live pid (this process).
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn client_lock_url_ignores_a_host_lock_under_isolation() {
        let home = crate::test_support::hermetic_temp_dir();
        let data = crate::test_support::hermetic_temp_dir();
        let _home = EnvVarGuard::set("HOME", home.path());
        let _sandbox = EnvVarGuard::unset(SANDBOX_ENV_VAR);
        let _data = EnvVarGuard::set(DATA_DIR_OVERRIDE_ENV, data.path());
        crate::core::daemon_identity::write_lock_at(&lock_file_path(), DEFAULT_DAEMON_URL, "");

        assert_eq!(client_lock_url(), None, "a host lock is not this sandbox's");
    }

    /// Why (#9556 delta critic): outside isolation the filtered reader is
    /// exactly `read_lock`'s address, the default included.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn client_lock_url_is_read_lock_outside_isolation() {
        let home = crate::test_support::hermetic_temp_dir();
        let _home = EnvVarGuard::set("HOME", home.path());
        let _sandbox = EnvVarGuard::unset(SANDBOX_ENV_VAR);
        let _data = EnvVarGuard::unset(DATA_DIR_OVERRIDE_ENV);
        crate::core::daemon_identity::write_lock_at(&lock_file_path(), DEFAULT_DAEMON_URL, "");

        assert_eq!(client_lock_url().as_deref(), Some(DEFAULT_DAEMON_URL));
        assert_eq!(
            client_lock_url(),
            crate::core::daemon_identity::read_lock().map(|lock| lock.addr)
        );
    }

    /// Why (#9556 critic): the host-lock rule must reject both the default
    /// address and a lock under the account home, and nothing else.
    /// Test: itself.
    #[test]
    fn host_lock_is_judged_by_address_and_home() {
        let sandbox = crate::test_support::hermetic_temp_dir();
        let account = crate::test_support::hermetic_temp_dir();
        let (sandbox, account) = (Some(sandbox.path()), Some(account.path()));
        let other = "http://127.0.0.1:17881";
        assert!(!lock_is_hosts(other, sandbox, account));
        assert!(lock_is_hosts(other, account, account));
        assert!(lock_is_hosts(DEFAULT_DAEMON_URL, sandbox, account));
        assert!(lock_is_hosts("http://127.0.0.1:7880/", sandbox, account));
        assert!(lock_is_hosts(other, None, account));
        assert!(lock_is_hosts(other, sandbox, None));
    }

    /// Why (#9556 critic): with isolation on and no recorded console, the
    /// banner's console probe target is "none", never the host's 7788.
    /// Test: itself.
    #[test]
    fn console_probe_target_is_none_under_isolation() {
        assert_eq!(console_addr_to_probe_from(None, true), None);
        assert_eq!(
            console_addr_to_probe_from(None, false).as_deref(),
            Some(DEFAULT_CONSOLE_ADDR)
        );
        assert_eq!(
            console_addr_to_probe_from(Some(" 127.0.0.1:9911\n".into()), true).as_deref(),
            Some("127.0.0.1:9911")
        );
    }

    /// Why (#9556): `tm start` and the bare-`tm` autostart must fail closed in
    /// a sandbox rather than spawn a daemon on the host default, and still
    /// spawn outside one.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn daemon_spawn_refused_only_under_isolation() {
        let data = crate::test_support::hermetic_temp_dir();
        {
            let _sandbox = EnvVarGuard::set(SANDBOX_ENV_VAR, "1");
            let _data = EnvVarGuard::unset(DATA_DIR_OVERRIDE_ENV);
            assert_eq!(
                refuse_daemon_spawn_when_isolated(),
                Err(DaemonUrlError::NoSandboxDaemon)
            );
        }
        {
            let _sandbox = EnvVarGuard::unset(SANDBOX_ENV_VAR);
            let _data = EnvVarGuard::set(DATA_DIR_OVERRIDE_ENV, data.path());
            assert_eq!(
                refuse_daemon_spawn_when_isolated(),
                Err(DaemonUrlError::NoSandboxDaemon)
            );
        }
        let _sandbox = EnvVarGuard::unset(SANDBOX_ENV_VAR);
        let _data = EnvVarGuard::unset(DATA_DIR_OVERRIDE_ENV);
        assert_eq!(refuse_daemon_spawn_when_isolated(), Ok(()));
    }

    /// Why (#9556): bare `tm` turns the sentinel back into the refusal instead
    /// of reading a refused request as a slow daemon.
    /// Test: itself.
    #[test]
    fn isolated_no_daemon_url_maps_back_to_the_refusal() {
        assert_eq!(
            refuse_isolated_no_daemon(ISOLATED_NO_DAEMON_URL),
            Err(DaemonUrlError::NoSandboxDaemon)
        );
        assert_eq!(
            refuse_isolated_no_daemon(REFUSED_DAEMON_URL),
            Err(DaemonUrlError::Refused)
        );
        assert_eq!(refuse_isolated_no_daemon("unix:/tmp/x.sock"), Ok(()));
    }

    /// Why (#9556): a fail-open caller handed the sentinel reaches nothing.
    /// Test: itself.
    #[tokio::test]
    async fn isolated_no_daemon_url_is_refused_before_any_io() {
        let err = reqwest::Client::new()
            .get(format!("{ISOLATED_NO_DAEMON_URL}/health"))
            .send()
            .await
            .expect_err("a non-HTTP scheme is never sent");
        assert!(
            err.is_builder(),
            "refused while building, not on the wire: {err}"
        );
        assert!(err.to_string().contains(ISOLATED_NO_DAEMON_URL), "{err}");
    }
}
