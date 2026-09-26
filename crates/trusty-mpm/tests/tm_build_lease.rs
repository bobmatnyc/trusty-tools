//! `tm build-lease` end to end, with real processes and real flocks (#8261).
//!
//! Every case runs the real `tm` binary under its own scratch `$HOME`, so each
//! has its own `~/.trusty-mpm/build-slots/` and its own `[builders]` config.
//! The config turns the pressure, load and census gates off (their decisions
//! are unit-tested with scripted readings in `core::build_lease`), so these
//! cases measure the lease mechanics alone and do not depend on how busy the
//! machine running them is. No daemon listens at the configured URL. The
//! heavy-build table is `sleep`, `true`, `sh` and `touch`, so the real binary
//! leases these stand-in builds (it refuses anything the table does not match),
//! and the debug-only fallback-store override keeps every case off the
//! machine's real `/tmp` store.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// A `[builders]` section with only the ceiling and the lease mechanics live.
fn home_with_ceiling(ceiling: u32, extra: &str) -> tempfile::TempDir {
    let home = tempfile::Builder::new()
        .prefix("tm-test-build-lease-")
        .tempdir_in("/tmp")
        .expect("scratch home");
    let dir = home.path().join(".trusty-mpm");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "[builders]\nmax_concurrent = {ceiling}\ncount_foreign_builds = false\n\
             memory_pressure_max = \"critical\"\nmin_available_pct = 0\nload_factor = 64\n\
             heavy_build_commands = [\"sleep\", \"true\", \"sh\", \"touch\"]\n{extra}"
        ),
    )
    .expect("config");
    home
}

fn build_lease(home: &Path) -> Command {
    let mut cmd = common::tm_command_in(home);
    cmd.current_dir(home)
        .env("TRUSTY_MPM_URL", "http://127.0.0.1:1")
        .env(
            "TRUSTY_MPM_TEST_BUILD_SLOT_FALLBACK",
            home.join("fallback-store"),
        )
        .env_remove("CARGO_TARGET_DIR")
        .arg("build-lease");
    cmd
}

/// `git init` `dir`, with `origin` as its remote when given.
fn git_repo(dir: &Path, origin: Option<&str>) {
    std::fs::create_dir_all(dir).expect("repo dir");
    let mut steps = vec![vec!["init", "-q"]];
    if let Some(url) = origin {
        steps.push(vec!["remote", "add", "origin", url]);
    }
    for args in steps {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(&args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }
}

/// Append `line` to the scratch home's `config.toml`.
fn append_config(home: &Path, line: &str) {
    let config = home.join(".trusty-mpm/config.toml");
    let body = std::fs::read_to_string(&config).expect("config");
    std::fs::write(&config, format!("{body}{line}\n")).expect("config");
}

fn spawn_holder(home: &Path, secs: &str) -> Child {
    build_lease(home)
        .args(["--", "sleep", secs])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn a holder")
}

/// The slot files' holder records, once `n` holders have written a child pid.
fn wait_for_holders(home: &Path, n: usize) -> Vec<serde_json::Value> {
    wait_for_holders_in(&home.join(".trusty-mpm/build-slots"), n)
}

/// How many `slot-K.lock` files `dir` holds.
fn count_lock_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|e| {
            e.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("slot-"))
                .count()
        })
        .unwrap_or(0)
}

/// [`wait_for_holders`] for the store at `dir`.
fn wait_for_holders_in(dir: &Path, n: usize) -> Vec<serde_json::Value> {
    let dir: PathBuf = dir.to_path_buf();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let records: Vec<serde_json::Value> = std::fs::read_dir(&dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".lock"))
                    .filter(|e| e.file_name().to_string_lossy().starts_with("slot-"))
                    .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                    .filter_map(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
                    .filter(|r| r["child_pid"].is_u64())
                    .collect()
            })
            .unwrap_or_default();
        if records.len() >= n {
            return records;
        }
        assert!(
            Instant::now() < deadline,
            "only {} of {n} holders came up",
            records.len()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn cli_parses_build_lease_and_passes_the_exit_code_through() {
    let home = home_with_ceiling(2, "");
    let out = build_lease(home.path())
        .args(["--", "sh", "-c", "echo out; exit 3"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "out\n",
        "stdout is the build's alone"
    );
}

/// The brief's case (b): with two holders on a ceiling of two, the third waits
/// its bound and exits 75 naming the holders, the readings and the ceiling.
#[test]
fn the_n_plus_first_waits_then_times_out_with_the_named_code() {
    let home = home_with_ceiling(2, "");
    let a = spawn_holder(home.path(), "30");
    let b = spawn_holder(home.path(), "30");
    wait_for_holders(home.path(), 2);
    let started = Instant::now();
    let out = build_lease(home.path())
        .args(["--wait-secs", "3", "--", "true"])
        .output()
        .expect("run the third");
    let waited = started.elapsed();
    stop(a);
    stop(b);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(75), "{err}");
    assert!(waited >= Duration::from_secs(2), "it waited ({waited:?})");
    for needle in [
        "waiting for a build slot",
        "no build slot freed within 3s",
        "2 of 2 slot(s) held",
        "slot 0: sleep 30",
        "slot 1: sleep 30",
        "ceiling 2",
        "host ",
        "memory pressure",
    ] {
        assert!(err.contains(needle), "missing {needle:?} in: {err}");
    }
}

/// A build killed with SIGKILL frees its slot at once — the holder exits with
/// the build's signal status and the kernel drops the flock. Killing the
/// `tm build-lease` process itself frees it the same way.
#[test]
fn a_sigkilled_build_or_holder_releases_the_slot() {
    let home = home_with_ceiling(1, "");
    let holder = spawn_holder(home.path(), "30");
    let records = wait_for_holders(home.path(), 1);
    let child_pid = i32::try_from(records[0]["child_pid"].as_u64().expect("pid")).expect("pid");
    // SAFETY: kill(2) on the build process this test started.
    assert_eq!(unsafe { libc::kill(child_pid, libc::SIGKILL) }, 0);
    let out = holder.wait_with_output().expect("holder exits");
    assert_eq!(out.status.code(), Some(128 + 9), "{}", stderr(&out));
    let next = build_lease(home.path())
        .args(["--wait-secs", "5", "--", "true"])
        .output()
        .expect("next");
    assert_eq!(
        next.status.code(),
        Some(0),
        "the slot was released: {}",
        stderr(&next)
    );

    // Now the lease holder itself dies; its orphaned build is reaped after.
    let mut holder = spawn_holder(home.path(), "30");
    let records = wait_for_holders(home.path(), 1);
    let orphan = i32::try_from(records[0]["child_pid"].as_u64().expect("pid")).expect("pid");
    holder.kill().expect("SIGKILL the holder");
    let _ = holder.wait();
    let next = build_lease(home.path())
        .args(["--wait-secs", "5", "--", "true"])
        .output()
        .expect("next");
    // SAFETY: kill(2) on the orphaned build this test started.
    unsafe { libc::kill(orphan, libc::SIGKILL) };
    assert_eq!(
        next.status.code(),
        Some(0),
        "the kernel released the flock: {}",
        stderr(&next)
    );
}

/// Fail-open arm "daemon down", plus old-config compatibility: a config still
/// carrying `free_memory_floor_mb` loads, warns, and the build runs.
#[test]
fn a_lease_runs_when_the_daemon_is_down() {
    let home = home_with_ceiling(1, "free_memory_floor_mb = 8192\n");
    let out = build_lease(home.path())
        .args(["--", "true"])
        .output()
        .expect("run");
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(err.contains("did not log this admitted decision"), "{err}");
    assert!(err.contains("free_memory_floor_mb = 8192"), "{err}");
    assert!(err.contains("is ignored"), "{err}");
}

/// Assert a degraded UNLEASED run: the command ran (exit 0), and stderr names
/// the lease store, the error and the repair (owner ruling "allow up to the
/// cap", #8261 round 3).
fn assert_degraded_unleased_run(out: &Output, ran: &Path, store: &str) {
    let err = stderr(out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(ran.exists(), "the census had room, so the build ran: {err}");
    for needle in [
        "DEGRADED",
        "running UNLEASED",
        store,
        "Repair:",
        "The census admitted it",
    ] {
        assert!(err.contains(needle), "missing {needle:?}: {err}");
    }
}

/// A `sh -c` build (heavy under the DEFAULT table too, since it wraps
/// `cargo build`, which fails fast here) that touches `ran`.
fn touching_build(ran: &Path) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!(
            "cargo build --offline >/dev/null 2>&1; touch '{}'",
            ran.display()
        ),
    ]
}

/// Neither `~/.trusty-mpm/build-slots` nor the fixed fallback can hold a store,
/// and `TMPDIR` is unusable too: the build runs UNLEASED within the cap (64),
/// and says so, naming the store, the error and the repair. Round 2's version
/// of this test accepted either 4 or 75 and so could not fail.
#[test]
fn an_unusable_store_runs_unleased_up_to_the_cap() {
    let home = home_with_ceiling(64, "");
    std::fs::write(home.path().join(".trusty-mpm/build-slots"), "a file").expect("file");
    let blocker = home.path().join("tmp-is-a-file");
    std::fs::write(&blocker, "x").expect("file");
    let ran = home.path().join("the-build-ran");
    let out = build_lease(home.path())
        .env("TMPDIR", blocker.join("sub"))
        .env("TRUSTY_MPM_TEST_BUILD_SLOT_FALLBACK", blocker.join("store"))
        .args(["--wait-secs", "3", "--"])
        .args(touching_build(&ran))
        .output()
        .expect("run");
    assert_degraded_unleased_run(&out, &ran, ".trusty-mpm/build-slots");
}

/// An `admission.lock` that is a directory: unleased within the cap (64), with
/// the degraded status naming it.
#[test]
fn an_admission_lock_directory_runs_unleased_up_to_the_cap() {
    let home = home_with_ceiling(64, "");
    let store = home.path().join(".trusty-mpm/build-slots");
    std::fs::create_dir_all(store.join("admission.lock")).expect("mkdir");
    let ran = home.path().join("the-build-ran");
    let out = build_lease(home.path())
        .args(["--wait-secs", "3", "--"])
        .args(touching_build(&ran))
        .output()
        .expect("run");
    assert_degraded_unleased_run(
        &out,
        &ran,
        &store.join("admission.lock").display().to_string(),
    );
}

/// No slot file can be locked — slot-0 is a directory and the store is
/// read-only: unleased within the cap (64), degraded status named.
#[test]
fn every_unlockable_slot_file_runs_unleased_up_to_the_cap() {
    use std::os::unix::fs::PermissionsExt;
    let home = home_with_ceiling(64, "");
    let store = home.path().join(".trusty-mpm/build-slots");
    std::fs::create_dir_all(store.join("slot-0.lock")).expect("mkdir");
    std::fs::write(store.join("admission.lock"), "").expect("admission");
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let ran = home.path().join("the-build-ran");
    let out = build_lease(home.path())
        .args(["--wait-secs", "3", "--"])
        .args(touching_build(&ran))
        .output()
        .expect("run");
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).expect("restore");
    assert_degraded_unleased_run(&out, &ran, &store.display().to_string());
}

/// "Allow up to the cap": one unleased build already runs (started under a
/// home whose cap is 64), and a second one under a cap of 1 is refused — the
/// census counts the earlier unleased `tm build-lease` whatever else runs.
#[test]
fn unleased_is_refused_once_the_census_reaches_the_cap() {
    let roomy = home_with_ceiling(64, "");
    let tight = home_with_ceiling(1, "");
    for home in [&roomy, &tight] {
        std::fs::create_dir_all(home.path().join(".trusty-mpm/build-slots/admission.lock"))
            .expect("mkdir");
    }
    let first = build_lease(roomy.path())
        .args(["--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the first unleased build");
    // Its degraded warning is printed once it is admitted.
    std::thread::sleep(Duration::from_millis(1500));
    let ran = tight.path().join("the-build-ran");
    let out = build_lease(tight.path())
        .args(["--wait-secs", "3", "--", "touch"])
        .arg(&ran)
        .output()
        .expect("the second");
    stop(first);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(75), "{err}");
    assert!(!ran.exists(), "{err}");
    assert!(err.contains("fill the ceiling 1"), "{err}");
    assert!(
        err.contains("UNKNOWN lease state"),
        "the degraded state is named: {err}"
    );
}

/// #8261 round 3 (critic finding 2): the fallback store is fixed, never
/// `$TMPDIR`, so two sessions with different `TMPDIR`s still share one cap.
/// The canonical store is unusable because `build-slots` is a FILE (so the
/// config, and its ceiling of 1, still load).
#[test]
fn two_sessions_with_different_tmpdirs_share_one_store() {
    let home = home_with_ceiling(1, "");
    std::fs::write(home.path().join(".trusty-mpm/build-slots"), "a file").expect("file");
    let (tmp_a, tmp_b) = (home.path().join("a"), home.path().join("b"));
    std::fs::create_dir_all(&tmp_a).expect("a");
    std::fs::create_dir_all(&tmp_b).expect("b");
    let holder = build_lease(home.path())
        .env("TMPDIR", &tmp_a)
        .args(["--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("holder A");
    // A's lease is visible in whichever store A resolved.
    let deadline = Instant::now() + Duration::from_secs(20);
    while count_lock_files(&home.path().join("fallback-store")) == 0
        && std::fs::read_dir(&tmp_a).map_or(0, Iterator::count) == 0
    {
        assert!(Instant::now() < deadline, "holder A never took a lease");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    let out = build_lease(home.path())
        .env("TMPDIR", &tmp_b)
        .args(["--wait-secs", "5", "--", "true"])
        .output()
        .expect("B");
    let waited = started.elapsed();
    stop(holder);
    let err = stderr(&out);
    assert_eq!(
        out.status.code(),
        Some(75),
        "B shares A's store and waits: {err}"
    );
    assert!(
        waited >= Duration::from_secs(2),
        "it waited ({waited:?}): {err}"
    );
    assert!(err.contains("fallback-store"), "{err}");
}

/// Owner ruling 2026-09-21 on #8261: an unsafe target never admits. The build
/// inherits the machine's SHARED `CARGO_TARGET_DIR` and its slot directory
/// cannot be made; before this it ran in the shared directory anyway — the
/// cross-worktree clobbering the lease exists to stop. Now it does not run.
#[test]
fn an_unusable_slot_directory_refuses_instead_of_sharing() {
    let home = home_with_ceiling(1, "");
    let pool_blocker = home.path().join("pool-is-a-file");
    std::fs::write(&pool_blocker, "x").expect("file");
    let config = home.path().join(".trusty-mpm/config.toml");
    let body = std::fs::read_to_string(&config).expect("config");
    std::fs::write(
        &config,
        format!("{body}slot_pool_root = \"{}\"\n", pool_blocker.display()),
    )
    .expect("config");
    let repo = home.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    for args in [
        &["init", "-q"][..],
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ][..],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }
    let shared = home.path().join(".trusty-tools/cargo-target/acme/widget");
    let ran = home.path().join("the-build-ran");
    let out = build_lease(home.path())
        .current_dir(&repo)
        .env("CARGO_TARGET_DIR", &shared)
        .args(["--wait-secs", "3", "--", "touch"])
        .arg(&ran)
        .output()
        .expect("run");
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(75), "{err}");
    assert!(
        !ran.exists(),
        "the build must not run in the shared dir: {err}"
    );
    assert!(err.contains("is unusable"), "{err}");
    assert!(err.contains("shared target directory"), "{err}");
}

/// #8261 round 3 (critic finding 4): the lease program is not a way to run an
/// arbitrary command — anything the heavy-build classifier does not match is
/// refused and never runs.
#[test]
fn a_command_that_is_not_a_heavy_build_is_refused() {
    let home = home_with_ceiling(2, "");
    let victim = home.path().join("keep-me");
    std::fs::write(&victim, "x").expect("file");
    let out = build_lease(home.path())
        .args(["--", "rm", "-f"])
        .arg(&victim)
        .output()
        .expect("run");
    let err = stderr(&out);
    assert!(!out.status.success(), "{err}");
    assert!(victim.exists(), "the command must not run: {err}");
    assert!(err.contains("not a heavy build"), "{err}");
}

/// #8261 round 3 (critic finding 8): a waiter's refusal names the holder by
/// program and subcommand only, never an argument value; slot files are 0600.
#[test]
fn a_waiter_never_sees_a_holders_argument_values() {
    use std::os::unix::fs::PermissionsExt;
    let home = home_with_ceiling(1, "");
    let holder = build_lease(home.path())
        .args([
            "--",
            "sh",
            "-c",
            "sleep 30",
            "sh",
            "--token",
            "s3cr3t-token-value",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("holder");
    wait_for_holders(home.path(), 1);
    let slot = home.path().join(".trusty-mpm/build-slots/slot-0.lock");
    let mode = std::fs::metadata(&slot)
        .expect("slot file")
        .permissions()
        .mode();
    let out = build_lease(home.path())
        .args(["--wait-secs", "2", "--", "true"])
        .output()
        .expect("waiter");
    stop(holder);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(75), "{err}");
    assert!(
        err.contains("slot 0: sh (pid "),
        "the holder is named: {err}"
    );
    assert!(!err.contains("s3cr3t-token-value"), "{err}");
    assert_eq!(mode & 0o777, 0o600, "slot file mode {mode:o}");
}

/// A repo `acme/widget` under the scratch home, with the pool root configured
/// under it, and the repo's shared target directory.
fn widget_repo(home: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let pool = home.join("pool");
    append_config(home, &format!("slot_pool_root = \"{}\"", pool.display()));
    let repo = home.join("repo");
    git_repo(&repo, Some("https://github.com/acme/widget.git"));
    let shared = home.join(".trusty-tools/cargo-target/acme/widget");
    (repo, pool, shared)
}

/// `sh -c` that records the `CARGO_TARGET_DIR` the build was given in `out`.
fn record_target_dir(out: &Path) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!("printf %s \"$CARGO_TARGET_DIR\" > '{}'", out.display()),
    ]
}

/// #8261 round 3 (critic finding 5): a SIGKILLed holder's build still holds
/// cargo's `debug/.cargo-lock` in slot 0. A lease from another checkout must
/// not be handed slot 0's directory, and slot 0's fingerprints must survive.
#[test]
fn a_busy_orphan_slot_is_not_reused() {
    use std::os::fd::AsRawFd;
    let home = home_with_ceiling(2, "");
    let (repo, pool, shared) = widget_repo(home.path());
    let slot0 = pool.join("acme/widget/slot-0");
    let fingerprint = slot0.join("debug/.fingerprint/widget-0123456789abcdef");
    std::fs::create_dir_all(&fingerprint).expect("fingerprint");
    std::fs::write(slot0.join(".trusty-slot-seeded"), "seeded").expect("marker");
    std::fs::write(slot0.join(".trusty-slot-last-checkout"), "/elsewhere/wt").expect("last");
    let lock = slot0.join("debug/.cargo-lock");
    std::fs::write(&lock, "").expect("cargo lock");
    let orphan = std::fs::File::open(&lock).expect("open");
    // SAFETY: `orphan` owns a valid descriptor; this stands in for the build.
    assert_eq!(unsafe { libc::flock(orphan.as_raw_fd(), libc::LOCK_EX) }, 0);
    let got = home.path().join("target-dir");
    let out = build_lease(home.path())
        .current_dir(&repo)
        .env("CARGO_TARGET_DIR", &shared)
        .arg("--wait-secs")
        .arg("3")
        .arg("--")
        .args(record_target_dir(&got))
        .output()
        .expect("run");
    drop(orphan);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    let target = std::fs::read_to_string(&got).expect("the build recorded its target");
    assert_ne!(Path::new(&target), slot0, "slot 0 is busy: {err}");
    assert!(target.ends_with("slot-1"), "{target}");
    assert!(fingerprint.exists(), "slot 0's fingerprints survive: {err}");
}

/// #8261 round 3 (critic finding 6): a checkout with no git origin and the
/// shared directory inherited has no pool to replace it with — refused.
#[test]
fn a_shared_target_without_a_repo_identity_refuses() {
    let home = home_with_ceiling(2, "");
    let repo = home.path().join("no-origin");
    git_repo(&repo, None);
    let shared = home.path().join(".trusty-tools/cargo-target/acme/widget");
    let ran = home.path().join("the-build-ran");
    let out = build_lease(home.path())
        .current_dir(&repo)
        .env("CARGO_TARGET_DIR", &shared)
        .args(["--wait-secs", "3", "--", "touch"])
        .arg(&ran)
        .output()
        .expect("run");
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(75), "{err}");
    assert!(
        !ran.exists(),
        "the build must not run in the shared dir: {err}"
    );
    assert!(err.contains("shared target directory"), "{err}");
}

/// #8261 round 3 (critic finding 6): an ambient `<pool>/acme/widget/slot-0`
/// while another lease holds slot 0 is shared, not pinned — the build runs in
/// its own slot's directory.
#[test]
fn an_ambient_pool_slot_held_by_another_lease_is_not_used() {
    let home = home_with_ceiling(2, "");
    let (repo, pool, _) = widget_repo(home.path());
    let holder = build_lease(home.path())
        .current_dir(&repo)
        .args(["--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("holder of slot 0");
    let records = wait_for_holders(home.path(), 1);
    assert_eq!(records[0]["slot"], 0);
    let slot0 = pool.join("acme/widget/slot-0");
    let got = home.path().join("target-dir");
    let out = build_lease(home.path())
        .current_dir(&repo)
        .env("CARGO_TARGET_DIR", &slot0)
        .arg("--wait-secs")
        .arg("3")
        .arg("--")
        .args(record_target_dir(&got))
        .output()
        .expect("run");
    stop(holder);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    let target = std::fs::read_to_string(&got).expect("the build recorded its target");
    assert_ne!(Path::new(&target), slot0, "{err}");
    assert!(target.ends_with("slot-1"), "{target}");
}
