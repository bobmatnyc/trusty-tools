//! Latency percentiles and throughput for the baseline suite (#9993 G2).
//!
//! Why: the suite printed p50/p99 from a floor-index helper that returned `0`
//! on an empty slice and reported sample 51 as the median of 1..=100. The
//! perf baseline needs p50/p95/p99, the sample count and qps per operation,
//! with an empty or zero-length measurement refused rather than reported.
//! What: [`nearest_rank`] and [`latency_stats`]. Percentile method:
//! **nearest-rank** — the p-th percentile of `n` ascending samples is the
//! sample at 1-based rank `ceil(p / 100 * n)`. It always returns an observed
//! sample, never an interpolated one. The shared method (2026-10-17) names no
//! percentile definition, so this one is stated here and in the envelope notes.
//! Test: `nearest_rank_over_one_to_one_hundred_is_exact`,
//! `latency_stats_reports_count_percentiles_and_qps`,
//! `empty_samples_and_zero_windows_are_errors`.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::fmt;
use std::time::Duration;

/// Why a set of samples could not be summarized.
#[derive(Debug, Clone, PartialEq)]
pub enum StatsError {
    /// No samples were taken.
    NoSamples,
    /// The timing window was zero seconds, so qps is undefined.
    ZeroWindow,
    /// A percentile outside `(0, 100]` was requested.
    PercentileOutOfRange(f64),
}

impl fmt::Display for StatsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSamples => write!(f, "no latency samples to summarize"),
            Self::ZeroWindow => write!(f, "zero-length timing window; qps is undefined"),
            Self::PercentileOutOfRange(p) => write!(f, "percentile {p} is outside (0, 100]"),
        }
    }
}

impl std::error::Error for StatsError {}

/// One operation's summary: count, nearest-rank percentiles in ms, and qps.
#[derive(Debug, Clone, PartialEq)]
pub struct LatencyStats {
    pub n: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub qps: f64,
}

/// The nearest-rank `p`-th percentile of an ascending slice.
///
/// What: returns `sorted[ceil(p / 100 * n) - 1]`. Precondition: `sorted` is
/// ascending (checked in debug builds). Postcondition: the result is an
/// element of `sorted`.
/// Test: `nearest_rank_over_one_to_one_hundred_is_exact`.
pub fn nearest_rank(sorted: &[f64], p: f64) -> Result<f64, StatsError> {
    if p.is_nan() || p <= 0.0 || p > 100.0 {
        return Err(StatsError::PercentileOutOfRange(p));
    }
    if sorted.is_empty() {
        return Err(StatsError::NoSamples);
    }
    debug_assert!(sorted.windows(2).all(|w| w[0] <= w[1]), "unsorted input");
    let n = sorted.len();
    // p * n first: exact for integral inputs, so 95 of 100 is rank 95, not 96.
    let rank = (p * n as f64 / 100.0).ceil() as usize;
    Ok(sorted[rank.clamp(1, n) - 1])
}

/// Summarize `samples` measured over `window`.
///
/// Why: one helper for every operation, so each envelope row uses the same
/// method.
/// What: sorts the samples as milliseconds, takes nearest-rank p50/p95/p99
/// and the max, and computes `qps = n / window_seconds`. The caller picks the
/// window: the wall time the samples ran in for a concurrent batch, or the
/// sum of the samples for a serial loop (the time between calls is untimed
/// guard work).
/// Test: `latency_stats_reports_count_percentiles_and_qps`,
/// `empty_samples_and_zero_windows_are_errors`.
pub fn latency_stats(samples: &[Duration], window: Duration) -> Result<LatencyStats, StatsError> {
    if samples.is_empty() {
        return Err(StatsError::NoSamples);
    }
    if window.is_zero() {
        return Err(StatsError::ZeroWindow);
    }
    let mut ms: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1e3).collect();
    ms.sort_by(f64::total_cmp);
    let max_ms = ms[ms.len() - 1];
    Ok(LatencyStats {
        n: ms.len(),
        p50_ms: nearest_rank(&ms, 50.0)?,
        p95_ms: nearest_rank(&ms, 95.0)?,
        p99_ms: nearest_rank(&ms, 99.0)?,
        max_ms,
        qps: ms.len() as f64 / window.as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_over_one_to_one_hundred_is_exact() {
        let sorted: Vec<f64> = (1..=100).map(f64::from).collect();
        // A floor(n * p / 100) index returns 51, 96 and 100 here.
        assert_eq!(nearest_rank(&sorted, 50.0), Ok(50.0));
        assert_eq!(nearest_rank(&sorted, 95.0), Ok(95.0));
        assert_eq!(nearest_rank(&sorted, 99.0), Ok(99.0));
        assert_eq!(nearest_rank(&sorted, 100.0), Ok(100.0));
        assert_eq!(nearest_rank(&sorted, 0.5), Ok(1.0));
        assert_eq!(
            nearest_rank(&sorted, 0.0),
            Err(StatsError::PercentileOutOfRange(0.0))
        );
        assert_eq!(
            nearest_rank(&sorted, 100.5),
            Err(StatsError::PercentileOutOfRange(100.5))
        );
    }

    #[test]
    fn latency_stats_reports_count_percentiles_and_qps() {
        // Reversed input: the helper must sort.
        let samples: Vec<Duration> = (1..=100).rev().map(Duration::from_millis).collect();
        let stats = latency_stats(&samples, Duration::from_secs(4)).expect("summary");
        assert_eq!(
            stats,
            LatencyStats {
                n: 100,
                p50_ms: 50.0,
                p95_ms: 95.0,
                p99_ms: 99.0,
                max_ms: 100.0,
                qps: 25.0,
            }
        );
    }

    #[test]
    fn empty_samples_and_zero_windows_are_errors() {
        assert_eq!(
            latency_stats(&[], Duration::from_secs(1)),
            Err(StatsError::NoSamples)
        );
        assert_eq!(
            latency_stats(&[Duration::from_millis(3)], Duration::ZERO),
            Err(StatsError::ZeroWindow)
        );
        assert_eq!(nearest_rank(&[], 50.0), Err(StatsError::NoSamples));
    }
}
