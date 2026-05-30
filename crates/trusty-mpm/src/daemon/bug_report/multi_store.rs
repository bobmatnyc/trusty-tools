//! Multi-daemon store aggregator for captured error records.
//!
//! Why: Each trusty-* daemon writes its own JSONL error store (trusty-search,
//!      trusty-memory, trusty-analyze, trusty-mpm). The MCP tools and HTTP
//!      endpoints surface errors *across all daemons*, so this module reads
//!      every known store path, merges the records in-memory, and deduplicates
//!      by fingerprint to produce a single ranked list.
//! What: [`aggregate_errors`] returns a `Vec<AggregatedError>` sorted by most
//!       recent occurrence, with occurrences counted across stores. A caller
//!       may optionally pass a limit on the result length.
//! Test: `tests::dedup_merges_by_fingerprint`,
//!       `tests::sorts_by_most_recent_timestamp`.

use std::collections::HashMap;
use std::path::PathBuf;

use trusty_common::error_capture::ErrorStore;

use super::types::AggregatedError;

/// Well-known app names whose JSONL stores we aggregate.
///
/// Why: trusty-mpm needs to surface errors from all trusty-* daemons —
///      trusty-search, trusty-memory, trusty-analyze, and itself — in a
///      unified view. Each daemon writes to `<data_dir>/<app_name>/errors.jsonl`.
/// What: static list of the four known daemon app names.
/// Test: implicit — `aggregate_errors` calls `store_path_for` for each.
const DAEMON_APP_NAMES: &[&str] = &[
    "trusty-search",
    "trusty-memory",
    "trusty-analyze",
    "trusty-mpm",
];

/// Maximum records read per store before merging.
const PER_STORE_LIMIT: usize = 200;

/// Resolve the JSONL error file path for an app name.
///
/// Why: `ErrorStore::open` uses this same resolution, but we cannot open the
///      live store (that would create files) — we need a read-only snapshot path.
/// What: calls `trusty_common::resolve_data_dir` and appends `errors.jsonl`.
///       Returns `None` when the data dir cannot be resolved.
/// Test: implicit through `aggregate_errors`.
fn store_path_for(app_name: &str) -> Option<PathBuf> {
    trusty_common::resolve_data_dir(app_name)
        .ok()
        .map(|dir| dir.join("errors.jsonl"))
}

/// Aggregate captured errors from all known daemon stores.
///
/// Why: the `list_recent_errors` MCP tool and the `preview_bug_report` tool
///      need a merged, deduplicated view across every daemon. This function is
///      the single entry point so both tools stay in sync.
/// What: reads up to [`PER_STORE_LIMIT`] records from each daemon's JSONL
///       store (using [`ErrorStore::read_records`]), merges them into a
///       `HashMap` keyed by fingerprint (most-recent record wins; occurrence
///       count accumulates), then returns the values sorted descending by
///       `timestamp_secs`. The result is truncated to `limit` entries.
/// Test: `tests::dedup_merges_by_fingerprint`,
///       `tests::sorts_by_most_recent_timestamp`.
#[must_use]
pub fn aggregate_errors(limit: usize) -> Vec<AggregatedError> {
    let mut map: HashMap<String, AggregatedError> = HashMap::new();

    for app_name in DAEMON_APP_NAMES {
        let Some(path) = store_path_for(app_name) else {
            continue;
        };
        let records = ErrorStore::read_records(&path, PER_STORE_LIMIT);
        for record in records {
            let fp = record.fingerprint.clone();
            map.entry(fp)
                .and_modify(|existing| {
                    existing.occurrences += 1;
                    if record.timestamp_secs > existing.record.timestamp_secs {
                        existing.record = record.clone();
                    }
                })
                .or_insert(AggregatedError {
                    record,
                    occurrences: 1,
                });
        }
    }

    let mut result: Vec<AggregatedError> = map.into_values().collect();
    result.sort_by_key(|b| std::cmp::Reverse(b.record.timestamp_secs));
    result.truncate(limit);
    result
}

/// Aggregate errors from explicit file paths (used in tests / custom setups).
///
/// Why: tests cannot use the OS data-dir resolution (it calls Cocoa APIs on
///      macOS and requires real directories); they need to inject arbitrary paths.
///      This variant is also useful for operators who move the data directory.
/// What: same merge/dedup/sort logic as [`aggregate_errors`] but reads from
///       the caller-supplied paths instead of the resolved daemon data dirs.
/// Test: `tests::dedup_merges_by_fingerprint` uses this variant.
#[must_use]
pub fn aggregate_errors_from_paths(paths: &[PathBuf], limit: usize) -> Vec<AggregatedError> {
    let mut map: HashMap<String, AggregatedError> = HashMap::new();

    for path in paths {
        let records = ErrorStore::read_records(path, PER_STORE_LIMIT);
        for record in records {
            let fp = record.fingerprint.clone();
            map.entry(fp)
                .and_modify(|existing| {
                    existing.occurrences += 1;
                    if record.timestamp_secs > existing.record.timestamp_secs {
                        existing.record = record.clone();
                    }
                })
                .or_insert(AggregatedError {
                    record,
                    occurrences: 1,
                });
        }
    }

    let mut result: Vec<AggregatedError> = map.into_values().collect();
    result.sort_by_key(|b| std::cmp::Reverse(b.record.timestamp_secs));
    result.truncate(limit);
    result
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use tempfile::TempDir;
    use trusty_common::error_capture::CapturedError;

    use super::*;

    fn make_record(
        message: &str,
        fingerprint: &str,
        timestamp_secs: u64,
    ) -> trusty_common::error_capture::CapturedError {
        CapturedError {
            timestamp_secs,
            crate_target: "test-crate".to_string(),
            crate_version: "0.1.0".to_string(),
            message: message.to_string(),
            fields: String::new(),
            file: Some("src/lib.rs".to_string()),
            line: Some(1),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
            fingerprint: fingerprint.to_string(),
        }
    }

    fn write_jsonl(
        dir: &TempDir,
        file: &str,
        records: &[trusty_common::error_capture::CapturedError],
    ) -> PathBuf {
        let path = dir.path().join(file);
        let mut f = std::fs::File::create(&path).unwrap();
        for r in records {
            writeln!(f, "{}", serde_json::to_string(r).unwrap()).unwrap();
        }
        path
    }

    #[test]
    fn dedup_merges_by_fingerprint() {
        let tmp = TempDir::new().unwrap();
        // Two stores, both containing the same fingerprint.
        let r1 = make_record("error A", "fp-aaaa", 1000);
        let r2 = make_record("error A again", "fp-aaaa", 1001); // newer
        let r3 = make_record("error B", "fp-bbbb", 900);

        let path1 = write_jsonl(&tmp, "store1.jsonl", &[r1]);
        let path2 = write_jsonl(&tmp, "store2.jsonl", &[r2, r3]);

        let agg = aggregate_errors_from_paths(&[path1, path2], 100);

        assert_eq!(agg.len(), 2, "two distinct fingerprints expected");

        let by_fp: std::collections::HashMap<&str, &AggregatedError> = agg
            .iter()
            .map(|e| (e.record.fingerprint.as_str(), e))
            .collect();

        // fp-aaaa: 2 occurrences, most-recent record (timestamp 1001) wins.
        let aaaa = by_fp["fp-aaaa"];
        assert_eq!(aaaa.occurrences, 2);
        assert_eq!(aaaa.record.timestamp_secs, 1001);
        assert_eq!(aaaa.record.message, "error A again");

        // fp-bbbb: 1 occurrence.
        assert_eq!(by_fp["fp-bbbb"].occurrences, 1);
    }

    #[test]
    fn sorts_by_most_recent_timestamp() {
        let tmp = TempDir::new().unwrap();
        let old = make_record("old error", "fp-old", 500);
        let new = make_record("new error", "fp-new", 2000);
        let mid = make_record("mid error", "fp-mid", 1000);

        let path = write_jsonl(&tmp, "store.jsonl", &[old, mid, new]);
        let agg = aggregate_errors_from_paths(&[path], 100);

        assert_eq!(agg.len(), 3);
        // Sorted descending by timestamp_secs.
        assert_eq!(agg[0].record.fingerprint, "fp-new");
        assert_eq!(agg[1].record.fingerprint, "fp-mid");
        assert_eq!(agg[2].record.fingerprint, "fp-old");
    }

    #[test]
    fn limit_truncates_result() {
        let tmp = TempDir::new().unwrap();
        let records: Vec<CapturedError> = (0..10)
            .map(|i| make_record(&format!("e{i}"), &format!("fp-{i:04}"), i as u64))
            .collect();
        let path = write_jsonl(&tmp, "store.jsonl", &records);
        let agg = aggregate_errors_from_paths(&[path], 3);
        assert_eq!(agg.len(), 3);
    }

    #[test]
    fn empty_stores_return_empty() {
        let tmp = TempDir::new().unwrap();
        let path = write_jsonl(&tmp, "empty.jsonl", &[]);
        let agg = aggregate_errors_from_paths(&[path], 100);
        assert!(agg.is_empty());
    }
}
