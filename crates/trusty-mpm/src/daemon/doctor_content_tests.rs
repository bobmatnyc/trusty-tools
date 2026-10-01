//! Tests for the `content` doctor row (#8378 PR-C).

use trusty_common::content::{ContentLock, LOCK_FILE_NAME};
use trusty_common::integrity::Sha256Digest;

use super::*;

const TAG: &str = "content-v0.1.0";

fn bundle() -> Vec<u8> {
    let manifest = format!("tag = \"{TAG}\"\nschema_major = 1\n");
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    tar.append_data(&mut header, "bundle-manifest.toml", manifest.as_bytes())
        .expect("append");
    tar.into_inner().expect("tar").finish().expect("gzip")
}

fn install(cache: &Path, bytes: &[u8]) {
    std::fs::write(cache.join(format!("{TAG}.tar.gz")), bytes).expect("bundle");
    ContentLock::new(TAG, Sha256Digest::of_bytes(bytes))
        .expect("lock")
        .store(&cache.join(LOCK_FILE_NAME))
        .expect("store");
}

#[test]
fn content_row_is_ok_for_a_verified_bundle() {
    let cache = tempfile::tempdir().unwrap();
    install(cache.path(), &bundle());
    let row = check_content(None, Some(cache.path()));
    assert_eq!(
        (row.name.as_str(), row.status),
        (CHECK_NAME, CheckStatus::Ok)
    );
    assert!(row.message.contains("source: bundle"), "{}", row.message);
    assert!(row.message.contains(TAG), "{}", row.message);
    assert!(
        row.message.contains("binary: trusty-mpm"),
        "{}",
        row.message
    );
}

/// Owner ruling (Bob item 207): INFO while the binary still embeds content.
/// See ADR-0064: PHASE_1 flips `BUILTIN_CONTENT_EMBEDDED` and deletes this test.
#[test]
fn content_row_is_info_when_nothing_is_installed_before_phase_1() {
    let cache = tempfile::tempdir().unwrap();
    let row = check_content(Some(cache.path()), Some(cache.path()));
    assert_eq!(row.status, CheckStatus::Ok);
    assert!(row.message.starts_with("info: "), "{}", row.message);
    assert!(row.message.contains("source: none"), "{}", row.message);
    assert!(
        row.message.contains("tm content install --from"),
        "{}",
        row.message
    );
}

/// After ADR-0064 PHASE_1 nothing else serves, so nothing installed is WARN.
#[test]
fn content_row_warns_when_nothing_is_installed_after_phase_1() {
    let cache = tempfile::tempdir().unwrap();
    let row = grade(&content_status(cache.path(), Some(cache.path())), false);
    assert_eq!(row.status, CheckStatus::Warn);
    assert!(!row.message.starts_with("info: "), "{}", row.message);
    assert!(row.message.contains("source: none"), "{}", row.message);
    assert!(
        row.message.contains("tm content install --from"),
        "{}",
        row.message
    );
}

#[test]
fn content_row_fails_on_a_tampered_bundle() {
    let cache = tempfile::tempdir().unwrap();
    install(cache.path(), &bundle());
    std::fs::write(cache.path().join(format!("{TAG}.tar.gz")), b"tampered").unwrap();
    let row = check_content(None, Some(cache.path()));
    assert_eq!(row.status, CheckStatus::Fail);
    assert!(row.message.contains("UNHEALTHY"), "{}", row.message);
}
