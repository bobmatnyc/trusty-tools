//! Fail-closed size guardrail for the reindex walk (issue #4356).
//!
//! Why: registering an index against an arbitrary directory used to commit the
//! daemon to whatever that directory held. A 50 GB / 109k-file checkout would
//! be walked, parsed, and embedded until the `TRUSTY_MAX_CHUNKS` cap silently
//! TRUNCATED it — leaving an index that reports success while missing the tail
//! of the walk, so a legitimate miss and a truncated corpus look identical to
//! the caller. This module refuses the whole reindex up front instead.
//!
//! What: [`IndexBudget`] resolves a machine-wide file-count and total-byte
//! ceiling (defaults below, overridable per-daemon via `TRUSTY_MAX_INDEX_FILES`
//! / `TRUSTY_MAX_INDEX_BYTES`), and [`IndexBudget::check`] runs it against the
//! POST-FILTER file list — after `.gitignore`, `SKIP_DIRS`, `extra_skip_dirs`,
//! `exclude_globs`, `extensions`, and `path_filter` have all had their say — so
//! it measures what would actually be indexed rather than what is on disk.
//!
//! Test: the `tests` module at the bottom of this file, plus
//! `reindex_refuses_walk_over_file_budget` and
//! `reindex_budget_is_inclusive_at_the_boundary` in the integration test binary
//! `crates/trusty-search/tests/index_budget_env.rs` — they set
//! `TRUSTY_MAX_INDEX_FILES`, so they need their own process (#3769).
//!
//! [`IndexBudget`]: crate::service::index_budget::IndexBudget
//! [`IndexBudget::check`]: crate::service::index_budget::IndexBudget::check

use std::path::PathBuf;
use std::str::FromStr;

/// Default ceiling on how many files one index may hold (issue #4356).
///
/// Chosen above the largest index observed on a developer machine (a
/// ~200k-chunk corpus walks roughly 20–40k files) and well below the 109k-file
/// checkout that motivated the guardrail, so a real monorepo still indexes and
/// an accidental `$HOME` / volume-root registration does not.
pub const DEFAULT_MAX_INDEX_FILES: usize = 50_000;

/// Default ceiling on the total bytes of source one index may hold (2 GiB).
///
/// The walker already caps any single file at `walker::MAX_FILE_BYTES` (1 MiB),
/// so this bounds the aggregate. 50k files of typical source average well under
/// 1 GiB; 2 GiB leaves roughly 3x headroom before the guard fires.
pub const DEFAULT_MAX_INDEX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Env var overriding [`DEFAULT_MAX_INDEX_FILES`]. `0` disables the file cap.
pub const ENV_MAX_INDEX_FILES: &str = "TRUSTY_MAX_INDEX_FILES";

/// Env var overriding [`DEFAULT_MAX_INDEX_BYTES`]. `0` disables the byte cap.
pub const ENV_MAX_INDEX_BYTES: &str = "TRUSTY_MAX_INDEX_BYTES";

/// Why a walk was refused. Carries the observed figure and the ceiling it
/// crossed so the caller can render one actionable message without re-deriving
/// either number.
///
/// Why: the refusal reaches an operator through three surfaces (the terminal
/// SSE `error` event, `GET /indexes/:id/status`'s `last_walk_error`, and the
/// daemon log), and all three need the same text.
/// What: a `thiserror` enum whose `Display` names the ceiling, the env var that
/// raises it, and the per-index narrowing knobs that are the better remedy.
/// Test: `file_count_error_names_the_remedy`, `byte_error_names_the_remedy`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BudgetExceeded {
    /// The post-filter walk yielded more files than the budget allows.
    #[error(
        "index walk produced {found} files, over the {limit}-file budget; \
         refusing to index a partial corpus. Narrow the index \
         (exclude_globs / extra_skip_dirs / include_paths / extensions via \
         PATCH /indexes/<id>/config), or raise the daemon-wide ceiling with \
         TRUSTY_MAX_INDEX_FILES (0 disables it)"
    )]
    FileCount { found: usize, limit: usize },

    /// The post-filter walk's total size crossed the byte budget. `found` is a
    /// lower bound — summation stops at the breach.
    #[error(
        "index walk reached at least {found} bytes across {files_scanned} files, \
         over the {limit}-byte budget; refusing to index a partial corpus. \
         Narrow the index (exclude_globs / extra_skip_dirs / include_paths / \
         extensions via PATCH /indexes/<id>/config), or raise the daemon-wide \
         ceiling with TRUSTY_MAX_INDEX_BYTES (0 disables it)"
    )]
    TotalBytes {
        found: u64,
        files_scanned: usize,
        limit: u64,
    },
}

/// Resolved ceilings for one reindex walk.
///
/// Why: keeping resolution (env parsing) separate from enforcement lets the
/// tests construct an explicit budget instead of mutating process env, which is
/// shared state across a test binary's threads.
/// What: `None` on either field means that dimension is uncapped.
/// Test: `default_is_protected`, `zero_disables_the_cap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexBudget {
    pub max_files: Option<usize>,
    pub max_total_bytes: Option<u64>,
}

impl Default for IndexBudget {
    /// The protected defaults. A caller that constructs a budget without saying
    /// otherwise gets the guardrail, never an uncapped walk.
    fn default() -> Self {
        Self {
            max_files: Some(DEFAULT_MAX_INDEX_FILES),
            max_total_bytes: Some(DEFAULT_MAX_INDEX_BYTES),
        }
    }
}

impl IndexBudget {
    /// Resolve the budget from the daemon's environment.
    ///
    /// Why: the ceiling is a machine-wide operational policy — one daemon serves
    /// every index on the box — so it is read from env rather than persisted
    /// per-index. The per-index remedy for a legitimately large tree is to
    /// NARROW it (`exclude_globs`, `extra_skip_dirs`, `include_paths`,
    /// `extensions`), all of which already exist on `POST /indexes` and
    /// `PATCH /indexes/:id/config`.
    /// What: reads [`ENV_MAX_INDEX_FILES`] / [`ENV_MAX_INDEX_BYTES`], falling
    /// back to the defaults. `0` means uncapped; a malformed value logs a
    /// `warn` and falls back to the default rather than silently uncapping.
    /// Test: `zero_disables_the_cap`, `malformed_value_falls_back_to_default`,
    /// `absent_value_falls_back_to_default`.
    pub fn from_env() -> Self {
        Self {
            max_files: zero_is_none(env_limit(ENV_MAX_INDEX_FILES, DEFAULT_MAX_INDEX_FILES)),
            max_total_bytes: zero_is_none(env_limit(ENV_MAX_INDEX_BYTES, DEFAULT_MAX_INDEX_BYTES)),
        }
    }

    /// Refuse `files` when either ceiling is crossed.
    ///
    /// Why: this runs before any staging corpus is opened or any chunk is
    /// written, so a refusal leaves the existing index exactly as it was. That
    /// is the whole point — a truncated corpus that reports success returns
    /// empty results indistinguishable from a legitimate miss.
    /// What: checks the file count first (free — the list is already
    /// materialised), then sums `metadata().len()` and stops at the breach. A
    /// file that fails to stat contributes zero rather than aborting the walk,
    /// matching `walker::should_skip_path`'s stat-failure handling.
    /// Test: `check_passes_under_both_caps`, `check_refuses_over_file_cap`,
    /// `check_refuses_over_byte_cap`, `check_is_inclusive_at_the_boundary`,
    /// `check_ignores_unstattable_files`.
    pub fn check(&self, files: &[PathBuf]) -> Result<(), BudgetExceeded> {
        if let Some(limit) = self.max_files {
            if files.len() > limit {
                return Err(BudgetExceeded::FileCount {
                    found: files.len(),
                    limit,
                });
            }
        }

        let Some(limit) = self.max_total_bytes else {
            return Ok(());
        };
        let mut total: u64 = 0;
        for (scanned, path) in files.iter().enumerate() {
            // A stat failure contributes zero: the walker will fail to read the
            // file too and skip it, so counting it as unbounded would refuse an
            // index over files that never get indexed.
            let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            total = total.saturating_add(len);
            if total > limit {
                return Err(BudgetExceeded::TotalBytes {
                    found: total,
                    files_scanned: scanned + 1,
                    limit,
                });
            }
        }
        Ok(())
    }
}

/// Map a resolved ceiling of `0` onto "uncapped".
///
/// Mirrors `TRUSTY_MAX_KG_NODES` and `TRUSTY_MEMORY_ENFORCE_SECS`, where `0`
/// already means "disable this cap entirely".
fn zero_is_none<T: PartialEq + Default>(v: T) -> Option<T> {
    (v != T::default()).then_some(v)
}

/// Read `name` as `T`, falling back to `default` when unset or unparseable.
///
/// The env read is all this does; the parse lives in [`parse_limit`] so a test
/// can reach the malformed-value branch without writing to process env.
fn env_limit<T: FromStr + std::fmt::Display>(name: &str, default: T) -> T {
    match std::env::var(name) {
        Ok(raw) => parse_limit(name, &raw, default),
        Err(_) => default,
    }
}

/// Parse one already-read ceiling, falling back to `default` on garbage.
///
/// Why: a malformed value must warn and use the default, because silently
/// treating garbage as `0` would UNCAP the walk — the failure this module
/// exists to prevent, and one a typo in a deploy script would produce
/// indistinguishably from a deliberate opt-out. Splitting this out of
/// [`env_limit`] is what lets `malformed_value_falls_back_to_default` cover the
/// branch as a pure function: `setenv` reallocates the C `environ` array, so a
/// test that writes env inside the shared lib test binary can tear a concurrent
/// `getenv` anywhere in the process (#3769, and `#[serial]` does not prevent it).
/// What: `raw.parse::<T>()`, warning and returning `default` on `Err`.
/// Test: `malformed_value_falls_back_to_default`.
fn parse_limit<T: FromStr + std::fmt::Display>(name: &str, raw: &str, default: T) -> T {
    match raw.parse::<T>() {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!("index_budget: {name}={raw:?} is not a valid number; using {default}");
            default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build `count` files of `bytes` each under `dir`.
    fn files_of(dir: &std::path::Path, count: usize, bytes: usize) -> Vec<PathBuf> {
        (0..count)
            .map(|i| {
                let p = dir.join(format!("f{i}.rs"));
                let mut f = std::fs::File::create(&p).expect("create temp file");
                f.write_all(&vec![b'x'; bytes]).expect("write temp file");
                p
            })
            .collect()
    }

    /// The guardrail is on by construction: a `Default` budget caps both
    /// dimensions, so a caller cannot reach an uncapped walk by omission.
    #[test]
    fn default_is_protected() {
        let b = IndexBudget::default();
        assert_eq!(b.max_files, Some(DEFAULT_MAX_INDEX_FILES));
        assert_eq!(b.max_total_bytes, Some(DEFAULT_MAX_INDEX_BYTES));
    }

    #[test]
    fn zero_disables_the_cap() {
        assert_eq!(zero_is_none(0usize), None);
        assert_eq!(zero_is_none(0u64), None);
        assert_eq!(zero_is_none(7usize), Some(7));
    }

    /// An unparseable value falls back to the default, NOT to `0`.
    ///
    /// Why: `0` means uncapped. Treating garbage as `0` would silently disable
    /// the guardrail — the exact failure this module exists to prevent — and a
    /// typo in a deploy script would be indistinguishable from a deliberate
    /// opt-out.
    /// What: feeds the malformed values straight to [`parse_limit`] rather than
    /// writing them to process env and reading them back through [`env_limit`].
    /// The env round-trip proved nothing this does not, and it put a `setenv`
    /// inside the ~1.6k-test lib binary, where the `environ` reallocation can
    /// tear a concurrent `getenv` (#3769). `absent_value_falls_back_to_default`
    /// still covers `env_limit`'s unset arm, which only reads.
    /// Test: this test.
    #[test]
    fn malformed_value_falls_back_to_default() {
        const VAR: &str = ENV_MAX_INDEX_FILES;
        assert_eq!(
            parse_limit(VAR, "banana", 99usize),
            99,
            "garbage must fall back to the default, never to 0 (uncapped)"
        );
        assert_eq!(
            parse_limit(VAR, "-1", 99usize),
            99,
            "a negative value does not parse as usize and must not uncap"
        );
        assert_eq!(
            parse_limit(VAR, "", 99usize),
            99,
            "an empty override must not uncap either"
        );
        assert_eq!(
            parse_limit(VAR, "7", 99usize),
            7,
            "a well-formed value is still honoured"
        );
    }

    #[test]
    fn absent_value_falls_back_to_default() {
        assert_eq!(env_limit("TRUSTY_MAX_INDEX_FILES_ABSENT_4356", 99usize), 99);
    }

    #[test]
    fn check_passes_under_both_caps() {
        let dir = tempfile::tempdir().expect("tempdir");
        let files = files_of(dir.path(), 3, 10);
        let b = IndexBudget {
            max_files: Some(10),
            max_total_bytes: Some(1_000),
        };
        assert_eq!(b.check(&files), Ok(()));
    }

    #[test]
    fn check_refuses_over_file_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let files = files_of(dir.path(), 5, 1);
        let b = IndexBudget {
            max_files: Some(4),
            max_total_bytes: None,
        };
        assert_eq!(
            b.check(&files),
            Err(BudgetExceeded::FileCount { found: 5, limit: 4 })
        );
    }

    #[test]
    fn check_refuses_over_byte_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let files = files_of(dir.path(), 4, 100);
        let b = IndexBudget {
            max_files: None,
            max_total_bytes: Some(250),
        };
        let err = b
            .check(&files)
            .expect_err("400 bytes must exceed a 250 cap");
        match err {
            BudgetExceeded::TotalBytes {
                found,
                files_scanned,
                limit,
            } => {
                assert_eq!(limit, 250);
                assert_eq!(found, 300, "summation stops at the breaching file");
                assert_eq!(files_scanned, 3);
            }
            other => panic!("expected TotalBytes, got {other:?}"),
        }
    }

    /// The cap is inclusive: exactly-at-the-limit indexes, one over refuses.
    /// Both dimensions share the same `>` comparison, so both are pinned here.
    #[test]
    fn check_is_inclusive_at_the_boundary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let files = files_of(dir.path(), 4, 25); // 4 files, 100 bytes total

        let exact = IndexBudget {
            max_files: Some(4),
            max_total_bytes: Some(100),
        };
        assert_eq!(exact.check(&files), Ok(()), "exactly at both caps passes");

        let one_file_under = IndexBudget {
            max_files: Some(3),
            max_total_bytes: Some(100),
        };
        assert!(
            one_file_under.check(&files).is_err(),
            "one file over refuses"
        );

        let one_byte_under = IndexBudget {
            max_files: Some(4),
            max_total_bytes: Some(99),
        };
        assert!(
            one_byte_under.check(&files).is_err(),
            "one byte over refuses"
        );
    }

    #[test]
    fn check_ignores_unstattable_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut files = files_of(dir.path(), 1, 10);
        files.push(dir.path().join("does-not-exist.rs"));
        let b = IndexBudget {
            max_files: None,
            max_total_bytes: Some(20),
        };
        assert_eq!(
            b.check(&files),
            Ok(()),
            "a file that cannot be stat'd contributes zero, not a refusal"
        );
    }

    #[test]
    fn file_count_error_names_the_remedy() {
        let msg = BudgetExceeded::FileCount {
            found: 109_000,
            limit: 50_000,
        }
        .to_string();
        assert!(msg.contains("109000") && msg.contains("50000"), "{msg}");
        assert!(msg.contains("TRUSTY_MAX_INDEX_FILES"), "{msg}");
        assert!(msg.contains("exclude_globs"), "{msg}");
    }

    #[test]
    fn byte_error_names_the_remedy() {
        let msg = BudgetExceeded::TotalBytes {
            found: 3_000_000_000,
            files_scanned: 40_000,
            limit: DEFAULT_MAX_INDEX_BYTES,
        }
        .to_string();
        assert!(msg.contains("TRUSTY_MAX_INDEX_BYTES"), "{msg}");
        assert!(msg.contains("exclude_globs"), "{msg}");
    }
}
