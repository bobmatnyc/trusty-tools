//! Cross-repo benchmark corpus + percentile placement (M3, epic #2312 / #2314).
//!
//! Why: M1/M2 fill and synthesise a single report; a technical-DD reader also
//! wants Appmarq-style *placement* — "this codebase sits in the 3rd quartile for
//! findings density among N peers".  M3 accumulates a corpus of per-repository
//! metrics snapshots and ranks a target against it, entirely deterministically
//! (no LLM anywhere).  The honesty rule is preserved: every ranking carries its
//! population size, and a corpus with fewer than five peers is NEVER ranked
//! against — it degrades to a visible `benchmark: corpus too small` marker.
//! What: [`CorpusSnapshot`] is the persisted per-repo record; [`corpus_dir`]
//! resolves the store location (CLI > manifest key > XDG data dir); [`load_corpus`]
//! reads snapshots skipping unreadable/mismatched ones with collected warnings;
//! [`comparable_metrics`] extracts the comparable scalar metrics; the
//! percentile/quartile/rank helpers implement the documented ranking method; and
//! [`build_benchmark_report`] assembles the per-target [`RepositoryBenchmark`]s.
//! Test: `benchmark_tests.rs` — snapshot round-trip, corrupt-skip, percentile /
//! quartile / rank edge cases (n=1, ties, target-in-population), and the small-n
//! honesty gate.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::ReportError;
use super::metrics::{AnalyzeMetrics, Severity};
use super::model::RepositoryReport;

/// The corpus snapshot schema tag.  Snapshots with any other value are skipped
/// (with a warning) by [`load_corpus`], so the format can evolve without a hard
/// failure on stale files.
pub const CORPUS_SCHEMA_VERSION: &str = "corpus-v0";

/// The minimum number of *peer* snapshots (excluding the target) required before
/// a target is ranked.  Below this the target renders the small-n honesty marker.
pub const MIN_CORPUS_PEERS: usize = 5;

// ─── Persisted snapshot ───────────────────────────────────────────────────────

/// One persisted per-repository metrics snapshot in the benchmark corpus.
///
/// Why: the corpus is just an accumulated directory of these JSON files; keeping
/// the record small and self-describing (identity + metrics + provenance) lets
/// the loader rank a target without re-running any analysis, and redacting the
/// source to a basename keeps competitor paths/URLs out of the shared corpus.
/// What: schema tag, slug/name identity, the redacted source basename, an
/// optional short git SHA, the run timestamp (injected by the caller), and the
/// v0 [`AnalyzeMetrics`] the ranking is computed from.
/// Test: `benchmark_tests.rs::snapshot_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusSnapshot {
    /// Schema tag; must equal [`CORPUS_SCHEMA_VERSION`] to be loaded.
    pub schema_version: String,
    /// Repository slug (matches the manifest entry slug).
    pub slug: String,
    /// Human-readable application name.
    pub name: String,
    /// Source origin redacted to its basename (no full path or URL) for privacy.
    pub source_basename: String,
    /// Short git HEAD SHA for local checkouts, if known.
    #[serde(default)]
    pub git_sha: Option<String>,
    /// Run timestamp/date, injected by the caller (kept out of core for testing).
    pub timestamp: String,
    /// The v0 metrics this snapshot ranks on.
    pub metrics: AnalyzeMetrics,
}

impl CorpusSnapshot {
    /// Build a snapshot from a resolved [`RepositoryReport`] and a run timestamp.
    ///
    /// Why: `--corpus-add` persists one snapshot per analyzed repo after a run;
    /// a repo with no metrics contributes nothing rankable, so it is skipped
    /// (returns `None`) rather than polluting the corpus with an empty record.
    /// What: copies identity + metrics, redacts the source to its basename, and
    /// records the git short-SHA when a local checkout supplied one.
    /// Test: `benchmark_tests.rs::from_repository_requires_metrics`.
    pub fn from_repository(repo: &RepositoryReport, timestamp: &str) -> Option<Self> {
        let metrics = repo.metrics.clone()?;
        Some(CorpusSnapshot {
            schema_version: CORPUS_SCHEMA_VERSION.to_string(),
            slug: repo.slug.clone(),
            name: repo.name.clone(),
            source_basename: basename(&repo.source),
            git_sha: repo.git_info.as_ref().map(|g| g.head_sha.clone()),
            timestamp: timestamp.to_string(),
            metrics,
        })
    }

    /// The corpus filename key: `<slug>-<sha-or-date>.json` (one per repo per run).
    ///
    /// Why: one stable key per repository means a re-run overwrites the same file
    /// rather than accumulating duplicates that would skew the population.
    /// What: uses the git SHA when present, else the timestamp; sanitises both to
    /// filename-safe characters.
    /// Test: `benchmark_tests.rs::file_key_uses_sha_then_date`.
    pub fn file_key(&self) -> String {
        let disc = self
            .git_sha
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.timestamp);
        format!("{}-{}.json", sanitize(&self.slug), sanitize(disc))
    }
}

/// The last path/URL component of a source string, for privacy-preserving keys.
fn basename(source: &str) -> String {
    let trimmed = source.trim_end_matches(['/', '\\']);
    let last = trimmed
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(trimmed)
        .trim_end_matches(".git");
    if last.is_empty() {
        "repo".to_string()
    } else {
        last.to_string()
    }
}

/// Map any non-alphanumeric run to a single `-` so a value is filename-safe.
fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let t = out.trim_matches('-').to_string();
    if t.is_empty() { "x".to_string() } else { t }
}

// ─── Corpus location + I/O ────────────────────────────────────────────────────

/// Resolve the benchmark corpus directory by precedence: CLI > manifest > XDG.
///
/// Why: operators may point at a shared corpus (`--corpus`), pin one per report
/// set (manifest `[report].corpus`), or rely on the per-user default; a single
/// resolver keeps that precedence in one place.
/// What: returns the CLI dir if given; else the manifest key resolved relative to
/// the manifest directory; else the XDG data dir
/// (`~/.local/share/trusty-review/benchmark/` or the platform equivalent).
/// Returns `None` only when no source is given AND the platform data dir is
/// unavailable — the caller turns that into a clear CLI error.
/// Test: `benchmark_tests.rs::corpus_dir_precedence`.
pub fn corpus_dir(
    cli: Option<&Path>,
    manifest_key: Option<&str>,
    manifest_dir: &Path,
) -> Option<PathBuf> {
    if let Some(dir) = cli {
        return Some(dir.to_path_buf());
    }
    if let Some(key) = manifest_key {
        let p = Path::new(key);
        return Some(if p.is_absolute() {
            p.to_path_buf()
        } else {
            manifest_dir.join(p)
        });
    }
    dirs::data_dir().map(|d| d.join("trusty-review").join("benchmark"))
}

/// Persist a snapshot into `dir` under its [`CorpusSnapshot::file_key`], atomically.
///
/// Why: `--corpus-add` writes one file per repo; an atomic temp-file + rename
/// means a concurrent loader never observes a half-written snapshot.
/// What: creates `dir`, serialises pretty JSON, writes via `tempfile` + persist,
/// and returns the written path.
/// Test: `benchmark_tests.rs::snapshot_roundtrip`.
pub fn write_snapshot(dir: &Path, snap: &CorpusSnapshot) -> Result<PathBuf, ReportError> {
    std::fs::create_dir_all(dir).map_err(|source| ReportError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let json = serde_json::to_string_pretty(snap).map_err(|source| ReportError::Metrics {
        path: PathBuf::from("<snapshot.json>"),
        source,
    })?;
    let path = dir.join(snap.file_key());
    let tmp = tempfile::NamedTempFile::new_in(dir).map_err(|source| ReportError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    std::fs::write(tmp.path(), json.as_bytes()).map_err(|source| ReportError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    tmp.persist(&path).map_err(|e| ReportError::Io {
        path: path.clone(),
        source: e.error,
    })?;
    Ok(path)
}

/// A loaded corpus: the usable snapshots plus non-fatal load warnings.
///
/// Why: a single unreadable or stale snapshot must never abort a report; the
/// loader collects a warning per skip so the reporter can surface them honestly.
/// What: `snapshots` are the successfully parsed, schema-matched records;
/// `warnings` are human-readable skip/absence notes.
/// Test: `benchmark_tests.rs::load_skips_corrupt_and_mismatched`.
#[derive(Debug, Clone, Default)]
pub struct LoadedCorpus {
    /// Successfully parsed, schema-matched snapshots.
    pub snapshots: Vec<CorpusSnapshot>,
    /// Non-fatal warnings (missing dir, unreadable file, schema mismatch).
    pub warnings: Vec<String>,
}

/// Load every `*.json` snapshot in `dir`, skipping bad ones with a warning.
///
/// Why: the corpus grows organically; the loader must be tolerant of a
/// half-migrated or hand-edited directory rather than fail the whole report.
/// What: reads each `*.json` entry; a read error, a JSON parse error, or a
/// `schema_version` other than [`CORPUS_SCHEMA_VERSION`] is collected as a
/// warning and skipped.  A missing directory yields an empty corpus + one
/// warning (not an error).  Results are sorted by file key for determinism.
/// Test: `benchmark_tests.rs::load_skips_corrupt_and_mismatched`.
pub fn load_corpus(dir: &Path) -> Result<LoadedCorpus, ReportError> {
    let mut out = LoadedCorpus::default();
    if !dir.exists() {
        out.warnings.push(format!(
            "corpus directory {} does not exist yet",
            dir.display()
        ));
        return Ok(out);
    }
    let entries = std::fs::read_dir(dir).map_err(|source| ReportError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    files.sort();

    for path in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                out.warnings
                    .push(format!("skipped {}: unreadable ({e})", path.display()));
                continue;
            }
        };
        match serde_json::from_str::<CorpusSnapshot>(&text) {
            Ok(snap) if snap.schema_version == CORPUS_SCHEMA_VERSION => out.snapshots.push(snap),
            Ok(snap) => out.warnings.push(format!(
                "skipped {}: schema_version '{}' != '{}'",
                path.display(),
                snap.schema_version,
                CORPUS_SCHEMA_VERSION
            )),
            Err(e) => out
                .warnings
                .push(format!("skipped {}: parse error ({e})", path.display())),
        }
    }
    Ok(out)
}

// ─── Comparable metrics + ranking ─────────────────────────────────────────────

/// Extract the comparable scalar metrics from a v0 metrics document.
///
/// Why: benchmarking ranks a fixed, documented set of size/quality scalars; a
/// metric whose inputs are absent (no LoC → no density; no complexity buckets →
/// no high-share) is simply omitted so it is excluded from that metric's
/// population rather than ranked as a spurious zero.
/// What: returns `(key, value)` pairs in a stable order — total LoC, file count,
/// function count, findings/1k-LoC (total/RED/AMBER), and the share (%) of
/// functions in the highest complexity bucket (the last bucket, by convention).
/// Test: `benchmark_tests.rs::comparable_metrics_extraction`.
pub fn comparable_metrics(m: &AnalyzeMetrics) -> Vec<(&'static str, f64)> {
    let mut out = Vec::new();
    if m.loc.total > 0 {
        out.push(("total_loc", m.loc.total as f64));
    }
    if m.counts.files > 0 {
        out.push(("file_count", m.counts.files as f64));
    }
    if m.counts.functions > 0 {
        out.push(("function_count", m.counts.functions as f64));
    }
    if m.loc.total > 0 {
        let per_k = 1000.0 / m.loc.total as f64;
        let count_sev = |s: Severity| m.findings.iter().filter(|f| f.severity == s).count() as f64;
        out.push(("findings_density_total", m.findings.len() as f64 * per_k));
        out.push(("findings_density_red", count_sev(Severity::Red) * per_k));
        out.push(("findings_density_amber", count_sev(Severity::Amber) * per_k));
    }
    let total_funcs: u64 = m.complexity.buckets.iter().map(|b| b.count).sum();
    if total_funcs > 0
        && let Some(highest) = m.complexity.buckets.last()
    {
        out.push((
            "complexity_high_share",
            highest.count as f64 / total_funcs as f64 * 100.0,
        ));
    }
    out
}

/// The human-readable label for a comparable-metric key.
///
/// Why: the rendered benchmark table shows a readable criterion name, not the
/// internal key.
/// What: maps each key from [`comparable_metrics`] to its label; an unknown key
/// falls back to the key itself.
/// Test: exercised via `benchmark_tests.rs` rendering assertions.
pub fn metric_label(key: &str) -> &'static str {
    match key {
        "total_loc" => "Total LoC",
        "file_count" => "File count",
        "function_count" => "Function count",
        "findings_density_total" => "Findings / 1k LoC",
        "findings_density_red" => "RED findings / 1k LoC",
        "findings_density_amber" => "AMBER findings / 1k LoC",
        "complexity_high_share" => "High-complexity function share (%)",
        _ => "metric",
    }
}

/// The percentile rank of `value` within `population` (which must include `value`).
///
/// Why: placement needs a single, documented, tie-stable percentile.
/// What: the cumulative-share method — `100 * (count of values <= value) /
/// population_len`.  Because the population includes the target's own value, a
/// unique maximum yields `100.0`, the sole member of an `n=1` population yields
/// `100.0`, and tied values share the same percentile (all are counted by the
/// `<=` test).  Returns `0.0` for an empty population (never ranked in practice).
/// Test: `benchmark_tests.rs::percentile_edges`.
pub fn percentile_rank(value: f64, population: &[f64]) -> f64 {
    if population.is_empty() {
        return 0.0;
    }
    let at_or_below = population.iter().filter(|&&v| v <= value).count();
    100.0 * at_or_below as f64 / population.len() as f64
}

/// Map a percentile (0..=100) to a 1st–4th quartile.
///
/// Why: the benchmark table expresses placement as a quartile; this pins the
/// boundary convention so it is testable and consistent.
/// What: `<=25 → 1`, `<=50 → 2`, `<=75 → 3`, else `4`.  Q1 is the bottom quartile
/// of the population by this metric; Q4 is the top quartile.
/// Test: `benchmark_tests.rs::quartile_boundaries`.
pub fn quartile_of(percentile: f64) -> u8 {
    if percentile <= 25.0 {
        1
    } else if percentile <= 50.0 {
        2
    } else if percentile <= 75.0 {
        3
    } else {
        4
    }
}

/// The 1-based ascending rank of `value` within `population` (1 = smallest).
///
/// Why: readers expect a "rank r of n" alongside the percentile.
/// What: `1 + count(values strictly < value)`; tied values share a rank.
/// Test: `benchmark_tests.rs::ascending_rank_ties`.
pub fn ascending_rank(value: f64, population: &[f64]) -> usize {
    1 + population.iter().filter(|&&v| v < value).count()
}

// ─── Assembled benchmark report ───────────────────────────────────────────────

/// One target repository's placement of a single comparable metric.
#[derive(Debug, Clone, Serialize)]
pub struct MetricPlacement {
    /// Internal metric key (see [`comparable_metrics`]).
    pub metric: String,
    /// The target's value for this metric.
    pub target_value: f64,
    /// Percentile rank within the population (0..=100).
    pub percentile: f64,
    /// Quartile 1..=4 derived from the percentile.
    pub quartile: u8,
    /// 1-based ascending rank (1 = smallest value).
    pub rank: usize,
    /// Population size used for THIS metric (target + peers reporting it).
    pub population: usize,
}

/// Whether a target was ranked, or held back by the small-corpus honesty gate.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", content = "peers", rename_all = "snake_case")]
pub enum BenchmarkStatus {
    /// The target was ranked against the corpus.
    Ranked,
    /// Fewer than [`MIN_CORPUS_PEERS`] peers — not ranked; carries the peer count.
    CorpusTooSmall(usize),
}

/// A target repository's benchmark outcome across all comparable metrics.
#[derive(Debug, Clone, Serialize)]
pub struct RepositoryBenchmark {
    /// Repository slug.
    pub slug: String,
    /// Human-readable application name.
    pub name: String,
    /// Ranked or held back (small corpus).
    pub status: BenchmarkStatus,
    /// Per-metric placements (empty when held back).
    pub placements: Vec<MetricPlacement>,
    /// Number of peer snapshots (excluding the target) in the corpus.
    pub peers: usize,
}

/// The full benchmark result recorded on the report model's JSON twin.
#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkReport {
    /// One entry per target repository (in target order).
    pub repositories: Vec<RepositoryBenchmark>,
    /// Corpus load warnings (missing dir, skipped snapshots).
    pub warnings: Vec<String>,
    /// Total usable snapshots loaded from the corpus.
    pub corpus_size: usize,
}

/// Rank every target against the loaded corpus, honouring the small-n gate.
///
/// Why: the single entry point the reporter calls to turn a corpus + targets
/// into render-ready placements, with the honesty gate applied uniformly.
/// What: for each target, counts distinct peers (corpus snapshots whose slug
/// differs from the target); if peers `< MIN_CORPUS_PEERS` the target is
/// `CorpusTooSmall` and unranked.  Otherwise each comparable metric is ranked
/// against a population of the target's value plus every peer reporting that
/// metric.  The target's own value is ALWAYS included in the population (the
/// documented choice), so placement reflects where the target sits within the
/// full set, and a stale corpus copy of the target is excluded to avoid a
/// double-count.
/// Test: `benchmark_tests.rs::{small_corpus_gate, ranks_target_in_population}`.
pub fn build_benchmark_report(
    corpus: &LoadedCorpus,
    targets: &[CorpusSnapshot],
) -> BenchmarkReport {
    let mut repositories = Vec::with_capacity(targets.len());

    for target in targets {
        let peers: Vec<&CorpusSnapshot> = corpus
            .snapshots
            .iter()
            .filter(|s| s.slug != target.slug)
            .collect();
        let peer_count = peers.len();

        if peer_count < MIN_CORPUS_PEERS {
            repositories.push(RepositoryBenchmark {
                slug: target.slug.clone(),
                name: target.name.clone(),
                status: BenchmarkStatus::CorpusTooSmall(peer_count),
                placements: Vec::new(),
                peers: peer_count,
            });
            continue;
        }

        let placements = comparable_metrics(&target.metrics)
            .into_iter()
            .map(|(key, value)| {
                let mut population: Vec<f64> = peers
                    .iter()
                    .filter_map(|s| metric_value(&s.metrics, key))
                    .collect();
                population.push(value);
                MetricPlacement {
                    metric: key.to_string(),
                    target_value: value,
                    percentile: percentile_rank(value, &population),
                    quartile: quartile_of(percentile_rank(value, &population)),
                    rank: ascending_rank(value, &population),
                    population: population.len(),
                }
            })
            .collect();

        repositories.push(RepositoryBenchmark {
            slug: target.slug.clone(),
            name: target.name.clone(),
            status: BenchmarkStatus::Ranked,
            placements,
            peers: peer_count,
        });
    }

    BenchmarkReport {
        repositories,
        warnings: corpus.warnings.clone(),
        corpus_size: corpus.snapshots.len(),
    }
}

/// The value of a single comparable metric `key` for one snapshot's metrics.
fn metric_value(m: &AnalyzeMetrics, key: &str) -> Option<f64> {
    comparable_metrics(m)
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

#[cfg(test)]
#[path = "benchmark_tests.rs"]
mod tests;
