//! Tests for the shared cross-process JSON-file primitives.
//!
//! Why: `SessionStore` and the #6568 resume-breaker sidecar both depend on these
//! three, and both depend on them for CORRECTNESS across processes, not merely
//! for convenience. Testing them once here is what makes it safe for the two
//! callers to stop carrying their own copies.
//! What: the fingerprint's absent/changed cases, the `None`-means-changed rule,
//! staging-path uniqueness, and the atomic write's success and cleanup paths.
//! Test: this is the test module.

use super::json_file::{FileSig, is_unchanged, sig_of, staging_path, write_atomic};

#[tokio::test]
async fn sig_of_is_none_for_a_missing_file() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    assert_eq!(sig_of(&tmp.path().join("absent.json")).await, None);
}

#[tokio::test]
async fn sig_of_changes_after_a_write() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("f.json");
    tokio::fs::write(&path, "{}").await.expect("write");
    let first = sig_of(&path).await.expect("present");
    // A different LENGTH is what catches a same-second rewrite whose mtime has
    // not moved — the case the fingerprint pairs length with mtime for.
    tokio::fs::write(&path, "{\"a\":1}").await.expect("rewrite");
    let second = sig_of(&path).await.expect("present");
    assert_ne!(first, second);
}

#[test]
fn unchanged_requires_both_signatures() {
    let a = FileSig::default();
    assert!(is_unchanged(Some(a), Some(a)));
    // A `None` on EITHER side must read as CHANGED. Inverting this is how a
    // reader silently serves stale data forever after one absent observation.
    assert!(!is_unchanged(None, Some(a)));
    assert!(!is_unchanged(Some(a), None));
    assert!(!is_unchanged(None, None));
}

#[test]
fn staging_paths_are_unique_per_instance() {
    // #5007: two writers sharing one staging name interleave their bytes and
    // rename a corrupt document into place.
    let path = std::path::Path::new("/tmp/x/sessions.json");
    let a = staging_path(path);
    let b = staging_path(path);
    assert_ne!(a, b);
    assert_eq!(a.parent(), path.parent());
    assert!(
        a.file_name()
            .expect("named")
            .to_string_lossy()
            .starts_with("sessions.json.tmp."),
        "got {a:?}"
    );
}

#[tokio::test]
async fn write_atomic_replaces_the_target() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // A nested target proves the parent is created rather than erroring.
    let path = tmp.path().join("nested").join("f.json");
    let staging = staging_path(&path);

    write_atomic(&path, &staging, "{\"v\":1}")
        .await
        .expect("first");
    assert_eq!(
        tokio::fs::read_to_string(&path).await.expect("read"),
        "{\"v\":1}"
    );

    write_atomic(&path, &staging, "{\"v\":2}")
        .await
        .expect("second");
    assert_eq!(
        tokio::fs::read_to_string(&path).await.expect("read"),
        "{\"v\":2}"
    );
}

#[tokio::test]
async fn write_atomic_leaves_no_staging_file_behind() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("f.json");
    let staging = staging_path(&path);
    write_atomic(&path, &staging, "{}").await.expect("write");
    assert!(
        !staging.exists(),
        "the staging file must be renamed away, not left for nothing to clean up"
    );
}

#[tokio::test]
async fn write_atomic_cleans_up_when_the_rename_fails() {
    // Failure path: renaming a file over an existing DIRECTORY fails on every
    // supported target. The staging file must not survive that — it is named
    // after this process and nothing else would ever remove it.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("target-is-a-dir");
    tokio::fs::create_dir(&path).await.expect("mkdir");
    tokio::fs::write(path.join("occupant"), "x")
        .await
        .expect("occupy so the dir is non-empty");
    let staging = staging_path(&path);

    let err = write_atomic(&path, &staging, "{}").await;
    assert!(
        err.is_err(),
        "renaming over a non-empty directory must fail"
    );
    assert!(
        !staging.exists(),
        "a failed swap must still remove its staging file"
    );
}
