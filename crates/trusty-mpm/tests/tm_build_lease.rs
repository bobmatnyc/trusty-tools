//! `tm build-lease` end to end, with real processes and real flocks (#8261).
//!
//! Every case runs the real `tm` binary under its own scratch `$HOME`, so each
//! has its own `~/.trusty-mpm/build-slots/` and its own `[builders]` config.
//! The config turns the pressure, load and census gates off (their decisions
//! are unit-tested with scripted readings in `core::build_lease`), so these
//! cases measure the lease mechanics alone and do not depend on how busy the
//! machine running them is. No daemon listens at the configured URL, except
//! in `a_held_slot_survives_a_daemon_restart_8819`, which restarts one. The
//! heavy-build table is `sleep`, `true`, `sh` and `touch`, so the real binary
//! leases these stand-in builds (it refuses anything the table does not match),
//! and the debug-only fallback-store override keeps every case off the
//! machine's real `/tmp` store.

use crate::common;

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

/// End a holder the way an interrupted build ends: SIGTERM, which `tm
/// build-lease` forwards to its build, then reap it.
///
/// Why: a SIGKILLed holder leaves its build running, and that orphaned build
/// keeps its slot until it exits (#8261), so a later lease in the
/// same home would wait on it.
fn stop(mut child: Child) {
    let pid = i32::try_from(child.id()).expect("pid");
    // SAFETY: kill(2) on a process this test started.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
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
/// `tm build-lease` process itself drops the flock too, but the build it
/// orphaned keeps the slot until that build exits (#8261).
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

    // Now the lease holder itself dies. The kernel releases its flock, but
    // its orphaned build still runs, so the slot stays taken until that build
    // exits (#8261).
    let mut holder = spawn_holder(home.path(), "30");
    let records = wait_for_holders(home.path(), 1);
    let orphan = u32::try_from(records[0]["child_pid"].as_u64().expect("pid")).expect("pid");
    holder.kill().expect("SIGKILL the holder");
    let _ = holder.wait();
    let refused = build_lease(home.path())
        .args(["--wait-secs", "2", "--", "true"])
        .output()
        .expect("next");
    // SAFETY: kill(2) on the orphaned build this test started.
    unsafe { libc::kill(i32::try_from(orphan).expect("pid"), libc::SIGKILL) };
    wait_until_dead(orphan);
    let next = build_lease(home.path())
        .args(["--wait-secs", "5", "--", "true"])
        .output()
        .expect("next");
    assert_eq!(
        refused.status.code(),
        Some(75),
        "the orphaned build keeps its slot: {}",
        stderr(&refused)
    );
    assert_eq!(
        next.status.code(),
        Some(0),
        "the slot is free once the orphan exits: {}",
        stderr(&next)
    );
}

/// Wait (bounded) until `pid` is gone; an orphan is reaped by init.
fn wait_until_dead(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while pid_alive(pid) {
        assert!(Instant::now() < deadline, "pid {pid} never exited");
        std::thread::sleep(Duration::from_millis(50));
    }
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

/// #8261 round 4: `--census` runs no build, lists each holder by program and
/// subcommand only, and prints the census header with the ceiling.
#[test]
fn the_census_view_lists_holders_without_argument_values() {
    let home = home_with_ceiling(3, "");
    let holder = build_lease(home.path())
        .args([
            "--",
            "sh",
            "-c",
            "sleep 30",
            "sh",
            "--token",
            "s3cr3t-census",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("holder");
    wait_for_holders(home.path(), 1);
    let out = build_lease(home.path())
        .arg("--census")
        .output()
        .expect("census");
    stop(holder);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(text.starts_with("tm build-lease census on "), "{text}");
    assert!(text.contains("1 lease(s) held, ceiling 3"), "{text}");
    assert!(text.contains("  lease slot 0: sh (pid "), "{text}");
    assert!(!text.contains("s3cr3t-census"), "{text}");
    // #8261 round 5: a marker proves the refused command never ran. #8261
    // round 6 (critic LOW): a stand-in `cargo` on `PATH`, not `touch` — this
    // test's own config lists `touch` as heavy only for its own convenience,
    // so a `cargo` marker proves the same thing without depending on that.
    use std::os::unix::fs::PermissionsExt;
    let ran = home.path().join("census-ran-the-command");
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    std::fs::write(
        bin.join("cargo"),
        format!("#!/bin/sh\ntouch '{}'\n", ran.display()),
    )
    .expect("stand-in cargo");
    std::fs::set_permissions(bin.join("cargo"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let both = build_lease(home.path())
        .env("PATH", path)
        .args(["--census", "--", "cargo", "build"])
        .output()
        .expect("both");
    assert_ne!(
        both.status.code(),
        Some(0),
        "--census with a command is refused"
    );
    assert!(!ran.exists(), "--census runs no command: {}", stderr(&both));
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

/// Like [`home_with_ceiling`], but the heavy-build table also matches a real
/// `cargo test` invocation (#8261 round 7, supervisor acceptance criterion 2:
/// the driven test needs the run path's own classifier to admit `cargo`, not
/// the `sleep`/`true`/`sh`/`touch` stand-ins the other cases use).
fn home_with_ceiling_and_cargo(ceiling: u32) -> tempfile::TempDir {
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
             heavy_build_commands = [\"sleep\", \"true\", \"sh\", \"touch\", \"cargo test\"]\n"
        ),
    )
    .expect("config");
    home
}

/// A fake `cargo` on `PATH` recording `$CARGO_TARGET_DIR` and its own
/// arguments (excluding argv[0], which the exec path resolves to an absolute
/// program path, not the literal `cargo` word), byte for byte, to the files
/// named by `FAKE_CARGO_TARGET_OUT` and `FAKE_CARGO_ARGV_OUT` (#8261 round 7,
/// supervisor acceptance criterion 2).
fn fake_cargo_bin(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).expect("fake bin dir");
    let script = dir.join("cargo");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         printf '%s' \"$CARGO_TARGET_DIR\" > \"$FAKE_CARGO_TARGET_OUT\"\n\
         : > \"$FAKE_CARGO_ARGV_OUT\"\n\
         for a in \"$@\"; do\n\
         printf '%s\\n' \"$a\" >> \"$FAKE_CARGO_ARGV_OUT\"\n\
         done\n",
    )
    .expect("write fake cargo");
    let mut perms = std::fs::metadata(&script).expect("meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod");
}

/// #8261 round 7 (supervisor acceptance criterion 2, 2026-09-27): before the
/// fix, `explicit_target_dir_arg` read a `--target-dir` value AFTER a literal
/// `--` as cargo's OWN flag, so a real `cargo test -- --target-dir <value>`
/// invocation used the forwarded test-binary argument as `ambient` instead of
/// `CARGO_TARGET_DIR`, and the rewrite then overwrote that forwarded argument
/// with the resolved slot directory. This drives the real run path — not
/// `plan`/`explicit_target_dir_arg`/`rewrite_target_dir_arg` directly — with a
/// private `CARGO_TARGET_DIR` and an `X` that is itself a directory this
/// repo's slot pool would replace, so a wrong `ambient` takes a visibly
/// different, observable route from a correct one.
/// Test: this function.
#[test]
fn a_target_dir_after_the_double_dash_is_never_read_as_cargos_own_flag() {
    let home = home_with_ceiling_and_cargo(2);
    let (repo, _pool, shared) = widget_repo(home.path());
    let fakebin = home.path().join("fakebin");
    fake_cargo_bin(&fakebin);
    let target_out = home.path().join("target-out");
    let argv_out = home.path().join("argv-out");
    let private_target = home.path().join("private-target");
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = build_lease(home.path())
        .current_dir(&repo)
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", &private_target)
        .env("FAKE_CARGO_TARGET_OUT", &target_out)
        .env("FAKE_CARGO_ARGV_OUT", &argv_out)
        .args(["--wait-secs", "3", "--"])
        .args(["cargo", "test", "--", "--target-dir"])
        .arg(&shared)
        .output()
        .expect("run");
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");

    // Criterion (a): the lease admits CARGO_TARGET_DIR, never the forwarded
    // `--target-dir` value after the double dash.
    let target =
        std::fs::read_to_string(&target_out).expect("the build recorded its CARGO_TARGET_DIR");
    assert_eq!(
        Path::new(&target),
        private_target,
        "the lease must admit CARGO_TARGET_DIR, not the forwarded --target-dir value: {err}"
    );

    // Criterion (b): the arguments handed to the child are byte-identical to
    // the input argv (excluding argv[0], the program name) — the forwarded
    // `--target-dir <shared>` is never rewritten.
    let want_argv: Vec<String> = ["test", "--", "--target-dir"]
        .into_iter()
        .map(String::from)
        .chain(std::iter::once(shared.display().to_string()))
        .collect();
    let got_argv: Vec<String> = std::fs::read_to_string(&argv_out)
        .expect("the build recorded its argv")
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        got_argv, want_argv,
        "the argv handed to the child must be byte-identical to the input argv: {err}"
    );
}

/// A real `tm daemon` on `port`, confined to `home` (#8819).
///
/// What: `--force` because no launchd supervises a test daemon; the scratch
/// `$HOME` disables its tmux and host-process discovery, and the orphan GC and
/// Telegram bot are switched off so it touches nothing outside `home`.
fn spawn_daemon(home: &Path, port: u16) -> Child {
    let child = common::tm_command_in(home)
        .current_dir(home)
        .env("TRUSTY_MPM_ORPHAN_GC", "0")
        .env_remove("TELEGRAM_BOT_TOKEN")
        .args(["daemon", "--force", "--addr"])
        .arg(format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a daemon");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !daemon_healthy(port) {
        assert!(Instant::now() < deadline, "no daemon answered on {port}");
        std::thread::sleep(Duration::from_millis(100));
    }
    child
}

/// Whether `GET /health` on `port` answers 200.
fn daemon_healthy(port: u16) -> bool {
    use std::io::{Read, Write};
    let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let request = "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    let mut reply = String::new();
    stream.write_all(request.as_bytes()).is_ok()
        && stream.read_to_string(&mut reply).is_ok()
        && reply.starts_with("HTTP/1.1 200")
}

/// Whether `pid` is a live process.
fn pid_alive(pid: u32) -> bool {
    let pid = i32::try_from(pid).expect("pid");
    // SAFETY: kill(2) with signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// #8819 under the build lease: a daemon restart forgets no held slot, because
/// the daemon holds none — the slot is the build's own kernel flock.
///
/// A holder leases the only slot and logs its decision in a real daemon. That
/// daemon is SIGKILLed (the `launchctl kickstart -k` case) and a new one is
/// started on the same port. While the holder runs, a second lease is refused
/// naming the holder's pid, and the restarted daemon logs the refusal. Once the
/// holding process exits, the slot is granted — so the refused lease also left
/// nothing held behind it (#8816).
#[test]
fn a_held_slot_survives_a_daemon_restart_8819() {
    let home = home_with_ceiling(1, "");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port();
    let url = format!("http://127.0.0.1:{port}");
    let mut daemon = spawn_daemon(home.path(), port);
    let holder = build_lease(home.path())
        .env("TRUSTY_MPM_URL", &url)
        .args(["--", "sleep", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a holder");
    let holder_pid = holder.id();
    wait_for_holders(home.path(), 1);

    daemon.kill().expect("SIGKILL the daemon");
    let _ = daemon.wait();
    let restarted = spawn_daemon(home.path(), port);

    let refused = build_lease(home.path())
        .env("TRUSTY_MPM_URL", &url)
        .args(["--wait-secs", "2", "--", "true"])
        .output()
        .expect("a second lease");
    let err = stderr(&refused);
    let holder_ran = pid_alive(holder_pid);
    stop(holder);
    let next = build_lease(home.path())
        .env("TRUSTY_MPM_URL", &url)
        .args(["--wait-secs", "5", "--", "true"])
        .output()
        .expect("a lease after the holder exits");
    stop(restarted);

    assert!(holder_ran, "the holder was alive during the refusal");
    assert_eq!(refused.status.code(), Some(75), "{err}");
    let holder_line = format!("slot 0: sleep 30 (pid {holder_pid},");
    assert!(err.contains(&holder_line), "missing {holder_line:?}: {err}");
    assert!(
        !err.contains("did not log"),
        "the restarted daemon logged the refusal: {err}"
    );
    assert_eq!(
        next.status.code(),
        Some(0),
        "the slot is released when its holder exits: {}",
        stderr(&next)
    );
}

/// Wait (bounded) for `path` to exist; `false` at the bound.
fn wait_for_file(path: &Path, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !path.exists() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// A one-test crate at `dir` whose test writes `$PROBE_DIR/running` and then
/// waits (at most 60 s) for `$PROBE_DIR/release`.
fn waiting_test_crate(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).expect("crate dir");
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"leaseprobe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("src/lib.rs"),
        "#[test]\nfn waits() {\n    let dir = std::path::PathBuf::from(std::env::var(\"PROBE_DIR\").unwrap());\n    \
         std::fs::write(dir.join(\"running\"), \"\").unwrap();\n    for _ in 0..600 {\n        \
         if dir.join(\"release\").exists() {\n            return;\n        }\n        \
         std::thread::sleep(std::time::Duration::from_millis(100));\n    }\n}\n",
    )
    .expect("lib");
}

/// The cargo that runs this suite, else the one on `PATH`.
fn real_cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// #8261: cargo releases its build-directory lock
/// (`.cargo-lock`) before it runs the test binaries. So `.cargo-lock` alone
/// cannot tell a slot's live `cargo test` run from an idle directory — the
/// reason a dead holder's record keeps its slot (see `core::build_lease::orphan`).
/// If a future cargo holds the lock through the run, this fails and the
/// record check becomes a second guard rather than the only one.
#[test]
fn cargo_releases_its_build_lock_while_test_binaries_run() {
    use trusty_mpm::core::build_lease::stale_guard::cargo_lock_held;
    let tmp = tempfile::tempdir().expect("tmp");
    let krate = tmp.path().join("probe");
    waiting_test_crate(&krate);
    let target = tmp.path().join("target");
    let run = Command::new(real_cargo())
        .args(["test", "--offline", "--quiet", "--manifest-path"])
        .arg(krate.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target)
        .env("PROBE_DIR", tmp.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cargo test");
    let running = wait_for_file(&tmp.path().join("running"), 180);
    let held = cargo_lock_held(&target);
    let lock_exists = target.join("debug/.cargo-lock").is_file();
    std::fs::write(tmp.path().join("release"), "").expect("release");
    let out = run.wait_with_output().expect("cargo test exits");
    assert!(running, "the test never ran: {}", stderr(&out));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(lock_exists, "cargo created its build lock");
    assert!(
        !held,
        "cargo held .cargo-lock while the test binary ran — the orphan record is \
         now a second guard, not the only one"
    );
}

/// #8261: a holder SIGKILLed while its `cargo test`
/// runs the test binary leaves that run alive with no flock and no
/// `.cargo-lock`. Its slot must stay taken until the run exits — before the
/// fix a second build took it at once.
#[test]
fn a_sigkilled_holders_live_test_run_keeps_its_slot() {
    let home = home_with_ceiling_and_cargo(1);
    let krate = home.path().join("probe");
    waiting_test_crate(&krate);
    let mut holder = build_lease(home.path())
        .current_dir(&krate)
        .env("PROBE_DIR", home.path())
        .args(["--", &real_cargo(), "test", "--offline", "--quiet"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a leased cargo test");
    let running = wait_for_file(&home.path().join("running"), 180);
    let records = wait_for_holders(home.path(), 1);
    let build = u32::try_from(records[0]["child_pid"].as_u64().expect("pid")).expect("pid");
    holder.kill().expect("SIGKILL the holder");
    let _ = holder.wait();
    let lock_free =
        !trusty_mpm::core::build_lease::stale_guard::cargo_lock_held(&krate.join("target"));
    let refused = build_lease(home.path())
        .args(["--wait-secs", "3", "--", "true"])
        .output()
        .expect("a second build");
    std::fs::write(home.path().join("release"), "").expect("release");
    wait_until_dead(build);
    let next = build_lease(home.path())
        .args(["--wait-secs", "5", "--", "true"])
        .output()
        .expect("a build after the run");
    let err = stderr(&refused);
    assert!(running, "the test binary never ran");
    assert!(
        lock_free,
        "the run holds no .cargo-lock while its test runs"
    );
    assert_eq!(
        refused.status.code(),
        Some(75),
        "two builders in one slot: {err}"
    );
    assert!(err.contains("slot 0: cargo test"), "{err}");
    assert_eq!(
        next.status.code(),
        Some(0),
        "the slot frees once the run exits: {}",
        stderr(&next)
    );
}

/// #6288 step 1: the best-effort decision log goes to the daemon's unix socket
/// (`mpm.build_lease.decision`) and never to TCP — `TRUSTY_MPM_URL` names a
/// listener that must see no connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tm_build_lease_logs_its_decision_over_the_socket() {
    use std::sync::{Arc, Mutex};
    use trusty_common::uds::server::{RpcRouter, RpcServeOptions, serve_until};

    let home = home_with_ceiling(2, "");
    let canary = std::net::TcpListener::bind("127.0.0.1:0").expect("canary");
    canary.set_nonblocking(true).expect("nonblocking");
    let socket = home.path().join("App Support").join("trusty-mpm.sock");
    std::fs::create_dir_all(socket.parent().expect("parent")).expect("socket dir");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let record = Arc::clone(&seen);
    let router = RpcRouter::new().typed::<serde_json::Value, serde_json::Value, _, _>(
        "mpm.build_lease.decision",
        move |params: serde_json::Value| {
            let record = Arc::clone(&record);
            async move {
                record.lock().expect("lock").push(params);
                Ok(serde_json::json!({}))
            }
        },
    );
    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(
            &listener,
            Arc::new(router),
            RpcServeOptions::default(),
            async {
                let _ = shutdown.await;
            },
        )
        .await;
    });

    let mut cmd = build_lease(home.path());
    cmd.env("TRUSTY_MPM_SOCKET", &socket)
        .env(
            "TRUSTY_MPM_URL",
            format!("http://{}", canary.local_addr().expect("addr")),
        )
        .args(["--", "true"]);
    let out = tokio::task::spawn_blocking(move || cmd.output())
        .await
        .expect("join")
        .expect("run");
    let _ = stop.send(());

    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(!err.contains("did not log"), "{err}");
    let seen = seen.lock().expect("lock");
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["verdict"], "admitted");
    assert!(
        matches!(canary.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "the decision log dialled TCP"
    );
}
