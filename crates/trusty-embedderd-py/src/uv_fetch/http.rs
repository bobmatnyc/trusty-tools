//! The HTTPS download behind the pinned uv fetch (#9966, PR B2 of #9468).
//!
//! Why: split out of `uv_fetch.rs` so the network code has one home and both
//! files stay under the SLOC cap.
//! What: [`http_fetch`] downloads one URL into memory under three bounds —
//! HTTPS only, a 300 s total budget, and [`super::MAX_ARCHIVE_BYTES`].
//! Test: `a_download_trickling_past_the_budget_times_out_and_places_nothing`,
//! `an_oversized_download_is_refused_before_its_body_is_buffered`,
//! `plain_http_is_refused_in_production`.

use std::io::Read as _;
use std::time::{Duration, Instant};

use super::UvError;

/// Wall-clock budget for one download, from connect to the last body byte.
pub(crate) const DOWNLOAD_BUDGET: Duration = Duration::from_secs(300);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// The bounds one download runs under; production uses [`PRODUCTION`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub budget: Duration,
    pub max_bytes: u64,
    pub https_only: bool,
}

/// HTTPS only, [`DOWNLOAD_BUDGET`], [`super::MAX_ARCHIVE_BYTES`].
pub(crate) const PRODUCTION: Limits = Limits {
    budget: DOWNLOAD_BUDGET,
    max_bytes: super::MAX_ARCHIVE_BYTES,
    https_only: true,
};

/// Download `url` over HTTPS into memory, honouring the operator's proxy.
///
/// Test: `real_pinned_asset_matches_the_embedded_digest` (`#[ignore]`, network).
pub(crate) fn http_fetch(url: &str) -> Result<Vec<u8>, UvError> {
    http_fetch_with(url, PRODUCTION)
}

/// Download `url` into memory on a dedicated thread, under `limits`.
///
/// Why: `reqwest::blocking` panics on a thread inside a tokio runtime, and the
/// bootstrap is reached from `block_in_place` (`trusty-search doctor`). A
/// fresh thread has no runtime context.
/// What: connect takes at most 30 s; the whole request, body included, takes at
/// most `limits.budget` however the server paces its bytes (#9966: reqwest's
/// per-request timeout is a total deadline). A declared `Content-Length` over
/// `limits.max_bytes` is refused before the body is read, and an undeclared
/// body is read at most one byte past the bound. Every failure is `Network`.
/// Test: `a_download_trickling_past_the_budget_times_out_and_places_nothing`,
/// `an_oversized_download_is_refused_before_its_body_is_buffered`.
pub(crate) fn http_fetch_with(url: &str, limits: Limits) -> Result<Vec<u8>, UvError> {
    let owned = url.to_owned();
    std::thread::Builder::new()
        .name("uv-fetch".to_owned())
        .spawn(move || fetch_on_this_thread(&owned, limits))
        .map_err(|e| network(url, format!("spawn download thread: {e}")))?
        .join()
        .map_err(|_| network(url, "download thread panicked".to_owned()))?
}

fn fetch_on_this_thread(url: &str, limits: Limits) -> Result<Vec<u8>, UvError> {
    let start = Instant::now();
    let failed = |reason: String| failure(url, start, limits.budget, reason);
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .https_only(limits.https_only)
        .user_agent(concat!("trusty-embedderd-py/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| network(url, error_chain(&e)))?;
    // #9966: a client-level timeout bounds each read only, so a trickle never
    // trips it; the per-request timeout is a deadline on the whole body.
    let resp = client
        .get(url)
        .timeout(limits.budget)
        .send()
        .map_err(|e| failed(error_chain(&e)))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(network(url, format!("HTTP {status}")));
    }
    let over = |n: u64| {
        format!(
            "the body is {n} bytes, over the {} byte bound",
            limits.max_bytes
        )
    };
    // #9966: refuse a declared oversize body before reading any of it.
    if let Some(declared) = resp.content_length().filter(|n| *n > limits.max_bytes) {
        return Err(network(url, over(declared)));
    }
    let mut body = Vec::new();
    resp.take(limits.max_bytes.saturating_add(1))
        .read_to_end(&mut body)
        .map_err(|e| failed(format!("reading the body: {}", error_chain(&e))))?;
    let read = u64::try_from(body.len()).unwrap_or(u64::MAX);
    if read > limits.max_bytes {
        return Err(network(url, over(read)));
    }
    Ok(body)
}

/// A `Network` error for `reason`, naming the budget once it has run out.
fn failure(url: &str, start: Instant, budget: Duration, reason: String) -> UvError {
    if start.elapsed() >= budget {
        let reason =
            format!("timed out: the download exceeded its {budget:?} total budget ({reason})");
        network(url, reason)
    } else {
        network(url, reason)
    }
}

fn network(url: &str, reason: String) -> UvError {
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
