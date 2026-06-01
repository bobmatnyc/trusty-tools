//! Crates.io-based update notification helper.
//!
//! Why: User-facing trusty-* CLIs should nudge operators when a newer release
//! is available, so they are not silently running stale binaries. Centralising
//! the check here keeps the throttle, cache, opt-out, and User-Agent logic
//! consistent across every consumer.
//!
//! What: [`check_throttled`] is the main entry point. It:
//!   1. Returns `None` immediately when `TRUSTY_NO_UPDATE_CHECK` or `CI` is set.
//!   2. Returns a cached result when the last network check was < 24 h ago.
//!   3. Performs a non-blocking GET to `https://crates.io/api/v1/crates/{name}`
//!      with a descriptive User-Agent, compares semver, caches the result, and
//!      returns `Some(UpdateInfo)` when a newer stable version exists.
//!
//! All failures (network, parse, 403, missing field) degrade gracefully to
//! `None` — the check is best-effort and must never panic or stall a CLI.
//!
//! Test: `cargo test -p trusty-common --features update-check`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// --- Opt-out env vars ---

/// Set to any non-empty value to disable update checks entirely.
pub const NO_UPDATE_CHECK_ENV: &str = "TRUSTY_NO_UPDATE_CHECK";

/// Standard CI environment variable — update checks are suppressed when set.
const CI_ENV: &str = "CI";

/// How frequently to hit crates.io (24 hours in seconds).
const CHECK_INTERVAL_SECS: u64 = 60 * 60 * 24;

/// Network timeout for each crates.io request.
const NETWORK_TIMEOUT_SECS: u64 = 4;

// --- Public types ---

/// Metadata about an available update.
///
/// Why: A typed struct is easier to format and test than raw strings.
/// What: Holds the crate name, the version currently installed, and the latest
/// stable version seen on crates.io.
/// Test: [`notice`] exercises all three fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    /// The crate name as published on crates.io (e.g. `trusty-search`).
    pub crate_name: String,
    /// The version of the running binary (from `env!("CARGO_PKG_VERSION")`).
    pub current: String,
    /// The latest stable version reported by crates.io.
    pub latest: String,
}

/// Produce a human-readable upgrade notice for `info`.
///
/// Why: A single formatting function keeps the message consistent and
/// testable without coupling consumers to the wording.
/// What: Returns a string like:
/// `"Update available: trusty-search 0.20.0 (you have 0.19.0) — run: cargo install trusty-search --locked"`.
/// Test: `notice_formats_correctly` in the `tests` module.
pub fn notice(info: &UpdateInfo) -> String {
    format!(
        "Update available: {} {} (you have {}) — run: cargo install {} --locked",
        info.crate_name, info.latest, info.current, info.crate_name
    )
}

// --- Cache types ---

/// On-disk cache record stored under the OS cache directory.
///
/// Why: Throttle crates.io requests to at most once per 24 h so the check
/// does not add measurable latency on typical runs.
/// What: JSON file with a Unix timestamp of the last check and the latest
/// version string seen. A missing or corrupt file is silently treated as "no
/// cache" — the next invocation will perform a fresh network check.
/// Test: `cache_freshness_*` tests in this module.
#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    /// Unix timestamp (seconds) of the last successful crates.io response.
    last_check_unix: u64,
    /// Latest stable version seen at `last_check_unix`.
    latest_version: String,
}

// --- Cache path resolution ---

/// Resolve the path to the per-crate cache file.
///
/// Why: We need a stable, OS-appropriate location that survives reboots and
/// is writable without elevated privileges.
/// What: Returns `<cache_dir>/trusty-tools/update-check/<crate_name>.json`.
/// Falls back to `<temp_dir>/trusty-tools-update-check/<crate_name>.json`
/// when `dirs::cache_dir()` returns `None` (rare in containers).
/// Test: Indirectly covered by the cache read/write helpers.
fn cache_path(crate_name: &str) -> PathBuf {
    let base = dirs::cache_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("trusty-tools-update-check-fallback"));
    base.join("trusty-tools")
        .join("update-check")
        .join(format!("{crate_name}.json"))
}

// --- Cache I/O ---

/// Read the cache file for `crate_name`, returning `None` on any failure.
///
/// Why: A corrupt or missing cache must not error — the caller treats `None`
/// as "do a fresh network check", which is safe and correct.
/// What: Reads the JSON file at [`cache_path`], deserializes a [`CacheEntry`],
/// and returns it. Returns `None` on I/O errors, missing files, or invalid JSON.
/// Test: `cache_round_trip` and `corrupt_cache_returns_none`.
fn read_cache(crate_name: &str) -> Option<CacheEntry> {
    let path = cache_path(crate_name);
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write a cache entry for `crate_name`, ignoring any I/O errors.
///
/// Why: Cache writes are best-effort — a failure (permissions, disk full)
/// should not propagate to the caller; the next run will simply re-check.
/// What: Serializes `entry` to JSON and writes it to [`cache_path`], creating
/// parent directories if necessary.
/// Test: `cache_round_trip` writes then reads back and checks equality.
fn write_cache(crate_name: &str, entry: &CacheEntry) {
    let path = cache_path(crate_name);
    // Best-effort: create parent dirs, silently ignore failures.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(entry) {
        let _ = std::fs::write(&path, json);
    }
}

// --- Semver comparison ---

/// Parse `MAJOR.MINOR.PATCH` from a version string, stripping pre-release /
/// build-metadata suffixes and ignoring non-numeric segments.
///
/// Why: We intentionally avoid the `semver` crate (not a workspace dep) to
/// keep the dependency surface minimal. The comparison logic required here is
/// simple: a tuple of three integers.
/// What: Returns `Some((major, minor, patch))` on success, `None` on any
/// parse failure.
/// Test: `semver_parse_strips_prerelease`, `semver_parse_handles_missing_patch`.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    // Strip pre-release / build-metadata suffix (everything after first '-' or '+').
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.splitn(3, '.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let patch: u64 = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// Return `true` when `latest_str` is strictly newer than `current_str`.
///
/// Why: The update check only fires for actual upgrades, not downgrades or
/// equal versions.
/// What: Parses both strings via [`parse_version`]; returns `false` on any
/// parse failure (best-effort).
/// Test: `semver_newer_returns_true`, `semver_equal_returns_false`,
/// `semver_older_returns_false`, `semver_prerelease_stripped`.
fn is_newer(latest_str: &str, current_str: &str) -> bool {
    match (parse_version(latest_str), parse_version(current_str)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

// --- crates.io API types ---

/// Minimal subset of the crates.io `GET /api/v1/crates/{name}` response.
///
/// Why: We only need `max_stable_version` (or fallbacks); deserializing the
/// full response shape is unnecessary and fragile.
/// What: Wraps the `crate` top-level key in the crates.io JSON payload.
/// Test: `check_crates_io` parses this shape.
#[derive(Deserialize)]
struct CratesIoResponse {
    #[serde(rename = "crate")]
    krate: CrateInfo,
}

#[derive(Deserialize)]
struct CrateInfo {
    /// The highest stable (non-pre-release) version.
    max_stable_version: Option<String>,
    /// Newest version including pre-releases — fallback when stable is absent.
    newest_version: Option<String>,
    /// Alternative field name seen in some older API responses.
    max_version: Option<String>,
}

// --- Network check ---

/// Query crates.io for the latest stable version of `crate_name`.
///
/// Why: One canonical place to encode the User-Agent requirement, timeout,
/// JSON parse, and graceful fallback so callers only see `Option<UpdateInfo>`.
/// What: GETs `https://crates.io/api/v1/crates/{crate_name}` with a
/// descriptive User-Agent (required by crates.io policy; a missing or generic
/// UA returns 403). Parses `crate.max_stable_version`, falls back to
/// `newest_version` / `max_version`. Returns `Some(UpdateInfo)` only when the
/// parsed version is strictly newer than `current_version`. Returns `None` on
/// any error — network failure, timeout, 4xx/5xx, or JSON parse failure.
/// Test: covered by integration; unit tests mock the network path via the
/// throttle + cache layer.
pub async fn check_crates_io(crate_name: &str, current_version: &str) -> Option<UpdateInfo> {
    let url = format!("https://crates.io/api/v1/crates/{crate_name}");
    let user_agent =
        format!("{crate_name}/{current_version} (https://github.com/bobmatnyc/trusty-tools)");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(NETWORK_TIMEOUT_SECS))
        .user_agent(&user_agent)
        .build()
        .ok()?;

    let resp = client.get(&url).send().await.ok()?;

    if !resp.status().is_success() {
        tracing::debug!(
            status = %resp.status(),
            crate_name,
            "crates.io update check returned non-2xx; skipping"
        );
        return None;
    }

    let payload: CratesIoResponse = resp.json().await.ok()?;

    let latest = payload
        .krate
        .max_stable_version
        .or(payload.krate.newest_version)
        .or(payload.krate.max_version)?;

    if !is_newer(&latest, current_version) {
        tracing::debug!(crate_name, current_version, latest, "already up to date");
        return None;
    }

    Some(UpdateInfo {
        crate_name: crate_name.to_owned(),
        current: current_version.to_owned(),
        latest,
    })
}

// --- Current Unix timestamp ---

/// Return seconds since UNIX_EPOCH, or 0 on error.
///
/// Why: `SystemTime` can theoretically fail on platforms with extreme clock
/// skew; returning 0 causes a cache miss rather than a panic.
/// What: Wraps `SystemTime::now().duration_since(UNIX_EPOCH)`.
/// Test: Used inline in `check_throttled` — the value is observable via
/// cache writes in `cache_round_trip`.
fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// --- Throttled public entry point ---

/// Check crates.io for an update, throttled to at most once per 24 hours.
///
/// Why: User-facing CLIs should inform operators of new releases without
/// adding latency or hammering crates.io on every invocation.
///
/// Behaviour:
///
/// 1. Returns `None` immediately when `TRUSTY_NO_UPDATE_CHECK` or `CI` is set
///    (no network call, no cache I/O).
/// 2. Reads the per-crate cache file. If `last_check_unix` is less than 24 h
///    ago, returns the cached result (a fresh `UpdateInfo` when a newer version
///    was recorded, `None` when the cache says we are current).
/// 3. Otherwise performs a `check_crates_io` network call, writes the cache,
///    and returns the result.
///
/// Any I/O or network failure degrades to `None` — the check is best-effort.
///
/// What: Returns `Some(UpdateInfo)` when a newer stable version is available,
/// `None` in every other case.
/// Test: `check_throttled_skips_when_env_set`, `check_throttled_uses_cache`,
/// `check_throttled_fresh_check_on_stale_cache` — all without real network.
pub async fn check_throttled(crate_name: &str, current_version: &str) -> Option<UpdateInfo> {
    // 1. Opt-out via environment.
    let no_check = std::env::var(NO_UPDATE_CHECK_ENV)
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let in_ci = std::env::var(CI_ENV)
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    if no_check || in_ci {
        tracing::trace!(crate_name, "update check suppressed by env var");
        return None;
    }

    let now = now_unix_secs();

    // 2. Try the cache.
    if let Some(entry) = read_cache(crate_name) {
        if now.saturating_sub(entry.last_check_unix) < CHECK_INTERVAL_SECS {
            tracing::trace!(
                crate_name,
                cached_latest = %entry.latest_version,
                "using cached update-check result"
            );
            return if is_newer(&entry.latest_version, current_version) {
                Some(UpdateInfo {
                    crate_name: crate_name.to_owned(),
                    current: current_version.to_owned(),
                    latest: entry.latest_version,
                })
            } else {
                None
            };
        }
        tracing::trace!(
            crate_name,
            "cached update-check entry is stale; re-checking"
        );
    }

    // 3. Network check.
    let result = check_crates_io(crate_name, current_version).await;

    // Write cache with the latest version seen (or the current version when
    // already up to date, so we still record the timestamp).
    let latest_to_cache = result
        .as_ref()
        .map(|u| u.latest.clone())
        .unwrap_or_else(|| current_version.to_owned());
    write_cache(
        crate_name,
        &CacheEntry {
            last_check_unix: now,
            latest_version: latest_to_cache,
        },
    );

    result
}

// --- Upgrade primitives (in sub-module to stay under 500-line cap) ---

/// Upgrade primitives: cargo-install, health-gate, launchd detection, and
/// safe self-restart. Re-exported from the parent `update` module so callers
/// use `trusty_common::update::perform_upgrade` etc. without path changes.
///
/// Why: Extracted to keep `update/mod.rs` under the 500-line cap.
/// What: See each function's own doc comment in `upgrade.rs`.
/// Test: `cargo test -p trusty-common --features update-check`.
pub mod upgrade;

pub use upgrade::{
    is_launchd_supervised, perform_upgrade, upgrade_and_restart, verify_installed_binary,
};

// --- Tests ---

/// Test suite for semver comparison, notice formatting, env-var opt-out,
/// cache freshness, cache I/O resilience, and the upgrade primitives.
///
/// Why: Split into a sibling file so `mod.rs` stays under the 500-line cap.
/// What: All tests are `#[cfg(test)]`-gated and run with
/// `cargo test -p trusty-common --features update-check`.
/// Test: run the above command to verify all cases pass.
#[cfg(test)]
mod tests;
