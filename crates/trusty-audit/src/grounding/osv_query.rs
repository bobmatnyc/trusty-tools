//! How an OSV answer is obtained: cache first, then the batch endpoint (#6780).
//!
//! Why: [`super::osv`] decides WHAT to ask and what the answer means. This
//! module decides how the asking happens, because the three rules that govern
//! it are all about the transport and none about vulnerabilities — an
//! air-gapped run must never open a socket, a rate-limited batch must be
//! retried rather than dropped, and no repository may spend an unbounded amount
//! of a sweep's wall clock on one endpoint.
//!
//! What: [`resolve`], which answers every coordinate from the on-disk cache,
//! then (unless [`Settings::offline`]) asks OSV for the rest in chunks of
//! `super::osv::MAX_QUERIES_PER_BATCH`, retrying a 429 or a 5xx with
//! exponential backoff and stopping at [`Settings::time_cap`].
//!
//! ## Offline
//!
//! `offline` is the air-gapped mode: the cache is the only source, and every
//! miss is a NAMED "cache miss" line rather than a silent absence. It opens no
//! socket at all — not one request, not a DNS lookup — which is what makes it
//! usable on a machine with no route out, and what the test asserts by counting
//! the mock server's requests rather than by watching for a timeout.
//!
//! ## The cache
//!
//! One JSON file per `(ecosystem, name, version)` under the audit work
//! directory, named by the SHA-256 of that triple. Hashed rather than spelled
//! because a coordinate name is `@scope/pkg` in npm and a whole URL path in Go,
//! neither of which is a filename; the coordinate is written inside the file so
//! a cache entry still says what it is. An entry older than [`Settings::ttl`]
//! is ignored — an advisory database gains rows, so a stale "no advisories" is
//! the answer worth expiring.
//!
//! ## No client of its own
//!
//! The `reqwest::Client` comes from `trusty_installer::download::http_client`,
//! this crate's one client constructor (CLAUDE.md's common-entry-point rule) —
//! the same one `crate::validate` and `crate::tools` reach for.
//!
//! Test: `super::osv::osv_tests`.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::osv::{Coordinate, Severity, Vuln, batches};

/// OSV's batch endpoint, as an engagement uses it unless it says otherwise.
pub const DEFAULT_ENDPOINT: &str = "https://api.osv.dev/v1/querybatch";

/// How long a cached answer is trusted, unless the engagement says otherwise.
pub const DEFAULT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long one repository may spend on OSV, unless the engagement says otherwise.
///
/// A sweep audits many repositories in one run, so an endpoint that is slow
/// rather than down must cost this repository its coverage and not the sweep.
pub const DEFAULT_TIME_CAP: Duration = Duration::from_secs(120);

/// Attempts per batch, the first included.
pub const MAX_ATTEMPTS: u32 = 3;

/// Backoff before the second attempt; doubled before each one after it.
pub const FIRST_BACKOFF: Duration = Duration::from_millis(250);

/// Directory under the working directory's `state/` area holding the cache.
pub const CACHE_DIR: &str = "osv-cache";

/// Everything the transport needs, as values rather than as environment reads.
///
/// Why: the same split every other leg here takes — an endpoint, a clock budget
/// and a directory as fields is what lets the retry, the cache and the offline
/// arm be driven against a local mock server with nothing in `std::env`, which
/// is `unsafe` in edition 2024 and unsound under the parallel harness.
/// What: where to ask, whether to ask at all, where answers are kept, and the
/// two budgets.
/// Test: `super::osv::osv_tests`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Settings {
    /// The batch endpoint. Overridden only by a test's mock server.
    pub endpoint: String,
    /// Answer from cache alone, and open no socket.
    pub offline: bool,
    /// Directory holding one file per cached coordinate.
    pub cache_dir: PathBuf,
    /// How long a cached answer is trusted.
    pub ttl: Duration,
    /// Wall clock this repository may spend on OSV.
    pub time_cap: Duration,
}

impl Settings {
    /// The settings an engagement's `[osv]` table asks for.
    ///
    /// Why: the cache lives under the working directory this client already
    /// owns, so deleting that directory deletes the cache with it — the
    /// completeness promise `crate::workdir` makes. It goes under `state/`
    /// rather than in an area of its own because it IS run state: derived,
    /// re-fetchable, and never part of the deliverable.
    /// What: the declared values, each falling back to the compiled default
    /// when absent or zero. `offline` is `true` when the config says so or
    /// [`ENV_OFFLINE`] is set to a non-empty value — the escape hatch for an
    /// air-gapped machine whose config was written elsewhere.
    /// Test: `super::osv::osv_tests::an_engagement_can_turn_the_collector_on`.
    #[must_use]
    pub fn for_engagement(
        declared: &crate::config::OsvSettings,
        work: &crate::workdir::WorkDir,
    ) -> Self {
        let hours = |value: Option<u64>| value.filter(|v| *v > 0).map(|v| v * 60 * 60);
        let secs = |value: Option<u64>| value.filter(|v| *v > 0);
        Self {
            endpoint: declared
                .endpoint
                .as_deref()
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .unwrap_or(DEFAULT_ENDPOINT)
                .to_owned(),
            offline: declared.offline || env_flag(ENV_OFFLINE),
            cache_dir: work.path(crate::workdir::Area::State).join(CACHE_DIR),
            ttl: hours(declared.cache_ttl_hours).map_or(DEFAULT_TTL, Duration::from_secs),
            time_cap: secs(declared.time_cap_secs).map_or(DEFAULT_TIME_CAP, Duration::from_secs),
        }
    }
}

/// Forces [`Settings::offline`] on, whatever the engagement config declares.
pub const ENV_OFFLINE: &str = "TRUSTY_AUDIT_OSV_OFFLINE";

/// Whether an environment variable is set to something non-empty.
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

/// One cached answer, with the moment it was fetched.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    /// What was asked, so a cache file says what it is.
    coordinate: Coordinate,
    /// Unix seconds at which the answer was written.
    fetched_at: u64,
    /// The answer, possibly empty — an empty answer is a real result.
    vulns: Vec<Vuln>,
}

/// Answer every coordinate: cache, then the endpoint unless offline.
///
/// Why/What: see the module docs.
///
/// # Postconditions
/// The returned vector is the same length as `coordinates` and in the same
/// order. `None` at an index means nothing answered for that coordinate, and
/// the reason is in the returned error list — never both silent.
///
/// Test: `super::osv::osv_tests::{a_cached_answer_is_never_fetched_again,
/// an_offline_miss_is_a_named_cache_miss,
/// a_rate_limited_batch_is_retried_and_then_answers}`.
pub async fn resolve(
    settings: &Settings,
    coordinates: &[Coordinate],
) -> (Vec<Option<Vec<Vuln>>>, Vec<String>) {
    let mut answers: Vec<Option<Vec<Vuln>>> = coordinates
        .iter()
        .map(|coordinate| read_cached(settings, coordinate))
        .collect();
    let mut errors = Vec::new();

    let missing: Vec<Coordinate> = coordinates
        .iter()
        .zip(&answers)
        .filter(|(_, answer)| answer.is_none())
        .map(|(coordinate, _)| coordinate.clone())
        .collect();
    if missing.is_empty() {
        return (answers, errors);
    }
    if settings.offline {
        errors.push(cache_miss(&missing));
        return (answers, errors);
    }

    let client = trusty_installer::download::http_client();
    let started = std::time::Instant::now();
    let mut fetched: Vec<(Coordinate, Vec<Vuln>)> = Vec::new();
    for batch in batches(&missing) {
        if started.elapsed() >= settings.time_cap {
            errors.push(format!(
                "the {}s OSV time cap was reached with {} package(s) unqueried",
                settings.time_cap.as_secs(),
                missing.len() - fetched.len()
            ));
            break;
        }
        match fetch(&client, settings, batch, started).await {
            Ok(results) => {
                for (coordinate, vulns) in batch.iter().zip(results) {
                    write_cached(settings, coordinate, &vulns);
                    fetched.push((coordinate.clone(), vulns));
                }
            }
            Err(cause) => errors.push(format!(
                "a batch of {} package(s) went unanswered ({cause})",
                batch.len()
            )),
        }
    }

    for (coordinate, vulns) in fetched {
        if let Some(index) = coordinates.iter().position(|c| *c == coordinate) {
            answers[index] = Some(vulns);
        }
    }
    (answers, errors)
}

/// The one line an offline run states instead of opening a socket.
fn cache_miss(missing: &[Coordinate]) -> String {
    const SHOWN: usize = 5;
    let named: Vec<String> = missing.iter().take(SHOWN).map(Coordinate::label).collect();
    let tail = if missing.len() > SHOWN {
        format!("; and {} more", missing.len() - SHOWN)
    } else {
        String::new()
    };
    format!(
        "offline mode: cache miss for {} package(s), which were not queried: {}{tail}",
        missing.len(),
        named.join("; ")
    )
}

/// POST one batch, retrying a 429 or a 5xx until [`MAX_ATTEMPTS`].
///
/// Why: OSV rate-limits, and a sweep asking about several repositories in a row
/// is exactly the shape that trips it. Dropping the batch on the first 429
/// would leave a whole repository unassessed over a condition that clears in a
/// quarter of a second.
/// What: exponential backoff from [`FIRST_BACKOFF`], bounded by attempts AND by
/// the caller's remaining time cap — whichever binds first. A 4xx that is not
/// 429 is not retried: it will not become a different answer.
///
/// # Errors
/// One line naming the last status or transport failure.
async fn fetch(
    client: &reqwest::Client,
    settings: &Settings,
    batch: &[Coordinate],
    started: std::time::Instant,
) -> Result<Vec<Vec<Vuln>>, String> {
    let body = request_body(batch);
    let mut backoff = FIRST_BACKOFF;
    let mut last = String::from("no attempt was made");
    for attempt in 1..=MAX_ATTEMPTS {
        if attempt > 1 {
            if started.elapsed() + backoff >= settings.time_cap {
                return Err(format!("{last}, and the time cap left no room to retry"));
            }
            tokio::time::sleep(backoff).await;
            backoff *= 2;
        }
        match client.post(&settings.endpoint).json(&body).send().await {
            Ok(response) if response.status().is_success() => {
                let text = response
                    .text()
                    .await
                    .map_err(|e| format!("the OSV response body could not be read ({e})"))?;
                return parse(&text);
            }
            Ok(response) => {
                let status = response.status();
                last = format!("OSV answered {status}");
                if !(status.as_u16() == 429 || status.is_server_error()) {
                    return Err(last);
                }
            }
            Err(e) => last = format!("the OSV request failed ({e})"),
        }
    }
    Err(format!("{last} after {MAX_ATTEMPTS} attempt(s)"))
}

/// The `querybatch` request document for one batch.
///
/// Test: `super::osv::osv_tests::the_request_body_is_one_query_per_coordinate`.
#[must_use]
pub fn request_body(batch: &[Coordinate]) -> serde_json::Value {
    serde_json::json!({
        "queries": batch
            .iter()
            .map(|coordinate| serde_json::json!({
                "package": { "name": coordinate.name, "ecosystem": coordinate.ecosystem },
                "version": coordinate.version,
            }))
            .collect::<Vec<_>>(),
    })
}

/// Reduce one `querybatch` response to one advisory list per query, in order.
///
/// `results` is positional: OSV answers the i-th query at the i-th index, and a
/// package with no advisories is an entry with no `vulns` key rather than an
/// omission. A response with fewer results than queries is short-padded with
/// empty lists by the caller's `zip`, which drops the unanswered tail rather
/// than misaligning it onto the wrong package.
///
/// # Errors
/// One line when the body is not JSON or declares no `results` array.
///
/// Test: `super::osv::osv_tests::an_id_only_answer_is_still_a_vulnerability`.
pub fn parse(body: &str) -> Result<Vec<Vec<Vuln>>, String> {
    let doc: serde_json::Value = serde_json::from_str(body.trim())
        .map_err(|e| format!("the OSV response is not readable as JSON ({e})"))?;
    let results = doc
        .get("results")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "the OSV response declares no `results` array".to_string())?;
    Ok(results.iter().map(vulns_of).collect())
}

/// One `results[i]` entry as an advisory list.
fn vulns_of(result: &serde_json::Value) -> Vec<Vuln> {
    result
        .get("vulns")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(vuln_of)
        .collect()
}

/// One `vulns[i]` entry, when it names an id.
fn vuln_of(value: &serde_json::Value) -> Option<Vuln> {
    let id = value.get("id").and_then(serde_json::Value::as_str)?;
    let aliases = value
        .get("aliases")
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(Vuln {
        id: id.to_owned(),
        aliases,
        summary: value
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        severity: stated_severity(value),
    })
}

/// The qualitative label OSV stated, wherever it stated it.
///
/// OSV puts it under `database_specific.severity` for a GHSA and under the
/// affected range's `ecosystem_specific` for some others; the `severity` array
/// beside them carries a CVSS vector, which this collector deliberately does
/// not score (see `super::osv::Severity`). Both places are read, the first that
/// answers wins, and neither answering is [`Severity::Unknown`].
fn stated_severity(value: &serde_json::Value) -> Severity {
    let label = value
        .pointer("/database_specific/severity")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .pointer("/affected/0/ecosystem_specific/severity")
                .and_then(serde_json::Value::as_str)
        });
    label.map_or(Severity::Unknown, Severity::parse)
}

/// The cache file one coordinate is kept in.
fn cache_path(settings: &Settings, coordinate: &Coordinate) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(coordinate.ecosystem.as_bytes());
    hasher.update([0]);
    hasher.update(coordinate.name.as_bytes());
    hasher.update([0]);
    hasher.update(coordinate.version.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    settings.cache_dir.join(&digest[..2]).join(digest)
}

/// The cached answer for `coordinate`, when one is present and fresh.
///
/// Every failure — no file, unreadable, unparseable, expired — reads as a miss.
/// A cache is an optimisation, so nothing about it may fail a scan.
fn read_cached(settings: &Settings, coordinate: &Coordinate) -> Option<Vec<Vuln>> {
    let text = std::fs::read_to_string(cache_path(settings, coordinate)).ok()?;
    let entry: Entry = serde_json::from_str(&text).ok()?;
    let age = now_secs().checked_sub(entry.fetched_at)?;
    (age <= settings.ttl.as_secs()).then_some(entry.vulns)
}

/// Keep `vulns` for `coordinate`. A cache that cannot be written is not an
/// error: the answer is already in hand, and the next run simply re-fetches.
fn write_cached(settings: &Settings, coordinate: &Coordinate, vulns: &[Vuln]) {
    let path = cache_path(settings, coordinate);
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let entry = Entry {
        coordinate: coordinate.clone(),
        fetched_at: now_secs(),
        vulns: vulns.to_vec(),
    };
    if let Ok(json) = serde_json::to_string(&entry) {
        let _ = std::fs::write(path, json);
    }
}

/// Seconds since the Unix epoch, or 0 on a clock before it.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Write a cache entry directly, for a test that needs a hit or a stale entry.
///
/// Why: the freshness rule is the one cache behaviour a test cannot set up
/// through the public path — a fetched entry is always fresh. Exposed under
/// `cfg(test)` alone so nothing in the shipped binary can plant one.
/// Test: `super::osv::osv_tests::a_cached_answer_is_never_fetched_again`.
#[cfg(test)]
pub(crate) fn seed_cache(
    settings: &Settings,
    coordinate: &Coordinate,
    vulns: &[Vuln],
    age: Duration,
) {
    let path = cache_path(settings, coordinate);
    std::fs::create_dir_all(path.parent().expect("a cache path has a parent"))
        .expect("mkdir cache");
    let entry = Entry {
        coordinate: coordinate.clone(),
        fetched_at: now_secs().saturating_sub(age.as_secs()),
        vulns: vulns.to_vec(),
    };
    std::fs::write(&path, serde_json::to_string(&entry).expect("serialise")).expect("seed cache");
}

/// The cache file for `coordinate`, for a test asserting one was written.
#[cfg(test)]
pub(crate) fn cached_path(settings: &Settings, coordinate: &Coordinate) -> PathBuf {
    cache_path(settings, coordinate)
}
