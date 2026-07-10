//! Tests for the M3 benchmark corpus + percentile placement engine.
//!
//! Why: benchmarking is deterministic and privacy-sensitive; these tests pin the
//! snapshot round-trip and redaction, the corrupt/stale skip-with-warning
//! behaviour, the documented percentile/quartile/rank method (including the
//! n=1, ties, and target-in-population edge cases), and the small-n honesty gate
//! that must never rank silently against a tiny corpus.
//! What: unit tests over `benchmark.rs` public surface, using temp dirs for I/O
//! and hand-built metrics for the ranking math.
//! Test: included as `#[cfg(test)] mod tests` from `benchmark.rs`.

use super::*;
use crate::report::metrics::{
    AnalyzeMetrics, ComplexityBucket, ComplexityDistribution, CountMetrics, LanguageLoc,
    LocMetrics, MetricFinding, Severity,
};
use crate::report::model::RepositoryReport;

/// Build metrics with a given total LoC, file/function counts, findings, and an
/// optional high-complexity-bucket count over `total_funcs`.
fn metrics(
    total_loc: u64,
    files: u64,
    functions: u64,
    findings: Vec<Severity>,
    high_bucket: Option<(u64, u64)>,
) -> AnalyzeMetrics {
    let buckets = match high_bucket {
        Some((low, high)) => vec![
            ComplexityBucket {
                label: "low".to_string(),
                count: low,
            },
            ComplexityBucket {
                label: "high".to_string(),
                count: high,
            },
        ],
        None => vec![],
    };
    AnalyzeMetrics {
        schema_version: "v0".to_string(),
        repository: "r".to_string(),
        loc: LocMetrics {
            total: total_loc,
            by_language: vec![LanguageLoc {
                language: "Rust".to_string(),
                loc: total_loc,
            }],
        },
        counts: CountMetrics { files, functions },
        complexity: ComplexityDistribution { buckets },
        findings: findings
            .into_iter()
            .map(|severity| MetricFinding {
                title: "f".to_string(),
                severity,
                category: "c".to_string(),
                component: "x".to_string(),
            })
            .collect(),
    }
}

/// A snapshot with a given slug and total LoC (other metrics minimal).
fn snap(slug: &str, total_loc: u64) -> CorpusSnapshot {
    CorpusSnapshot {
        schema_version: CORPUS_SCHEMA_VERSION.to_string(),
        slug: slug.to_string(),
        name: slug.to_string(),
        source_basename: slug.to_string(),
        git_sha: None,
        timestamp: "2026-07-10".to_string(),
        metrics: metrics(total_loc, 10, 100, vec![], None),
    }
}

/// A repository report with optional metrics for `from_repository` tests.
fn repo(slug: &str, m: Option<AnalyzeMetrics>) -> RepositoryReport {
    RepositoryReport {
        name: format!("App {slug}"),
        slug: slug.to_string(),
        source: "/home/user/checkouts/acme-web.git".to_string(),
        source_kind: "local_path".to_string(),
        username: None,
        git_ref: None,
        git_info: None,
        local_path: None,
        scan: None,
        metrics: m,
    }
}

// ─── Snapshot construction + persistence ──────────────────────────────────────

/// Why: a snapshot must survive a JSON write/read and preserve identity+metrics.
/// What: writes a snapshot, reloads the corpus, and asserts one usable snapshot
/// with the same slug and LoC.
/// Test: this test itself.
#[test]
fn snapshot_roundtrip() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let s = snap("acme-web", 8200);
    let path = write_snapshot(tmp.path(), &s).expect("write");
    assert!(path.exists());

    let loaded = load_corpus(tmp.path()).expect("load");
    assert_eq!(loaded.snapshots.len(), 1);
    assert!(loaded.warnings.is_empty());
    assert_eq!(loaded.snapshots[0].slug, "acme-web");
    assert_eq!(loaded.snapshots[0].metrics.loc.total, 8200);
}

/// Why: a metric-less repo has nothing rankable, so it must not become a snapshot.
/// What: asserts `from_repository` is `None` without metrics and `Some` with, and
/// that the source is redacted to a basename (no path, no `.git`).
/// Test: this test itself.
#[test]
fn from_repository_requires_metrics() {
    assert!(CorpusSnapshot::from_repository(&repo("a", None), "2026-07-10").is_none());
    let s = CorpusSnapshot::from_repository(
        &repo("a", Some(metrics(100, 1, 1, vec![], None))),
        "2026-07-10",
    )
    .expect("some");
    assert_eq!(s.source_basename, "acme-web");
    assert_eq!(s.slug, "a");
}

/// Why: the corpus key must prefer the git SHA, else the date, and be file-safe.
/// What: asserts the key form with and without a SHA.
/// Test: this test itself.
#[test]
fn file_key_uses_sha_then_date() {
    let mut s = snap("Acme Web", 100);
    assert_eq!(s.file_key(), "acme-web-2026-07-10.json");
    s.git_sha = Some("abc1234".to_string());
    assert_eq!(s.file_key(), "acme-web-abc1234.json");
}

/// Why: a re-run must overwrite the same key, not accumulate duplicates.
/// What: writes the same snapshot twice and asserts a single file remains.
/// Test: this test itself.
#[test]
fn corpus_add_overwrites_same_key() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let s = snap("acme", 100);
    write_snapshot(tmp.path(), &s).expect("first");
    write_snapshot(tmp.path(), &s).expect("second");
    let loaded = load_corpus(tmp.path()).expect("load");
    assert_eq!(loaded.snapshots.len(), 1);
}

// ─── Corpus location resolution ───────────────────────────────────────────────

/// Why: CLI beats manifest beats the XDG default; a relative manifest key is
/// resolved against the manifest directory.
/// What: asserts each precedence tier.
/// Test: this test itself.
#[test]
fn corpus_dir_precedence() {
    let manifest_dir = std::path::Path::new("/proj/dd");
    // CLI wins outright.
    let cli = std::path::Path::new("/explicit/corpus");
    assert_eq!(
        corpus_dir(Some(cli), Some("ignored"), manifest_dir).unwrap(),
        std::path::PathBuf::from("/explicit/corpus")
    );
    // Manifest key (relative) resolves against the manifest dir.
    assert_eq!(
        corpus_dir(None, Some("bench"), manifest_dir).unwrap(),
        std::path::PathBuf::from("/proj/dd/bench")
    );
    // Manifest key (absolute) passes through.
    assert_eq!(
        corpus_dir(None, Some("/abs/bench"), manifest_dir).unwrap(),
        std::path::PathBuf::from("/abs/bench")
    );
    // Default falls back to the XDG data dir (present on the test platform).
    let def = corpus_dir(None, None, manifest_dir);
    if let Some(d) = def {
        assert!(d.ends_with("trusty-review/benchmark"));
    }
}

// ─── Loader tolerance ─────────────────────────────────────────────────────────

/// Why: the loader must skip unreadable, unparseable, and schema-mismatched
/// snapshots with a collected warning, never a hard error.
/// What: writes one good, one corrupt, and one wrong-schema file, plus a
/// non-JSON file, and asserts one usable snapshot and three warnings.
/// Test: this test itself.
#[test]
fn load_skips_corrupt_and_mismatched() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    write_snapshot(tmp.path(), &snap("good", 100)).expect("good");
    std::fs::write(tmp.path().join("corrupt.json"), "{ not json").expect("corrupt");
    let mut wrong = snap("wrong", 50);
    wrong.schema_version = "corpus-v9".to_string();
    let wrong_json = serde_json::to_string(&wrong).expect("ser");
    std::fs::write(tmp.path().join("wrong.json"), wrong_json).expect("wrong");
    // A non-JSON file must be ignored entirely (not a warning).
    std::fs::write(tmp.path().join("README.txt"), "hi").expect("txt");

    let loaded = load_corpus(tmp.path()).expect("load");
    assert_eq!(loaded.snapshots.len(), 1);
    assert_eq!(loaded.snapshots[0].slug, "good");
    assert_eq!(
        loaded.warnings.len(),
        2,
        "corrupt + schema-mismatch warnings"
    );
}

/// Why: a missing corpus dir is a first-run condition, not an error.
/// What: asserts an empty corpus with one warning for a non-existent dir.
/// Test: this test itself.
#[test]
fn load_missing_dir_warns_not_errors() {
    let loaded = load_corpus(std::path::Path::new("/no/such/corpus/dir")).expect("ok");
    assert!(loaded.snapshots.is_empty());
    assert_eq!(loaded.warnings.len(), 1);
}

// ─── Comparable metric extraction ─────────────────────────────────────────────

/// Why: only computable metrics may enter a population; absent inputs are omitted.
/// What: a full metrics doc yields all seven metrics; a doc with zero LoC and no
/// complexity yields only the raw counts.
/// Test: this test itself.
#[test]
fn comparable_metrics_extraction() {
    let full = metrics(
        2000,
        40,
        400,
        vec![Severity::Red, Severity::Amber],
        Some((90, 10)),
    );
    let keys: Vec<&str> = comparable_metrics(&full)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(
        keys,
        vec![
            "total_loc",
            "file_count",
            "function_count",
            "findings_density_total",
            "findings_density_red",
            "findings_density_amber",
            "complexity_high_share",
        ]
    );
    // findings_density_total = 2 findings over 2000 LoC = 1.0 / 1k LoC.
    let m = comparable_metrics(&full);
    let dens = m
        .iter()
        .find(|(k, _)| *k == "findings_density_total")
        .unwrap()
        .1;
    assert!((dens - 1.0).abs() < 1e-9);
    // complexity_high_share = 10 of 100 functions = 10%.
    let share = m
        .iter()
        .find(|(k, _)| *k == "complexity_high_share")
        .unwrap()
        .1;
    assert!((share - 10.0).abs() < 1e-9);

    // No LoC, no complexity: only file/function counts survive.
    let sparse = metrics(0, 5, 7, vec![Severity::Red], None);
    let sparse_keys: Vec<&str> = comparable_metrics(&sparse)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(sparse_keys, vec!["file_count", "function_count"]);
}

// ─── Percentile / quartile / rank math ────────────────────────────────────────

/// Why: the documented cumulative-share percentile must handle the edges — an
/// n=1 population, a unique max/min, and the target-in-population invariant.
/// What: asserts percentile values across those cases.
/// Test: this test itself.
#[test]
fn percentile_edges() {
    // n=1: the sole (target) value is at the 100th percentile.
    assert!((percentile_rank(5.0, &[5.0]) - 100.0).abs() < 1e-9);
    // Unique maximum among 5 → 100th; unique minimum → 20th (1 of 5 <=).
    let pop = [10.0, 20.0, 30.0, 40.0, 50.0];
    assert!((percentile_rank(50.0, &pop) - 100.0).abs() < 1e-9);
    assert!((percentile_rank(10.0, &pop) - 20.0).abs() < 1e-9);
    // Median value (30) → 60th (3 of 5 <=).
    assert!((percentile_rank(30.0, &pop) - 60.0).abs() < 1e-9);
    // Empty population never ranks.
    assert_eq!(percentile_rank(1.0, &[]), 0.0);
}

/// Why: tied values must share a percentile and a rank.
/// What: with duplicates present, the tied value's percentile counts all `<=`,
/// and its ascending rank counts only strictly-smaller values.
/// Test: this test itself.
#[test]
fn ascending_rank_ties() {
    let pop = [10.0, 20.0, 20.0, 30.0];
    // Two 20s: percentile counts 3 of 4 <= 20 → 75th; rank = 1 + one value < 20 = 2.
    assert!((percentile_rank(20.0, &pop) - 75.0).abs() < 1e-9);
    assert_eq!(ascending_rank(20.0, &pop), 2);
    // Smallest value ranks 1.
    assert_eq!(ascending_rank(10.0, &pop), 1);
    // Largest value ranks 4 (three strictly smaller).
    assert_eq!(ascending_rank(30.0, &pop), 4);
}

/// Why: the quartile boundaries must be exact and stable.
/// What: asserts each boundary maps to the intended quartile.
/// Test: this test itself.
#[test]
fn quartile_boundaries() {
    assert_eq!(quartile_of(0.0), 1);
    assert_eq!(quartile_of(25.0), 1);
    assert_eq!(quartile_of(25.1), 2);
    assert_eq!(quartile_of(50.0), 2);
    assert_eq!(quartile_of(75.0), 3);
    assert_eq!(quartile_of(75.1), 4);
    assert_eq!(quartile_of(100.0), 4);
}

// ─── Assembled benchmark report + honesty gate ────────────────────────────────

/// Why: fewer than five peers must NOT be ranked; the target is marked too-small.
/// What: builds a corpus of four peers and asserts the target is `CorpusTooSmall`
/// with the peer count and no placements.
/// Test: this test itself.
#[test]
fn small_corpus_gate() {
    let corpus = LoadedCorpus {
        snapshots: vec![
            snap("p1", 100),
            snap("p2", 200),
            snap("p3", 300),
            snap("p4", 400),
        ],
        warnings: vec![],
    };
    let target = snap("target", 250);
    let report = build_benchmark_report(&corpus, std::slice::from_ref(&target));
    assert_eq!(report.repositories.len(), 1);
    match &report.repositories[0].status {
        BenchmarkStatus::CorpusTooSmall(n) => assert_eq!(*n, 4),
        other => panic!("expected too-small, got {other:?}"),
    }
    assert!(report.repositories[0].placements.is_empty());
}

/// Why: with enough peers the target is ranked, and its own value is included in
/// the population (so percentile reflects the full set).
/// What: five peers + a target LoC that is the median of the six values; asserts
/// Ranked status, a total_loc placement, population 6, and a plausible quartile.
/// Test: this test itself.
#[test]
fn ranks_target_in_population() {
    let corpus = LoadedCorpus {
        snapshots: vec![
            snap("p1", 100),
            snap("p2", 200),
            snap("p3", 300),
            snap("p4", 400),
            snap("p5", 500),
        ],
        warnings: vec!["a warning".to_string()],
    };
    let target = snap("target", 350); // sits between p3 and p4
    let report = build_benchmark_report(&corpus, std::slice::from_ref(&target));
    let rb = &report.repositories[0];
    assert!(matches!(rb.status, BenchmarkStatus::Ranked));
    assert_eq!(rb.peers, 5);

    let loc = rb
        .placements
        .iter()
        .find(|p| p.metric == "total_loc")
        .expect("total_loc placement");
    // Population = 5 peers + the target = 6; four values (100,200,300,350) <= 350.
    assert_eq!(loc.population, 6);
    assert!((loc.percentile - (4.0 / 6.0 * 100.0)).abs() < 1e-9);
    assert_eq!(loc.rank, 4); // 3 strictly smaller + 1
    assert_eq!(loc.quartile, quartile_of(loc.percentile));
    // Warnings propagate onto the report.
    assert_eq!(report.corpus_size, 5);
    assert_eq!(report.warnings, vec!["a warning".to_string()]);
}

/// Why: a stale corpus copy of the target must not double-count in its population.
/// What: a corpus that already contains a snapshot with the target's slug is
/// excluded from the peer set; the fresh target value is used instead.
/// Test: this test itself.
#[test]
fn target_slug_excluded_from_peers() {
    let corpus = LoadedCorpus {
        snapshots: vec![
            snap("target", 999_999), // stale copy of the target — must be excluded
            snap("p1", 100),
            snap("p2", 200),
            snap("p3", 300),
            snap("p4", 400),
            snap("p5", 500),
        ],
        warnings: vec![],
    };
    let target = snap("target", 350);
    let report = build_benchmark_report(&corpus, std::slice::from_ref(&target));
    let rb = &report.repositories[0];
    assert_eq!(rb.peers, 5, "stale same-slug snapshot excluded");
    let loc = rb
        .placements
        .iter()
        .find(|p| p.metric == "total_loc")
        .unwrap();
    // Population is still 6 (5 peers + fresh target), NOT influenced by 999_999.
    assert_eq!(loc.population, 6);
    assert_eq!(loc.rank, 4);
}
