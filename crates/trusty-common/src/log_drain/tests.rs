//! Coverage for the log-drain core (#6533).
//!
//! Every test that needs a destination uses a REAL `file://`
//! [`ObjectStoreDestination`] over a `tempfile::TempDir`, not a mock. A pass
//! here is therefore evidence about the same `put`/`get`/`head`/`list` code the
//! `s3://` destination runs, differing only in transport.
//!
//! `TRUSTY_LOG_DRAIN_S3_SMOKE_URI` opts into one real-S3 round trip; unset, that
//! test prints why it is skipping and passes.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::*;

// ── helpers ─────────────────────────────────────────────────────────────────

/// A `file://` destination rooted in a fresh temp dir.
async fn file_dest(root: &Path) -> ObjectStoreDestination {
    let uri = DestinationUri::parse(&format!("file://{}", root.display()))
        .expect("temp dir path parses as a file:// URI");
    ObjectStoreDestination::connect(&uri)
        .await
        .expect("local destination connects")
}

fn target() -> DrainTarget {
    DrainTarget {
        github_id: "bobmatnyc".to_string(),
        session_id: "sess-01".to_string(),
    }
}

/// One source rooted at `root`, matching every `.log` file.
fn source(root: &Path, level_filter: Option<Level>) -> LogSource {
    LogSource {
        crate_name: "trusty-mpm".to_string(),
        root: root.to_path_buf(),
        include: vec!["**/*.log".to_string()],
        level_filter,
    }
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("fixture dir");
    }
    std::fs::write(path, body).expect("fixture write");
}

/// Read an object back through the destination and gunzip it.
async fn read_gunzipped(dest: &dyn LogDestination, key: &str) -> String {
    let raw = dest
        .get(key)
        .await
        .expect("get succeeds")
        .unwrap_or_else(|| panic!("object `{key}` is absent"));
    let mut decoder = flate2::read::GzDecoder::new(&raw[..]);
    let mut text = String::new();
    decoder
        .read_to_string(&mut text)
        .expect("body is valid gzip");
    text
}

/// Force a file's mtime forward without changing its bytes.
fn touch_forward(path: &Path) {
    let body = std::fs::read(path).expect("read for touch");
    // A rewrite of identical bytes moves mtime while leaving content identical —
    // exactly the case `manifest_sha_beats_mtime` is about.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(path, body).expect("rewrite for touch");
}

// ── DestinationUri: the table ───────────────────────────────────────────────

#[test]
fn uri_table_accepts() {
    let cases: &[(&str, DestinationUri)] = &[
        (
            "s3://my-bucket",
            DestinationUri::S3 {
                bucket: "my-bucket".into(),
                prefix: String::new(),
                region: None,
            },
        ),
        (
            "s3://my-bucket/",
            DestinationUri::S3 {
                bucket: "my-bucket".into(),
                prefix: String::new(),
                region: None,
            },
        ),
        (
            "s3://my-bucket/logs",
            DestinationUri::S3 {
                bucket: "my-bucket".into(),
                prefix: "logs".into(),
                region: None,
            },
        ),
        (
            "s3://my-bucket/logs/nested/",
            DestinationUri::S3 {
                bucket: "my-bucket".into(),
                prefix: "logs/nested".into(),
                region: None,
            },
        ),
        (
            "  s3://my-bucket/logs  ",
            DestinationUri::S3 {
                bucket: "my-bucket".into(),
                prefix: "logs".into(),
                region: None,
            },
        ),
        (
            "S3://my-bucket/logs",
            DestinationUri::S3 {
                bucket: "my-bucket".into(),
                prefix: "logs".into(),
                region: None,
            },
        ),
        (
            "file:///var/log/trusty",
            DestinationUri::File {
                path: PathBuf::from("/var/log/trusty"),
            },
        ),
        (
            "file:///var/log/trusty/",
            DestinationUri::File {
                path: PathBuf::from("/var/log/trusty"),
            },
        ),
    ];

    for (input, expected) in cases {
        let parsed = DestinationUri::parse(input)
            .unwrap_or_else(|e| panic!("`{input}` should parse, got {e}"));
        assert_eq!(&parsed, expected, "parsing `{input}`");
    }
}

#[test]
fn uri_region_override() {
    let parsed = DestinationUri::parse("s3://bucket/logs?region=us-west-2").expect("parses");
    assert_eq!(
        parsed,
        DestinationUri::S3 {
            bucket: "bucket".into(),
            prefix: "logs".into(),
            region: Some("us-west-2".into()),
        }
    );
}

#[test]
fn uri_table_rejects() {
    // Each case must fail with DrainError::Uri, not a scheme error.
    let cases = [
        ("s3:/bucket", "no `://` separator"),
        ("bucket/logs", "no `://` separator"),
        ("", "no `://` separator"),
        ("s3:///logs", "empty bucket"),
        ("file://relative/path", "non-absolute file path"),
        ("file:///", "filesystem root"),
        ("file:///tmp/x?region=eu", "query on file://"),
        ("s3://bucket/logs?", "empty query"),
        ("s3://bucket/logs?region=", "empty region value"),
        ("s3://bucket/logs?reigon=eu-west-1", "misspelled parameter"),
        ("s3://bucket/logs?region", "parameter with no `=`"),
    ];

    for (input, why) in cases {
        match DestinationUri::parse(input) {
            Err(DrainError::Uri { .. }) => {}
            other => panic!("`{input}` ({why}) should be DrainError::Uri, got {other:?}"),
        }
    }
}

#[test]
fn uri_reserved_schemes() {
    // gs:// and az:// are RECOGNISED and then refused, so the message names
    // what is supported instead of reading as a syntax error.
    for input in ["gs://bucket/logs", "az://container/logs", "https://example"] {
        match DestinationUri::parse(input) {
            Err(DrainError::UnsupportedScheme { .. }) => {}
            other => panic!("`{input}` should be UnsupportedScheme, got {other:?}"),
        }
    }

    let message = DestinationUri::parse("gs://bucket/logs")
        .expect_err("gs:// is refused")
        .to_string();
    assert!(
        message.contains("s3://") && message.contains("file://"),
        "the refusal must name what IS supported, got: {message}"
    );
}

// ── key layout ──────────────────────────────────────────────────────────────

#[test]
fn key_layout_shape() {
    let t = target();
    assert_eq!(t.logs_prefix(), "bobmatnyc/sess-01/logs");
    assert_eq!(
        t.object_key("trusty-mpm/daemon.log"),
        "bobmatnyc/sess-01/logs/trusty-mpm/daemon.log"
    );
    assert_eq!(
        t.manifest_key(),
        "bobmatnyc/sess-01/logs/.drain-manifest.json"
    );
}

#[test]
fn identity_refusal_is_fail_closed() {
    for (github_id, session_id, field) in [
        ("", "sess", "github_id"),
        ("   ", "sess", "github_id"),
        ("bob", "", "session_id"),
        ("bob", "  ", "session_id"),
    ] {
        let t = DrainTarget {
            github_id: github_id.to_string(),
            session_id: session_id.to_string(),
        };
        match t.validate() {
            Err(DrainError::MissingIdentity { field: got }) => assert_eq!(got, field),
            other => panic!("`{github_id}`/`{session_id}` should refuse, got {other:?}"),
        }
    }
}

// ── destination round trip ──────────────────────────────────────────────────

#[tokio::test]
async fn destination_roundtrip() {
    let root = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(root.path()).await;

    assert!(
        dest.head("absent/key")
            .await
            .expect("head succeeds")
            .is_none(),
        "a missing object must be Ok(None), never an error"
    );
    assert!(
        dest.get("absent/key")
            .await
            .expect("get succeeds")
            .is_none()
    );

    let body = bytes::Bytes::from_static(b"hello drain");
    dest.put("a/b.txt", body.clone(), PutMeta::gzipped_text())
        .await
        .expect("put succeeds");

    let meta = dest
        .head("a/b.txt")
        .await
        .expect("head succeeds")
        .expect("object is present after put");
    assert_eq!(meta.size, body.len() as u64);
    assert_eq!(meta.key, "a/b.txt");

    assert_eq!(
        dest.get("a/b.txt").await.expect("get succeeds").as_deref(),
        Some(&body[..])
    );
}

#[tokio::test]
async fn destination_list_is_bounded_and_prefix_scoped() {
    let root = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(root.path()).await;

    for i in 0..5 {
        dest.put(
            &format!("keep/{i}.txt"),
            bytes::Bytes::from_static(b"x"),
            PutMeta::default(),
        )
        .await
        .expect("put");
    }
    dest.put(
        "other/z.txt",
        bytes::Bytes::from_static(b"x"),
        PutMeta::default(),
    )
    .await
    .expect("put");

    let listed = dest.list("keep").await.expect("list succeeds");
    assert_eq!(listed.len(), 5, "list is scoped to its prefix");
    assert!(listed.iter().all(|m| m.key.starts_with("keep/")));
    assert!(
        listed.len() <= LIST_LIMIT,
        "list must never exceed its documented cap"
    );
}

// ── collector: level filtering ──────────────────────────────────────────────

/// A fixture in `tracing_subscriber::fmt`'s default line shape.
const TRACING_FIXTURE: &str = concat!(
    "2026-09-01T14:12:32.273982Z DEBUG tm::commands: dropped debug detail\n",
    "2026-09-01T14:12:32.283982Z  INFO tm::daemon: daemon started\n",
    "2026-09-01T14:12:32.293982Z TRACE tm::inner: dropped trace detail\n",
    "2026-09-01T14:12:32.303982Z  WARN tm::daemon: port already bound\n",
    "2026-09-01T14:12:32.313982Z ERROR tm::daemon: bind failed\n",
    "    caused by: address in use\n",
);

#[test]
fn collect_filters_below_info() {
    let root = tempfile::tempdir().expect("tempdir");
    write(&root.path().join("daemon.log"), TRACING_FIXTURE);

    let collected = collect(
        &[source(root.path(), Some(Level::Info))],
        &[],
        DEFAULT_MAX_FILE_BYTES,
    )
    .expect("collect succeeds");

    assert_eq!(collected.files.len(), 1);
    let mut decoder = flate2::read::GzDecoder::new(&collected.files[0].body[..]);
    let mut text = String::new();
    decoder.read_to_string(&mut text).expect("gunzip");

    assert!(
        !text.contains("dropped debug detail"),
        "DEBUG must be dropped"
    );
    assert!(
        !text.contains("dropped trace detail"),
        "TRACE must be dropped"
    );
    assert!(text.contains("daemon started"), "INFO must be kept");
    assert!(text.contains("port already bound"), "WARN must be kept");
    assert!(text.contains("bind failed"), "ERROR must be kept");
    assert!(
        text.contains("address in use"),
        "a continuation line inherits its parent's disposition"
    );
}

#[test]
fn collect_drops_continuation_of_a_dropped_line() {
    let root = tempfile::tempdir().expect("tempdir");
    write(
        &root.path().join("daemon.log"),
        "2026-09-01T14:12:32.2Z DEBUG tm: noisy\n        continuation of noise\n\
         2026-09-01T14:12:32.3Z  INFO tm: kept\n",
    );

    let collected = collect(
        &[source(root.path(), Some(Level::Info))],
        &[],
        DEFAULT_MAX_FILE_BYTES,
    )
    .expect("collect");
    let mut decoder = flate2::read::GzDecoder::new(&collected.files[0].body[..]);
    let mut text = String::new();
    decoder.read_to_string(&mut text).expect("gunzip");

    assert!(!text.contains("continuation of noise"));
    assert!(text.contains("kept"));
}

#[test]
fn collect_passes_through_non_tracing() {
    let root = tempfile::tempdir().expect("tempdir");
    let plain = "just a line\nand another\nno levels here at all\n";
    write(&root.path().join("plain.log"), plain);

    let collected = collect(
        &[source(root.path(), Some(Level::Info))],
        &[],
        DEFAULT_MAX_FILE_BYTES,
    )
    .expect("collect");

    let mut decoder = flate2::read::GzDecoder::new(&collected.files[0].body[..]);
    let mut text = String::new();
    decoder.read_to_string(&mut text).expect("gunzip");

    assert_eq!(
        text, plain,
        "a file with no recognisable level line is not tracing output and must \
         pass through verbatim rather than filter to nothing"
    );
}

#[test]
fn collect_recognises_ansi_coloured_levels() {
    let root = tempfile::tempdir().expect("tempdir");
    write(
        &root.path().join("colour.log"),
        "\u{1b}[2m2026-09-01T14:12:32.2Z\u{1b}[0m \u{1b}[34mDEBUG\u{1b}[0m tm: noise\n\
         \u{1b}[2m2026-09-01T14:12:32.3Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m tm: kept\n",
    );

    let collected = collect(
        &[source(root.path(), Some(Level::Info))],
        &[],
        DEFAULT_MAX_FILE_BYTES,
    )
    .expect("collect");
    let mut decoder = flate2::read::GzDecoder::new(&collected.files[0].body[..]);
    let mut text = String::new();
    decoder.read_to_string(&mut text).expect("gunzip");

    assert!(
        !text.contains("noise"),
        "a colourised DEBUG line is still DEBUG"
    );
    assert!(text.contains("kept"));
}

// ── collector: size ceiling ─────────────────────────────────────────────────

#[test]
fn collect_skips_oversize() {
    let root = tempfile::tempdir().expect("tempdir");
    write(&root.path().join("big.log"), &"x".repeat(4096));
    write(&root.path().join("small.log"), "tiny\n");

    let collected = collect(&[source(root.path(), None)], &[], 1024).expect("collect");

    assert_eq!(collected.files.len(), 1, "only the small file is collected");
    assert_eq!(collected.files[0].relative_key, "trusty-mpm/small.log");
    assert_eq!(collected.oversize.len(), 1);
    assert_eq!(collected.oversize[0].size, 4096);
    assert!(collected.oversize[0].path.ends_with("big.log"));
}

// ── collector: the scrub ────────────────────────────────────────────────────

#[tokio::test]
async fn collect_scrubs_secrets_before_they_reach_the_destination() {
    let root = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");

    // Well over `scrub_secrets`' minimum needle length, so it is actually scrubbed.
    let secret = "sk-planted-secret-value-0123456789";
    write(
        &root.path().join("daemon.log"),
        &format!("2026-09-01T14:12:32.2Z  INFO tm: token={secret} ok\n"),
    );

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path()).with_secrets(vec![secret.to_string()]);
    let t = target();

    let report = run_once(&cfg, &dest, &t, &[source(root.path(), Some(Level::Info))])
        .await
        .expect("run_once succeeds");
    assert_eq!(report.uploaded, 1);

    let key = t.object_key("trusty-mpm/daemon.log");
    let text = read_gunzipped(&dest, &key).await;

    assert!(
        !text.contains(secret),
        "the planted secret reached the destination: {text}"
    );
    assert!(
        text.contains("[REDACTED]"),
        "the scrub must leave its marker"
    );
    assert!(text.contains("token="), "surrounding text must survive");

    // Belt and braces: the secret must not be in the raw bytes on disk either.
    let on_disk = std::fs::read(dest_root.path().join(&key)).expect("object file exists");
    assert!(
        !String::from_utf8_lossy(&on_disk).contains(secret),
        "the secret survived in the stored bytes"
    );
}

// ── manifest ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn manifest_roundtrip() {
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(dest_root.path()).await;

    let mut manifest = DrainManifest::default();
    assert_eq!(manifest.version, MANIFEST_VERSION);
    manifest.record(ManifestEntry {
        relative_file: "trusty-mpm/a.log".into(),
        size: 10,
        mtime_unix: 1_700_000_000,
        sha256: "abc".into(),
        uploaded_at: "2026-09-01T00:00:00Z".into(),
    });

    manifest
        .save(&dest, state.path(), "k/manifest.json", "bob/sess")
        .await
        .expect("save");

    let loaded = DrainManifest::load(&dest, state.path(), "k/manifest.json", "bob/sess")
        .await
        .expect("load");
    assert_eq!(loaded.entries, manifest.entries);
}

#[tokio::test]
async fn manifest_remote_wins_over_local_cache() {
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(dest_root.path()).await;

    // Local cache claims a file was uploaded; the remote manifest says otherwise.
    let mut stale = DrainManifest::default();
    stale.record(ManifestEntry {
        relative_file: "trusty-mpm/ghost.log".into(),
        size: 1,
        mtime_unix: 1,
        sha256: "stale".into(),
        uploaded_at: "2026-01-01T00:00:00Z".into(),
    });
    let cache_path = state
        .path()
        .join("log-drain")
        .join("bob/sess")
        .join("manifest.json");
    std::fs::create_dir_all(cache_path.parent().expect("parent")).expect("cache dir");
    std::fs::write(&cache_path, serde_json::to_vec(&stale).expect("encode")).expect("cache write");

    let remote = DrainManifest::default();
    remote
        .save(&dest, state.path(), "k/manifest.json", "unrelated")
        .await
        .expect("save remote");

    let loaded = DrainManifest::load(&dest, state.path(), "k/manifest.json", "bob/sess")
        .await
        .expect("load");
    assert!(
        loaded.entries.is_empty(),
        "the remote copy is authoritative; the stale cache entry must not survive"
    );

    let refreshed: DrainManifest =
        serde_json::from_slice(&std::fs::read(&cache_path).expect("cache")).expect("decode");
    assert!(
        refreshed.entries.is_empty(),
        "loading must rewrite the cache from the authoritative remote copy"
    );
}

#[tokio::test]
async fn manifest_corrupt_remote_falls_back_to_cache() {
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(dest_root.path()).await;

    dest.put(
        "k/manifest.json",
        bytes::Bytes::from_static(b"{ not json"),
        PutMeta::default(),
    )
    .await
    .expect("put corrupt manifest");

    // An undecodable remote manifest is treated as ABSENT, never as an error —
    // re-uploading is safer than skipping something that was never written.
    let loaded = DrainManifest::load(&dest, state.path(), "k/manifest.json", "bob/sess")
        .await
        .expect("a corrupt manifest must not fail the load");
    assert!(loaded.entries.is_empty());
}

#[test]
fn manifest_stat_fast_path_and_sha_tiebreak() {
    let mut manifest = DrainManifest::default();
    manifest.record(ManifestEntry {
        relative_file: "c/a.log".into(),
        size: 100,
        mtime_unix: 42,
        sha256: "deadbeef".into(),
        uploaded_at: "2026-09-01T00:00:00Z".into(),
    });

    assert_eq!(
        manifest.decide("c/a.log", 100, 42),
        StatDecision::SkipUnchanged
    );
    assert_eq!(manifest.decide("c/a.log", 101, 42), StatDecision::NeedsHash);
    assert_eq!(manifest.decide("c/a.log", 100, 43), StatDecision::NeedsHash);
    assert_eq!(
        manifest.decide("c/absent.log", 100, 42),
        StatDecision::NeedsHash
    );

    assert!(manifest.digest_matches("c/a.log", "deadbeef"));
    assert!(!manifest.digest_matches("c/a.log", "cafe"));
    assert!(!manifest.digest_matches("c/absent.log", "deadbeef"));
}

// ── run_once ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn run_once_end_to_end() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");

    write(&logs.path().join("daemon.log"), TRACING_FIXTURE);
    write(&logs.path().join("nested/worker.log"), TRACING_FIXTURE);
    write(&logs.path().join("ignored.txt"), "not a .log file\n");

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path());
    let t = target();

    let report = run_once(&cfg, &dest, &t, &[source(logs.path(), Some(Level::Info))])
        .await
        .expect("run_once");

    assert_eq!(report.uploaded, 2, "both .log files, not the .txt");
    assert_eq!(report.skipped_unchanged, 0);
    assert_eq!(report.skipped_too_large, 0);
    assert!(
        report.errors.is_empty(),
        "unexpected errors: {:?}",
        report.errors
    );
    assert!(report.bytes_plain > 0);
    assert!(report.bytes_wire > 0);

    // The objects landed under the documented key layout.
    for relative in ["trusty-mpm/daemon.log", "trusty-mpm/nested/worker.log"] {
        let key = t.object_key(relative);
        assert!(
            dest.head(&key).await.expect("head").is_some(),
            "expected an object at `{key}`"
        );
    }
    // And so did the manifest.
    assert!(
        dest.head(&t.manifest_key()).await.expect("head").is_some(),
        "the manifest must be rewritten after a successful batch"
    );
}

#[tokio::test]
async fn run_once_is_idempotent() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    write(&logs.path().join("daemon.log"), TRACING_FIXTURE);

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path());
    let t = target();
    let sources = [source(logs.path(), Some(Level::Info))];

    let first = run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("first run");
    assert_eq!(first.uploaded, 1);

    let second = run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("second run");
    assert_eq!(second.uploaded, 0, "nothing changed, so nothing re-uploads");
    assert_eq!(second.skipped_unchanged, 1);
    assert_eq!(second.bytes_wire, 0);
}

#[tokio::test]
async fn run_once_reuploads_a_mutated_file() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let log = logs.path().join("daemon.log");
    write(&log, TRACING_FIXTURE);

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path());
    let t = target();
    let sources = [source(logs.path(), Some(Level::Info))];

    run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("first run");

    write(
        &log,
        &format!("{TRACING_FIXTURE}2026-09-01T15:00:00.0Z  INFO tm: a new line\n"),
    );

    let second = run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("second run");
    assert_eq!(second.uploaded, 1, "changed bytes must re-upload");
    assert_eq!(second.skipped_unchanged, 0);

    let text = read_gunzipped(&dest, &t.object_key("trusty-mpm/daemon.log")).await;
    assert!(
        text.contains("a new line"),
        "the destination holds the new body"
    );
}

#[tokio::test]
async fn run_once_sha_beats_a_moved_mtime() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let log = logs.path().join("daemon.log");
    write(&log, TRACING_FIXTURE);

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path());
    let t = target();
    let sources = [source(logs.path(), Some(Level::Info))];

    run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("first run");
    let recorded_before = manifest_mtime(&dest, &t).await;

    // Same bytes, later mtime. SHA-256 wins: no re-upload.
    touch_forward(&log);

    // Guard against a vacuous pass: if the rewrite did not actually move the
    // mtime, `decide` would take the stat-only fast path and the sha tiebreak
    // below would never run.
    let on_disk_mtime = std::fs::metadata(&log)
        .expect("stat")
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_secs() as i64;
    assert!(
        on_disk_mtime > recorded_before,
        "fixture is broken: mtime did not move ({on_disk_mtime} vs {recorded_before}), \
         so this test would pass without exercising the sha tiebreak"
    );

    let second = run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("second run");
    assert_eq!(
        second.uploaded, 0,
        "identical bytes must not re-upload just because mtime moved"
    );
    assert_eq!(second.skipped_unchanged, 1);

    // The sha tiebreak refreshed the entry's mtime — that refresh is what makes
    // the NEXT run cheap, and it only happens on the sha path.
    let recorded_after = manifest_mtime(&dest, &t).await;
    assert_eq!(
        recorded_after, on_disk_mtime,
        "the sha tiebreak must refresh the recorded mtime so the next run takes \
         the stat-only fast path"
    );

    let third = run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("third run");
    assert_eq!(third.uploaded, 0);
    assert_eq!(third.skipped_unchanged, 1);
}

/// The mtime the destination's manifest currently records for `daemon.log`.
async fn manifest_mtime(dest: &dyn LogDestination, t: &DrainTarget) -> i64 {
    let raw = dest
        .get(&t.manifest_key())
        .await
        .expect("get manifest")
        .expect("manifest exists");
    let manifest: DrainManifest = serde_json::from_slice(&raw).expect("decode manifest");
    manifest
        .entries
        .iter()
        .find(|e| e.relative_file == "trusty-mpm/daemon.log")
        .expect("entry for the drained file")
        .mtime_unix
}

#[tokio::test]
async fn run_once_counts_oversize_without_uploading() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    write(&logs.path().join("big.log"), &"x".repeat(4096));
    write(&logs.path().join("small.log"), "small\n");

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path()).with_max_file_bytes(1024);
    let t = target();

    let report = run_once(&cfg, &dest, &t, &[source(logs.path(), None)])
        .await
        .expect("run_once");

    assert_eq!(report.uploaded, 1);
    assert_eq!(report.skipped_too_large, 1);
    assert!(
        dest.head(&t.object_key("trusty-mpm/big.log"))
            .await
            .expect("head")
            .is_none(),
        "an oversize file must never be uploaded, not even truncated"
    );
}

#[tokio::test]
async fn run_once_refuses_empty_github_id() {
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(dest_root.path()).await;

    let t = DrainTarget {
        github_id: String::new(),
        session_id: "sess-01".to_string(),
    };
    let err = run_once(&DrainConfig::new(state.path()), &dest, &t, &[])
        .await
        .expect_err("an empty github_id must be refused");
    assert!(matches!(
        err,
        DrainError::MissingIdentity { field: "github_id" }
    ));

    // Fail-closed means NOTHING was written, not even a manifest.
    assert_eq!(
        std::fs::read_dir(dest_root.path())
            .expect("read dest root")
            .count(),
        0,
        "a refused run must leave the destination untouched"
    );
}

#[tokio::test]
async fn run_once_refuses_empty_session_id() {
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(dest_root.path()).await;

    let t = DrainTarget {
        github_id: "bobmatnyc".to_string(),
        session_id: "   ".to_string(),
    };
    let err = run_once(&DrainConfig::new(state.path()), &dest, &t, &[])
        .await
        .expect_err("an empty session_id must be refused");
    assert!(matches!(
        err,
        DrainError::MissingIdentity {
            field: "session_id"
        }
    ));
}

#[tokio::test]
async fn run_once_collects_per_file_errors_without_aborting() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    write(&logs.path().join("a.log"), "one\n");
    write(&logs.path().join("b.log"), "two\n");

    let dest = FailingDestination {
        inner: file_dest(dest_root.path()).await,
        fail_on: "trusty-mpm/a.log".to_string(),
    };
    let cfg = DrainConfig::new(state.path());
    let t = target();

    let report = run_once(&cfg, &dest, &t, &[source(logs.path(), None)])
        .await
        .expect("a per-file failure must not fail the run");

    assert_eq!(report.uploaded, 1, "the healthy file still uploads");
    assert_eq!(
        report.errors.len(),
        1,
        "the failure is reported, not raised"
    );
    assert!(report.errors[0].0.ends_with("trusty-mpm/a.log"));
}

/// A destination that fails `put` for one key and delegates everything else.
///
/// Proves [`run_once`] collects a per-file error and CONTINUES the batch —
/// the behaviour that stops one unreadable file stranding every other log.
#[derive(Debug)]
struct FailingDestination {
    inner: ObjectStoreDestination,
    fail_on: String,
}

#[async_trait::async_trait]
impl LogDestination for FailingDestination {
    async fn put(&self, key: &str, body: bytes::Bytes, meta: PutMeta) -> Result<(), DrainError> {
        if key.ends_with(&self.fail_on) {
            return Err(DrainError::Manifest {
                key: key.to_string(),
                reason: "injected failure".to_string(),
            });
        }
        self.inner.put(key, body, meta).await
    }

    async fn head(&self, key: &str) -> Result<Option<ObjectMeta>, DrainError> {
        self.inner.head(key).await
    }

    async fn get(&self, key: &str) -> Result<Option<bytes::Bytes>, DrainError> {
        self.inner.get(key).await
    }

    async fn list(&self, prefix: &str) -> Result<Vec<ObjectMeta>, DrainError> {
        self.inner.list(prefix).await
    }
}

// ── gated real-S3 smoke ─────────────────────────────────────────────────────

/// Env var that opts this run into one real S3 round trip.
const S3_SMOKE_ENV: &str = "TRUSTY_LOG_DRAIN_S3_SMOKE_URI";

/// One real `s3://` upload, read back and verified.
///
/// Why an explicit skip rather than `#[ignore]`: an `#[ignore]`d test is
/// invisible in a normal run, so nobody learns the S3 path exists or how to
/// exercise it. This prints the exact env var to set and passes.
///
/// Set `TRUSTY_LOG_DRAIN_S3_SMOKE_URI=s3://<bucket>/<prefix>` (optionally with
/// `?region=`) and provide AWS credentials the usual way.
#[tokio::test]
async fn s3_smoke() {
    let Ok(uri_str) = std::env::var(S3_SMOKE_ENV) else {
        eprintln!(
            "SKIP s3_smoke: {S3_SMOKE_ENV} is unset. \
             Set it to `s3://<bucket>/<prefix>` with AWS credentials available \
             to exercise the real S3 destination."
        );
        return;
    };

    let uri = DestinationUri::parse(&uri_str).expect("smoke URI parses");
    let dest = ObjectStoreDestination::connect(&uri)
        .await
        .expect("S3 destination connects");

    let logs = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    write(&logs.path().join("smoke.log"), TRACING_FIXTURE);

    let t = DrainTarget {
        github_id: "log-drain-smoke".to_string(),
        session_id: format!("run-{}", chrono::Utc::now().timestamp()),
    };
    let cfg = DrainConfig::new(state.path());

    let report = run_once(&cfg, &dest, &t, &[source(logs.path(), Some(Level::Info))])
        .await
        .expect("run_once against S3");
    assert_eq!(report.uploaded, 1);

    let text = read_gunzipped(&dest, &t.object_key("trusty-mpm/smoke.log")).await;
    assert!(text.contains("daemon started"));
    assert!(!text.contains("dropped debug detail"));
}
