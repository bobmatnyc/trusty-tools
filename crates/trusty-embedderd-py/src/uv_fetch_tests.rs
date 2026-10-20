//! Unit tests for the verified uv cache and fetch (#9468, #9966). Only the
//! `#[ignore]`d `real_pinned_asset_matches_the_embedded_digest` leaves
//! loopback; no test here mutates the process environment.

use super::*;
use std::io::BufRead as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tar::EntryType::{self, Link, Regular, Symlink};

const FAKE_UV: &[u8] = b"#!/bin/sh\necho fake-uv\n";

/// A fetcher for paths that must never download.
fn no_fetch(url: &str) -> Result<Vec<u8>, UvError> {
    panic!("fetched {url} where no download may happen")
}

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
    let fetch = Some(&no_fetch as Fetcher<'_>);
    let got = resolve_uv(None, &|| Some(on_path.clone()), &root_fn, fetch).unwrap();
    assert_eq!(got, on_path, "a PATH uv wins over a cached one");
    let got = resolve_uv(None, &|| None, &root_fn, fetch).unwrap();
    assert_eq!(got, placed, "the cached uv is used when PATH has none");
}

#[test]
fn a_tampered_cached_uv_is_refused_not_run() {
    let (_tmp, root, placed) = installed();
    fs::write(&placed, b"#!/bin/sh\nevil\n").unwrap();
    // With the fetch off a refused cache is the error (#9966, 4c).
    let err = resolve_uv(None, &|| None, &|| Ok(root.clone()), None).unwrap_err();
    assert!(
        format!("{err:#}").contains("not run: sha256 mismatch"),
        "{err:#}"
    );
}

#[test]
fn disabled_fetch_names_both_variables_and_never_fetches() {
    // #9966 (criterion 2): `TRUSTY_UV_FETCH=0` is `fetch: None`.
    let tmp = tempfile::tempdir().unwrap();
    let err = resolve_uv(None, &|| None, &|| Ok(tmp.path().to_path_buf()), None).unwrap_err();
    let shown = err.to_string();
    let disabled = matches!(
        err.downcast_ref::<UvError>(),
        Some(UvError::Disabled { .. })
    );
    assert!(disabled, "{shown}");
    assert!(
        shown.contains("TRUSTY_UV_FETCH=0") && shown.contains("TRUSTY_UV_BIN"),
        "{shown}"
    );
    assert!(
        !tmp.path().join("uv").exists(),
        "the opt-out must not touch the cache"
    );
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
    let got = resolve_uv(Some(value), &on_path, &root_fn, Some(&no_fetch)).unwrap();
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

// ── #9966: the network half ────────────────────────────────────────────────

/// A fetcher serving `bytes` and counting its calls in `calls`.
fn serving<'a>(
    bytes: &'a [u8],
    calls: &'a AtomicUsize,
) -> impl Fn(&str) -> Result<Vec<u8>, UvError> + Sync + 'a {
    move |_url| {
        calls.fetch_add(1, Ordering::SeqCst);
        // Long enough that every racing fetcher is queued on the lock.
        std::thread::sleep(Duration::from_millis(50));
        Ok(bytes.to_vec())
    }
}

/// A gzip'd tar holding one regular `uv-x/uv` with `data`, and its digest.
fn uv_tarball(data: &[u8]) -> (Vec<u8>, Sha256Digest) {
    let bytes = tarball(&[("uv-x/uv", Regular, data)]);
    let digest = Sha256Digest::of_bytes(&bytes);
    (bytes, digest)
}

fn offline(url: &str) -> Result<Vec<u8>, UvError> {
    let (url, reason) = (url.to_owned(), "offline".to_owned());
    Err(UvError::Network { url, reason })
}

#[test]
fn fetch_enabled_parses_the_off_switch() {
    for off in ["0", "false", "OFF", " no "] {
        assert!(!fetch_enabled(Some(off)), "{off:?} must turn the fetch off");
    }
    for on in [None, Some("1"), Some("true"), Some("")] {
        assert!(fetch_enabled(on), "{on:?} must leave the fetch on");
    }
}

#[test]
fn an_oversized_archive_is_refused_before_it_is_read() {
    // #9966 (4a): a sparse file one byte over the bound; reading it would
    // buffer 64 MiB and then fail the checksum instead.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("py-embedder");
    let path = tmp.path().join("huge.tar.gz");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_ARCHIVE_BYTES + 1).unwrap();
    let digest = Sha256Digest::of_bytes(b"anything");
    let err = install_verified(&root, &path, &digest).unwrap_err();
    assert!(matches!(err, UvError::Archive { .. }), "{err:?}");
    assert!(err.to_string().contains("not read"), "{err}");
    assert!(!root.exists(), "a refused archive must create nothing");
}

#[test]
fn a_fetch_that_waited_on_the_lock_reuses_the_cache() {
    // #9966 (4b): every racer but the first must find the cache under the lock.
    const THREADS: usize = 4;
    let tmp = tempfile::tempdir().unwrap();
    let (bytes, digest) = uv_tarball(FAKE_UV);
    let calls = AtomicUsize::new(0);
    let fetch = serving(&bytes, &calls);
    let barrier = std::sync::Barrier::new(THREADS);
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                s.spawn(|| {
                    barrier.wait();
                    fetch_verified(tmp.path(), "https://x/uv.tar.gz", &digest, &fetch)
                        .map_err(|e| e.to_string())
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), Ok(cache_path(tmp.path())));
        }
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1, "downloaded more than once");
    assert_eq!(leftovers(tmp.path()), [".fetch.lock", "uv", "uv.sha256"]);
}

#[test]
fn a_refused_cache_is_replaced_under_the_lock() {
    // #9966 (4c): a tampered binary and a torn `uv.sha256` are each replaced
    // by a verified download; neither wedges the bootstrap nor runs.
    for torn in [false, true] {
        let (_tmp, root, placed) = installed();
        if torn {
            fs::write(placed.with_file_name("uv.sha256"), b"50487a").unwrap();
        } else {
            fs::write(&placed, b"#!/bin/sh\nevil\n").unwrap();
        }
        assert!(
            cached_uv(&root).is_err(),
            "torn={torn}: the plant must be refused"
        );
        let (bytes, digest) = uv_tarball(b"the replacement uv");
        let calls = AtomicUsize::new(0);
        let fetch = serving(&bytes, &calls);
        let got = fetch_verified(&root, "https://x/uv.tar.gz", &digest, &fetch);
        assert_eq!(got.unwrap(), placed, "torn={torn}");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "torn={torn}");
        assert_eq!(
            cached_uv(&root).unwrap(),
            Some(placed.clone()),
            "torn={torn}"
        );
        assert_eq!(fs::read(&placed).unwrap(), b"the replacement uv");
    }
}

#[test]
fn a_refused_cache_is_refetched_with_the_fetch_on_and_refused_with_it_off() {
    // #9966 (4c) at the resolver: on → the fetch runs; off → `CacheRejected`.
    let (_tmp, root, placed) = installed();
    fs::write(&placed, b"#!/bin/sh\nevil\n").unwrap();
    let root_fn = || -> anyhow::Result<PathBuf> { Ok(root.clone()) };
    let calls = AtomicUsize::new(0);
    let failing = |url: &str| {
        calls.fetch_add(1, Ordering::SeqCst);
        offline(url)
    };
    let refused = |e: &anyhow::Error| {
        matches!(
            e.downcast_ref::<UvError>(),
            Some(UvError::CacheRejected { .. })
        )
    };
    let err = resolve_uv(None, &|| None, &root_fn, Some(&failing)).unwrap_err();
    let want_calls = usize::from(pinned_digest(&host_triple()).is_some());
    assert_eq!(calls.load(Ordering::SeqCst), want_calls, "{err:#}");
    assert!(!refused(&err), "with the fetch on: {err:#}");
    let err = resolve_uv(None, &|| None, &root_fn, None).unwrap_err();
    assert!(refused(&err), "with the fetch off: {err:#}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        want_calls,
        "the opt-out must not fetch"
    );
    assert_eq!(
        fs::read(&placed).unwrap(),
        b"#!/bin/sh\nevil\n",
        "nothing placed"
    );
}

#[test]
fn a_network_error_or_bad_digest_places_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let digest = Sha256Digest::of_bytes(b"x");
    let err = fetch_verified(tmp.path(), "https://x/uv.tar.gz", &digest, &offline).unwrap_err();
    assert!(
        err.to_string().contains("TRUSTY_UV_BIN"),
        "remediation: {err}"
    );
    let (bytes, _) = uv_tarball(FAKE_UV);
    let calls = AtomicUsize::new(0);
    let fetch = serving(&bytes, &calls);
    let err = fetch_verified(tmp.path(), "https://x/uv.tar.gz", &digest, &fetch);
    assert!(matches!(err, Err(UvError::Checksum { .. })), "{err:?}");
    assert_eq!(leftovers(tmp.path()), [".fetch.lock"]);
}

#[test]
fn an_unsupported_host_never_fetches() {
    let tmp = tempfile::tempdir().unwrap();
    let err = fetch_for_triple(tmp.path(), "riscv64gc-unknown-linux-gnu", &no_fetch).unwrap_err();
    assert!(matches!(err, UvError::UnsupportedHost { .. }), "{err:?}");
    assert!(err.to_string().contains("TRUSTY_UV_BIN"), "{err}");
}

/// A one-shot loopback HTTP/1.1 server: sends `head`, then each of `chunks`
/// after `gap`, then holds the connection open for `hold`. Returns its URL.
fn serve_once(head: &str, chunks: Vec<Vec<u8>>, gap: Duration, hold: Duration) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let head = head.to_owned();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut reader = io::BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|n| n > 0) && line != "\r\n" {
            line.clear();
        }
        if stream.write_all(head.as_bytes()).is_err() {
            return;
        }
        for chunk in chunks {
            std::thread::sleep(gap);
            if stream
                .write_all(&chunk)
                .and_then(|()| stream.flush())
                .is_err()
            {
                return;
            }
        }
        std::thread::sleep(hold);
    });
    format!("http://{addr}/uv.tar.gz")
}

fn loopback(budget: Duration, max_bytes: u64) -> http::Limits {
    let https_only = false;
    http::Limits {
        budget,
        max_bytes,
        https_only,
    }
}

#[test]
fn a_download_trickling_past_the_budget_times_out_and_places_nothing() {
    // #9966 (criterion 1): 1 KiB every 100 ms never trips a per-read timeout,
    // but the ~1.2 s body overruns a 300 ms total budget.
    let head = "HTTP/1.1 200 OK\r\nContent-Length: 12288\r\nConnection: close\r\n\r\n";
    let gap = Duration::from_millis(100);
    let url = serve_once(head, vec![vec![0u8; 1024]; 12], gap, Duration::ZERO);
    let tmp = tempfile::tempdir().unwrap();
    let limits = loopback(Duration::from_millis(300), 1 << 20);
    let fetch = |_: &str| http::http_fetch_with(&url, limits);
    let started = Instant::now();
    let digest = Sha256Digest::of_bytes(b"never matches");
    let err = fetch_verified(tmp.path(), &url, &digest, &fetch).unwrap_err();
    let elapsed = started.elapsed();
    assert!(matches!(err, UvError::Network { .. }), "{err:?}");
    assert!(err.to_string().contains("timed out"), "{err}");
    assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");
    assert_eq!(leftovers(tmp.path()), [".fetch.lock"]);
}

#[test]
fn an_oversized_download_is_refused_before_its_body_is_buffered() {
    // #9966 (4a): a declared oversize body is refused on its header, with the
    // body never sent; an undeclared one is cut one byte past the bound.
    let budget = Duration::from_secs(5);
    let limits = loopback(budget, 1024);
    let declared = "HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\n";
    let url = serve_once(declared, vec![], Duration::ZERO, budget);
    let started = Instant::now();
    let err = http::http_fetch_with(&url, limits).unwrap_err();
    let want = "4096 bytes, over the 1024 byte bound";
    assert!(err.to_string().contains(want), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "it waited for the body"
    );
    let undeclared = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
    let url = serve_once(
        undeclared,
        vec![vec![0u8; 4096]],
        Duration::ZERO,
        Duration::ZERO,
    );
    let err = http::http_fetch_with(&url, limits).unwrap_err();
    let want = "1025 bytes, over the 1024 byte bound";
    assert!(err.to_string().contains(want), "{err}");
}

#[test]
fn plain_http_is_refused_in_production() {
    // #9966 (criterion 1): the production limits allow HTTPS only.
    let err = http::http_fetch("http://127.0.0.1:9/uv.tar.gz").unwrap_err();
    assert!(err.to_string().contains("scheme is not allowed"), "{err}");
}

/// Downloads the real pinned asset for this host (#9966, AC4): a wrong digest
/// must refuse it and place nothing; the pinned one must place a uv that runs
/// and, on macOS, carries no quarantine xattr. Network, so `#[ignore]`.
#[test]
#[ignore = "network: downloads the real pinned uv release (#9966)"]
fn real_pinned_asset_matches_the_embedded_digest() {
    let triple = host_triple();
    let url = asset_url(&triple);
    let bytes = http::http_fetch(&url).unwrap_or_else(|e| panic!("download {url}: {e}"));
    let calls = AtomicUsize::new(0);
    let fetch = serving(&bytes, &calls);
    let tmp = tempfile::tempdir().unwrap();
    let wrong = Sha256Digest::of_bytes(b"not the pinned archive");
    let err = fetch_verified(tmp.path(), &url, &wrong, &fetch).unwrap_err();
    assert!(matches!(err, UvError::Checksum { .. }), "{err:?}");
    assert!(
        !cache_path(tmp.path()).exists(),
        "a wrong digest placed a uv"
    );
    let placed = fetch_for_triple(tmp.path(), &triple, &fetch).unwrap();
    let out = std::process::Command::new(&placed)
        .arg("--version")
        .output()
        .unwrap();
    let version = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && version.contains(UV_VERSION),
        "{out:?}"
    );
    if cfg!(target_os = "macos") {
        let x = std::process::Command::new("xattr")
            .args(["-p", "com.apple.quarantine"])
            .arg(&placed)
            .output()
            .unwrap();
        assert!(!x.status.success(), "quarantine xattr present: {x:?}");
    }
}
