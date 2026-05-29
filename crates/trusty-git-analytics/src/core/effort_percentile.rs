//! Corpus-percentile effort binning (#445 batch C).
//!
//! ## Design
//!
//! After `fact_commit_effort` scores are computed, this module:
//!
//! 1. Reads all `score` values from the table.
//! 2. Computes p20/p40/p60/p80 breakpoints using the empirical distribution.
//! 3. Persists the breakpoints in `effort_percentile_thresholds` so
//!    incremental ingestion can bin new commits without a full corpus re-scan.
//! 4. Assigns `effort_tshirt` 1–5 by which quintile each score falls in.
//!
//! ## Tiny-corpus fallback
//!
//! When the corpus has fewer than 5 rows (fewer rows than quintile bands),
//! meaningful percentile boundaries cannot be computed. In that case the
//! function falls back to the static `effort_tshirt_from_size` mapping so
//! the column is never NULL. The fallback is logged at `WARN` level.
//!
//! ## Note on label vs. percentile divergence
//!
//! The `size` TEXT column (XS/S/M/L/XL) continues to use absolute score
//! thresholds (≤6, ≤10, ≤14, ≤18, >18) calibrated against the trusty-tools
//! corpus. The `effort_tshirt` INTEGER is now percentile-based and is
//! intentionally allowed to diverge from the label: a corpus with very large
//! commits everywhere will yield `size = "XL"` with `effort_tshirt = 1` for
//! the smallest XL commits. This is by design — the integer encodes relative
//! standing, not the same absolute band as the label.

use rusqlite::{params, Connection};

use crate::core::effort::effort_tshirt_from_size;
use crate::core::errors::{Result, TgaError};

/// The minimum number of rows required to compute meaningful percentile
/// thresholds. Below this count, the static mapping is used instead.
const MIN_CORPUS_SIZE: usize = 5;

/// The name of the default dataset used in `effort_percentile_thresholds`.
const DEFAULT_DATASET: &str = "default";

/// Percentile breakpoints for effort binning.
///
/// Why: persisted so incremental commits can bin against the last-known corpus
/// distribution without re-scanning the whole table.
/// What: p20/p40/p60/p80 of `fact_commit_effort.score` plus a sample count
/// and the Unix-epoch timestamp when the thresholds were computed.
/// Test: `tests::percentile_thresholds_round_trip`.
#[derive(Debug, Clone, PartialEq)]
pub struct EffortPercentileThresholds {
    /// Score at the 20th percentile (bottom of band 2).
    pub p20: f64,
    /// Score at the 40th percentile (bottom of band 3).
    pub p40: f64,
    /// Score at the 60th percentile (bottom of band 4).
    pub p60: f64,
    /// Score at the 80th percentile (bottom of band 5).
    pub p80: f64,
    /// Number of rows used to compute the thresholds.
    pub sample_count: usize,
}

impl EffortPercentileThresholds {
    /// Assign an `effort_tshirt` value (1–5) for a given raw score.
    ///
    /// Why: centralises the percentile-band decision so it can be used at
    /// backfill time and during incremental ingestion.
    /// What: returns the quintile band (1 = bottom 20 %, 5 = top 20 %).
    /// Test: `tests::band_assignment_uses_stored_thresholds`.
    pub fn band_for_score(&self, score: f64) -> i64 {
        if score < self.p20 {
            1
        } else if score < self.p40 {
            2
        } else if score < self.p60 {
            3
        } else if score < self.p80 {
            4
        } else {
            5
        }
    }
}

/// Load the stored percentile thresholds from `effort_percentile_thresholds`.
///
/// Why: incremental commit ingestion needs the thresholds without re-scanning
/// the full corpus every time.
/// What: queries `effort_percentile_thresholds WHERE dataset = 'default'`;
/// returns `None` if no row exists yet (first run before any backfill).
/// Test: `tests::percentile_thresholds_round_trip`.
///
/// # Errors
///
/// Returns [`TgaError`] on SQL failures.
pub fn load_thresholds(conn: &Connection) -> Result<Option<EffortPercentileThresholds>> {
    let result = conn.query_row(
        "SELECT p20, p40, p60, p80, sample_count \
         FROM effort_percentile_thresholds \
         WHERE dataset = ?1",
        params![DEFAULT_DATASET],
        |row| {
            Ok(EffortPercentileThresholds {
                p20: row.get(0)?,
                p40: row.get(1)?,
                p60: row.get(2)?,
                p80: row.get(3)?,
                sample_count: row.get::<_, i64>(4)? as usize,
            })
        },
    );

    match result {
        Ok(t) => Ok(Some(t)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(TgaError::from(e)),
    }
}

/// Persist percentile thresholds to `effort_percentile_thresholds`.
///
/// Why: after computing the corpus-wide percentile breakpoints, we store them
/// so incremental ingestion can use them without a full re-scan.
/// What: upserts the `'default'` dataset row with the supplied thresholds and
/// the current Unix epoch timestamp.
/// Test: `tests::percentile_thresholds_round_trip`.
///
/// # Errors
///
/// Returns [`TgaError`] on SQL failures.
pub fn persist_thresholds(
    conn: &Connection,
    thresholds: &EffortPercentileThresholds,
) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    conn.execute(
        "INSERT OR REPLACE INTO effort_percentile_thresholds \
         (dataset, p20, p40, p60, p80, sample_count, computed_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            DEFAULT_DATASET,
            thresholds.p20,
            thresholds.p40,
            thresholds.p60,
            thresholds.p80,
            thresholds.sample_count as i64,
            now,
        ],
    )
    .map_err(TgaError::from)?;
    Ok(())
}

/// Compute percentile breakpoints from a slice of scores.
///
/// Why: extracts the pure mathematical computation from the database layer
/// so it is easily unit-tested with synthetic data.
/// What: sorts the scores and uses nearest-rank interpolation for p20/p40/
/// p60/p80. Returns `None` for corpora smaller than [`MIN_CORPUS_SIZE`].
/// Test: `tests::compute_percentiles_known_distribution`.
pub fn compute_percentiles(scores: &[f64]) -> Option<EffortPercentileThresholds> {
    if scores.len() < MIN_CORPUS_SIZE {
        return None;
    }

    let mut sorted = scores.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();

    // Nearest-rank method: index = ceil(p/100 * n) - 1, clamped to [0, n-1].
    let percentile_value = |p: f64| -> f64 {
        let rank = ((p / 100.0) * n as f64).ceil() as usize;
        let idx = rank.saturating_sub(1).min(n - 1);
        sorted[idx]
    };

    Some(EffortPercentileThresholds {
        p20: percentile_value(20.0),
        p40: percentile_value(40.0),
        p60: percentile_value(60.0),
        p80: percentile_value(80.0),
        sample_count: n,
    })
}

/// Compute and persist percentile thresholds, then rebin all rows in
/// `fact_commit_effort` by updating `effort_tshirt`.
///
/// Why: the `tga backfill effort-tshirt` command uses this after scores are
/// known to replace the static mapping with corpus-relative quintile bins.
/// What: reads all scores, computes breakpoints, persists them, then batch-
/// updates every row in `fact_commit_effort`. Falls back to static mapping
/// when the corpus is too small (< 5 rows).
///
/// Returns `(rows_updated, thresholds_or_none_if_fallback)`.
///
/// Test: `tests::rebin_assigns_quintiles` and `tests::rebin_tiny_corpus_fallback`.
///
/// # Errors
///
/// Returns [`TgaError`] on SQL or transaction failures.
pub fn rebin_all(conn: &mut Connection) -> Result<(usize, Option<EffortPercentileThresholds>)> {
    // Read all (sha, repository, score, size) rows.
    let rows: Vec<(String, String, f64, String)> = {
        let mut stmt = conn
            .prepare("SELECT sha, repository, score, size FROM fact_commit_effort")
            .map_err(TgaError::from)?;
        let iter = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(TgaError::from)?;
        let mut v = Vec::new();
        for r in iter {
            v.push(r.map_err(TgaError::from)?);
        }
        v
    };

    let scores: Vec<f64> = rows.iter().map(|(_, _, s, _)| *s).collect();
    let thresholds = compute_percentiles(&scores);

    if thresholds.is_none() {
        tracing::warn!(
            count = rows.len(),
            min_required = MIN_CORPUS_SIZE,
            "effort percentile: corpus too small for percentile binning; \
             falling back to static size-label mapping"
        );
    }

    // Persist thresholds if we have them.
    if let Some(ref t) = thresholds {
        persist_thresholds(conn, t)?;
    }

    // Assign effort_tshirt for each row.
    let updates: Vec<(i64, String, String)> = rows
        .iter()
        .map(|(sha, repo, score, size)| {
            let tshirt = match &thresholds {
                Some(t) => t.band_for_score(*score),
                None => effort_tshirt_from_size(size),
            };
            (tshirt, sha.clone(), repo.clone())
        })
        .collect();

    // Batch update in a single transaction.
    let tx = conn.transaction().map_err(TgaError::from)?;
    {
        let mut stmt = tx
            .prepare(
                "UPDATE fact_commit_effort SET effort_tshirt = ?1 \
                 WHERE sha = ?2 AND repository = ?3",
            )
            .map_err(TgaError::from)?;
        for (tshirt, sha, repo) in &updates {
            stmt.execute(params![tshirt, sha, repo])
                .map_err(TgaError::from)?;
        }
    }
    tx.commit().map_err(TgaError::from)?;

    Ok((updates.len(), thresholds))
}

/// Assign an `effort_tshirt` value for a single commit score, using the
/// stored corpus thresholds when available or falling back to static mapping.
///
/// Why: incremental commit ingestion (after a full backfill) should bin new
/// commits against the stored corpus distribution rather than re-scanning the
/// full table.
/// What: loads stored thresholds via [`load_thresholds`] and calls
/// [`EffortPercentileThresholds::band_for_score`]; if no thresholds are
/// stored yet, falls back to [`effort_tshirt_from_size`] using the `size` label.
/// Test: `tests::incremental_bins_against_stored_thresholds`.
///
/// # Errors
///
/// Returns [`TgaError`] if the threshold query fails.
pub fn tshirt_for_score_incremental(
    conn: &Connection,
    score: f64,
    size_label: &str,
) -> Result<i64> {
    match load_thresholds(conn)? {
        Some(t) => Ok(t.band_for_score(score)),
        None => Ok(effort_tshirt_from_size(size_label)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::Database;
    use rusqlite::params;

    /// Helper: insert a row into fact_commit_effort with the given score and size.
    fn insert_effort_row(conn: &Connection, sha: &str, repo: &str, score: f64, size: &str) {
        conn.execute(
            "INSERT OR REPLACE INTO fact_commit_effort \
             (sha, repository, size, score, loc, files, test_loc, tests_factor, \
              formula_version, computed_at, effort_tshirt) \
             VALUES (?1, ?2, ?3, ?4, 10, 1, 0, 1.0, 'v1', 0, 0)",
            params![sha, repo, size, score],
        )
        .expect("insert effort row");
    }

    /// Why: verify that the pure percentile computation is correct for a
    /// known ten-element distribution.
    /// What: [1,2,3,4,5,6,7,8,9,10] → p20=2, p40=4, p60=6, p80=8.
    /// Test: this test itself.
    #[test]
    fn compute_percentiles_known_distribution() {
        let scores: Vec<f64> = (1..=10).map(|v| v as f64).collect();
        let t = compute_percentiles(&scores).expect("thresholds computed");
        // Nearest-rank: p20 = ceil(0.2*10)=2 → index 1 → value 2.0
        assert!((t.p20 - 2.0).abs() < 1e-9, "p20 expected 2.0 got {}", t.p20);
        // p40 = ceil(0.4*10)=4 → index 3 → value 4.0
        assert!((t.p40 - 4.0).abs() < 1e-9, "p40 expected 4.0 got {}", t.p40);
        // p60 = ceil(0.6*10)=6 → index 5 → value 6.0
        assert!((t.p60 - 6.0).abs() < 1e-9, "p60 expected 6.0 got {}", t.p60);
        // p80 = ceil(0.8*10)=8 → index 7 → value 8.0
        assert!((t.p80 - 8.0).abs() < 1e-9, "p80 expected 8.0 got {}", t.p80);
        assert_eq!(t.sample_count, 10);
    }

    /// Why: a corpus smaller than MIN_CORPUS_SIZE must not panic and must
    /// return None (triggering the static-fallback path).
    /// What: pass 3 scores (<5) and assert None.
    /// Test: this test itself.
    #[test]
    fn compute_percentiles_tiny_corpus_returns_none() {
        let scores = vec![1.0_f64, 2.0, 3.0];
        let result = compute_percentiles(&scores);
        assert!(result.is_none(), "tiny corpus must return None, not panic");
    }

    /// Why: ensure the band assignment uses stored thresholds, not the static
    /// label mapping.
    /// What: with p20=5, p40=10, p60=15, p80=20 a score of 7 should be band 2.
    /// Test: this test itself.
    #[test]
    fn band_assignment_uses_stored_thresholds() {
        let t = EffortPercentileThresholds {
            p20: 5.0,
            p40: 10.0,
            p60: 15.0,
            p80: 20.0,
            sample_count: 100,
        };
        assert_eq!(t.band_for_score(0.0), 1, "score below p20 → band 1");
        assert_eq!(t.band_for_score(4.9), 1);
        assert_eq!(t.band_for_score(5.0), 2, "score at p20 → band 2");
        assert_eq!(t.band_for_score(9.9), 2);
        assert_eq!(t.band_for_score(10.0), 3, "score at p40 → band 3");
        assert_eq!(t.band_for_score(14.9), 3);
        assert_eq!(t.band_for_score(15.0), 4, "score at p60 → band 4");
        assert_eq!(t.band_for_score(19.9), 4);
        assert_eq!(t.band_for_score(20.0), 5, "score at p80 → band 5");
        assert_eq!(t.band_for_score(999.0), 5);
    }

    /// Why: end-to-end round-trip: persist thresholds, load them back, and
    /// verify the values are unchanged.
    /// What: opens in-memory DB (all migrations), persists test thresholds,
    /// loads them back, and asserts each field.
    /// Test: this test itself.
    #[test]
    fn percentile_thresholds_round_trip() {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();

        let t = EffortPercentileThresholds {
            p20: 3.5,
            p40: 7.0,
            p60: 11.5,
            p80: 17.25,
            sample_count: 42,
        };
        persist_thresholds(conn, &t).expect("persist");

        let loaded = load_thresholds(conn).expect("load").expect("must be Some");
        assert!((loaded.p20 - t.p20).abs() < 1e-9, "p20 round-trip");
        assert!((loaded.p40 - t.p40).abs() < 1e-9, "p40 round-trip");
        assert!((loaded.p60 - t.p60).abs() < 1e-9, "p60 round-trip");
        assert!((loaded.p80 - t.p80).abs() < 1e-9, "p80 round-trip");
        assert_eq!(loaded.sample_count, t.sample_count);
    }

    /// Why: `rebin_all` must compute correct quintile bands and persist thresholds.
    /// What: insert 10 rows with scores 1–10, run rebin_all, assert effort_tshirt.
    /// Scores 1–2 → band 1, 3–4 → band 2, 5–6 → band 3, 7–8 → band 4, 9–10 → band 5.
    /// Test: this test itself.
    #[test]
    fn rebin_assigns_quintiles() {
        let mut db = Database::open_in_memory().expect("open db");

        // Insert 10 effort rows with scores 1.0 to 10.0.
        {
            let conn = db.connection();
            for i in 1..=10u32 {
                let sha = format!("sha{i:03}");
                insert_effort_row(conn, &sha, "repo", i as f64, "M");
            }
        }

        let (updated, thresholds) = rebin_all(db.connection_mut()).expect("rebin");
        assert_eq!(updated, 10, "all 10 rows must be rebinned");
        let t = thresholds.expect("thresholds computed for 10-row corpus");

        // Verify stored thresholds (nearest-rank on 1..=10).
        assert!((t.p20 - 2.0).abs() < 1e-9, "p20");
        assert!((t.p80 - 8.0).abs() < 1e-9, "p80");

        // Verify that effort_tshirt was updated correctly.
        let conn = db.connection();
        let bands: Vec<(i64, i64)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT CAST(score AS INTEGER), effort_tshirt \
                     FROM fact_commit_effort \
                     ORDER BY score ASC",
                )
                .expect("prepare");
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .expect("query")
                .map(|r| r.expect("row"))
                .collect()
        };
        // Scores 1,2 → p20=2 → band 1; score 2 is AT p20, so band 2.
        // Nearest-rank p20 of [1..10] = sorted[ceil(2)-1] = sorted[1] = 2.
        // band_for_score: score < p20 → 1; score >= p20 → 2.
        // score=1 < 2 → 1; score=2 >= 2 → 2; score=3 >= 2 and < 4 → 2; etc.
        assert_eq!(bands[0], (1, 1), "score=1 → band 1");
        assert_eq!(bands[1], (2, 2), "score=2 → band 2 (at p20)");
        assert_eq!(bands[9], (10, 5), "score=10 → band 5");
    }

    /// Why: a corpus smaller than 5 rows must not panic; rebin_all must use the
    /// static mapping and return None for thresholds.
    /// What: insert 3 rows (all "M" = 3), run rebin_all, assert no panic and
    /// all rows get effort_tshirt=3 (static M=3 mapping).
    /// Test: this test itself.
    #[test]
    fn rebin_tiny_corpus_fallback() {
        let mut db = Database::open_in_memory().expect("open db");

        {
            let conn = db.connection();
            for i in 1..=3u32 {
                insert_effort_row(conn, &format!("sha{i}"), "repo", i as f64, "M");
            }
        }

        let (updated, thresholds) = rebin_all(db.connection_mut()).expect("rebin tiny");
        assert_eq!(updated, 3);
        assert!(
            thresholds.is_none(),
            "tiny corpus must yield None thresholds"
        );

        // All rows should have effort_tshirt=3 (static M → 3).
        let conn = db.connection();
        let tshirts: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT effort_tshirt FROM fact_commit_effort")
                .expect("prepare");
            stmt.query_map([], |r| r.get(0))
                .expect("query")
                .map(|r| r.expect("row"))
                .collect()
        };
        assert!(
            tshirts.iter().all(|&v| v == 3),
            "all rows should be effort_tshirt=3 (static M mapping), got {tshirts:?}"
        );
    }

    /// Why: incremental ingestion bins a new score against stored thresholds.
    /// What: persist thresholds with p20=5, then call tshirt_for_score_incremental
    /// with score=4 → band 1; score=6 → band 2.
    /// Test: this test itself.
    #[test]
    fn incremental_bins_against_stored_thresholds() {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();

        let t = EffortPercentileThresholds {
            p20: 5.0,
            p40: 10.0,
            p60: 15.0,
            p80: 20.0,
            sample_count: 50,
        };
        persist_thresholds(conn, &t).expect("persist");

        let band1 = tshirt_for_score_incremental(conn, 4.9, "XS").expect("band1");
        let band2 = tshirt_for_score_incremental(conn, 5.0, "S").expect("band2");
        let band5 = tshirt_for_score_incremental(conn, 25.0, "XL").expect("band5");

        assert_eq!(band1, 1, "score < p20 → band 1");
        assert_eq!(band2, 2, "score at p20 → band 2");
        assert_eq!(band5, 5, "score >= p80 → band 5");
    }

    /// Why: when no thresholds are stored, incremental ingestion must fall back
    /// to the static label mapping without panicking.
    /// What: empty DB (no stored thresholds), call tshirt_for_score_incremental
    /// with "M" → 3.
    /// Test: this test itself.
    #[test]
    fn incremental_fallback_when_no_stored_thresholds() {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();
        // No thresholds stored.
        let result = tshirt_for_score_incremental(conn, 12.0, "M").expect("fallback");
        assert_eq!(result, 3, "static M → 3");
    }
}
