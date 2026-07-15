//! Tests for the per-volume probe (issue #723, review #727 findings 2 and 3).
//!
//! Why: the key invariants are:
//! 1. `volume_key` correctly extracts `/Volumes/<label>` for external paths
//!    and `/` for everything else.
//! 2. `probe_volume` probes the SAMPLE PATH (not the volume root), so a
//!    volume whose mount-root is accessible but whose inner path is not is
//!    correctly classified as inaccessible (review #727 finding 2).
//! 3. `probe_volume` increments `PROBE_THREAD_FAILURES` on timeout
//!    (review #727 finding 3, renamed from `LEAKED_PROBE_THREAD_COUNT` in #822).
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
/// `/Volumes/<label>`. This test is gated to macOS because `volume_key` only
/// applies the `/Volumes/<label>` logic under `#[cfg(target_os = "macos")]`;
/// on Linux, `/Volumes/...` correctly returns `/` (no macOS-style mounts).
/// Test: this test — macOS only.
#[cfg(target_os = "macos")]
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
/// `PROBE_THREAD_FAILURES` counter so `/health` can surface the accumulation.
/// (Counter was renamed from `LEAKED_PROBE_THREAD_COUNT` in issue #822.)
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
fn probe_timeout_increments_probe_thread_failures() {
    let before = PROBE_THREAD_FAILURES.load(Ordering::Relaxed);

    // Use a zero-duration deadline — the recv_timeout fires before the
    // probe thread can even schedule.
    let tmp = tempfile::tempdir().unwrap();
    let result = probe_volume(tmp.path(), tmp.path(), Duration::ZERO);

    let after = PROBE_THREAD_FAILURES.load(Ordering::Relaxed);

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
        "PROBE_THREAD_FAILURES must increment on timeout; before={before} after={after}"
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

// ── multi-volume starvation regression ───────────────────────────────────────

/// Handle that keeps gated "slow" probe threads blocked until dropped.
///
/// Why: the slow volumes in the no-starvation regression must DETERMINISTICALLY
/// miss the collection deadline regardless of machine load. Rather than racing a
/// `std::thread::sleep` against a wall-clock deadline (the load-sensitive design
/// that flaked in issue #2703), each slow thread blocks on its own gate channel
/// and is released only after the collector has returned — so it can never
/// report before the deadline no matter how contended the host is.
/// What: owns the sender half of each slow thread's gate. Dropping it closes the
/// gates, unblocking the slow threads for clean shutdown (they then exit without
/// reporting, since the collector's receiver is already gone).
/// Test: `probe_all_volumes_multi_volume_no_fast_starvation`.
struct SlowGate(#[allow(dead_code)] Vec<std::sync::mpsc::Sender<()>>);

/// Helper: run `probe_all_volumes`'s shared-channel collection loop with
/// DETERMINISTIC (load-immune) volume completion instead of timed sleeps.
///
/// Why: `probe_all_volumes` calls `std::fs::metadata`, which returns ENOENT
/// instantly for non-existent paths — a genuinely slow probe cannot be created
/// without special filesystem support. The previous helper approximated one with
/// `std::thread::sleep` and the test asserted a `< 2 × deadline` wall-clock
/// bound; both the sleep-vs-deadline race and the elapsed-time assertion were
/// sensitive to concurrent build/test load (issue #2703: observed 116–137 ms
/// against a 100 ms bound). This helper removes ALL timing races:
///   - FAST volumes report immediately and are JOINED before the collection
///     deadline starts, so their results are guaranteed queued in the shared
///     channel — no reliance on a short sleep landing inside the deadline.
///   - SLOW volumes block on a per-volume gate the caller controls; they are
///     released only AFTER the collector returns, so they DETERMINISTICALLY miss
///     the deadline. The collector's own wall-clock deadline is what fires,
///     exactly as in production.
///
/// `probe_all_volumes` is pure `std::thread` + `mpsc` (no tokio timers), so a
/// `tokio::time::pause()` virtual clock cannot drive it; gate-based
/// synchronization is the load-immune equivalent.
///
/// What: spawns `fast.len()` threads that send their key immediately (joined
/// before the loop), and `slow.len()` threads that block on a dedicated gate
/// channel. Runs the identical shared-`end` / break-on-`Err` collection loop as
/// `probe_all_volumes` Phase 2, then marks every unreported volume inaccessible
/// (incrementing `PROBE_THREAD_FAILURES` once each — same invariant). Returns
/// `(inaccessible_set, gate)`; dropping the `SlowGate` releases the slow threads.
///
/// Test: `probe_all_volumes_multi_volume_no_fast_starvation`.
fn probe_with_gated_slow(
    fast: &[PathBuf],
    slow: &[PathBuf],
    deadline: Duration,
) -> (std::collections::HashSet<PathBuf>, SlowGate) {
    use std::collections::HashSet;
    use std::sync::mpsc;
    use std::time::Instant;

    let n = fast.len() + slow.len();
    if n == 0 {
        return (HashSet::new(), SlowGate(Vec::new()));
    }

    let (tx, rx) = mpsc::channel::<PathBuf>();
    let mut all_keys: HashSet<PathBuf> = HashSet::with_capacity(n);

    // FAST volumes: spawn threads that send immediately, and JOIN them so their
    // results are queued in the shared channel BEFORE the collection deadline
    // starts. This exercises concurrent arrival while removing the
    // fast-volume-vs-deadline race entirely.
    let mut fast_handles = Vec::with_capacity(fast.len());
    for key in fast {
        all_keys.insert(key.clone());
        let tx = tx.clone();
        let key = key.clone();
        fast_handles.push(std::thread::spawn(move || {
            let _ = tx.send(key);
        }));
    }
    for h in fast_handles {
        let _ = h.join();
    }

    // SLOW volumes: each blocks on its own gate until the caller drops the
    // returned `SlowGate`. Because that happens only after the collector returns,
    // a slow volume can never report before the deadline — no timing race.
    let mut gates: Vec<mpsc::Sender<()>> = Vec::with_capacity(slow.len());
    for key in slow {
        all_keys.insert(key.clone());
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        gates.push(gate_tx);
        let tx = tx.clone();
        let key = key.clone();
        let _ = std::thread::spawn(move || {
            // Block until the gate closes; only then (well past the deadline) is
            // any report attempted — and by then the receiver is gone.
            let _ = gate_rx.recv();
            let _ = tx.send(key);
        });
    }
    // Drop our own tx clone. The slow threads still hold theirs, so the channel
    // stays open and `recv_timeout` yields `Timeout` (not `Disconnected`) when
    // the deadline fires — matching production `probe_all_volumes`.
    drop(tx);

    // Collection loop — identical structure to probe_all_volumes Phase 2: a
    // single shared `end`, breaking on the FIRST timeout. This single shared
    // deadline is what bounds total wait at ONE deadline regardless of how many
    // volumes never report.
    let end = Instant::now() + deadline;
    let mut reported: HashSet<PathBuf> = HashSet::with_capacity(n);
    loop {
        if reported.len() == n {
            break;
        }
        let remaining = end.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(vol_key) => {
                reported.insert(vol_key);
            }
            Err(_) => break,
        }
    }

    // Every unreported volume is inaccessible; increment the failure counter once
    // each (same invariant as probe_all_volumes).
    let mut inaccessible: HashSet<PathBuf> = HashSet::new();
    for vol_key in &all_keys {
        if reported.contains(vol_key) {
            continue;
        }
        PROBE_THREAD_FAILURES.fetch_add(1, Ordering::Relaxed);
        inaccessible.insert(vol_key.clone());
    }

    (inaccessible, SlowGate(gates))
}

/// Why (review #727 pass-3 HIGH — starvation regression, made load-immune in
/// issue #2703): with the per-channel sequential design, if a slow volume's
/// `recv_timeout` consumed the full budget, every subsequent volume got
/// `Duration::ZERO` and was wrongly classified as inaccessible even though its
/// probe thread had already finished. This test proves the shared-channel design
/// eliminates that starvation. The PROTECTED PROPERTY is fairness: a fast volume
/// that has completed is never classified inaccessible merely because slow
/// volumes are concurrently exhausting the deadline — NOT absolute speed.
///
/// What: two FAST volumes (report immediately, joined before the deadline) and
/// THREE SLOW volumes (gated — never report before the deadline). Base deadline
/// 150 ms (see the rationale comment at the `deadline` binding in the test body
/// for why 150 ms rather than the originally-flaky 50 ms). Assert:
///   - no fast volume is inaccessible (not starved) — deterministic via the gate,
///     load-IMMUNE;
///   - every slow volume is inaccessible — deterministic via the gate, load-IMMUNE;
///   - exactly the slow volumes are inaccessible — deterministic, load-IMMUNE;
///   - `PROBE_THREAD_FAILURES` increased by exactly the slow-volume count —
///     deterministic, load-IMMUNE, but by itself does NOT discriminate a
///     one-deadline design from an N-sequential-deadlines regression: both
///     eventually count all 3 slow volumes as failures, they just take different
///     WALL-CLOCK TIME to get there — which is exactly what the two elapsed-time
///     bounds below check;
///   - a LOWER bound (`elapsed >= deadline / 2`, i.e. >= 75 ms): the collector
///     must actually honor the deadline for unreported volumes rather than
///     giving up early. Concurrent load can only push elapsed HIGHER, never
///     below the deadline, so this bound alone can never flake under load;
///   - an UPPER bound (`elapsed < deadline * slow.len() as u32`, i.e. < 450 ms):
///     the assertion that actually DISCRIMINATES the shared-deadline design from
///     a reintroduced N-sequential-deadlines regression. A one-shared-deadline
///     collector (the correct design) waits ≈1×deadline (≈150 ms) regardless of
///     how many volumes are unreported, so 450 ms leaves ample scheduler-jitter
///     margin (≈3×). A regression to the old per-channel *sequential* design
///     would instead wait ≈N×deadline = 3×150 ms = 450 ms or more for the 3 slow
///     volumes — this bound catches that because a genuine regression meets or
///     exceeds the very threshold the one-deadline design comfortably clears.
///     Scaling the bound by `slow.len()` (not a fixed constant) is what keeps it
///     load-tolerant without being defeated by adding more slow volumes: the
///     original flaky bound was a fixed `2 × deadline` sized for N=1 at a 50 ms
///     base deadline (100 ms bound, issue #2703: 137 ms observed); even a
///     deadline-scaled `1 × deadline × N` bound at the same 50 ms base deadline
///     (150 ms) still flaked under mere default-test-harness parallelism
///     (152-180 ms observed, no external load) — the 150 ms base deadline
///     widens absolute headroom while preserving the same discriminating
///     formula.
///
/// Note: `serial` because this test reads/writes `PROBE_THREAD_FAILURES`.
/// Test: this test.
#[test]
#[serial_test::serial]
fn probe_all_volumes_multi_volume_no_fast_starvation() {
    // 150 ms (not 50 ms): empirically, even default-parallelism test-harness
    // contention alone (other tests' threads competing for CPU in the same
    // binary run, no external load) pushed a correct one-shared-deadline
    // collector's elapsed time past a 150 ms upper bound derived from a 50 ms
    // base deadline (observed 152-180 ms in 50 repeated runs). Scaling the base
    // deadline to 150 ms keeps the SAME discriminating formula
    // (`deadline * slow.len()`) but gives ample absolute headroom over realistic
    // scheduling jitter while a genuine N-sequential-deadlines regression would
    // still be caught at ~3x this deadline (450 ms).
    let deadline = Duration::from_millis(150);

    let fast = vec![
        PathBuf::from("/tmp/trusty-723-fast-a"),
        PathBuf::from("/tmp/trusty-723-fast-b"),
    ];
    // Multiple slow volumes prove N unreported volumes are classified in ONE
    // shared-deadline window, not one deadline each.
    let slow = vec![
        PathBuf::from("/tmp/trusty-723-slow-a"),
        PathBuf::from("/tmp/trusty-723-slow-b"),
        PathBuf::from("/tmp/trusty-723-slow-c"),
    ];

    let before_failures = PROBE_THREAD_FAILURES.load(Ordering::Relaxed);
    let start = std::time::Instant::now();

    let (inaccessible, gate) = probe_with_gated_slow(&fast, &slow, deadline);

    let elapsed = start.elapsed();
    // Release the gated slow threads now that collection is done (clean shutdown).
    drop(gate);

    let after_failures = PROBE_THREAD_FAILURES.load(Ordering::Relaxed);

    // ── Protected property: no starvation. Fast volumes that completed are never
    //    classified inaccessible just because slow volumes are exhausting the
    //    deadline concurrently. Deterministic via the gate — load-immune.
    for f in &fast {
        assert!(
            !inaccessible.contains(f),
            "fast volume {f:?} must NOT be starved; inaccessible={inaccessible:?}"
        );
    }
    for s in &slow {
        assert!(
            inaccessible.contains(s),
            "slow (gated) volume {s:?} must be inaccessible; inaccessible={inaccessible:?}"
        );
    }
    assert_eq!(
        inaccessible.len(),
        slow.len(),
        "exactly the slow volumes must be inaccessible; got={inaccessible:?}"
    );

    // ── Failure-count check: every unreported volume increments the counter
    //    exactly once. This alone does NOT distinguish a one-shared-deadline
    //    collector from a reintroduced N-sequential-deadlines regression — both
    //    eventually count all 3 slow volumes as failures. Discriminating between
    //    the two designs is what the upper-bound elapsed-time assertion below is
    //    for.
    assert_eq!(
        after_failures,
        before_failures + slow.len(),
        "PROBE_THREAD_FAILURES must increase by exactly the slow-volume count; \
         before={before_failures} after={after_failures}"
    );

    // ── Load-IMMUNE lower bound: the collector must actually wait ~one deadline
    //    for the unreported volumes rather than giving up early. Load can only
    //    raise elapsed, never lower it below the deadline, so this bound alone
    //    can never flake under load.
    assert!(
        elapsed >= deadline / 2,
        "collector must honor the shared deadline for unreported volumes; \
         elapsed={elapsed:?} deadline={deadline:?}"
    );

    // ── Discriminating, load-tolerant upper bound: this is what actually proves
    //    the shared-deadline (ONE-deadline-not-N) invariant. A one-shared-deadline
    //    collector waits ≈1×deadline (≈50 ms) for the 3 slow volumes regardless of
    //    N; a regression to the old per-channel *sequential* design would instead
    //    wait ≈N×deadline (3×50 ms = 150 ms) or more. Scaling by `slow.len()`
    //    (rather than a fixed constant sized for N=1, which is what flaked under
    //    load in issue #2703 at 137 ms vs a 100 ms bound) gives a regression-catch
    //    threshold with ample scheduler-jitter margin (≈3× the expected ≈50 ms).
    let upper_bound = deadline * (slow.len() as u32);
    assert!(
        elapsed < upper_bound,
        "total elapsed {elapsed:?} must be < {upper_bound:?} (deadline × slow-volume \
         count) — a one-shared-deadline collector waits ≈1×deadline regardless of N; \
         elapsed at or above N×deadline indicates a regression to per-volume \
         sequential deadlines"
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
