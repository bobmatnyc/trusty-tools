//! Index-freshness probe helpers for one sandbox index (#9993 G7).
//!
//! Why: the perf plan's freshness row (file change to searchable lag) had no
//! instrument. The probe must write only its own nonce files, only into a
//! sandbox corpus copy, and must remove them even when a sample fails.
//! What: [`nonce`] makes a unique all-letter identifier; [`poll_until_hit`]
//! polls a search closure every [`POLL_INTERVAL`] until it reports a hit, and
//! a timeout or a search error is an `Err`, never a dropped sample;
//! [`hit_names_file`] decides whether a `search.query` reply names the nonce
//! file; [`refuse_temp_root`] and [`check_write_target`] refuse a corpus under
//! a temp root and a write into the daemon's data or storage directories;
//! [`NonceDir`] owns the dedicated subdirectory and removes what it wrote on
//! drop; [`summarize`] and [`freshness_rows`] turn the samples into schema-1
//! rows, dimension `indexing`, unit `ms`.
//! Test: `nonces_are_unique_lowercase_letters`,
//! `poll_returns_the_lag_of_the_first_hit`,
//! `poll_fails_on_timeout_and_on_a_search_error`,
//! `hit_matching_names_only_the_nonce_file`,
//! `write_target_refuses_temp_roots_and_storage_dirs`,
//! `nonce_dir_removes_its_files_on_early_return_and_panic`,
//! `summary_over_known_samples_is_exact`.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::time::Instant;

use super::latency_stats::latency_stats;
use super::perf_envelope::{ResultRow, PERCENTILE_NOTE};

/// The plan's poll cadence: one search every 100 ms.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Per-sample budget. Crossing it fails the test.
pub const SAMPLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Rounds per write path.
pub const ROUNDS: usize = 5;
/// Files per round, written one after another.
pub const FILES_PER_ROUND: usize = 10;
/// Wait after creating the nonce directory, so the watcher has a watch on it
/// before the first file lands (a file created before the watch is missed).
pub const DIR_SETTLE: Duration = Duration::from_secs(2);
/// Name prefix of the dedicated nonce subdirectory in the corpus copy.
pub const DIR_PREFIX: &str = "freshness-probe-9993-";
/// Prefix of every nonce: rare letters, so no corpus word shares it.
pub const NONCE_PREFIX: &str = "zqfp";
/// Base-26 digits after the prefix; 26^28 exceeds 2^128, so the encoding of a
/// `u128` is injective.
pub const NONCE_LETTERS: usize = 28;
/// Below this many samples, a row is labelled indicative (shared method).
pub const INDICATIVE_BELOW: usize = 100;

/// How a nonce file reaches the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritePath {
    /// A plain disk write the daemon's file watcher picks up.
    Watcher,
    /// An explicit `search.index.file.put`; nothing is written to disk.
    Put,
}

impl WritePath {
    pub fn label(self) -> &'static str {
        match self {
            Self::Watcher => "watcher",
            Self::Put => "file.put",
        }
    }
}

/// `u128` as exactly [`NONCE_LETTERS`] lowercase letters, most significant first.
fn letters(mut value: u128) -> String {
    let mut out = [b'a'; NONCE_LETTERS];
    for slot in out.iter_mut().rev() {
        *slot = b'a' + (value % 26) as u8;
        value /= 26;
    }
    out.iter().map(|b| char::from(*b)).collect()
}

/// The nonce for one `(nanos, pid, seq)` triple.
///
/// What: `zqfp` then the base-26 letters of `nanos << 64 | pid << 32 | seq`.
/// Letters only, so no tokenizer splits it at a digit boundary. Distinct
/// triples give distinct nonces.
/// Test: `nonces_are_unique_lowercase_letters`.
pub fn nonce_from(nanos: u64, pid: u32, seq: u32) -> String {
    let value = (u128::from(nanos) << 64) | (u128::from(pid) << 32) | u128::from(seq);
    format!("{NONCE_PREFIX}{}", letters(value))
}

static SEQ: AtomicU32 = AtomicU32::new(0);

/// A nonce no earlier call in this process, or any earlier run, returned.
///
/// What: wall-clock nanos and the pid separate runs; a process counter
/// separates calls within one run.
/// Test: `nonces_are_unique_lowercase_letters`.
pub fn nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the wall clock is after 1970")
        .as_nanos() as u64;
    nonce_from(
        nanos,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
    )
}

/// The nonce file's text: one Rust function named by the nonce.
pub fn nonce_content(nonce: &str) -> String {
    format!("// trusty-search freshness probe nonce (#9993 G7)\npub fn {nonce}() {{}}\n")
}

/// One measured sample: the lag from `t0` to the first hit, and the polls it took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub lag: Duration,
    pub attempts: u32,
}

/// Why a sample has no lag. Every variant fails the test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollError {
    /// No hit within the budget; `waited` is the time since `t0`.
    Timeout { attempts: u32, waited: Duration },
    /// A search call failed; the error text is the call's own.
    Search { attempts: u32, error: String },
}

impl std::fmt::Display for PollError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout { attempts, waited } => write!(
                f,
                "no hit after {attempts} searches in {} ms (timeout)",
                waited.as_millis()
            ),
            Self::Search { attempts, error } => {
                write!(f, "search {attempts} failed: {error}")
            }
        }
    }
}

/// Poll `search` until it reports a hit; the lag is measured from `t0`.
///
/// Why: a sample that never lands must fail the run, not vanish from it
/// (#9993 G7, Fail-Open).
/// What: calls `search`, then sleeps `interval`, until a call returns
/// `Ok(true)`. A call is bounded by the time left in `timeout`. A call
/// returning `Err` ends the poll with [`PollError::Search`]; no hit by
/// `t0 + timeout`, or a call outliving it, ends it with
/// [`PollError::Timeout`]. Postcondition: an `Ok` lag is at most `timeout`
/// plus the last call's own time.
/// Test: `poll_returns_the_lag_of_the_first_hit`,
/// `poll_fails_on_timeout_and_on_a_search_error`.
pub async fn poll_until_hit<F, Fut>(
    t0: Instant,
    interval: Duration,
    timeout: Duration,
    mut search: F,
) -> Result<Sample, PollError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool, String>>,
{
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let left = timeout.saturating_sub(t0.elapsed());
        match tokio::time::timeout(left, search()).await {
            Err(_) => {
                return Err(PollError::Timeout {
                    attempts,
                    waited: t0.elapsed(),
                });
            }
            Ok(Err(error)) => return Err(PollError::Search { attempts, error }),
            Ok(Ok(true)) => {
                return Ok(Sample {
                    lag: t0.elapsed(),
                    attempts,
                });
            }
            Ok(Ok(false)) => {}
        }
        if t0.elapsed() >= timeout {
            return Err(PollError::Timeout {
                attempts,
                waited: t0.elapsed(),
            });
        }
        tokio::time::sleep(interval).await;
    }
}

/// Whether a `search.query` reply has a hit naming the file at `rel`.
///
/// What: a hit names the file when its `path` (root-relative) or `file`
/// (absolute) ends with `rel`, compared by path component. A hit labelled
/// `fallback:*` read the disk, not the index, so it never counts.
/// Test: `hit_matching_names_only_the_nonce_file`.
pub fn hit_names_file(reply: &Value, rel: &Path) -> bool {
    let Some(hits) = reply["results"].as_array() else {
        return false;
    };
    hits.iter().any(|hit| {
        let from_disk = hit["match_reason"]
            .as_str()
            .is_some_and(|r| r.starts_with("fallback:"));
        !from_disk
            && ["path", "file"]
                .iter()
                .filter_map(|key| hit[*key].as_str())
                .any(|named| Path::new(named).ends_with(rel))
    })
}

/// The temp roots a corpus copy may not live under.
fn temp_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = ["/tmp", "/private/tmp", "/var/tmp"]
        .iter()
        .map(PathBuf::from)
        .collect();
    let env_tmp = std::env::temp_dir();
    if let Ok(canonical) = env_tmp.canonicalize() {
        roots.push(canonical);
    }
    roots.push(env_tmp);
    roots
}

/// Refuse a corpus root under a temp directory (#9993: no /tmp roots).
///
/// What: the path as given and, when it resolves, its canonical form are
/// both checked against `/tmp`, `/private/tmp`, `/var/tmp` and the process
/// temp dir.
/// Test: `write_target_refuses_temp_roots_and_storage_dirs`.
pub fn refuse_temp_root(root: &Path) -> Result<(), String> {
    let mut forms = vec![root.to_path_buf()];
    if let Ok(canonical) = root.canonicalize() {
        forms.push(canonical);
    }
    for form in &forms {
        if let Some(tmp) = temp_roots().iter().find(|t| form.starts_with(t)) {
            return Err(format!(
                "refusing corpus {}: it is under the temp root {}",
                root.display(),
                tmp.display()
            ));
        }
    }
    Ok(())
}

/// Refuse a nonce directory that is not a direct child of `corpus`, or that
/// lies inside (or contains) any `forbidden` directory.
///
/// Why: the probe writes into the corpus copy only, never into the daemon's
/// data dir or an index storage dir (#9993 G7).
/// Test: `write_target_refuses_temp_roots_and_storage_dirs`.
pub fn check_write_target(dir: &Path, corpus: &Path, forbidden: &[PathBuf]) -> Result<(), String> {
    if dir.parent() != Some(corpus) {
        return Err(format!(
            "nonce dir {} is not directly under the corpus {}",
            dir.display(),
            corpus.display()
        ));
    }
    if let Some(bad) = forbidden
        .iter()
        .find(|f| dir.starts_with(f) || f.starts_with(dir))
    {
        return Err(format!(
            "nonce dir {} overlaps the daemon directory {}",
            dir.display(),
            bad.display()
        ));
    }
    Ok(())
}

/// The dedicated nonce subdirectory and everything the probe put in it.
///
/// Why: cleanup must run when a sample fails or panics (#9993 G7).
/// What: [`Self::create`] makes a NEW directory (an existing one is refused,
/// so nothing the probe did not create is ever removed). [`Self::write`]
/// records a file before writing it. [`Self::finish`] removes each recorded
/// file and then the directory with `remove_dir`, which fails on content the
/// probe did not write; every failure is returned. `Drop` runs the same
/// removal when `finish` did not: it panics on a failure unless the thread
/// is already panicking, when it prints the failure instead. Paths pushed
/// with `file.put` live only in the index; [`Self::pushed`] lists them for
/// the caller's async `file.remove`.
/// Test: `nonce_dir_removes_its_files_on_early_return_and_panic`.
#[derive(Debug)]
pub struct NonceDir {
    path: PathBuf,
    files: Vec<PathBuf>,
    pushed: Vec<PathBuf>,
    done: bool,
}

impl NonceDir {
    /// Create `<parent>/<DIR_PREFIX><tag>`; it must not exist yet.
    pub fn create(parent: &Path, tag: &str) -> Result<Self, String> {
        let path = parent.join(format!("{DIR_PREFIX}{tag}"));
        std::fs::create_dir(&path)
            .map_err(|e| format!("create nonce dir {}: {e}", path.display()))?;
        Ok(Self {
            path,
            files: Vec::new(),
            pushed: Vec::new(),
            done: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record, then write, `<dir>/<name>`; returns its absolute path.
    pub fn write(&mut self, name: &str, content: &str) -> Result<PathBuf, String> {
        let file = self.path.join(name);
        self.files.push(file.clone());
        std::fs::write(&file, content)
            .map_err(|e| format!("write nonce file {}: {e}", file.display()))?;
        Ok(file)
    }

    /// Record a path pushed into the index with `file.put` (not on disk).
    pub fn record_pushed(&mut self, path: PathBuf) {
        self.pushed.push(path);
    }

    pub fn pushed(&self) -> &[PathBuf] {
        &self.pushed
    }

    /// Remove every recorded file, then the directory; `Err` lists failures.
    pub fn finish(mut self) -> Result<(), String> {
        self.remove_all()
    }

    fn remove_all(&mut self) -> Result<(), String> {
        self.done = true;
        let mut failures = Vec::new();
        for file in &self.files {
            match std::fs::remove_file(file) {
                Ok(()) => {}
                // A write that failed before creating the file left nothing.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => failures.push(format!("remove {}: {e}", file.display())),
            }
        }
        if let Err(e) = std::fs::remove_dir(&self.path) {
            failures.push(format!("remove dir {}: {e}", self.path.display()));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

impl Drop for NonceDir {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        if let Err(e) = self.remove_all() {
            if std::thread::panicking() {
                eprintln!("freshness probe cleanup failed: {e}");
            } else {
                panic!("freshness probe cleanup failed: {e}");
            }
        }
    }
}

/// One write path's samples, summarized.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub n: usize,
    pub min_ms: f64,
    /// Nearest-rank p50, the reported median.
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

/// Summarize lags with G2's nearest-rank method; no samples is an `Err`.
///
/// Test: `summary_over_known_samples_is_exact`.
pub fn summarize(lags: &[Duration]) -> Result<Summary, String> {
    // The window only feeds qps, which a freshness row does not report.
    let window = lags.iter().sum::<Duration>().max(Duration::from_nanos(1));
    let stats = latency_stats(lags, window).map_err(|e| e.to_string())?;
    let min_ms = lags
        .iter()
        .map(|d| d.as_secs_f64() * 1e3)
        .fold(f64::INFINITY, f64::min);
    Ok(Summary {
        n: stats.n,
        min_ms,
        p50_ms: stats.p50_ms,
        p95_ms: stats.p95_ms,
        p99_ms: stats.p99_ms,
        max_ms: stats.max_ms,
    })
}

/// Two schema-1 rows for one write path: the lag distribution and its minimum.
///
/// What: dimension `indexing`, unit `ms`. The first row carries p50 (the
/// median), p95, p99 and max; the second carries the minimum in `value`.
/// No alert threshold exists yet (baseline first). The notes state the
/// method and, below [`INDICATIVE_BELOW`] samples, that the row is indicative.
/// Test: `summary_over_known_samples_is_exact`.
pub fn freshness_rows(path: WritePath, s: &Summary) -> Vec<ResultRow> {
    let mut notes = format!(
        "{PERCENTILE_NOTE}; p50 is the median; {ROUNDS} rounds x {FILES_PER_ROUND} files, \
         written one after another; lag from the write returning to the first \
         search.query stage=lexical reply naming the file; polled every {} ms, \
         {} s timeout fails the run; no alert threshold yet (baseline first)",
        POLL_INTERVAL.as_millis(),
        SAMPLE_TIMEOUT.as_secs()
    );
    if s.n < INDICATIVE_BELOW {
        notes.push_str(&format!("; n {} < {INDICATIVE_BELOW}: indicative", s.n));
    }
    let row = |metric: String| ResultRow {
        dimension: "indexing".into(),
        metric,
        unit: "ms".into(),
        n: Some(s.n),
        k: None,
        p50: None,
        p95: None,
        p99: None,
        max: None,
        value: None,
        threshold: None,
        notes: Some(notes.clone()),
    };
    vec![
        ResultRow {
            p50: Some(s.p50_ms),
            p95: Some(s.p95_ms),
            p99: Some(s.p99_ms),
            max: Some(s.max_ms),
            ..row(format!("freshness lag ({})", path.label()))
        },
        ResultRow {
            value: Some(s.min_ms),
            ..row(format!("freshness lag min ({})", path.label()))
        },
    ]
}

#[cfg(test)]
#[path = "freshness_probe_tests.rs"]
mod tests;
