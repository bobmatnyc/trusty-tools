//! Tests for the content resolver: the lock, tag + sha256 verification, the
//! dev override, and every fail-closed arm (ADR-0064 PHASE_3 (i) and (iii)).

use std::path::{Path, PathBuf};

use super::*;

const TAG: &str = "content-v0.1.0";

/// Builds a gzip tar the way `scripts/package_content.sh` does: the manifest
/// first, then one entry per `(path, bytes)`.
fn bundle(manifest_tag: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let manifest = format!(
        "bundle_version = \"0.1.0\"\ntag = \"{manifest_tag}\"\nschema_major = 1\nfile_count = {}\n",
        files.len()
    );
    bundle_with_manifest(&manifest, files)
}

/// [`bundle`] with the manifest text given verbatim.
fn bundle_with_manifest(manifest: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let mut entries: Vec<(&str, &[u8])> = vec![(bundle::MANIFEST_ENTRY, manifest.as_bytes())];
    entries.extend_from_slice(files);
    for (path, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, path, data).expect("append");
    }
    tar.into_inner().expect("tar").finish().expect("gzip")
}

/// Writes `bytes` as the cached bundle for [`TAG`] and a lock pinning `pin`.
fn install(cache: &Path, bytes: &[u8], pin: &Sha256Digest) {
    std::fs::create_dir_all(cache).expect("cache dir");
    std::fs::write(cache.join(format!("{TAG}.tar.gz")), bytes).expect("bundle");
    ContentLock::new(TAG, pin.clone())
        .expect("lock")
        .store(&cache.join(LOCK_FILE_NAME))
        .expect("store");
}

/// Installs a well-formed bundle holding one skill, pinned to its own digest.
fn install_valid(cache: &Path) {
    let bytes = bundle(TAG, &[("skills/tm/SKILL.md", b"installed skill")]);
    install(cache, &bytes, &Sha256Digest::of_bytes(&bytes));
}

/// Creates a trusted checkout: every class directory, a `.git` marker, a
/// `[workspace]` `Cargo.toml`, plus one skill file.
fn make_checkout(root: &Path) {
    for (_, rel) in DEV_CLASS_SOURCES {
        std::fs::create_dir_all(root.join(rel)).expect("class dir");
    }
    std::fs::create_dir_all(root.join(".git")).expect(".git");
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    )
    .expect("Cargo.toml");
    let skill = root.join("content/skills/tm");
    std::fs::create_dir_all(&skill).expect("skill dir");
    std::fs::write(skill.join("SKILL.md"), b"working-tree skill").expect("skill");
    std::fs::write(skill.join(".hidden"), b"x").expect("dot-file");
}

fn options(cache: &Path, dev: DevOverride) -> ResolveOptions {
    ResolveOptions::new(cache, dev)
}

/// One tar entry written straight into its header, bypassing the checks
/// `tar::Builder` applies to names and link targets.
struct Raw<'a> {
    name: &'a str,
    kind: tar::EntryType,
    data: &'a [u8],
    link: Option<&'a str>,
    /// The header's size field; `None` writes `data.len()`.
    size: Option<u64>,
}

impl<'a> Raw<'a> {
    fn file(name: &'a str, data: &'a [u8]) -> Self {
        Self::of(tar::EntryType::Regular, name, data)
    }

    fn of(kind: tar::EntryType, name: &'a str, data: &'a [u8]) -> Self {
        Self {
            name,
            kind,
            data,
            link: None,
            size: None,
        }
    }
}

/// A gzip tar holding a valid manifest for [`TAG`], then `entries` verbatim.
fn raw_bundle(entries: &[Raw<'_>]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let manifest = format!("tag = \"{TAG}\"\nschema_major = 1\n");
    let mut manifest_header = tar::Header::new_gnu();
    manifest_header.set_size(manifest.len() as u64);
    manifest_header.set_mode(0o644);
    tar.append_data(
        &mut manifest_header,
        bundle::MANIFEST_ENTRY,
        manifest.as_bytes(),
    )
    .expect("manifest");
    for raw in entries {
        let mut header = tar::Header::new_gnu();
        let name = raw.name.as_bytes();
        header.as_gnu_mut().expect("gnu").name[..name.len()].copy_from_slice(name);
        header.set_size(raw.size.unwrap_or(raw.data.len() as u64));
        header.set_mode(0o644);
        header.set_entry_type(raw.kind);
        if let Some(link) = raw.link {
            let link = link.as_bytes();
            header.as_gnu_mut().expect("gnu").linkname[..link.len()].copy_from_slice(link);
        }
        header.set_cksum();
        tar.append(&header, raw.data).expect("append");
    }
    tar.into_inner().expect("tar").finish().expect("gzip")
}

/// Installs `bytes` pinned to their own digest and asserts `resolve` refuses
/// them as `BundleCorrupt` for a reason containing `needle`.
fn assert_corrupt(bytes: &[u8], needle: &str) {
    let dir = tempfile::tempdir().expect("tempdir");
    install(dir.path(), bytes, &Sha256Digest::of_bytes(bytes));
    match resolve_err(&options(dir.path(), DevOverride::Off)) {
        ContentError::BundleCorrupt { reason, .. } => {
            assert!(
                reason.contains(needle),
                "reason {reason:?} lacks {needle:?}"
            );
        }
        other => panic!("expected BundleCorrupt, got {other:?}"),
    }
}

fn resolve_err(opts: &ResolveOptions) -> ContentError {
    match resolve(opts) {
        Ok(content) => panic!("expected an error, resolved {:?}", content.source()),
        Err(e) => e,
    }
}

#[test]
fn lock_round_trips_through_store_and_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested").join(LOCK_FILE_NAME);
    let lock = ContentLock::new(TAG, Sha256Digest::of_bytes(b"x")).expect("lock");
    lock.store(&path).expect("store");
    assert_eq!(ContentLock::load(&path).expect("load"), lock);
    assert_eq!(lock.bundle_file_name(), "content-v0.1.0.tar.gz");
}

#[test]
fn lock_rejects_a_tag_that_would_escape_the_cache() {
    let digest = Sha256Digest::of_bytes(b"x");
    for bad in [
        "content-v0.1.0/../../evil",
        "../content-v0.1.0",
        "trusty-common-v0.52.8",
        "content-v1.0",
        "content-v01.0.0",
        "content-v1.0.0-",
        "content-v1.0.0-rc/1",
    ] {
        assert!(
            matches!(
                ContentLock::new(bad, digest.clone()),
                Err(ContentError::InvalidTag { .. })
            ),
            "{bad:?} must be refused"
        );
    }
    for good in ["content-v0.1.0", "content-v10.20.30", "content-v1.0.0-rc.1"] {
        assert!(ContentLock::new(good, digest.clone()).is_ok(), "{good:?}");
    }
}

#[test]
fn lock_load_rejects_a_malformed_sha256() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(LOCK_FILE_NAME);
    for body in [
        format!("tag = \"{TAG}\"\nsha256 = \"abc\"\n"),
        format!("tag = \"../x\"\nsha256 = \"{}\"\n", "a".repeat(64)),
        format!("tag = \"{TAG}\"\n"),
        "not toml at all [".to_owned(),
    ] {
        std::fs::write(&path, &body).expect("write");
        assert!(
            matches!(
                ContentLock::load(&path),
                Err(ContentError::LockInvalid { .. })
            ),
            "{body:?} must be LockInvalid"
        );
    }
}

#[test]
fn resolve_without_a_lock_is_not_installed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    match resolve_err(&options(&cache, DevOverride::Off)) {
        ContentError::NotInstalled { lock_path } => {
            assert_eq!(lock_path, cache.join(LOCK_FILE_NAME));
        }
        other => panic!("expected NotInstalled, got {other:?}"),
    }
}

#[test]
fn resolve_with_an_unreadable_lock_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().to_path_buf();
    install_valid(&cache);
    // A directory where the lock file should be: present, but unreadable.
    std::fs::remove_file(cache.join(LOCK_FILE_NAME)).expect("rm lock");
    std::fs::create_dir(cache.join(LOCK_FILE_NAME)).expect("dir as lock");
    assert!(matches!(
        resolve_err(&options(&cache, DevOverride::Off)),
        ContentError::LockUnreadable { .. }
    ));
}

#[test]
fn resolve_with_a_missing_bundle_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().to_path_buf();
    install_valid(&cache);
    std::fs::remove_file(cache.join(format!("{TAG}.tar.gz"))).expect("rm bundle");
    assert!(matches!(
        resolve_err(&options(&cache, DevOverride::Off)),
        ContentError::BundleMissing { .. }
    ));
}

#[test]
fn resolve_with_an_unreadable_bundle_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().to_path_buf();
    install_valid(&cache);
    let path = cache.join(format!("{TAG}.tar.gz"));
    std::fs::remove_file(&path).expect("rm bundle");
    std::fs::create_dir(&path).expect("dir as bundle");
    assert!(matches!(
        resolve_err(&options(&cache, DevOverride::Off)),
        ContentError::BundleUnreadable { .. }
    ));
}

/// Fail-open check: a tampered bundle is refused, and no other source —
/// neither a checkout lookup nor the unverified bytes — is served instead.
#[test]
fn resolve_refuses_a_bundle_whose_sha256_does_not_match() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    let good = bundle(TAG, &[("skills/tm/SKILL.md", b"installed skill")]);
    let tampered = bundle(TAG, &[("skills/tm/SKILL.md", b"tampered skill")]);
    install(&cache, &tampered, &Sha256Digest::of_bytes(&good));
    let outside = dir.path().join("elsewhere");
    std::fs::create_dir_all(&outside).expect("outside dir");
    match resolve_err(&options(&cache, DevOverride::DetectFrom(outside))) {
        ContentError::ChecksumMismatch {
            expected, actual, ..
        } => {
            assert_eq!(expected, Sha256Digest::of_bytes(&good));
            assert_eq!(actual, Sha256Digest::of_bytes(&tampered));
        }
        other => panic!("expected ChecksumMismatch, got {other:?}"),
    }
}

#[test]
fn resolve_refuses_a_bundle_whose_manifest_names_another_tag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = bundle("content-v9.9.9", &[("skills/a.md", b"a")]);
    install(dir.path(), &bytes, &Sha256Digest::of_bytes(&bytes));
    match resolve_err(&options(dir.path(), DevOverride::Off)) {
        ContentError::TagMismatch {
            lock_tag,
            bundle_tag,
        } => {
            assert_eq!(lock_tag, TAG);
            assert_eq!(bundle_tag, "content-v9.9.9");
        }
        other => panic!("expected TagMismatch, got {other:?}"),
    }
}

/// #8378 PR-C, ADR-0064 PHASE_3 (iv): a newer layout is refused, naming both
/// majors; the same major with a newer bundle version still loads.
#[test]
fn resolve_refuses_a_newer_schema_major() {
    let newer = SUPPORTED_SCHEMA_MAJOR + 1;
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = format!("tag = \"{TAG}\"\nschema_major = {newer}\n");
    let bytes = bundle_with_manifest(&manifest, &[("skills/a.md", b"a")]);
    install(dir.path(), &bytes, &Sha256Digest::of_bytes(&bytes));
    match resolve_err(&options(dir.path(), DevOverride::Off)) {
        ContentError::UnsupportedSchema {
            bundle, supported, ..
        } => {
            assert_eq!(bundle, newer);
            assert_eq!(supported, SUPPORTED_SCHEMA_MAJOR);
        }
        other => panic!("expected UnsupportedSchema, got {other:?}"),
    }

    let same = tempfile::tempdir().expect("tempdir");
    let manifest = format!(
        "bundle_version = \"0.9.0\"\ntag = \"{TAG}\"\nschema_major = {SUPPORTED_SCHEMA_MAJOR}\n"
    );
    let bytes = bundle_with_manifest(&manifest, &[("skills/a.md", b"a")]);
    install(same.path(), &bytes, &Sha256Digest::of_bytes(&bytes));
    resolve(&options(same.path(), DevOverride::Off)).expect("same major loads");
}

/// #8378 PR-C: the major is read before the rest of the manifest, so a newer
/// layout that renamed `tag` is refused as too new, not as malformed.
#[test]
fn a_newer_schema_major_without_a_tag_key_is_unsupported_not_corrupt() {
    let newer = SUPPORTED_SCHEMA_MAJOR + 1;
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = format!("release_tag = \"{TAG}\"\nschema_major = {newer}\n");
    let bytes = bundle_with_manifest(&manifest, &[("skills/a.md", b"a")]);
    install(dir.path(), &bytes, &Sha256Digest::of_bytes(&bytes));
    match resolve_err(&options(dir.path(), DevOverride::Off)) {
        ContentError::UnsupportedSchema { bundle, .. } => assert_eq!(bundle, newer),
        other => panic!("expected UnsupportedSchema, got {other:?}"),
    }
}

#[test]
fn resolve_refuses_a_manifest_without_a_schema_major() {
    let manifest = format!("tag = \"{TAG}\"\n");
    let bytes = bundle_with_manifest(&manifest, &[("skills/a.md", b"a")]);
    assert_corrupt(&bytes, "no schema_major");
}

#[test]
fn resolve_refuses_a_bundle_that_is_not_an_archive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = b"not a gzip tar".to_vec();
    install(dir.path(), &bytes, &Sha256Digest::of_bytes(&bytes));
    assert!(matches!(
        resolve_err(&options(dir.path(), DevOverride::Off)),
        ContentError::BundleCorrupt { .. }
    ));
}

#[test]
fn resolve_refuses_a_bundle_with_a_climbing_entry() {
    let bytes = raw_bundle(&[Raw::file("../escape.md", b"escaped")]);
    assert_corrupt(&bytes, "not a relative path");
}

#[test]
fn resolve_refuses_a_bundle_with_an_absolute_entry() {
    let bytes = raw_bundle(&[Raw::file("/etc/escape.md", b"escaped")]);
    assert_corrupt(&bytes, "not a relative path");
}

#[test]
fn resolve_refuses_a_bundle_with_a_symlink_entry() {
    let bytes = raw_bundle(&[Raw {
        link: Some("/etc/passwd"),
        ..Raw::of(tar::EntryType::Symlink, "skills/link.md", b"")
    }]);
    assert_corrupt(&bytes, "not a regular file");
}

#[test]
fn resolve_refuses_a_bundle_with_a_hardlink_entry() {
    let bytes = raw_bundle(&[Raw {
        link: Some(bundle::MANIFEST_ENTRY),
        ..Raw::of(tar::EntryType::Link, "skills/link.md", b"")
    }]);
    assert_corrupt(&bytes, "not a regular file");
}

#[test]
fn resolve_refuses_a_bundle_with_a_device_entry() {
    let bytes = raw_bundle(&[Raw::of(tar::EntryType::Char, "skills/dev.md", b"")]);
    assert_corrupt(&bytes, "not a regular file");
}

/// `./a` and `a` name one file; the second entry must not replace the first.
#[test]
fn resolve_refuses_a_bundle_with_a_duplicate_entry() {
    for second in ["skills/a.md", "./skills/a.md"] {
        let bytes = raw_bundle(&[
            Raw::file("skills/a.md", b"first"),
            Raw::file(second, b"second"),
        ]);
        assert_corrupt(&bytes, "appears twice");
    }
}

#[test]
fn resolve_refuses_a_bundle_without_a_manifest() {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let mut header = tar::Header::new_gnu();
    header.set_size(1);
    header.set_mode(0o644);
    tar.append_data(&mut header, "skills/a.md", &b"a"[..])
        .expect("append");
    let bytes = tar.into_inner().expect("tar").finish().expect("gzip");
    assert_corrupt(&bytes, "no bundle-manifest.toml entry");
}

/// Each cap refuses a bundle the production caps accept.
#[test]
fn bundle_over_a_cap_is_too_large() {
    let dir = tempfile::tempdir().expect("tempdir");
    install_valid(dir.path());
    let lock = ContentLock::load(&dir.path().join(LOCK_FILE_NAME)).expect("lock");
    let size = std::fs::metadata(dir.path().join(lock.bundle_file_name()))
        .expect("bundle")
        .len();
    let default = bundle::Limits::DEFAULT;
    assert!(bundle::load_verified_with(dir.path(), &lock, default).is_ok());
    for (limits, needle) in [
        (
            bundle::Limits {
                bundle_bytes: size - 1,
                ..default
            },
            "over the",
        ),
        (
            bundle::Limits {
                unpacked_bytes: 10,
                ..default
            },
            "declare more than 10 bytes",
        ),
        (
            bundle::Limits {
                entries: 1,
                ..default
            },
            "more than 1 entries",
        ),
    ] {
        match bundle::load_verified_with(dir.path(), &lock, limits) {
            Err(ContentError::BundleTooLarge { reason, .. }) => {
                assert!(
                    reason.contains(needle),
                    "reason {reason:?} lacks {needle:?}"
                );
            }
            other => panic!("expected BundleTooLarge for {limits:?}, got {other:?}"),
        }
    }
}

/// Installs `bytes` pinned to their own digest, loads them under `limits`,
/// and returns the reason they were refused as `BundleTooLarge`.
fn too_large_reason(bytes: &[u8], limits: bundle::Limits) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    install(dir.path(), bytes, &Sha256Digest::of_bytes(bytes));
    let lock = ContentLock::load(&dir.path().join(LOCK_FILE_NAME)).expect("lock");
    match bundle::load_verified_with(dir.path(), &lock, limits) {
        Err(ContentError::BundleTooLarge { reason, .. }) => reason,
        Ok(files) => panic!("expected BundleTooLarge, unpacked {} files", files.len()),
        Err(other) => panic!("expected BundleTooLarge, got {other:?}"),
    }
}

/// #8378 review: a pax `size=` record replaces the header's size field, so the
/// cap must count the size tar reads, not the header's zero.
#[test]
fn a_pax_size_override_counts_against_the_unpacked_cap() {
    let data = vec![b'a'; 4096];
    let bytes = raw_bundle(&[
        Raw::of(tar::EntryType::XHeader, "pax", b"13 size=4096\n"),
        Raw {
            size: Some(0),
            ..Raw::file("skills/big.md", &data)
        },
    ]);
    let limits = bundle::Limits {
        unpacked_bytes: 1024,
        ..bundle::Limits::DEFAULT
    };
    let reason = too_large_reason(&bytes, limits);
    assert!(reason.contains("declare more than 1024 bytes"), "{reason}");
}

/// #8378 review: tar reads a GNU long-name record whole before `unpack` sees
/// the entry it names, so only a cap on the decompressed stream bounds it.
#[test]
fn an_oversized_extension_record_is_too_large() {
    let long_name = format!("skills/{}.md", "a".repeat(64 * 1024));
    let bytes = raw_bundle(&[
        Raw::of(
            tar::EntryType::GNULongName,
            "././@LongLink",
            long_name.as_bytes(),
        ),
        Raw::file("skills/short.md", b"x"),
    ]);
    let limits = bundle::Limits {
        unpacked_bytes: 1024,
        entries: 4,
        ..bundle::Limits::DEFAULT
    };
    let reason = too_large_reason(&bytes, limits);
    assert!(reason.contains("decompressed"), "{reason}");
}

#[cfg(unix)]
#[test]
fn lock_store_onto_a_symlink_is_a_lock_write_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("elsewhere.toml");
    std::fs::write(&target, b"untouched").expect("target");
    let path = dir.path().join(LOCK_FILE_NAME);
    std::os::unix::fs::symlink(&target, &path).expect("symlink");
    let lock = ContentLock::new(TAG, Sha256Digest::of_bytes(b"x")).expect("lock");
    match lock.store(&path) {
        Err(ContentError::LockWrite { path: failed, .. }) => assert_eq!(failed, path),
        other => panic!("expected LockWrite, got {other:?}"),
    }
    assert_eq!(std::fs::read(&target).expect("target"), b"untouched");
}

#[test]
fn read_to_string_of_invalid_utf8_is_io() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = bundle(TAG, &[("skills/bin.md", &[0xff, 0xfe][..])]);
    install(dir.path(), &bytes, &Sha256Digest::of_bytes(&bytes));
    let content = resolve(&options(dir.path(), DevOverride::Off)).expect("resolve");
    assert!(matches!(
        content.read_to_string("skills/bin.md"),
        Err(ContentError::Io { .. })
    ));
}

#[test]
fn installed_bundle_serves_its_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    install_valid(dir.path());
    let content = resolve(&options(dir.path(), DevOverride::Off)).expect("resolve");
    match content.source() {
        ContentSource::Installed { tag, bundle, .. } => {
            assert_eq!(tag, TAG);
            assert_eq!(bundle, &dir.path().join(format!("{TAG}.tar.gz")));
        }
        other => panic!("expected Installed, got {other:?}"),
    }
    assert_eq!(
        content.read_to_string("skills/tm/SKILL.md").expect("read"),
        "installed skill"
    );
    assert_eq!(
        content.list("skills").expect("list"),
        ["skills/tm/SKILL.md"]
    );
    assert!(content.list("agents").expect("list").is_empty());
}

/// #8378: a former class name fails loud in both backings; a valid class with
/// no files stays `Ok(empty)`.
#[test]
fn list_of_an_unknown_class_is_an_error_in_both_backings() {
    let dir = tempfile::tempdir().expect("tempdir");
    install_valid(dir.path());
    let installed = resolve(&options(dir.path(), DevOverride::Off)).expect("resolve");
    let checkout = tempfile::tempdir().expect("tempdir");
    make_checkout(checkout.path());
    let dev = resolve(&options(
        &checkout.path().join("no-cache"),
        DevOverride::At(checkout.path().to_path_buf()),
    ))
    .expect("resolve");
    for content in [&installed, &dev] {
        for class in ["output-styles", "output-styles/tm", "", "agentsX"] {
            assert!(
                matches!(content.list(class), Err(ContentError::UnknownClass { .. })),
                "{class:?} must be UnknownClass"
            );
        }
        assert!(content.list("agents").expect("known class").is_empty());
    }
}

#[test]
fn dev_checkout_serves_working_tree_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_checkout(dir.path());
    let content = resolve(&options(
        &dir.path().join("no-cache"),
        DevOverride::At(dir.path().to_path_buf()),
    ))
    .expect("resolve");
    assert_eq!(
        content.read_to_string("skills/tm/SKILL.md").expect("read"),
        "working-tree skill"
    );
    // Dot-files are skipped, as the packager skips them.
    assert_eq!(
        content.list("skills").expect("list"),
        ["skills/tm/SKILL.md"]
    );
    assert!(matches!(
        content.list("no-such-class"),
        Err(ContentError::UnknownClass { .. })
    ));
}

#[test]
fn dev_override_wins_over_a_valid_installed_bundle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let checkout = dir.path().join("trusty-tools");
    make_checkout(&checkout);
    let cache = dir.path().join("cache");
    install_valid(&cache);
    let nested = checkout.join("crates/trusty-mpm/src");
    let content = resolve(&options(&cache, DevOverride::DetectFrom(nested))).expect("resolve");
    assert_eq!(
        content.source(),
        &ContentSource::DevCheckout {
            root: checkout.clone()
        }
    );
    assert_eq!(
        content.read_to_string("skills/tm/SKILL.md").expect("read"),
        "working-tree skill"
    );
}

/// Fail-open check: a named checkout that is incomplete is an error, even
/// though a valid installed bundle would otherwise resolve.
#[test]
fn explicit_dev_root_that_is_not_a_checkout_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    install_valid(&cache);
    let partial = dir.path().join("partial");
    std::fs::create_dir_all(partial.join(DEV_CLASS_SOURCES[0].1)).expect("one class");
    match resolve_err(&options(&cache, DevOverride::At(partial.clone()))) {
        ContentError::NotACheckout { root, missing } => {
            assert_eq!(root, partial);
            assert_eq!(missing, partial.join(DEV_CLASS_SOURCES[1].1));
        }
        other => panic!("expected NotACheckout, got {other:?}"),
    }
}

#[test]
fn detect_outside_a_checkout_uses_the_installed_bundle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    install_valid(&cache);
    let content =
        resolve(&options(&cache, DevOverride::DetectFrom(cache.clone()))).expect("resolve");
    assert!(matches!(content.source(), ContentSource::Installed { .. }));
}

#[test]
fn dev_checkout_is_found_from_a_nested_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_checkout(dir.path());
    let nested = dir.path().join("content/skills/tm");
    assert_eq!(find_dev_checkout(&nested), Some(dir.path().to_path_buf()));
}

/// A linked worktree's `.git` is a file (`gitdir: …`), not a directory.
#[test]
fn dev_checkout_is_found_from_a_nested_directory_of_a_linked_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_checkout(dir.path());
    std::fs::remove_dir(dir.path().join(".git")).expect("rm .git");
    std::fs::write(
        dir.path().join(".git"),
        "gitdir: /elsewhere/.git/worktrees/x\n",
    )
    .expect(".git file");
    let nested = dir.path().join("content/skills/tm");
    assert_eq!(find_dev_checkout(&nested), Some(dir.path().to_path_buf()));
}

/// The real trusty-tools tree is detected as a checkout and serves agents.
#[test]
fn dev_checkout_detects_this_repository() {
    // #9298: the running checkout, not the one that built this binary.
    let workspace =
        crate::test_harness::test_repo_root().expect("the running checkout's workspace root");
    let crate_dir = workspace.join("crates/trusty-common");
    let root = find_dev_checkout(&crate_dir).expect("this repository is a checkout");
    assert_eq!(root.canonicalize().expect("root"), workspace);
    let content = resolve(&options(
        &root.join("no-cache"),
        DevOverride::DetectFrom(crate_dir),
    ))
    .expect("resolve");
    assert!(!content.list("agents").expect("agents").is_empty());
    // #8378: a former class is served from its subfolder of instructions/.
    assert!(
        content
            .list("instructions/harness_understanding")
            .expect("harness_understanding")
            .contains(&"instructions/harness_understanding/HARNESS_AGNOSTIC.md".to_owned())
    );
}

#[test]
fn dev_checkout_is_not_found_outside_a_checkout() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(DEV_CLASS_SOURCES[0].1)).expect("one class");
    assert_eq!(find_dev_checkout(dir.path()), None);
}

/// A nested repository is its own boundary: the walk never climbs past its
/// `.git` to a checkout further up.
#[test]
fn dev_checkout_stops_at_the_repository_boundary() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_checkout(dir.path());
    let inner = dir.path().join("vendor/other-repo");
    std::fs::create_dir_all(inner.join(".git")).expect("inner .git");
    std::fs::create_dir_all(inner.join("src")).expect("inner src");
    assert_eq!(find_dev_checkout(&inner.join("src")), None);
}

/// Planted class directories without `.git` (e.g. under `/tmp`) are not a
/// checkout, whether detected or named.
#[test]
fn dev_checkout_requires_a_git_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_checkout(dir.path());
    std::fs::remove_dir(dir.path().join(".git")).expect("rm .git");
    assert_eq!(find_dev_checkout(dir.path()), None);
    match dev::check_checkout(dir.path(), dev::current_euid()) {
        Err(ContentError::UntrustedCheckout { reason, .. }) => {
            assert!(reason.contains(".git"), "{reason}");
        }
        other => panic!("expected UntrustedCheckout, got {other:?}"),
    }
}

#[test]
fn dev_checkout_requires_a_workspace_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_checkout(dir.path());
    let manifest = dir.path().join("Cargo.toml");
    for body in [Some("[package]\nname = \"x\"\n"), None] {
        match body {
            Some(body) => std::fs::write(&manifest, body).expect("Cargo.toml"),
            None => std::fs::remove_file(&manifest).expect("rm Cargo.toml"),
        }
        assert_eq!(find_dev_checkout(dir.path()), None, "{body:?}");
    }
}

/// A checkout another user owns is refused (git's `safe.directory` rule).
/// The owner is faked by asking as a different effective uid.
#[cfg(unix)]
#[test]
fn dev_checkout_refuses_a_root_owned_by_another_user() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_checkout(dir.path());
    let me = dev::current_euid().expect("unix euid");
    assert_eq!(
        dev::find_dev_checkout_as(dir.path(), Some(me)),
        Some(dir.path().to_path_buf())
    );
    let other = me.wrapping_add(1);
    assert_eq!(dev::find_dev_checkout_as(dir.path(), Some(other)), None);
    match dev::check_checkout(dir.path(), Some(other)) {
        Err(ContentError::UntrustedCheckout { reason, .. }) => {
            assert!(reason.contains("owned by uid"), "{reason}");
        }
        other => panic!("expected UntrustedCheckout, got {other:?}"),
    }
}

/// #8378 review: owning the root is not enough when another user could have
/// planted `.git` or `Cargo.toml` in it. A fake uid provider stands in for a
/// `chown`, which needs root.
#[cfg(unix)]
#[test]
fn dev_checkout_refuses_a_marker_owned_by_another_user() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_checkout(dir.path());
    let me = dev::current_euid().expect("unix euid");
    assert!(dev::check_checkout_with(dir.path(), Some(me), &dev::file_owner).is_ok());
    for marker in [".git", "Cargo.toml"] {
        let owner = move |path: &Path, meta: &std::fs::Metadata| {
            if path.ends_with(marker) {
                Some(me.wrapping_add(1))
            } else {
                dev::file_owner(path, meta)
            }
        };
        match dev::check_checkout_with(dir.path(), Some(me), &owner) {
            Err(ContentError::UntrustedCheckout { reason, .. }) => {
                assert!(
                    reason.contains(marker) && reason.contains("owned by uid"),
                    "{reason}"
                );
            }
            other => panic!("{marker}: expected UntrustedCheckout, got {other:?}"),
        }
    }
}

/// #8378 review: a root every user can write to is refused even when the
/// current user owns it — the `/tmp` shape, sticky bit or not. Group write is
/// allowed: a user-private-group umask of 002 sets it on ordinary checkouts.
#[cfg(unix)]
#[test]
fn dev_checkout_refuses_a_world_writable_root() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    make_checkout(dir.path());
    let chmod = |mode| {
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).expect("chmod")
    };
    chmod(0o775);
    assert_eq!(
        find_dev_checkout(dir.path()),
        Some(dir.path().to_path_buf())
    );
    for mode in [0o777, 0o1777] {
        chmod(mode);
        assert_eq!(find_dev_checkout(dir.path()), None, "{mode:o}");
        match dev::check_checkout(dir.path(), dev::current_euid()) {
            Err(ContentError::UntrustedCheckout { reason, .. }) => {
                assert!(reason.contains("writable by every user"), "{reason}");
            }
            other => panic!("{mode:o}: expected UntrustedCheckout, got {other:?}"),
        }
    }
    chmod(0o700);
}

/// Fail-closed check: a named tree with every class but no `.git` is an
/// error, not a fall-through to the installed bundle.
#[test]
fn explicit_dev_root_without_a_git_marker_is_untrusted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    install_valid(&cache);
    let root = dir.path().join("planted");
    make_checkout(&root);
    std::fs::remove_dir(root.join(".git")).expect("rm .git");
    match resolve_err(&options(&cache, DevOverride::At(root.clone()))) {
        ContentError::UntrustedCheckout { root: refused, .. } => assert_eq!(refused, root),
        other => panic!("expected UntrustedCheckout, got {other:?}"),
    }
}

/// Dev mode serves what a bundle could hold: a regular file reached through
/// real directories. A symlink, a directory or a dot-file is `NotFound`.
#[cfg(unix)]
#[test]
fn dev_read_serves_only_regular_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_checkout(dir.path());
    let skills = dir.path().join("content/skills");
    std::os::unix::fs::symlink(dir.path().join("Cargo.toml"), skills.join("link.md"))
        .expect("file symlink");
    std::os::unix::fs::symlink(skills.join("tm"), skills.join("linked-dir")).expect("dir symlink");
    std::fs::create_dir_all(skills.join(".cache")).expect("dot-dir");
    std::fs::write(skills.join(".cache/a.md"), b"x").expect("file in dot-dir");
    let content = resolve(&options(
        &dir.path().join("no-cache"),
        DevOverride::At(dir.path().to_path_buf()),
    ))
    .expect("resolve");
    assert!(content.read("skills/tm/SKILL.md").is_ok());
    // #8378 review: dot-files never reach a bundle, so dev mode hides them too.
    for bad in [
        "skills/link.md",
        "skills/linked-dir/SKILL.md",
        "skills/tm",
        "skills/tm/.hidden",
        "skills/.cache/a.md",
    ] {
        assert!(
            matches!(content.read(bad), Err(ContentError::NotFound { .. })),
            "{bad:?} must be NotFound"
        );
    }
}

#[test]
fn read_rejects_a_climbing_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_checkout(dir.path());
    let content = resolve(&options(
        &dir.path().join("no-cache"),
        DevOverride::At(dir.path().to_path_buf()),
    ))
    .expect("resolve");
    for bad in ["../Cargo.toml", "skills/../../x", "/etc/passwd", ""] {
        assert!(
            matches!(content.read(bad), Err(ContentError::InvalidPath { .. })),
            "{bad:?} must be InvalidPath"
        );
    }
}

#[test]
fn read_of_an_absent_path_is_not_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    install_valid(dir.path());
    let content = resolve(&options(dir.path(), DevOverride::Off)).expect("resolve");
    assert!(matches!(
        content.read("skills/absent.md"),
        Err(ContentError::NotFound { .. })
    ));
}

/// Reads a repository script, relative to the workspace root.
fn repo_script(rel: &str) -> String {
    let path = crate::test_harness::test_repo_root()
        .expect("the running checkout's workspace root")
        .join(rel); // #9298
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// The packager's `LEGACY_SOURCES` rows as `(destination, source)`.
fn packager_table() -> Vec<(String, String)> {
    let text = repo_script("scripts/package_content.sh");
    let block = text
        .split("LEGACY_SOURCES=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("LEGACY_SOURCES block");
    block
        .lines()
        .filter_map(|line| line.trim().split_once('='))
        .map(|(dest, path)| (dest.to_owned(), path.to_owned()))
        .collect()
}

/// The dev table must name the same destinations and sources the packager
/// bundles, or a path would resolve to different files in the two modes.
/// PHASE_1 (#8387) changes both together.
#[test]
fn dev_class_table_matches_the_packager() {
    let table: Vec<(String, String)> = DEV_CLASS_SOURCES
        .iter()
        .map(|(dest, path)| ((*dest).to_owned(), (*path).to_owned()))
        .collect();
    assert_eq!(table, packager_table());
}

/// #8378 (owner ruling 2026-10-01): the bundle's top level is the three class
/// directories `scripts/check_content.py` enforces for `content/`, so every
/// packaged destination starts with one of them.
#[test]
fn packaged_destinations_lie_under_the_three_content_classes() {
    let checker = repo_script("scripts/check_content.py");
    let line = checker
        .lines()
        .find_map(|l| l.strip_prefix("CLASSES = ("))
        .expect("CLASSES tuple in check_content.py");
    let classes: Vec<&str> = line
        .trim_end_matches(')')
        .split(',')
        .map(|c| c.trim().trim_matches('"'))
        .filter(|c| !c.is_empty())
        .collect();
    assert_eq!(classes.len(), 3, "three content classes: {classes:?}");
    let outside: Vec<String> = packager_table()
        .into_iter()
        .map(|(dest, _)| dest)
        .filter(|dest| !classes.contains(&dest.split('/').next().unwrap_or_default()))
        .collect();
    assert!(
        outside.is_empty(),
        "destinations outside {classes:?}: {outside:?}"
    );
}

/// A nested destination reads from its own source and lists under its class;
/// a former class name lists nothing (#8378).
#[test]
fn dev_checkout_serves_a_nested_destination_from_its_own_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_checkout(dir.path());
    let styles = dir.path().join("content/instructions/output-styles");
    std::fs::write(styles.join("tm.md"), b"style").expect("style");
    let rules = dir.path().join("content/instructions");
    std::fs::write(rules.join("BASE.md"), b"base").expect("instruction");
    let content = resolve(&options(
        &dir.path().join("no-cache"),
        DevOverride::At(dir.path().to_path_buf()),
    ))
    .expect("resolve");
    assert_eq!(
        content
            .read_to_string("instructions/output-styles/tm.md")
            .expect("read"),
        "style"
    );
    assert_eq!(
        content.list("instructions").expect("list"),
        ["instructions/BASE.md", "instructions/output-styles/tm.md"]
    );
    assert_eq!(
        content.list("instructions/output-styles").expect("list"),
        ["instructions/output-styles/tm.md"]
    );
    assert!(matches!(
        content.list("output-styles"),
        Err(ContentError::UnknownClass { .. })
    ));
    assert!(matches!(
        content.read("output-styles/tm.md"),
        Err(ContentError::NotFound { .. })
    ));
}

/// The published seed release verifies and unpacks. Needs the real asset:
/// `gh release download content-v0.1.0 --repo bobmatnyc/trusty-tools`, then
/// `TRUSTY_CONTENT_V010_BUNDLE=<path to content-v0.1.0.tar.gz>`.
#[test]
#[ignore = "needs the published content-v0.1.0 asset on disk"]
fn resolves_the_published_content_v0_1_0_bundle() {
    const PUBLISHED: &str = "6081f903bc865b8ae72d7ea158ab48220d497ef709febb09471fb4380acb4c70";
    let src = PathBuf::from(std::env::var("TRUSTY_CONTENT_V010_BUNDLE").expect("env var set"));
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = std::fs::read(&src).expect("read asset");
    install(
        dir.path(),
        &bytes,
        &Sha256Digest::parse_hex(PUBLISHED).expect("digest"),
    );
    let content = resolve(&options(dir.path(), DevOverride::Off)).expect("resolve");
    let agents = content.list("agents").expect("agents");
    assert_eq!(agents.len(), 43, "content-v0.1.0 ships 43 agents");
}
