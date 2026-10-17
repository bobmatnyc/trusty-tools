//! The schema-1 perf results envelope, shared with trusty-memory (#9993 G2).
//!
//! Why: the shared perf method (2026-10-17) has both crates write one JSON
//! envelope so one reader works for both. Its shape is defined in
//! `.trusty-mpm/plans/memory-perf-analysis-plan.md` section 3:
//! `{schema: 1, tool, version, commit, label, host: {id, os, cpu, ram_gb},
//! date, corpus, load: {before, after}, residency: {cached, total},
//! results: [{dimension, metric, unit, n, k, p50, p95, max, value, threshold,
//! notes}]}`. A result row here also carries `p99`, which the suite reports
//! and the trusty-memory shape does not name; it is additive.
//! What: [`Envelope`] and its parts, [`latency_rows`] (one `speed` row and one
//! `throughput` row per operation), and [`record`], which writes the envelope
//! to the path in `TRUSTY_SEARCH_BENCH_ENVELOPE`. Unset or empty writes
//! nothing. The first [`record`] in a process starts a fresh envelope, so a
//! file left by an earlier run is replaced, never merged.
//! Test: `envelope_round_trips_through_serde`,
//! `latency_rows_carry_the_stats_and_the_method`,
//! `envelope_path_is_optional_and_absolute`.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::latency_stats::LatencyStats;

/// Where the envelope goes. Unset or empty: nothing is written.
pub const ENVELOPE_ENV: &str = "TRUSTY_SEARCH_BENCH_ENVELOPE";
/// Commit of the binary under test, recorded verbatim. Optional.
pub const COMMIT_ENV: &str = "TRUSTY_SEARCH_BENCH_COMMIT";
/// `mac` or `evo`. Optional; defaults from the OS (macos: mac, linux: evo).
pub const LABEL_ENV: &str = "TRUSTY_SEARCH_BENCH_LABEL";

/// The method note every latency row carries.
pub const PERCENTILE_NOTE: &str = "nearest-rank percentiles";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub schema: u32,
    pub tool: String,
    pub version: Option<String>,
    pub commit: Option<String>,
    pub label: Option<String>,
    pub host: Host,
    pub date: String,
    pub corpus: Option<String>,
    pub load: Load,
    pub residency: Residency,
    pub results: Vec<ResultRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Host {
    pub id: Option<String>,
    pub os: String,
    pub cpu: Option<String>,
    pub ram_gb: Option<f64>,
}

/// 1-minute load averages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Load {
    pub before: Option<f64>,
    pub after: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Residency {
    pub cached: Option<u64>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultRow {
    pub dimension: String,
    pub metric: String,
    pub unit: String,
    pub n: Option<usize>,
    pub k: Option<u32>,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub p99: Option<f64>,
    pub max: Option<f64>,
    pub value: Option<f64>,
    pub threshold: Option<f64>,
    pub notes: Option<String>,
}

/// What the caller knows about this run; the rest is read from the host.
#[derive(Debug, Clone, Default)]
pub struct RunContext {
    /// The daemon's `search.health` version.
    pub version: Option<String>,
    /// The corpus or index the rows measure.
    pub corpus: Option<String>,
    /// 1-minute load average taken before the measurement started.
    pub load_before: Option<f64>,
}

/// One `speed` row (ms percentiles) and one `throughput` row (qps) for `op`.
///
/// What: `threshold` is the p99 alert threshold in ms when the caller has one.
/// Test: `latency_rows_carry_the_stats_and_the_method`.
pub fn latency_rows(op: &str, stats: &LatencyStats, threshold_ms: Option<f64>) -> Vec<ResultRow> {
    let base = ResultRow {
        dimension: String::new(),
        metric: String::new(),
        unit: String::new(),
        n: Some(stats.n),
        k: None,
        p50: None,
        p95: None,
        p99: None,
        max: None,
        value: None,
        threshold: None,
        notes: None,
    };
    vec![
        ResultRow {
            dimension: "speed".into(),
            metric: format!("{op} latency"),
            unit: "ms".into(),
            p50: Some(stats.p50_ms),
            p95: Some(stats.p95_ms),
            p99: Some(stats.p99_ms),
            max: Some(stats.max_ms),
            threshold: threshold_ms,
            notes: Some(PERCENTILE_NOTE.into()),
            ..base.clone()
        },
        ResultRow {
            dimension: "throughput".into(),
            metric: format!("{op} qps"),
            unit: "ops/s".into(),
            value: Some(stats.qps),
            ..base
        },
    ]
}

/// The envelope path from `raw`: `None` when unset or empty.
///
/// What: a relative path is refused, so a run never writes into whatever
/// directory cargo happens to run the test binary from.
/// Test: `envelope_path_is_optional_and_absolute`.
pub fn envelope_path(raw: Option<&OsStr>) -> Result<Option<PathBuf>, String> {
    let Some(raw) = raw.map(|v| v.to_string_lossy().trim().to_string()) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(format!(
            "{ENVELOPE_ENV} must be absolute, got {}",
            path.display()
        ));
    }
    Ok(Some(path))
}

/// The 1-minute load average now.
pub fn load_now() -> Option<f64> {
    let one = sysinfo::System::load_average().one;
    (one.is_finite() && one >= 0.0).then_some(one)
}

/// A fresh envelope for this host with no results.
fn new_envelope(ctx: &RunContext) -> Envelope {
    let mut sys = sysinfo::System::new();
    sys.refresh_cpu_all();
    sys.refresh_memory();
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|b| !b.is_empty());
    let ram = sys.total_memory();
    let label = std::env::var(LABEL_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| match std::env::consts::OS {
            "macos" => Some("mac".into()),
            "linux" => Some("evo".into()),
            _ => None,
        });
    Envelope {
        schema: 1,
        tool: "trusty-search".into(),
        version: ctx.version.clone(),
        commit: std::env::var(COMMIT_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty()),
        label,
        host: Host {
            id: sysinfo::System::host_name(),
            os: sysinfo::System::long_os_version().unwrap_or_else(|| std::env::consts::OS.into()),
            cpu,
            ram_gb: (ram > 0).then(|| ram as f64 / (1u64 << 30) as f64),
        },
        date: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        corpus: ctx.corpus.clone(),
        load: Load {
            before: ctx.load_before,
            after: None,
        },
        residency: Residency {
            cached: None,
            total: None,
        },
        results: Vec::new(),
    }
}

/// The envelope this process has written so far.
static RUN: Mutex<Option<Envelope>> = Mutex::new(None);

/// Add `rows` to this process's envelope and rewrite the file named by
/// `TRUSTY_SEARCH_BENCH_ENVELOPE`. Unset: nothing is written.
///
/// Why: tests in one binary run on parallel threads; one in-memory envelope
/// behind a mutex keeps every row in one file without reading a stale one.
/// What: the first call builds the header from `ctx` and the host; later
/// calls keep it and append. `load.after` is the load at each write. The
/// file is written to `<path>.tmp` and renamed into place. The parent
/// directory must already exist.
/// Test: `envelope_round_trips_through_serde` covers the serialized shape.
pub fn record(ctx: &RunContext, rows: Vec<ResultRow>) -> Result<(), String> {
    let Some(path) = envelope_path(std::env::var_os(ENVELOPE_ENV).as_deref())? else {
        return Ok(());
    };
    let mut guard = RUN
        .lock()
        .map_err(|_| "envelope lock poisoned".to_string())?;
    let envelope = guard.get_or_insert_with(|| new_envelope(ctx));
    envelope.results.extend(rows);
    envelope.load.after = load_now();
    write_atomic(&path, envelope)
}

fn write_atomic(path: &Path, envelope: &Envelope) -> Result<(), String> {
    let text = serde_json::to_string_pretty(envelope)
        .map_err(|e| format!("serialize perf envelope: {e}"))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename to {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> LatencyStats {
        LatencyStats {
            n: 100,
            p50_ms: 50.0,
            p95_ms: 95.0,
            p99_ms: 99.0,
            max_ms: 100.0,
            qps: 25.0,
        }
    }

    #[test]
    fn envelope_round_trips_through_serde() {
        let mut envelope = new_envelope(&RunContext {
            version: Some("0.0.0-test".into()),
            corpus: Some("perf-abc".into()),
            load_before: Some(0.5),
        });
        // Fixed host, clock and env fields: a live reading (ram_gb) may not
        // round-trip through serde_json bit-exactly, which flaked on CI.
        envelope.commit = Some("abc1234".into());
        envelope.label = Some("test".into());
        envelope.host = Host {
            id: Some("test-host".into()),
            os: "TestOS 1".into(),
            cpu: Some("Test CPU".into()),
            ram_gb: Some(16.0),
        };
        envelope.date = "2026-01-01T00:00:00Z".into();
        envelope.load.after = Some(0.75);
        envelope.results = latency_rows("search.query serial", &stats(), Some(2000.0));
        let text = serde_json::to_string(&envelope).expect("serialize");
        let back: Envelope = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(back, envelope);
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(value["schema"], 1);
        assert_eq!(value["tool"], "trusty-search");
        assert_eq!(value["results"][0]["p99"], 99.0);
        // Absent optional fields serialize as null, so every row has one shape.
        assert!(value["results"][1]["k"].is_null());
    }

    #[test]
    fn latency_rows_carry_the_stats_and_the_method() {
        let rows = latency_rows("search.grep", &stats(), None);
        assert_eq!(rows.len(), 2);
        let (speed, qps) = (&rows[0], &rows[1]);
        assert_eq!(speed.metric, "search.grep latency");
        assert_eq!(
            (speed.n, speed.p50, speed.p95, speed.p99, speed.max),
            (Some(100), Some(50.0), Some(95.0), Some(99.0), Some(100.0))
        );
        assert_eq!(speed.notes.as_deref(), Some(PERCENTILE_NOTE));
        assert_eq!(
            (qps.unit.as_str(), qps.n, qps.value),
            ("ops/s", Some(100), Some(25.0))
        );
    }

    #[test]
    fn envelope_path_is_optional_and_absolute() {
        assert_eq!(envelope_path(None), Ok(None));
        assert_eq!(envelope_path(Some(OsStr::new("  "))), Ok(None));
        assert!(envelope_path(Some(OsStr::new("out/perf.json"))).is_err());
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("perf.json");
        assert_eq!(envelope_path(Some(path.as_os_str())), Ok(Some(path)));
    }
}
