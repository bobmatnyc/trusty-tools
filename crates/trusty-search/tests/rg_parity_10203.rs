//! Hybrid `search` vs ripgrep parity on literal and identifier queries (#10203).
//!
//! Why: the #7681 owner directive is "our hybrid search should return as good
//! results as ripgrep". The #111 / #281 suites certify Stage-1 lexical-only
//! mode, and the #7675 floor tests pin a few in-process literals; nothing ran
//! default hybrid `search` against `rg` on the real corpus.
//! What: for each query in [`PARITY_QUERIES`], `rg --count --fixed-strings`
//! lists the files carrying the literal; files outside the indexer's own walk
//! universe (`walker::walk_source_files`) are set aside, not counted as
//! misses. The daemon then answers a default hybrid `search.query` with
//! `top_k = K`, where K is [`top_k_for`] of rg's matching-line count.
//!
//! **Pass bar:** every rg file in the walk universe appears among the files of
//! the top-K hybrid hits, with K = max(10, rg matching lines). A chunk carrying
//! the literal holds at least one matching line, so K is enough room for every
//! carrier when the #7675 exact-match floor ranks carriers first. Extra hybrid
//! files are allowed. A query whose rg line count exceeds [`MAX_TOP_K`], or
//! that matches no indexed file, is a stale query set and fails the test.
//!
//! Test: `hybrid_search_finds_every_rg_file_for_literal_queries` is the live
//! test. It is `#[ignore]` because it needs a sandbox daemon; run it with the
//! fixture variables from `support/socket_benchmark.rs`:
//! ```bash
//! TRUSTY_SEARCH_BENCH_SOCKET=<sandbox dir>/data/trusty-search.sock \
//! TRUSTY_SEARCH_BENCH_INDEX=<index id> \
//! TRUSTY_SEARCH_TEST_CORPUS_ROOT=<marked trusty-tools copy> \
//! cargo test -p trusty-search --test rg_parity_10203 -- --include-ignored --nocapture
//! ```
//! `TRUSTY_SEARCH_RG_BIN` names the `rg` binary; unset, `rg` is taken from
//! `PATH`. A missing `rg` fails the test; nothing is skipped. The pure
//! comparison helpers carry unit tests that run in the default suite.

// #10203: reuse the #9993 socket fixture and its corpus-root guard.
#[path = "support/isolated_benchmark.rs"]
mod isolated_benchmark;
#[path = "support/socket_benchmark.rs"]
mod socket_benchmark;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

/// Names the `rg` binary; unset means `rg` on `PATH`.
const RG_BIN_ENV: &str = "TRUSTY_SEARCH_RG_BIN";

/// Smallest `top_k` the test asks for.
const MIN_TOP_K: usize = 10;

/// Largest `top_k`; a query above it no longer fits the pass bar.
const MAX_TOP_K: usize = 100;

/// Fixed literal / identifier query set: `(query text sent to search, literal rg looks for)`.
///
/// Shapes: snake_case functions, SCREAMING_CASE constants and env vars,
/// CamelCase types, a bare filename, and quoted phrases (the floor applies to
/// an unquoted phrase only when quoted, per the #7675 contract).
const PARITY_QUERIES: &[(&str, &str)] = &[
    ("apply_archive_downrank", "apply_archive_downrank"),
    ("rel_path_in_skipped_dir", "rel_path_in_skipped_dir"),
    ("exceeds_data_cap", "exceeds_data_cap"),
    ("resolve_fastembed_cache_dir", "resolve_fastembed_cache_dir"),
    ("validate_registered_root", "validate_registered_root"),
    ("METHOD_QUERY_ALL", "METHOD_QUERY_ALL"),
    ("EXCLUDE_ARCHIVED_HINT", "EXCLUDE_ARCHIVED_HINT"),
    ("TRUSTY_MAX_INDEX_FILES", "TRUSTY_MAX_INDEX_FILES"),
    ("PrecomputedQueryVector", "PrecomputedQueryVector"),
    ("SearchDrops", "SearchDrops"),
    ("session_mcp_scope.rs", "session_mcp_scope.rs"),
    (
        "\"corpus must be a disposable copy\"",
        "corpus must be a disposable copy",
    ),
    ("\"kill-by-pid only\"", "kill-by-pid only"),
];

/// The `top_k` the pass bar grants a query with `rg_lines` matching lines.
///
/// Why: a carrier chunk holds at least one matching line, so the line count
/// bounds the number of carrier chunks the floor must fit into the top K.
/// What: `max(MIN_TOP_K, rg_lines)`; `None` above [`MAX_TOP_K`].
/// Test: `top_k_scales_with_rg_lines_within_bounds`.
fn top_k_for(rg_lines: usize) -> Option<usize> {
    (rg_lines <= MAX_TOP_K).then(|| rg_lines.max(MIN_TOP_K))
}

/// Parse `rg --count` output (`<path>:<count>` per line) into a path → count map.
///
/// What: splits on the LAST `:`, so a path holding a colon survives; strips a
/// leading `./`. A line with no parsable count is an error.
/// Test: `rg_count_output_parses_paths_and_counts`.
fn parse_rg_counts(stdout: &str) -> Result<BTreeMap<String, usize>, String> {
    let mut out = BTreeMap::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let (path, count) = line
            .rsplit_once(':')
            .ok_or_else(|| format!("rg --count line without a count: {line}"))?;
        let count: usize = count
            .trim()
            .parse()
            .map_err(|e| format!("rg --count line {line}: {e}"))?;
        out.insert(normalize_rel(path), count);
    }
    Ok(out)
}

/// A root-relative path with any leading `./` removed.
fn normalize_rel(path: &str) -> String {
    path.trim_start_matches("./").to_string()
}

/// A hit's file as a corpus-relative path.
///
/// What: reads `file`, else `path`; strips the corpus root when the path is
/// absolute under it; leaves any other path as the daemon gave it.
/// Test: `hit_paths_normalize_absolute_and_relative_forms`.
fn hit_rel_path(hit: &Value, corpus: &Path) -> Option<String> {
    let raw = hit["file"].as_str().or_else(|| hit["path"].as_str())?;
    let path = Path::new(raw);
    let rel = path
        .strip_prefix(corpus)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| raw.to_string());
    Some(normalize_rel(&rel))
}

/// The outcome of one query's comparison.
#[derive(Debug, Default, PartialEq, Eq)]
struct Parity {
    /// rg files inside the walk universe — the files hybrid must return.
    expected: BTreeSet<String>,
    /// Expected files absent from the hybrid top K.
    missed: BTreeSet<String>,
    /// rg files the indexer never walks; reported, not counted.
    unindexed: BTreeSet<String>,
}

/// Compare rg's file set with the hybrid top-K files.
///
/// Why: the pass bar is recall of rg's indexable files; extra hybrid files
/// are not a parity failure, and a file the walker excludes by design is not
/// a search miss.
/// What: splits `rg_files` into `expected` (in `universe`) and `unindexed`,
/// then `missed = expected − hybrid_files`.
/// Test: `a_missing_rg_file_is_a_miss_and_extra_hybrid_files_are_not`,
/// `rg_files_outside_the_walk_universe_are_set_aside_not_missed`.
fn compare(
    rg_files: &BTreeMap<String, usize>,
    universe: &BTreeSet<String>,
    hybrid_files: &BTreeSet<String>,
) -> Parity {
    let mut parity = Parity::default();
    for file in rg_files.keys() {
        if universe.contains(file) {
            parity.expected.insert(file.clone());
            if !hybrid_files.contains(file) {
                parity.missed.insert(file.clone());
            }
        } else {
            parity.unindexed.insert(file.clone());
        }
    }
    parity
}

/// Run `rg --count --fixed-strings` for `literal` under `root`.
///
/// What: `--no-config` keeps a user `RIPGREPRC` out. Exit 1 (no match) is an
/// empty map; any other non-zero exit, or a binary that will not start, is an
/// error naming the binary.
fn rg_counts(root: &Path, literal: &str) -> Result<BTreeMap<String, usize>, String> {
    let bin = std::env::var_os(RG_BIN_ENV).unwrap_or_else(|| "rg".into());
    let output = Command::new(&bin)
        .current_dir(root)
        .args(["--count", "--fixed-strings", "--no-config", "--", literal, "."])
        .output()
        .map_err(|e| {
            format!(
                "cannot run rg ({}); install ripgrep or set {RG_BIN_ENV}: {e}",
                bin.to_string_lossy()
            )
        })?;
    match output.status.code() {
        Some(0) => parse_rg_counts(&String::from_utf8_lossy(&output.stdout)),
        Some(1) => Ok(BTreeMap::new()),
        _ => Err(format!(
            "rg failed for {literal:?} ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )),
    }
}

/// Every file the indexer's default walk admits, corpus-relative.
fn walk_universe(root: &Path) -> BTreeSet<String> {
    trusty_search::service::walker::walk_source_files(root)
        .files
        .iter()
        .filter_map(|p| p.strip_prefix(root).ok())
        .map(|p| normalize_rel(&p.to_string_lossy()))
        .collect()
}

/// Live parity check: every rg file appears in the hybrid top K.
///
/// Why: the #7681 acceptance test (#10203).
/// What: the pass bar in the module doc, over [`PARITY_QUERIES`], printing
/// one row per query and every miss before the final assertion.
/// Test: this IS the test.
#[tokio::test]
#[ignore = "needs a sandbox trusty-search daemon (support/socket_benchmark.rs) and rg"]
async fn hybrid_search_finds_every_rg_file_for_literal_queries() {
    let fx = socket_benchmark::fixture().await;
    fx.assert_ready().await;
    let universe = walk_universe(&fx.corpus);
    assert!(
        !universe.is_empty(),
        "the walk of {} admitted no file",
        fx.corpus.display()
    );

    println!(
        "\n{:<40} {:>8} {:>5} {:>4} {:>7} {:>6}",
        "query", "rg_files", "lines", "K", "hybrid", "missed"
    );
    let mut failures = Vec::new();
    for (query, literal) in PARITY_QUERIES {
        let rg = rg_counts(&fx.corpus, literal).unwrap_or_else(|e| panic!("{e}"));
        let lines: usize = rg
            .iter()
            .filter(|(f, _)| universe.contains(*f))
            .map(|(_, n)| n)
            .sum();
        let Some(top_k) = top_k_for(lines) else {
            failures.push(format!(
                "{query}: stale query set — {lines} rg lines exceed MAX_TOP_K {MAX_TOP_K}"
            ));
            continue;
        };
        let (_, body) = fx
            .index_call(
                "search.query",
                Some(json!({ "text": query, "top_k": top_k })),
            )
            .await
            .unwrap_or_else(|e| panic!("query {query}: {e}"));
        let hybrid: BTreeSet<String> = body["results"]
            .as_array()
            .map(|hits| {
                hits.iter()
                    .filter_map(|h| hit_rel_path(h, &fx.corpus))
                    .collect()
            })
            .unwrap_or_default();
        let parity = compare(&rg, &universe, &hybrid);
        println!(
            "{:<40} {:>8} {:>5} {:>4} {:>7} {:>6}",
            query,
            parity.expected.len(),
            lines,
            top_k,
            hybrid.len(),
            parity.missed.len()
        );
        if !parity.unindexed.is_empty() {
            println!("  outside the walk universe: {:?}", parity.unindexed);
        }
        if parity.expected.is_empty() {
            failures.push(format!("{query}: stale query set — no indexed rg file"));
        } else if !parity.missed.is_empty() {
            failures.push(format!(
                "{query} (K={top_k}): rg files missing from hybrid top K: {:?}; hybrid files: {:?}",
                parity.missed, hybrid
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} queries fail hybrid-vs-rg parity:\n{}",
        failures.len(),
        PARITY_QUERIES.len(),
        failures.join("\n")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn top_k_scales_with_rg_lines_within_bounds() {
        assert_eq!(top_k_for(0), Some(MIN_TOP_K));
        assert_eq!(top_k_for(3), Some(MIN_TOP_K));
        assert_eq!(top_k_for(34), Some(34));
        assert_eq!(top_k_for(MAX_TOP_K), Some(MAX_TOP_K));
        assert_eq!(top_k_for(MAX_TOP_K + 1), None);
    }

    #[test]
    fn rg_count_output_parses_paths_and_counts() {
        let out = "./crates/a/src/lib.rs:3\n./docs/odd:name.md:1\n\n";
        let parsed = parse_rg_counts(out).expect("parses");
        assert_eq!(parsed.get("crates/a/src/lib.rs"), Some(&3));
        assert_eq!(parsed.get("docs/odd:name.md"), Some(&1));
        assert!(parse_rg_counts("no-count-here").is_err());
    }

    #[test]
    fn hit_paths_normalize_absolute_and_relative_forms() {
        let corpus = Path::new("/corpus");
        let abs = json!({ "file": "/corpus/crates/a/src/lib.rs" });
        let rel = json!({ "path": "crates/a/src/lib.rs" });
        assert_eq!(
            hit_rel_path(&abs, corpus).as_deref(),
            Some("crates/a/src/lib.rs")
        );
        assert_eq!(
            hit_rel_path(&rel, corpus).as_deref(),
            Some("crates/a/src/lib.rs")
        );
        assert_eq!(hit_rel_path(&json!({}), corpus), None);
    }

    #[test]
    fn a_missing_rg_file_is_a_miss_and_extra_hybrid_files_are_not() {
        let rg = BTreeMap::from([("a.rs".to_string(), 2), ("b.rs".to_string(), 1)]);
        let universe = set(&["a.rs", "b.rs", "c.rs"]);
        let parity = compare(&rg, &universe, &set(&["a.rs", "c.rs"]));
        assert_eq!(parity.missed, set(&["b.rs"]));
        let parity = compare(&rg, &universe, &set(&["a.rs", "b.rs", "c.rs"]));
        assert!(parity.missed.is_empty());
    }

    #[test]
    fn rg_files_outside_the_walk_universe_are_set_aside_not_missed() {
        let rg = BTreeMap::from([("a.rs".to_string(), 1), ("vendor/x.rs".to_string(), 1)]);
        let parity = compare(&rg, &set(&["a.rs"]), &set(&["a.rs"]));
        assert_eq!(parity.expected, set(&["a.rs"]));
        assert_eq!(parity.unindexed, set(&["vendor/x.rs"]));
        assert!(parity.missed.is_empty());
    }
}
