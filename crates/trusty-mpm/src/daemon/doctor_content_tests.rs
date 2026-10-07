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

/// After ADR-0064 PHASE_1 nothing else serves, so nothing installed is WARN.
/// #9012 dropped the last embedded content, so this is the only arm.
#[test]
fn content_row_warns_when_nothing_is_installed_after_phase_1() {
    let cache = tempfile::tempdir().unwrap();
    let row = check_content(Some(cache.path()), Some(cache.path()));
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

/// #9012: a verified-or-serving source whose PM package this binary cannot
/// parse is FAIL, naming the remedy, not OK.
#[test]
fn content_row_fails_when_the_pm_package_does_not_parse() {
    use crate::core::framework_content::tests::{
        fake_checkout_with_package, package_with_an_unknown_section,
    };
    let checkout = tempfile::tempdir().unwrap();
    fake_checkout_with_package(checkout.path(), &package_with_an_unknown_section());
    let project = checkout.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let row = check_content(Some(&project), Some(cache.path()));
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    // #9396: the checkout serves, so the remedy is in the checkout.
    assert!(row.message.contains("git pull"), "{}", row.message);
}

/// A first-use fetch that panics: the doctor row must never reach it.
fn fetch_panics(
    _: &Path,
) -> Result<Option<crate::content::bundle_cache::UpdateOutcome>, crate::content::bundle_cache::CacheError>
{
    panic!("tm doctor fetched the content release")
}

/// #9396: `tm doctor` is read-only. With no checkout at the project or the
/// cwd and no lock in the cache, the content row reports WARN naming the
/// same remedy as every not-installed error, and never fetches.
#[test]
fn the_content_row_never_fetches() {
    use crate::content::first_use::FETCH_OVERRIDE;
    let (project, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    FETCH_OVERRIDE.with(|f| f.set(Some(fetch_panics)));
    let row = std::panic::catch_unwind(|| {
        check_content_in(Some(project.path()), Some(cache.path()), None)
    });
    FETCH_OVERRIDE.with(|f| f.set(None));
    let row = row.expect("the content row fetched");
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(
        row.message
            .contains(trusty_agents_common::agent_content::REMEDY),
        "{}",
        row.message
    );
    assert!(!cache.path().join(LOCK_FILE_NAME).exists(), "nothing pinned");
}
