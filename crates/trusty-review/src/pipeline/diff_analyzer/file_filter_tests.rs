//! Unit tests for Stage A FileFilter (spec REV-201–202).

use super::{FileFilter, FilterConfig, split_patch_into_hunks};
use crate::pipeline::diff_analyzer::models::FileDisposition;

fn filter() -> FileFilter {
    FileFilter::new(FilterConfig::default())
}

fn files(list: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    list.iter()
        .map(|(p, s, pa)| (p.to_string(), s.to_string(), pa.to_string()))
        .collect()
}

#[test]
fn lockfile_is_dropped() {
    let ff = filter();
    let (kept, dropped) = ff.apply(&files(&[("Cargo.lock", "modified", "+foo\n")]));
    assert!(kept.is_empty(), "lockfile must not be kept");
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].reason, "lockfile");
}

#[test]
fn yarn_lock_is_dropped() {
    let ff = filter();
    let (kept, dropped) = ff.apply(&files(&[("yarn.lock", "modified", "+x\n")]));
    assert!(kept.is_empty());
    assert_eq!(dropped[0].reason, "lockfile");
}

#[test]
fn snapshot_is_dropped() {
    let ff = filter();
    let (kept, dropped) = ff.apply(&files(&[(
        "tests/__snapshots__/foo.snap",
        "modified",
        "+x\n",
    )]));
    assert!(kept.is_empty());
    assert_eq!(dropped[0].reason, "snapshot");
}

#[test]
fn generated_file_is_dropped() {
    let ff = filter();
    let (kept, dropped) = ff.apply(&files(&[(
        "src/gen/api_pb2.py",
        "added",
        "+class X: pass\n",
    )]));
    assert!(kept.is_empty());
    assert!(
        dropped[0].reason.contains("generated"),
        "expected generated, got: {}",
        dropped[0].reason
    );
}

#[test]
fn plain_rust_file_is_kept() {
    let ff = filter();
    let patch = "@@ -1,1 +1,1 @@\n-fn foo() {}\n+fn bar() {}\n";
    let (kept, dropped) = ff.apply(&files(&[("src/auth.rs", "modified", patch)]));
    assert!(dropped.is_empty());
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].disposition, FileDisposition::Kept);
    assert_eq!(kept[0].filename, "src/auth.rs");
}

#[test]
fn security_file_is_preserved() {
    let ff = filter();
    // A file in a security/ path should be force-kept even if it looks like a lockfile.
    let (kept, dropped) = ff.apply(&files(&[("security/Cargo.lock", "modified", "+x\n")]));
    assert!(dropped.is_empty(), "security file must be force-kept");
    assert_eq!(kept.len(), 1);
}

#[test]
fn credential_filename_preserved() {
    let ff = filter();
    // "token" in the filename triggers preserve.
    let (kept, dropped) = ff.apply(&files(&[("config/api_token.json", "modified", "+x\n")]));
    assert!(dropped.is_empty());
    assert_eq!(kept[0].disposition, FileDisposition::Kept);
}

#[test]
fn env_file_is_preserved() {
    let ff = filter();
    let (kept, dropped) = ff.apply(&files(&[(".env", "modified", "+SECRET=xxx\n")]));
    assert!(dropped.is_empty(), ".env must be force-kept");
    assert_eq!(kept[0].disposition, FileDisposition::Kept);
}

#[test]
fn fixture_file_is_summarised_when_large() {
    let ff = filter();
    let big_patch = "+row\n".repeat(50);
    let (kept, dropped) = ff.apply(&files(&[(
        "tests/fixtures/data.json",
        "modified",
        &big_patch,
    )]));
    assert!(dropped.is_empty());
    assert_eq!(kept[0].disposition, FileDisposition::SummaryOnly);
    assert!(kept[0].summary_line.is_some());
}

#[test]
fn fixture_file_kept_when_small() {
    let ff = filter();
    let small_patch = "@@ -1,2 +1,2 @@\n+row1\n+row2\n";
    let (kept, dropped) = ff.apply(&files(&[(
        "tests/fixtures/data.json",
        "modified",
        small_patch,
    )]));
    assert!(dropped.is_empty());
    assert_eq!(kept[0].disposition, FileDisposition::Kept);
}

#[test]
fn split_patch_into_hunks_basic() {
    let patch = "@@ -1,2 +1,2 @@\n-old\n+new\n@@ -10,1 +10,1 @@\n-x\n+y\n";
    let hunks = split_patch_into_hunks(patch);
    assert_eq!(hunks.len(), 2);
    assert_eq!(hunks[0].0, "@@ -1,2 +1,2 @@");
    assert_eq!(hunks[1].0, "@@ -10,1 +10,1 @@");
}

#[test]
fn split_opaque_patch_treated_as_single_hunk() {
    let patch = "-old line\n+new line\n";
    let hunks = split_patch_into_hunks(patch);
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].0, "@@ (opaque) @@");
}

#[test]
fn filefilter_user_preserve_keeps_file() {
    let config = FilterConfig {
        preserve_patterns: vec![r"vendor/.*\.lock$".to_string()],
        ..Default::default()
    };
    let ff = FileFilter::new(config);
    let (kept, dropped) = ff.apply(&files(&[("vendor/foo.lock", "modified", "+x\n")]));
    assert!(dropped.is_empty(), "user preserve must override drop");
    assert_eq!(kept.len(), 1);
}

#[test]
fn multiple_files_classified_correctly() {
    let ff = filter();
    let patch = "@@ -1,1 +1,1 @@\n+fn foo() {}\n";
    let input = files(&[
        ("Cargo.lock", "modified", "+x\n"),
        ("src/main.rs", "modified", patch),
        ("yarn.lock", "modified", "+y\n"),
    ]);
    let (kept, dropped) = ff.apply(&input);
    assert_eq!(dropped.len(), 2, "both lockfiles must be dropped");
    assert_eq!(kept.len(), 1, "only main.rs should be kept");
    assert_eq!(kept[0].filename, "src/main.rs");
}
