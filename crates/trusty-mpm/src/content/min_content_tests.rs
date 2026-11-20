//! Tests for the minimum content release check (#11470).

use std::path::PathBuf;

use trusty_common::content::ContentSource;
use trusty_common::integrity::Sha256Digest;

use super::*;

/// Pins `tag` in `cache` through the offline install, as an operator would.
pub(crate) fn pin(cache: &std::path::Path, tag: &str) {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let manifest = format!("tag = \"{tag}\"\nschema_major = 1\n");
    for (path, data) in [
        ("bundle-manifest.toml", manifest.as_bytes()),
        ("skills/tm/SKILL.md", b"skill".as_slice()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, path, data).expect("append");
    }
    let bytes = tar.into_inner().expect("tar").finish().expect("gzip");
    let dir = tempfile::tempdir().expect("tempdir");
    let bundle = dir.path().join(format!("{tag}.tar.gz"));
    std::fs::write(&bundle, &bytes).expect("bundle");
    let sidecar = format!("{}  {tag}.tar.gz\n", Sha256Digest::of_bytes(&bytes));
    std::fs::write(dir.path().join(format!("{tag}.tar.gz.sha256")), sidecar).expect("sidecar");
    crate::content::bundle_cache::install_from_file(cache, &bundle).expect("install");
}

fn installed(tag: &str) -> ContentSource {
    ContentSource::Installed {
        tag: tag.to_owned(),
        sha256: Sha256Digest::of_bytes(b"x"),
        bundle: PathBuf::from("/nonexistent/bundle.tar.gz"),
    }
}

/// #11470: `content-v2.10.0` is newer than `content-v2.5.0`; an unparsable
/// tag never passes.
#[test]
fn tags_compare_by_version_not_by_string() {
    assert!(is_below("content-v2.4.0", "content-v2.5.0"));
    assert!(!is_below("content-v2.10.0", "content-v2.5.0"));
    assert!(!is_below("content-v2.5.0", "content-v2.5.0"));
    assert!(is_below("garbage", "content-v2.5.0"));
}

/// #11470 criterion 1: the remedy names `tm content update` and the release
/// it would pin when one qualifies.
#[test]
fn a_stale_pin_names_the_update_when_a_qualifying_release_exists() {
    let err = check_pin_with(&installed("content-v2.4.0"), "content-v2.5.0", || {
        Ok(Some("content-v2.5.0".to_owned()))
    })
    .expect_err("stale pin");
    let msg = err.to_string();
    assert!(
        msg.contains("content-v2.4.0 is older than content-v2.5.0"),
        "{msg}"
    );
    assert!(
        msg.contains("run `tm content update` to pin content-v2.5.0"),
        "{msg}"
    );
}

/// #11470 criterion 1: with no qualifying release, the message says none is
/// published and does not offer `tm content update` as the fix.
#[test]
fn a_stale_pin_says_none_is_published_when_none_qualifies() {
    let err = check_pin_with(&installed("content-v2.4.0"), "content-v2.5.0", || Ok(None))
        .expect_err("stale pin");
    let msg = err.to_string();
    assert!(
        msg.contains("no content release at or above content-v2.5.0"),
        "{msg}"
    );
    assert!(msg.contains("cannot fix this"), "{msg}");
    let failed = check_pin_with(&installed("content-v2.4.0"), "content-v2.5.0", || {
        Err("offline".to_owned())
    })
    .expect_err("stale pin");
    assert!(failed.to_string().contains("could not check"), "{failed}");
}

/// A pin at the minimum and a checkout never run the probe.
#[test]
fn a_pin_at_the_minimum_and_a_checkout_pass_without_a_probe() {
    let never = || -> ProbeResult { panic!("probed") };
    check_pin_with(&installed("content-v2.5.0"), "content-v2.5.0", never).expect("at minimum");
    let checkout = ContentSource::DevCheckout {
        root: PathBuf::from("/x"),
    };
    check_pin_with(&checkout, "content-v2.5.0", never).expect("checkout");
}

/// #11470 regression: the launch resolver refuses a cache pinned below the
/// minimum before any seed file is read. Pre-fix it served the pin, and the
/// caller then failed on a missing `output-styles/owner-plain.md`.
#[test]
fn the_launch_resolver_refuses_a_pin_below_the_minimum() {
    use crate::core::content_source::{Fetch, resolve_for_in};
    let cache = tempfile::tempdir().expect("tempdir");
    pin(cache.path(), "content-v2.4.0");
    PROBE_OVERRIDE.with(|p| p.set(Some(|| Ok(None))));
    let result = resolve_for_in(None, None, Some(cache.path()), Fetch::OnFirstUse);
    PROBE_OVERRIDE.with(|p| p.set(None));
    let err = result.expect_err("a pin below the minimum must not resolve");
    let msg = err.to_string();
    assert!(msg.contains("older than"), "{msg}");
    assert!(msg.contains("published yet"), "{msg}");
}
