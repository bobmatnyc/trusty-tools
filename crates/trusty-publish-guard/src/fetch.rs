//! Real crates.io-backed [`PublishedFetcher`] implementation.
//!
//! This is the ONE place in the crate that touches the network. It is
//! deliberately thin (two HTTP calls, no logic beyond status-code mapping) so
//! the untestable-without-a-live-registry part stays small; everything that
//! benefits from unit testing (extraction, diffing, the parity decision)
//! lives in `lib.rs` behind the [`PublishedFetcher`] trait and is exercised
//! there against an in-memory fake instead.
//!
//! Not exercised by `cargo test` — doing so would require live network access
//! to crates.io, which this workspace's test suite must not depend on. This
//! module's correctness is instead covered by the manual verification in the
//! issue #3366 PR description (running `publish-guard` against the real
//! workspace and confirming it reproduces the actual reported drift).

use crate::PublishedFetcher;
use anyhow::{Context, Result, bail};

/// crates.io's published API-usage policy requires a descriptive User-Agent
/// identifying the tool and a contact point; generic/UA-less clients are
/// rejected with 403 (same requirement `scripts/preflight-publish.sh`
/// already documents for its own crates.io calls).
const USER_AGENT: &str =
    "trusty-tools-publish-guard/0.1 (https://github.com/bobmatnyc/trusty-tools; issue #3366)";

pub struct CratesIoFetcher {
    client: reqwest::blocking::Client,
}

impl CratesIoFetcher {
    pub fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .context("building crates.io HTTP client")?;
        Ok(Self { client })
    }
}

impl PublishedFetcher for CratesIoFetcher {
    fn is_version_live(&self, name: &str, version: &str) -> Result<bool> {
        let url = format!("https://crates.io/api/v1/crates/{name}/{version}");
        let resp = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        match resp.status().as_u16() {
            200 => Ok(true),
            404 => Ok(false),
            // Fail closed: an unexpected status (rate limit, 5xx, etc.) means
            // we cannot verify safety, so this must not be silently treated
            // as "not published" — that would let real drift sail through.
            other => bail!("unexpected HTTP {other} from {url}"),
        }
    }

    fn fetch_tarball(&self, name: &str, version: &str) -> Result<Vec<u8>> {
        let url = format!("https://crates.io/api/v1/crates/{name}/{version}/download");
        let resp = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            bail!("unexpected HTTP {} downloading {url}", resp.status());
        }
        Ok(resp
            .bytes()
            .context("reading tarball response body")?
            .to_vec())
    }
}
