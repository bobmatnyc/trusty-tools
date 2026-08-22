//! Getting the two daemons the code analysis stands on to answer (#6081).
//!
//! Why: the chain is trusty-search → trusty-analyze → the report's analyze
//! sections, and it is a CHAIN — `trusty-analyze serve` exits immediately when
//! trusty-search is unreachable, and an analyze daemon that is already running
//! answers `503 degraded` for as long as it stays unreachable. So the search
//! daemon is ensured first and its failure is stated as itself rather than as
//! the analyze symptom it will later produce: an operator reading
//! "trusty-analyze is degraded" reaches for the wrong daemon.
//!
//! What: [`ensure_search`] and [`ensure_analyze`], plus the pure rules they
//! apply — the addresses, the ports, and the two argument vectors — so each can
//! be asserted without spawning anything.
//!
//! ## Nothing here is implemented twice
//!
//! Probing, detaching a child, and polling to readiness are
//! [`trusty_common::daemon_guard`]'s, which is where they were promoted for
//! exactly this kind of caller (#5670, #985). This module contributes the two
//! addresses, the two argument vectors, and the failure text — nothing else.
//!
//! Test: `daemon_tests`, and `super::grounding_tests` for the live arms.

use std::time::Duration;

use std::time::Instant;

use trusty_common::daemon_guard::{DaemonAddrLayout, probe_once, spawn_detached};

use super::Tools;

/// Environment variable naming the `trusty-search` binary.
///
/// The variable `tga audit` already reads (`tga::audit::repo_index`), and the one
/// `crate::run` exports onto every sweep child — so an engagement that pinned its
/// tools pins this leg the same way, with no second name to set.
pub const ENV_SEARCH_BIN: &str = "TRUSTY_SEARCH_BIN";

/// Environment variable naming the `trusty-analyze` binary.
pub const ENV_ANALYZE_BIN: &str = "TRUSTY_ANALYZE_BIN";

/// Environment variable overriding the trusty-analyze daemon's address.
///
/// Why a literal rather than a shared constant: this crate has no Cargo edge to
/// `tga`, which owns the same three names (`tga::audit::ENV_ANALYZE_URL`), and
/// linking a workspace-version `tga` into a client whose entire discipline is
/// running PINNED binaries would defeat the pinning. It is a copied
/// cross-process contract — unlike the index id, which since #6149 is one
/// shared function in `trusty-common` rather than three copies — and it is
/// pinned by `daemon_tests::the_analyze_defaults_match_the_contract_tga_uses`.
pub const ENV_ANALYZE_URL: &str = "TRUSTY_ANALYZE_URL";

/// The trusty-analyze daemon's address when nothing overrides it.
const DEFAULT_ANALYZE_URL: &str = "http://127.0.0.1:7879";

/// The port `trusty-analyze serve` binds when the address names none.
const DEFAULT_ANALYZE_PORT: u16 = 7879;

/// Wall-clock budget for a freshly-spawned daemon to answer `/health`.
///
/// Why 60s rather than `daemon_guard::DEFAULT_STARTUP_TIMEOUT`'s 30s: the HTTP
/// port binds in about a second, but a first run on a recipient's machine with
/// no model cache spends 15–30s in ONNX load before trusty-search answers, and
/// giving up at 30s would turn a slow cold start into an unassessed engagement.
/// Matches `trusty-search`'s own guard and `tga::audit::SEARCH_STARTUP_TIMEOUT`.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a freshly-spawned daemon has to BIND its port before it is given up
/// on. See [`wait_ready`] for why this is separate from [`STARTUP_TIMEOUT`].
pub const BIND_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a single TCP connection attempt may take.
///
/// Loopback, so a reachable port answers in microseconds and an unreachable one
/// is refused immediately. This bounds the third case — a host that silently
/// drops packets — rather than the two that matter.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);

/// The trusty-search daemon's address, as every client of it resolves one.
///
/// Why: trusty-search binds an OS-assigned port and records it in its own
/// discovery files, so a hard-coded `127.0.0.1:7878` would miss an auto-ported
/// daemon and every `TRUSTY_DATA_DIR`-isolated one. [`DaemonAddrLayout`] is that
/// resolution, promoted into `trusty-common` for sibling callers (#5670).
#[must_use]
pub fn search_base_url() -> String {
    DaemonAddrLayout::TRUSTY_SEARCH.resolve_base_url()
}

/// The trusty-analyze daemon's address: [`ENV_ANALYZE_URL`], else the default.
#[must_use]
pub fn analyze_base_url() -> String {
    url_from_override(std::env::var(ENV_ANALYZE_URL).ok().as_deref())
}

/// The override rule itself: a non-empty value wins, everything else defaults.
///
/// Split out so the rule is asserted without any test reading or writing the
/// process environment — `set_var` is `unsafe` in edition 2024 and unsound under
/// the parallel harness.
fn url_from_override(value: Option<&str>) -> String {
    value
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_ANALYZE_URL)
        .to_string()
}

/// The health endpoint for a base address, tolerating a trailing slash.
fn health_url(base: &str) -> String {
    format!("{}/health", base.trim_end_matches('/'))
}

/// The port to bind, read out of the daemon's address.
///
/// Why: `trusty-analyze serve` takes `--port`, not a URL, so an operator who
/// moved the daemon with [`ENV_ANALYZE_URL`] must get a daemon on THAT port —
/// spawning on 7879 and then probing the override would wait out the whole
/// budget and refuse a correctly-configured run.
fn port_of(url: &str) -> u16 {
    url.trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .and_then(|authority| authority.rsplit(':').next())
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or(DEFAULT_ANALYZE_PORT)
}

/// The argument vector `trusty-search` is started with.
///
/// `--foreground` is load-bearing rather than decorative: a bare
/// `trusty-search start` re-spawns itself as a background daemon and the parent
/// exits, so the PID [`spawn_detached`] returns would describe a process that is
/// already gone. We detach the child ourselves, so the flag is what keeps the
/// spawned process and the daemon the same one.
fn search_start_args() -> [&'static str; 2] {
    ["start", "--foreground"]
}

/// The argument vector `trusty-analyze` is started with.
fn analyze_serve_args(port: u16) -> [String; 3] {
    ["serve".to_string(), "--port".to_string(), port.to_string()]
}

/// Ensure the trusty-search daemon answers, starting it if it does not.
///
/// # Errors
///
/// One line, safe to show the recipient, naming the address and the binary, when
/// the binary will not spawn or the daemon never becomes ready. The caller turns
/// it into a gap; nothing here fails a run.
///
/// Test: `super::grounding_tests::{a_search_daemon_that_will_not_start_is_a_named_gap,
/// a_reachable_search_daemon_is_not_restarted}`.
pub async fn ensure_search(tools: &Tools) -> Result<(), String> {
    let health = health_url(&tools.search_url);
    if probe_once(&health).await {
        return Ok(());
    }
    let binary = tools.search.display().to_string();
    spawn_detached(&tools.search, &search_start_args())
        .map_err(|e| refusal("trusty-search", &tools.search_url, &binary, &e.to_string()))?;
    wait_ready(&health, &tools.search_url, tools)
        .await
        .map_err(|cause| refusal("trusty-search", &tools.search_url, &binary, &cause))
}

/// Ensure the trusty-analyze daemon answers, starting it if it does not.
///
/// # Errors
///
/// The same shape [`ensure_search`] returns. A daemon that is up but degraded
/// answers `/health` non-2xx, so it is reported here rather than surfacing later
/// as an empty hotspot list.
///
/// Test: `super::grounding_tests::an_analyze_daemon_that_will_not_start_is_a_named_gap`.
pub async fn ensure_analyze(tools: &Tools) -> Result<(), String> {
    let health = health_url(&tools.analyze_url);
    if probe_once(&health).await {
        return Ok(());
    }
    let binary = tools.analyze.display().to_string();
    let args = analyze_serve_args(port_of(&tools.analyze_url));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    spawn_detached(&tools.analyze, &args).map_err(|e| {
        refusal(
            "trusty-analyze",
            &tools.analyze_url,
            &binary,
            &e.to_string(),
        )
    })?;
    wait_ready(&health, &tools.analyze_url, tools)
        .await
        .map_err(|cause| refusal("trusty-analyze", &tools.analyze_url, &binary, &cause))
}

/// Wait for a daemon this call just spawned, in two phases.
///
/// Why not `trusty_common::daemon_guard::spin_until_ready`: it applies ONE
/// budget to both of the things that can be slow, and they are slow for
/// unrelated reasons. Binding the port takes about a second whatever the
/// machine; answering `/health` afterwards can take 15–30s on a first run that
/// has to load ONNX weights. Under one budget a binary that dies immediately —
/// which is exactly what `trusty-analyze serve` does when trusty-search is
/// unreachable — costs the FULL startup budget before anything is reported. In
/// an unattended engagement that is 60 seconds per repository spent waiting for
/// a process that is already gone.
///
/// What: the port must accept a TCP connection within
/// [`Tools::bind_timeout`], and only then does `/health` get the full
/// [`Tools::startup_timeout`]. `spin_until_ready`'s spinner is dropped with it,
/// which this crate wants anyway — it writes to the same stderr the sweep's
/// progress relay is reading.
/// Test: `super::grounding_tests::{a_search_daemon_that_will_not_start_is_a_named_gap,
/// a_daemon_that_binds_but_never_answers_is_a_named_gap}`.
async fn wait_ready(health: &str, url: &str, tools: &Tools) -> Result<(), String> {
    let bound_by = Instant::now() + tools.bind_timeout;
    loop {
        if probe_once(health).await {
            return Ok(());
        }
        if accepting(url).await {
            break;
        }
        if Instant::now() >= bound_by {
            return Err(format!(
                "nothing was listening there {}s after it was started",
                tools.bind_timeout.as_secs_f32()
            ));
        }
        tokio::time::sleep(tools.poll_interval).await;
    }
    let ready_by = Instant::now() + tools.startup_timeout;
    loop {
        tokio::time::sleep(tools.poll_interval).await;
        if probe_once(health).await {
            return Ok(());
        }
        if Instant::now() >= ready_by {
            return Err(format!(
                "it is listening but did not answer /health within {}s",
                tools.startup_timeout.as_secs_f32()
            ));
        }
    }
}

/// Whether anything accepts a TCP connection at this address.
///
/// Deliberately weaker than a health probe: it separates "the process is not
/// there" from "the process is there and still warming up", which is the
/// distinction [`wait_ready`]'s two budgets rest on.
async fn accepting(url: &str) -> bool {
    let Some(authority) = authority_of(url) else {
        return false;
    };
    matches!(
        tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio::net::TcpStream::connect(authority.as_str()),
        )
        .await,
        Ok(Ok(_))
    )
}

/// The `host:port` of a base URL, or `None` when it names no host.
fn authority_of(url: &str) -> Option<String> {
    let rest = url
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let authority = rest.split('/').next().unwrap_or_default();
    (!authority.is_empty() && authority.contains(':')).then(|| authority.to_owned())
}

/// One unavailable daemon, rendered as one line the recipient can act on.
///
/// Why the cause is quoted rather than replaced: the remedies differ — a binary
/// that is not installed needs an install, a daemon that will not stay up needs
/// its own log — and only the spawn or the readiness poll knows which fired.
fn refusal(service: &str, url: &str, binary: &str, cause: &str) -> String {
    format!("{service} is not reachable at {url} and `{binary}` could not start it ({cause})")
}

#[cfg(test)]
mod daemon_tests {
    use super::*;

    /// The three values this crate copies rather than imports, pinned against
    /// the contract `tga::audit::analyze` spells on the other side of the wire.
    #[test]
    fn the_analyze_defaults_match_the_contract_tga_uses() {
        assert_eq!(ENV_ANALYZE_URL, "TRUSTY_ANALYZE_URL");
        assert_eq!(DEFAULT_ANALYZE_URL, "http://127.0.0.1:7879");
        assert_eq!(DEFAULT_ANALYZE_PORT, 7879);
    }

    #[test]
    fn an_absent_or_empty_override_falls_back_to_the_default_address() {
        assert_eq!(url_from_override(None), DEFAULT_ANALYZE_URL);
        assert_eq!(url_from_override(Some("")), DEFAULT_ANALYZE_URL);
        assert_eq!(
            url_from_override(Some("http://127.0.0.1:9999")),
            "http://127.0.0.1:9999"
        );
    }

    #[test]
    fn the_spawn_port_comes_from_the_configured_url() {
        assert_eq!(port_of("http://127.0.0.1:9999"), 9999);
        assert_eq!(port_of("http://127.0.0.1:9999/"), 9999);
        assert_eq!(port_of("http://localhost"), DEFAULT_ANALYZE_PORT);
        assert_eq!(port_of("not a url"), DEFAULT_ANALYZE_PORT);
    }

    #[test]
    fn health_is_one_slash_whatever_the_base_looks_like() {
        assert_eq!(health_url("http://x:1"), "http://x:1/health");
        assert_eq!(health_url("http://x:1/"), "http://x:1/health");
    }

    /// `--foreground` is the flag that keeps the spawned process and the daemon
    /// the same one; a later edit dropping it would otherwise pass unnoticed.
    #[test]
    fn the_search_start_stays_in_the_foreground() {
        assert_eq!(search_start_args(), ["start", "--foreground"]);
    }

    #[test]
    fn the_analyze_spawn_names_the_port_it_will_be_probed_on() {
        assert_eq!(
            analyze_serve_args(9999),
            [
                "serve".to_string(),
                "--port".to_string(),
                "9999".to_string()
            ]
        );
    }

    #[test]
    fn a_refusal_names_the_service_the_address_the_binary_and_the_cause() {
        let line = refusal(
            "trusty-analyze",
            "http://127.0.0.1:7879",
            "/w/tools/trusty-analyze",
            "no such file",
        );
        assert!(line.contains("trusty-analyze"), "{line}");
        assert!(line.contains("http://127.0.0.1:7879"), "{line}");
        assert!(line.contains("/w/tools/trusty-analyze"), "{line}");
        assert!(line.contains("no such file"), "{line}");
        assert_eq!(line.lines().count(), 1, "must stay one line: {line}");
    }
}
