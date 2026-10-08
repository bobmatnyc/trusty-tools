//! The HTTPS download behind the pinned uv fetch (#9468), split out of
//! `uv_fetch.rs` to keep both files under the SLOC cap.

use std::io::Read as _;
use std::time::{Duration, Instant};

use super::UvError;

/// Upper bound on the downloaded tarball (the real ones are ~17-20 MB).
const MAX_TARBALL_BYTES: u64 = 64 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Wall-clock budget for one download, from request start to the last body byte.
const DOWNLOAD_BUDGET: Duration = Duration::from_secs(300);

/// Download `url` over HTTPS into memory, honouring the operator's proxy.
///
/// Why: `reqwest::blocking` refuses to run on a thread that is inside a tokio
/// runtime, and `build_venv` is reached from `spawn_blocking` and
/// `block_in_place`. A dedicated thread has no runtime context.
/// What: [`http_fetch_within`] with the production [`DOWNLOAD_BUDGET`].
/// Test: `real_pinned_asset_matches_the_embedded_digest` (`#[ignore]`, network).
pub(crate) fn http_fetch(url: &str) -> Result<Vec<u8>, UvError> {
    http_fetch_within(url, DOWNLOAD_BUDGET)
}

/// Download `url` into memory on a dedicated thread, within `budget`.
///
/// What: three bounds. Connecting takes at most [`CONNECT_TIMEOUT`]. The whole
/// request, from connect through the last body byte, takes at most `budget`
/// of wall-clock time, however the server paces the bytes. The body held in
/// memory is at most [`MAX_TARBALL_BYTES`]. A non-success status, or any
/// failure, is [`UvError::Network`]; one past `budget` names the timeout.
/// Test: `a_download_dripping_past_the_total_budget_times_out_and_places_nothing`.
pub(crate) fn http_fetch_within(url: &str, budget: Duration) -> Result<Vec<u8>, UvError> {
    let owned = url.to_owned();
    std::thread::Builder::new()
        .name("uv-fetch".to_owned())
        .spawn(move || http_fetch_on_this_thread(&owned, budget))
        .map_err(|e| network(url, format!("spawn download thread: {e}")))?
        .join()
        .map_err(|_| network(url, "download thread panicked".to_owned()))?
}

fn http_fetch_on_this_thread(url: &str, budget: Duration) -> Result<Vec<u8>, UvError> {
    let start = Instant::now();
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("trusty-embedderd-py/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| network(url, error_chain(&e)))?;
    // #9468: a client-level timeout bounds each blocking read only, so a server
    // dripping bytes kept the download alive forever. A per-request timeout is
    // a deadline on the whole request, body included.
    let resp = client
        .get(url)
        .timeout(budget)
        .send()
        .map_err(|e| failure(url, start, budget, error_chain(&e)))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(network(url, format!("HTTP {status}")));
    }
    let mut body = Vec::new();
    resp.take(MAX_TARBALL_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|e| failure(url, start, budget, format!("reading body: {e}")))?;
    if body.len() as u64 > MAX_TARBALL_BYTES {
        return Err(network(
            url,
            format!("body exceeds {MAX_TARBALL_BYTES} bytes"),
        ));
    }
    Ok(body)
}

/// A [`UvError::Network`] for `reason`, naming the budget when it ran out.
fn failure(url: &str, start: Instant, budget: Duration, reason: String) -> UvError {
    if start.elapsed() >= budget {
        network(
            url,
            format!("timed out: the download exceeded its {budget:?} total budget ({reason})"),
        )
    } else {
        network(url, reason)
    }
}

pub(super) fn network(url: &str, reason: String) -> UvError {
    UvError::Network {
        url: url.to_owned(),
        reason,
    }
}

/// `e` and its source chain, joined by `: ` (reqwest's own Display is terse).
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(src) = cur {
        out.push_str(": ");
        out.push_str(&src.to_string());
        cur = src.source();
    }
    out
}
