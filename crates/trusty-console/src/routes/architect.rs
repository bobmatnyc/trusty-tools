//! The Architect dashboard link (#9474).
//!
//! Why: the owner asked the console to link to the local Architect dashboard.
//! That dashboard is a separate server that may or may not run on a given host,
//! so the console must discover it and show the link only while it is live — a
//! host with no Architect gets no link, never a dead one.
//! What: `GET /api/console/architect-dashboard` answers `{"url": "<url>"}` when
//! the dashboard recorded its address in the trusty `http_addr` discovery file
//! (`resolve_data_dir("trusty-architect")/http_addr`, the convention every
//! trusty daemon already uses) AND that address is a loopback host that accepts
//! a TCP connection within [`PROBE_TIMEOUT`]. Every other case answers
//! `{"url": null}`. The answer is always 200; the header link is optional.
//! Test: the `architect_link_*` route tests in `server/tests.rs`, and the
//! allowlist tests in `architect_tests.rs`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use axum::Json;
use reqwest::Url;
use serde_json::{Value, json};

/// The data-dir app name the Architect dashboard server records its address
/// under: it writes `host:port` to `resolve_data_dir("trusty-architect")/http_addr`.
pub(crate) const ARCHITECT_DASHBOARD_APP: &str = "trusty-architect";

/// The longest the liveness probe may take, across every candidate address.
///
/// A loopback connect answers in microseconds, so this only ever runs out
/// against a wedged listener. 300 ms matches the connector `tcp_probe`.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

const V4: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

/// `GET /api/console/architect-dashboard` — the dashboard URL, or `null`.
///
/// Why: the header renders the link from this answer, so the show/hide rule
/// lives in one tested place on the server rather than in the browser.
/// What: reads the discovery file off the async worker, then hands it to
/// [`live_dashboard_url`]. Always 200 with a `url` field.
/// Test: `architect_link_present_when_dashboard_discovered_and_live`,
/// `architect_link_absent_without_discovery_file`,
/// `architect_link_absent_when_dashboard_not_live`,
/// `architect_link_refuses_non_localhost_url`.
pub async fn dashboard_link_handler() -> Json<Value> {
    // #9474: `resolve_data_dir` touches the filesystem, so it runs on the
    // blocking pool rather than on the worker serving other requests.
    let recorded = tokio::task::spawn_blocking(|| {
        trusty_common::resolve_daemon_base_url(ARCHITECT_DASHBOARD_APP)
    })
    .await
    .inspect_err(|e| tracing::error!(error = %e, "architect dashboard discovery task failed"))
    .ok()
    .flatten();
    Json(json!({ "url": live_dashboard_url(recorded.as_deref()).await }))
}

/// The dashboard URL when `recorded` names a loopback host that is listening.
///
/// Why: a discovery file outlives a crashed server, so presence alone is not
/// liveness; and the URL becomes an `href`, so only loopback `http` is trusted.
/// What: `None` for no recorded address, a refused address (see
/// [`dashboard_target`]), or no listener answering within [`PROBE_TIMEOUT`];
/// otherwise the normalised URL (`http://127.0.0.1:7890/`).
/// Test: the four `architect_link_*` route tests in `server/tests.rs`.
pub(crate) async fn live_dashboard_url(recorded: Option<&str>) -> Option<String> {
    let recorded = recorded?;
    let Some((url, addrs)) = dashboard_target(recorded) else {
        tracing::warn!(
            recorded,
            "architect dashboard address is not a loopback http URL; no link shown"
        );
        return None;
    };
    let probe = async {
        for addr in addrs {
            if tokio::net::TcpStream::connect(addr).await.is_ok() {
                return true;
            }
        }
        false
    };
    let live = matches!(tokio::time::timeout(PROBE_TIMEOUT, probe).await, Ok(true));
    if !live {
        tracing::debug!(%url, "architect dashboard recorded but not listening");
    }
    live.then(|| url.to_string())
}

/// Parse a recorded address and resolve it to loopback socket addresses.
///
/// Why: #9474 rule — the console only ever links to, or connects to, this
/// host's loopback interface. Name resolution is not consulted: `localhost`
/// maps to both loopback addresses directly, so a hosts-file entry cannot
/// redirect the probe.
/// What: accepts `http` URLs whose host is exactly `127.0.0.1`, `[::1]` or
/// `localhost` (case-insensitive), with no userinfo; a bare `host:port` is
/// read as `http://host:port`. Returns the normalised URL and the addresses to
/// probe, or `None` for anything else.
/// Test: `dashboard_target_accepts_only_loopback_http`.
pub(crate) fn dashboard_target(recorded: &str) -> Option<(Url, Vec<SocketAddr>)> {
    let recorded = recorded.trim();
    let url = if recorded.contains("://") {
        Url::parse(recorded)
    } else {
        Url::parse(&format!("http://{recorded}"))
    }
    .ok()?;
    if url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let ips: &[IpAddr] = match url.host_str()? {
        "127.0.0.1" => &[V4],
        "[::1]" => &[V6],
        "localhost" => &[V4, V6],
        _ => return None,
    };
    let port = url.port_or_known_default()?;
    let addrs = ips.iter().map(|ip| SocketAddr::new(*ip, port)).collect();
    Some((url, addrs))
}

#[cfg(test)]
#[path = "architect_tests.rs"]
mod tests;
