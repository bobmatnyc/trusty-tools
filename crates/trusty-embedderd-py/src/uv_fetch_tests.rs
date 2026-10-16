//! Unit tests for the verified uv cache (#9468) — no network, no real uv.

use super::*;
use tar::EntryType::{self, Link, Regular, Symlink};

const FAKE_UV: &[u8] = b"#!/bin/sh\necho fake-uv\n";

/// A gzip'd tar of `(path, type, data)` entries. Header names are written raw
/// so `..` and absolute paths survive (tar's `set_path` refuses them).
fn tarball(entries: &[(&str, EntryType, &[u8])]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(gz);
    for (path, kind, data) in entries {
        let mut header = tar::Header::new_gnu();
        let gnu = header.as_gnu_mut().unwrap();
        gnu.name[..path.len()].copy_from_slice(path.as_bytes());
        gnu.linkname[..11].copy_from_slice(b"/etc/passwd"); // read by link types only
        header.set_entry_type(*kind);
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

/// Write `entries` as an archive under `dir`; returns its path and true digest.
fn archive(dir: &Path, entries: &[(&str, EntryType, &[u8])]) -> (PathBuf, Sha256Digest) {
    let (path, bytes) = (dir.join("uv.tar.gz"), tarball(entries));
    fs::write(&path, &bytes).unwrap();
    (path, Sha256Digest::of_bytes(&bytes))
}

fn good_archive(dir: &Path) -> (PathBuf, Sha256Digest) {
    archive(
        dir,
        &[("uv-x/uvx", Regular, b"uvx"), ("uv-x/uv", Regular, FAKE_UV)],
    )
}

/// A temp dir, its `py-embedder` root, and a verified uv installed under it.
fn installed() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("py-embedder");
    let (path, digest) = good_archive(tmp.path());
    let placed = install_verified(&root, &path, &digest).unwrap();
    (tmp, root, placed)
}

#[test]
fn digest_match_installs_an_executable_uv() {
    let (_tmp, root, placed) = installed();
    assert_eq!(placed, cache_path(&root));
    assert_eq!(fs::read(&placed).unwrap(), FAKE_UV);
    #[cfg(unix)]
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&placed).unwrap().permissions());
    #[cfg(unix)]
    assert_ne!(mode & 0o111, 0, "the cached uv must be executable");
    assert_eq!(cached_uv(&root).unwrap(), Some(placed.clone()));
    let left = fs::read_dir(placed.parent().unwrap()).unwrap().count();
    assert_eq!(left, 3, "only .fetch.lock, uv and uv.sha256 may remain");
}

#[test]
fn digest_mismatch_places_nothing_and_names_both_digests() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("py-embedder");
    let (path, actual) = good_archive(tmp.path());
    let expected = Sha256Digest::of_bytes(b"some other archive");
    let err = install_verified(&root, &path, &expected).unwrap_err();
    let shown = err.to_string();
    let named = shown.contains(expected.as_hex()) && shown.contains(actual.as_hex());
    assert!(matches!(err, UvError::Checksum { .. }) && named, "{shown}");
    assert!(!root.exists(), "a refused archive must create nothing");
}

#[test]
fn unsafe_archive_entries_are_refused() {
    let cases: &[(&str, EntryType, &str)] = &[
        ("../uv", Regular, "escapes the archive root"),
        ("/tmp/uv", Regular, "escapes the archive root"),
        ("uv-x/uv", Symlink, "not a regular file"),
        ("uv-x/uv", Link, "not a regular file"),
        ("uv-x/uvx", Regular, "no `uv` file"),
    ];
    for (name, kind, want) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("py-embedder");
        let (path, digest) = archive(tmp.path(), &[(name, *kind, FAKE_UV)]);
        let err = install_verified(&root, &path, &digest).unwrap_err();
        assert!(err.to_string().contains(want), "{name} ({kind:?}): {err}");
        assert!(!root.exists(), "{name}: nothing may be placed");
        assert!(!tmp.path().join("uv").exists(), "{name}: wrote outside");
    }
}

#[test]
fn resolve_uv_prefers_path_then_the_verified_cache() {
    let (_tmp, root, placed) = installed();
    let on_path = PathBuf::from("/opt/homebrew/bin/uv");
    let root_fn = || -> anyhow::Result<PathBuf> { Ok(root.clone()) };
    let got = resolve_uv(None, &|| Some(on_path.clone()), &root_fn).unwrap();
    assert_eq!(got, on_path, "a PATH uv wins over a cached one");
    let got = resolve_uv(None, &|| None, &root_fn).unwrap();
    assert_eq!(got, placed, "the cached uv is used when PATH has none");
}

#[test]
fn a_tampered_cached_uv_is_refused_not_run() {
    let (_tmp, root, placed) = installed();
    fs::write(&placed, b"#!/bin/sh\nevil\n").unwrap();
    let err = resolve_uv(None, &|| None, &|| Ok(root.clone())).unwrap_err();
    assert!(
        format!("{err:#}").contains("not run: sha256 mismatch"),
        "{err:#}"
    );
}

#[test]
fn nothing_installed_or_cached_says_the_fetch_is_not_yet_available() {
    let tmp = tempfile::tempdir().unwrap();
    let err = resolve_uv(None, &|| None, &|| Ok(tmp.path().to_path_buf())).unwrap_err();
    assert!(err.to_string().contains("not yet available"), "{err}");
}

#[test]
fn pinned_table_parses_and_an_unknown_host_has_none() {
    for (triple, _) in PINNED_DIGESTS {
        assert!(pinned_digest(triple).is_some(), "{triple}: bad digest");
    }
    assert!(pinned_digest("riscv64gc-unknown-linux-gnu").is_none());
    let tmp = tempfile::tempdir().unwrap();
    let (path, _) = good_archive(tmp.path());
    let pinned = pinned_digest("x86_64-unknown-linux-gnu").unwrap();
    let err = install_verified(tmp.path(), &path, &pinned);
    assert!(matches!(err, Err(UvError::Checksum { .. })));
}

/// The sorted entry names in the uv cache dir under `root`.
fn leftovers(root: &Path) -> Vec<String> {
    let dir = cache_path(root).parent().unwrap().to_path_buf();
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The cache dir under a fresh `py-embedder` root, with `stale` planted in it.
fn root_with_stale(tmp: &Path, stale: &[&str]) -> PathBuf {
    let root = tmp.join("py-embedder");
    let dir = cache_path(&root).parent().unwrap().to_path_buf();
    fs::create_dir_all(&dir).unwrap();
    for name in stale {
        fs::write(dir.join(name), b"half-written uv").unwrap();
    }
    root
}

#[test]
fn resolve_uv_prefers_explicit_override_and_never_fetches() {
    let (tmp, root, _placed) = installed();
    let explicit = tmp.path().join("my-uv");
    fs::write(&explicit, b"x").unwrap();
    // The cache root is the only road to the cache (and, in B2, a fetch).
    let asked = std::cell::Cell::new(false);
    let root_fn = || -> anyhow::Result<PathBuf> {
        asked.set(true);
        Ok(root.clone())
    };
    let on_path = || Some(PathBuf::from("/opt/homebrew/bin/uv"));
    let value = explicit.to_str().unwrap();
    let got = resolve_uv(Some(value), &on_path, &root_fn).unwrap();
    assert_eq!(
        got, explicit,
        "TRUSTY_UV_BIN wins over installed and cached"
    );
    assert!(!asked.get(), "an explicit uv must not consult the cache");
}

#[test]
fn concurrent_fetches_download_once_and_place_a_whole_file() {
    // B1 has no download: racing installs on one root must all succeed and
    // leave one whole, verified uv. Without the flock they share a pid, so
    // one thread's temp file is removed or renamed under another.
    const THREADS: usize = 4;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("py-embedder");
    let payload = vec![7u8; 1 << 20];
    let (path, digest) = archive(tmp.path(), &[("uv-x/uv", Regular, &payload)]);
    let barrier = std::sync::Barrier::new(THREADS);
    for round in 0..8 {
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    s.spawn(|| {
                        barrier.wait();
                        install_verified(&root, &path, &digest).map_err(|e| e.to_string())
                    })
                })
                .collect();
            for h in handles {
                assert_eq!(h.join().unwrap(), Ok(cache_path(&root)), "round {round}");
            }
        });
    }
    assert_eq!(cached_uv(&root).unwrap(), Some(cache_path(&root)));
    assert_eq!(fs::read(cache_path(&root)).unwrap(), payload);
    assert_eq!(leftovers(&root), [".fetch.lock", "uv", "uv.sha256"]);
}

#[test]
fn stale_temp_files_are_swept_before_a_fetch() {
    let tmp = tempfile::tempdir().unwrap();
    // What a SIGKILL mid-write by other pids leaves behind.
    let root = root_with_stale(tmp.path(), &[".uv.tmp.99999", ".uv.tmp.12345"]);
    let (path, digest) = good_archive(tmp.path());
    let placed = install_verified(&root, &path, &digest).unwrap();
    assert_eq!(fs::read(&placed).unwrap(), FAKE_UV);
    assert_eq!(leftovers(&root), [".fetch.lock", "uv", "uv.sha256"]);
}

#[test]
fn a_stale_temp_that_cannot_be_removed_stops_the_install() {
    let tmp = tempfile::tempdir().unwrap();
    let root = root_with_stale(tmp.path(), &[]);
    // `remove_file` refuses a directory on every platform.
    let stuck = cache_path(&root).with_file_name(".uv.tmp.4242");
    fs::create_dir(&stuck).unwrap();
    let (path, digest) = good_archive(tmp.path());
    let err = install_verified(&root, &path, &digest).unwrap_err();
    let shown = err.to_string();
    assert!(shown.contains("remove stale uv temp file"), "{shown}");
    assert!(!cache_path(&root).exists(), "nothing may be placed");
}
