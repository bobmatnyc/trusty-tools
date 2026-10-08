//! Unit tests for `uv_fetch` (#9468) — injected fetcher, no network except the
//! one `#[ignore]` test. No test here mutates the process environment: every
//! env-derived input is passed through `UvSources`, so nothing needs `#[serial]`
//! or the child-process isolation `bootstrap_tests.rs` uses.

use super::http::{http_fetch_within, network};
use super::*;
use std::io::{BufRead as _, Write as _};
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

/// Build a `.tar.gz` in memory from `(path, contents)` regular-file entries.
fn tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(gz);
    for (path, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append_data(&mut header, path, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

/// A fetcher serving `bytes` and counting its calls.
fn serving(
    bytes: Vec<u8>,
    calls: &AtomicUsize,
) -> impl Fn(&str) -> Result<Vec<u8>, UvError> + Sync + '_ {
    move |_url| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(bytes.clone())
    }
}

/// Files in the cache dir other than the fetch lock — proves no temp file leaks.
fn leftovers(py_root: &Path) -> Vec<String> {
    let dir = cache_path(py_root).parent().unwrap().to_path_buf();
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != ".fetch.lock")
        .collect()
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn no_path() -> Option<PathBuf> {
    None
}

#[test]
fn every_pinned_digest_parses() {
    for (triple, _) in PINNED_DIGESTS {
        assert!(
            pinned_digest(triple).is_some(),
            "{triple} digest is malformed"
        );
    }
}

#[test]
fn fetch_enabled_parses_the_off_switch() {
    assert!(fetch_enabled(None));
    assert!(fetch_enabled(Some("1")));
    for off in ["0", "false", "OFF", " no "] {
        assert!(!fetch_enabled(Some(off)), "{off:?} must disable the fetch");
    }
}

#[test]
fn a_digest_mismatch_places_nothing_and_names_the_checksum() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = AtomicUsize::new(0);
    let fetch = serving(tarball(&[("uv-x/uv", b"binary")]), &calls);
    let wrong = Sha256Digest::of_bytes(b"something else");

    let err = fetch_verified(tmp.path(), "aarch64-apple-darwin", &wrong, &fetch).unwrap_err();

    assert!(matches!(err, UvError::Checksum { .. }), "got {err:?}");
    assert!(err.to_string().contains("checksum"), "got: {err}");
    assert!(
        !cache_path(tmp.path()).exists(),
        "a mismatched tarball must place nothing"
    );
    assert_eq!(leftovers(tmp.path()), Vec::<String>::new());
}

#[test]
fn extraction_finds_uv_by_basename_under_any_prefix() {
    for prefix in ["", "uv-aarch64-apple-darwin/", "a/b/c/"] {
        let tmp = tempfile::tempdir().unwrap();
        let calls = AtomicUsize::new(0);
        let uvx = format!("{prefix}uvx");
        let uv = format!("{prefix}uv");
        let bytes = tarball(&[(&uvx, b"not me"), (&uv, b"the uv binary")]);
        let digest = Sha256Digest::of_bytes(&bytes);
        let fetch = serving(bytes, &calls);

        let placed = fetch_verified(tmp.path(), "aarch64-apple-darwin", &digest, &fetch)
            .unwrap_or_else(|e| panic!("prefix {prefix:?}: {e}"));

        assert_eq!(placed, cache_path(tmp.path()));
        assert_eq!(
            fs::read(&placed).unwrap(),
            b"the uv binary",
            "prefix {prefix:?}"
        );
        assert_eq!(mode(&placed), 0o755);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(leftovers(tmp.path()), vec!["uv".to_owned()]);
    }
}

#[test]
fn a_tarball_without_uv_places_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = AtomicUsize::new(0);
    let bytes = tarball(&[("uv-x/uvx", b"only uvx")]);
    let digest = Sha256Digest::of_bytes(&bytes);
    let fetch = serving(bytes, &calls);

    let err = fetch_verified(tmp.path(), "aarch64-apple-darwin", &digest, &fetch).unwrap_err();

    assert!(matches!(err, UvError::Archive { .. }), "got {err:?}");
    assert!(!cache_path(tmp.path()).exists());
    assert_eq!(leftovers(tmp.path()), Vec::<String>::new());
}

#[test]
fn a_network_error_places_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let fetch = |url: &str| Err(network(url, "connection refused".to_owned()));
    let digest = Sha256Digest::of_bytes(b"x");

    let err = fetch_verified(tmp.path(), "aarch64-apple-darwin", &digest, &fetch).unwrap_err();

    assert!(err.to_string().contains("connection refused"), "got: {err}");
    assert!(!cache_path(tmp.path()).exists());
}

#[test]
fn an_unsupported_host_never_fetches() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = AtomicUsize::new(0);
    let fetch = serving(Vec::new(), &calls);

    let err = fetch_for_triple(tmp.path(), "riscv64-linux", &fetch).unwrap_err();

    assert!(
        matches!(err, UvError::UnsupportedHost { .. }),
        "got {err:?}"
    );
    assert!(err.to_string().contains("riscv64-linux"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!cache_path(tmp.path()).exists());
}

#[test]
fn resolve_uv_prefers_explicit_override_and_never_fetches() {
    let tmp = tempfile::tempdir().unwrap();
    let explicit = tmp.path().join("my-uv");
    fs::write(&explicit, b"x").unwrap();
    let calls = AtomicUsize::new(0);
    let fetch = serving(Vec::new(), &calls);
    let on_path = || Some(PathBuf::from("/should/not/win"));
    let src = UvSources {
        explicit: Some(explicit.to_string_lossy().into_owned()),
        on_path: &on_path,
        fetch_enabled: true,
        fetch: &fetch,
    };

    assert_eq!(resolve_uv(tmp.path(), &src).unwrap(), explicit);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn resolve_uv_bad_override_errors_without_fetching() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = AtomicUsize::new(0);
    let fetch = serving(Vec::new(), &calls);
    let src = UvSources {
        explicit: Some("/nonexistent/uv/binary".to_owned()),
        on_path: &no_path,
        fetch_enabled: true,
        fetch: &fetch,
    };

    let err = resolve_uv(tmp.path(), &src).unwrap_err();

    assert!(err
        .to_string()
        .contains("does not point to an existing file"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn resolve_uv_prefers_path_and_never_fetches() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = AtomicUsize::new(0);
    let fetch = serving(Vec::new(), &calls);
    let on_path = || Some(PathBuf::from("/opt/homebrew/bin/uv"));
    let src = UvSources {
        explicit: None,
        on_path: &on_path,
        fetch_enabled: true,
        fetch: &fetch,
    };

    assert_eq!(
        resolve_uv(tmp.path(), &src).unwrap(),
        PathBuf::from("/opt/homebrew/bin/uv")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_cache_hit_never_fetches() {
    let tmp = tempfile::tempdir().unwrap();
    let cached = cache_path(tmp.path());
    fs::create_dir_all(cached.parent().unwrap()).unwrap();
    fs::write(&cached, b"cached uv").unwrap();
    set_executable(&cached).unwrap();
    let calls = AtomicUsize::new(0);
    let fetch = serving(Vec::new(), &calls);
    // The cache is consulted before the off switch: TRUSTY_UV_FETCH=0 still
    // uses an already-verified copy.
    for fetch_enabled in [true, false] {
        let src = UvSources {
            explicit: None,
            on_path: &no_path,
            fetch_enabled,
            fetch: &fetch,
        };
        assert_eq!(resolve_uv(tmp.path(), &src).unwrap(), cached);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_non_executable_cache_file_is_not_a_hit() {
    let tmp = tempfile::tempdir().unwrap();
    let cached = cache_path(tmp.path());
    fs::create_dir_all(cached.parent().unwrap()).unwrap();
    fs::write(&cached, b"not executable").unwrap();

    assert_eq!(cached_uv(tmp.path()), None);
}

#[test]
fn disabled_fetch_names_both_variables_and_never_fetches() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = AtomicUsize::new(0);
    let fetch = serving(Vec::new(), &calls);
    let src = UvSources {
        explicit: None,
        on_path: &no_path,
        fetch_enabled: fetch_enabled(Some("0")),
        fetch: &fetch,
    };

    let err = resolve_uv(tmp.path(), &src).unwrap_err().to_string();

    assert!(err.contains("TRUSTY_UV_FETCH"), "got: {err}");
    assert!(err.contains("TRUSTY_UV_BIN"), "got: {err}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!cache_path(tmp.path()).exists());
}

#[test]
fn concurrent_fetches_download_once_and_place_a_whole_file() {
    let tmp = tempfile::tempdir().unwrap();
    let payload = vec![7u8; 256 * 1024];
    let bytes = tarball(&[("uv-x/uv", &payload)]);
    let digest = Sha256Digest::of_bytes(&bytes);
    let calls = AtomicUsize::new(0);
    let fetch = |_url: &str| {
        calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        Ok(bytes.clone())
    };

    std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                s.spawn(|| fetch_verified(tmp.path(), "aarch64-apple-darwin", &digest, &fetch))
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap().unwrap(), cache_path(tmp.path()));
        }
    });

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the flock must serialize to one download"
    );
    assert_eq!(fs::read(cache_path(tmp.path())).unwrap(), payload);
    assert_eq!(leftovers(tmp.path()), vec!["uv".to_owned()]);
}

#[test]
fn stale_temp_files_are_swept_before_a_fetch() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = cache_path(tmp.path()).parent().unwrap().to_path_buf();
    fs::create_dir_all(&dir).unwrap();
    // What a SIGKILL during extract leaves behind: the drop guard never ran.
    for stale in [".uv.tmp.99999.0", ".uv.tmp.12345.7"] {
        fs::write(dir.join(stale), b"half-extracted uv").unwrap();
    }
    let bytes = tarball(&[("uv-x/uv", b"the uv binary")]);
    let digest = Sha256Digest::of_bytes(&bytes);
    let calls = AtomicUsize::new(0);
    let fetch = serving(bytes, &calls);

    let placed = fetch_verified(tmp.path(), "aarch64-apple-darwin", &digest, &fetch).unwrap();

    assert_eq!(fs::read(&placed).unwrap(), b"the uv binary");
    assert_eq!(leftovers(tmp.path()), vec!["uv".to_owned()]);
}

/// A one-shot HTTP/1.1 server on loopback that sends a `chunks` KiB body, one
/// KiB every `gap`. Returns its URL.
fn dripping_server(chunks: usize, gap: Duration) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut reader = io::BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) if line == "\r\n" => break,
                Ok(_) => {}
            }
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            chunks * 1024
        );
        if stream.write_all(head.as_bytes()).is_err() {
            return;
        }
        for _ in 0..chunks {
            std::thread::sleep(gap);
            if stream.write_all(&[0u8; 1024]).is_err() || stream.flush().is_err() {
                return;
            }
        }
    });
    format!("http://{addr}/uv.tar.gz")
}

#[test]
fn a_download_dripping_past_the_total_budget_times_out_and_places_nothing() {
    // Each KiB arrives 100 ms after the last, far inside any per-read timeout,
    // but the whole body takes ~1.2 s against a 300 ms total budget.
    let url = dripping_server(12, Duration::from_millis(100));
    let tmp = tempfile::tempdir().unwrap();
    let fetch = |_asset: &str| http_fetch_within(&url, Duration::from_millis(300));
    let digest = Sha256Digest::of_bytes(b"never matches");
    let started = Instant::now();

    let err = fetch_verified(tmp.path(), "aarch64-apple-darwin", &digest, &fetch).unwrap_err();

    let elapsed = started.elapsed();
    assert!(matches!(err, UvError::Network { .. }), "got {err:?}");
    assert!(err.to_string().contains("timed out"), "got: {err}");
    assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");
    assert!(!cache_path(tmp.path()).exists());
    assert_eq!(leftovers(tmp.path()), Vec::<String>::new());
}

/// Downloads the real pinned asset for this host and checks the embedded
/// digest end to end, then runs the placed binary. Network, so `#[ignore]`.
#[test]
#[ignore = "network: downloads the real pinned uv release (#9468)"]
fn real_pinned_asset_matches_the_embedded_digest() {
    let tmp = tempfile::tempdir().unwrap();
    let triple = host_triple();
    let placed = fetch_for_triple(tmp.path(), &triple, &http_fetch)
        .unwrap_or_else(|e| panic!("fetch for {triple}: {e}"));
    assert_eq!(mode(&placed), 0o755);

    let out = std::process::Command::new(&placed)
        .arg("--version")
        .output()
        .unwrap();
    let version = String::from_utf8_lossy(&out.stdout);
    let _ = writeln!(io::stderr(), "placed {} -> {version}", placed.display());
    assert!(out.status.success(), "uv --version failed: {out:?}");
    assert!(version.contains(UV_VERSION), "got {version:?}");

    if cfg!(target_os = "macos") {
        let x = std::process::Command::new("xattr")
            .arg("-l")
            .arg(&placed)
            .output()
            .unwrap();
        let _ = writeln!(
            io::stderr(),
            "xattr -l (exit {}): {:?}",
            x.status,
            String::from_utf8_lossy(&x.stdout)
        );
    }
}
