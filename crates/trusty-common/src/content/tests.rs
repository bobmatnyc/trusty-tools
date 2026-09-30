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

/// Creates a checkout holding every class directory, plus one skill file.
fn make_checkout(root: &Path) {
    for (_, rel) in DEV_CLASS_SOURCES {
        std::fs::create_dir_all(root.join(rel)).expect("class dir");
    }
    let skill = root.join("crates/trusty-mpm/src/assets/skills/tm");
    std::fs::create_dir_all(&skill).expect("skill dir");
    std::fs::write(skill.join("SKILL.md"), b"working-tree skill").expect("skill");
    std::fs::write(skill.join(".hidden"), b"x").expect("dot-file");
}

fn options(cache: &Path, dev: DevOverride) -> ResolveOptions {
    ResolveOptions {
        cache_dir: cache.to_path_buf(),
        dev,
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
    // `tar::Builder::append_data` refuses `..`, so write the raw header name.
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    // A valid manifest, so the climbing entry is the only defect.
    let manifest = format!("tag = \"{TAG}\"\n");
    let mut manifest_header = tar::Header::new_gnu();
    manifest_header.set_size(manifest.len() as u64);
    manifest_header.set_mode(0o644);
    tar.append_data(
        &mut manifest_header,
        bundle::MANIFEST_ENTRY,
        manifest.as_bytes(),
    )
    .expect("manifest");
    let data = b"escaped";
    let mut header = tar::Header::new_gnu();
    let name = b"../escape.md";
    header.as_gnu_mut().expect("gnu").name[..name.len()].copy_from_slice(name);
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    tar.append(&header, &data[..]).expect("append");
    let bytes = tar.into_inner().expect("tar").finish().expect("gzip");

    let dir = tempfile::tempdir().expect("tempdir");
    install(dir.path(), &bytes, &Sha256Digest::of_bytes(&bytes));
    assert!(matches!(
        resolve_err(&options(dir.path(), DevOverride::Off)),
        ContentError::BundleCorrupt { .. }
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
    assert!(content.list("no-such-class").expect("list").is_empty());
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
    let nested = dir.path().join("crates/trusty-mpm/src/assets/skills/tm");
    assert_eq!(find_dev_checkout(&nested), Some(dir.path().to_path_buf()));
}

/// The real trusty-tools tree is detected as a checkout and serves agents.
#[test]
fn dev_checkout_detects_this_repository() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = find_dev_checkout(crate_dir).expect("this repository is a checkout");
    assert_eq!(
        root.canonicalize().expect("root"),
        crate_dir
            .join("../..")
            .canonicalize()
            .expect("workspace root")
    );
    let content = resolve(&options(
        &root.join("no-cache"),
        DevOverride::DetectFrom(crate_dir.to_path_buf()),
    ))
    .expect("resolve");
    assert!(!content.list("agents").expect("agents").is_empty());
}

#[test]
fn dev_checkout_is_not_found_outside_a_checkout() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(DEV_CLASS_SOURCES[0].1)).expect("one class");
    assert_eq!(find_dev_checkout(dir.path()), None);
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

/// The dev table must name the same class directories the packager bundles,
/// or a path would resolve to different files in the two modes. PHASE_1
/// (#8387) changes both together.
#[test]
fn dev_class_table_matches_the_packager() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/package_content.sh");
    let text = std::fs::read_to_string(&script).expect("read package_content.sh");
    let block = text
        .split("LEGACY_SOURCES=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("LEGACY_SOURCES block");
    let packager: Vec<(String, String)> = block
        .lines()
        .filter_map(|line| line.trim().split_once('='))
        .map(|(class, path)| (class.to_owned(), path.to_owned()))
        .collect();
    let table: Vec<(String, String)> = DEV_CLASS_SOURCES
        .iter()
        .map(|(class, path)| ((*class).to_owned(), (*path).to_owned()))
        .collect();
    assert_eq!(table, packager);
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
