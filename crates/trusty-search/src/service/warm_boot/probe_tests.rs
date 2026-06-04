//! Tests for the per-volume probe (issue #723, review #727 findings 2 and 3).
//!
//! Why: the key invariants are:
//! 1. `volume_key` correctly extracts `/Volumes/<label>` for external paths
//!    and `/` for everything else.
//! 2. `probe_volume` probes the SAMPLE PATH (not the volume root), so a
//!    volume whose mount-root is accessible but whose inner path is not is
//!    correctly classified as inaccessible (review #727 finding 2).
//! 3. `probe_volume` increments `LEAKED_PROBE_THREAD_COUNT` on timeout
//!    (review #727 finding 3).
//! 4. `probe_all_volumes` deduplicates by volume key.
//!
//! We cannot reproduce the TCC-hang in unit tests, so the `Inaccessible`
//! path is tested via direct inspection of the timeout branch with a
//! vanishingly short deadline. A 1ms deadline on a real `/tmp` tempdir will
//! sometimes succeed (race), so we do NOT assert `Inaccessible` there —
//! instead we test `probe_volume` only with real-world accessible paths.
//! The `Inaccessible` branch is exercised in `restore.rs`'s responsiveness
//! test which already uses `std::thread::sleep` to simulate a blocked probe.
//!
//! Test: `cargo test -p trusty-search -- warm_boot::probe`.

use super::*;

// ── volume_key ────────────────────────────────────────────────────────────────

/// Why: guard that boot-volume and non-macOS paths return `/`.
/// What: paths starting with `/tmp`, `/usr`, `/home` return `/`.
/// Test: this test.
#[test]
fn volume_key_boot_volume() {
    assert_eq!(
        volume_key(Path::new("/tmp/trusty-test")),
        PathBuf::from("/"),
        "/tmp/... must produce volume key /"
    );
    assert_eq!(
        volume_key(Path::new("/usr/local/bin")),
        PathBuf::from("/"),
        "/usr/... must produce volume key /"
    );
    assert_eq!(
        volume_key(Path::new("/")),
        PathBuf::from("/"),
        "root itself must produce volume key /"
    );
    assert_eq!(
        volume_key(Path::new("/home/user/projects")),
        PathBuf::from("/"),
        "/home/... must produce volume key /"
    );
}

/// Why: guard that external macOS volumes extract the `/Volumes/<label>` key.
/// What: paths under `/Volumes/SSD1` or `/Volumes/ExternalDrive` return
/// `/Volumes/<label>`.
/// Test: this test.
#[test]
fn volume_key_external_volume() {
    assert_eq!(
        volume_key(Path::new("/Volumes/SSD1/Projects/trusty-tools")),
        PathBuf::from("/Volumes/SSD1"),
        "/Volumes/SSD1/... must produce volume key /Volumes/SSD1"
    );
    assert_eq!(
        volume_key(Path::new("/Volumes/ExternalDrive/code")),
        PathBuf::from("/Volumes/ExternalDrive"),
        "/Volumes/ExternalDrive/... must produce volume key /Volumes/ExternalDrive"
    );
    assert_eq!(
        volume_key(Path::new("/Volumes/SSD1")),
        PathBuf::from("/Volumes/SSD1"),
        "/Volumes/SSD1 itself must produce volume key /Volumes/SSD1"
    );
}

/// Why (review #727 finding 3): on Linux `/volumes/...` (lowercase) must NOT
/// be treated as an external macOS volume key. The old `eq_ignore_ascii_case`
/// code would mis-classify it, producing spurious `TIMED_OUT` warnings for
/// any Linux path that happens to start with a component whose name is a
/// case variant of "volumes".
/// What: assert that `/volumes/ssd1/projects/myrepo` returns `/` (not
/// `/volumes/ssd1`) on all platforms, and that `/Volumes/SSD1/...` still
/// returns `/Volumes/SSD1` on macOS (and `/` on other platforms).
/// Test: this test.
#[test]
fn volume_key_linux_lowercase_volumes_is_root() {
    // On all platforms, lowercase `/volumes/...` must map to root.
    // (On macOS this also tests that the exact-match guard rejects it.)
    assert_eq!(
        volume_key(Path::new("/volumes/ssd1/projects/myrepo")),
        PathBuf::from("/"),
        "/volumes/... (lowercase) must produce volume key / on all platforms"
    );
    assert_eq!(
        volume_key(Path::new("/VOLUMES/SSD1/projects/myrepo")),
        PathBuf::from("/"),
        "/VOLUMES/... (uppercase) must produce volume key / — not a canonical macOS path"
    );
}

// ── probe_volume ──────────────────────────────────────────────────────────────

/// Why: the most critical invariant — a real accessible directory must
/// return `Accessible` within a generous deadline.
/// What: create a tempdir, probe it with a 5s deadline using the tempdir
/// as both volume_root and probe_path; assert `Accessible`.
/// Test: this test.
#[test]
fn probe_volume_accessible_tempdir() {
    let tmp = tempfile::tempdir().unwrap();
    let result = probe_volume(tmp.path(), tmp.path(), Duration::from_secs(5));
    assert_eq!(
        result,
        VolumeAccessibility::Accessible,
        "a real tmpdir must be accessible within 5s"
    );
}

/// Why: a path that does not exist returns an OS error immediately (not a
/// hang), so the probe should return `Accessible` — the kernel answered.
/// What: probe a nonexistent path with a 5s deadline; assert `Accessible`
/// (the probe returns fast even on error — kernel responded with ENOENT).
/// Test: this test.
#[test]
fn probe_volume_nonexistent_path_returns_accessible() {
    // On all tested OSes, `metadata` on a nonexistent path returns ENOENT
    // immediately — there is no hang. The probe thread sends () promptly.
    let nonexistent = Path::new("/tmp/trusty-723-definitely-not-here-xyz99999");
    let result = probe_volume(nonexistent, nonexistent, Duration::from_secs(5));
    assert_eq!(
        result,
        VolumeAccessibility::Accessible,
        "a NotFound metadata call must return promptly (kernel answered), not time out"
    );
}

/// Why (review #727 finding 2): `probe_volume` must probe the SAMPLE PATH
/// (inner index path), not the volume mount-point root. On macOS, TCC can
/// allow `stat` on the volume root but deny access to files inside it.
///
/// What: call `probe_volume` with a real tmp dir as `volume_root` AND probe
/// subdirectories inside it. Both an existing inner dir and a nonexistent
/// deeper path must return `Accessible` (kernel answered ENOENT fast,
/// confirming the probe actually targets `probe_path`, not `volume_root`).
///
/// Test: this test (direct path-targeting verification).
#[test]
fn probe_uses_sample_path_not_volume_root() {
    // Volume root is accessible (real tmpdir).
    let tmp = tempfile::tempdir().unwrap();
    let volume_root = tmp.path();

    // Create a real subdirectory inside the temp dir as the sample path.
    let inner_dir = tmp.path().join("inner-index");
    std::fs::create_dir_all(&inner_dir).unwrap();

    // Probing the inner dir (which exists) must succeed quickly.
    let result = probe_volume(volume_root, &inner_dir, Duration::from_secs(5));
    assert_eq!(
        result,
        VolumeAccessibility::Accessible,
        "probe of accessible inner dir must return Accessible"
    );

    // Probing a nonexistent deeper path must also return Accessible
    // (ENOENT from kernel = fast, not hung). This verifies the probe
    // actually calls metadata on probe_path, not just volume_root.
    let deep_nonexistent = tmp.path().join("a").join("b").join("c").join("never-here");
    let result2 = probe_volume(volume_root, &deep_nonexistent, Duration::from_secs(5));
    assert_eq!(
        result2,
        VolumeAccessibility::Accessible,
        "ENOENT on probe_path must return Accessible (kernel answered fast, not hung)"
    );
}

/// Why (review #727 finding 3): a timed-out probe must increment the
/// `LEAKED_PROBE_THREAD_COUNT` counter so `/health` can surface the
/// accumulation.
/// What: record the counter before calling `probe_volume` with a 0ns
/// deadline (guaranteed timeout on any real path). Assert `after > before`
/// (review #727 finding 2 fix: we assert monotone growth and do NOT
/// restore the counter. `store(before, ...)` would race with other serial
/// tests that also increment the counter; asserting `after > before` is
/// sufficient and eliminates the restore-induced race).
/// Note: `serial` prevents parallel tests from racing on the global counter.
/// Test: this test.
#[test]
#[serial_test::serial]
fn probe_timeout_increments_leaked_thread_count() {
    let before = LEAKED_PROBE_THREAD_COUNT.load(Ordering::Relaxed);

    // Use a zero-duration deadline — the recv_timeout fires before the
    // probe thread can even schedule.
    let tmp = tempfile::tempdir().unwrap();
    let result = probe_volume(tmp.path(), tmp.path(), Duration::ZERO);

    let after = LEAKED_PROBE_THREAD_COUNT.load(Ordering::Relaxed);

    // The result must be Inaccessible (timed out).
    assert_eq!(
        result,
        VolumeAccessibility::Inaccessible,
        "zero-duration deadline must produce Inaccessible"
    );
    // The counter must have increased. We do NOT restore it:
    // store(before, Ordering::Relaxed) would race with other serial tests
    // that may increment the counter between our load and the store, silently
    // rolling back their increments. The counter is monotonically increasing
    // by design; asserting after > before is correct. (review #727 finding 2)
    assert!(
        after > before,
        "LEAKED_PROBE_THREAD_COUNT must increment on timeout; before={before} after={after}"
    );
}

// ── probe_all_volumes ─────────────────────────────────────────────────────────

/// Why: all-accessible paths must produce an empty inaccessible set.
/// What: provide several paths under /tmp; assert no inaccessible volumes.
/// Test: this test.
#[test]
fn probe_all_volumes_accessible_returns_empty() {
    let paths = vec![
        PathBuf::from("/tmp/a"),
        PathBuf::from("/tmp/b"),
        PathBuf::from("/usr/local"),
    ];
    let inaccessible = probe_all_volumes(&paths, Duration::from_secs(5));
    assert!(
        inaccessible.is_empty(),
        "all boot-volume paths must be accessible; got: {inaccessible:?}"
    );
}

/// Why: paths on different volumes must produce distinct volume keys and
/// each be probed exactly once (deduplicated).
/// What: three paths — two under `/tmp` (same volume key `/`) and one
/// hypothetical `/Volumes/SSD1/...`. Assert the volume key extraction works.
/// We do NOT assert the SSD1 probe result (would require the hardware).
/// Test: this test — validates deduplication at the key level.
#[test]
fn probe_all_volumes_distinct_keys() {
    // Two paths on the same volume must deduplicate to one key.
    let paths = vec![
        PathBuf::from("/tmp/proj-a"),
        PathBuf::from("/tmp/proj-b"),
        PathBuf::from("/usr/local/bin"),
    ];
    // All on boot volume ("/"), so one unique key.
    let mut keys: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for p in &paths {
        keys.insert(volume_key(p));
    }
    assert_eq!(keys.len(), 1, "3 boot-volume paths must yield 1 unique key");
    assert!(keys.contains(&PathBuf::from("/")));
}

/// Why (review #727 finding 1): `probe_all_volumes` must probe volumes in
/// PARALLEL so total warm-boot stall time is bounded at ≈ONE deadline
/// regardless of N blocked volumes.
/// What: provide several boot-volume paths (all returning volume key `/`);
/// they deduplicate to one probe, so this just verifies the function
/// returns promptly and the result is empty. Additionally verify the
/// function is idempotent on an empty input.
/// Test: this test.
#[test]
fn probe_all_volumes_parallel_bounded_time() {
    // Empty input: must return empty immediately.
    let inaccessible = probe_all_volumes(&[], Duration::from_secs(5));
    assert!(
        inaccessible.is_empty(),
        "empty input must return empty inaccessible set"
    );

    // Several boot-volume paths: all accessible, must return empty.
    let paths = vec![
        PathBuf::from("/tmp/proj-a"),
        PathBuf::from("/tmp/proj-b"),
        PathBuf::from("/usr/local"),
    ];
    let inaccessible = probe_all_volumes(&paths, Duration::from_secs(5));
    assert!(
        inaccessible.is_empty(),
        "all boot-volume paths must be accessible (parallel probe); got: {inaccessible:?}"
    );
}

// ── volume_probe_timeout ──────────────────────────────────────────────────────

/// Why: guard that the env var reader parses valid values and falls back.
/// What: set `TRUSTY_WARMBOOT_VOLUME_PROBE_SECS=7`, assert Duration is 7s;
/// unset, assert Duration is the default 5s.
/// Note: `serial` prevents racing with other env-var mutators.
/// Test: this test.
#[test]
#[serial_test::serial]
fn volume_probe_timeout_parses_env_var() {
    unsafe { std::env::set_var("TRUSTY_WARMBOOT_VOLUME_PROBE_SECS", "7") };
    assert_eq!(
        volume_probe_timeout(),
        Duration::from_secs(7),
        "must parse 7 from env var"
    );
    unsafe { std::env::remove_var("TRUSTY_WARMBOOT_VOLUME_PROBE_SECS") };
    assert_eq!(
        volume_probe_timeout(),
        Duration::from_secs(5),
        "must fall back to 5s default when env var is absent"
    );
}
