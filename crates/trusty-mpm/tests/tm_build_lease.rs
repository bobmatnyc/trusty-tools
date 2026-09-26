//! `tm build-lease` end to end, with real processes and real flocks (#8261).
//!
//! Every case runs the real `tm` binary under its own scratch `$HOME`, so each
//! has its own `~/.trusty-mpm/build-slots/` and its own `[builders]` config.
//! The config turns the pressure, load and census gates off (their decisions
//! are unit-tested with scripted readings in `core::build_lease`), so these
//! cases measure the lease mechanics alone and do not depend on how busy the
//! machine running them is. No daemon listens at the configured URL.

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
             memory_pressure_max = \"critical\"\nmin_available_pct = 0\nload_factor = 64\n{extra}"
        ),
    )
    .expect("config");
    home
}

fn build_lease(home: &Path) -> Command {
    let mut cmd = common::tm_command_in(home);
    cmd.current_dir(home)
        .env("TRUSTY_MPM_URL", "http://127.0.0.1:1")
        .env_remove("CARGO_TARGET_DIR")
        .arg("build-lease");
    cmd
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
    let dir: PathBuf = home.join(".trusty-mpm/build-slots");
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

/// Fail-open arm "lock dir uncreatable": neither `~/.trusty-mpm` nor the temp
/// fallback can hold a directory. The build is census-bounded, never a free
/// pass: it either runs UNLEASED with a warning naming the census, or — when
/// builds without a lease already fill the ceiling on this machine — waits and
/// exits 75. (The exact bound is unit-tested in
/// `unleased_builds_are_bounded_by_the_census`; this machine's own load decides
/// which arm runs here.)
#[test]
fn an_uncreatable_slot_dir_is_bounded_by_the_census() {
    let home = tempfile::Builder::new()
        .prefix("tm-test-build-lease-")
        .tempdir_in("/tmp")
        .expect("scratch home");
    std::fs::write(home.path().join(".trusty-mpm"), "not a directory").expect("file");
    let blocker = home.path().join("tmp-is-a-file");
    std::fs::write(&blocker, "x").expect("file");
    let out = build_lease(home.path())
        .env("TMPDIR", blocker.join("sub"))
        .args(["--wait-secs", "3", "--", "sh", "-c", "exit 4"])
        .output()
        .expect("run");
    let err = stderr(&out);
    match out.status.code() {
        Some(4) => {
            assert!(err.contains("running UNLEASED"), "{err}");
            assert!(err.contains("The census admitted it"), "{err}");
        }
        // Full, or — owner ruling 2026-09-21 — a census that cannot be read
        // either, which leaves nothing to bound the build.
        Some(75) => assert!(
            err.contains("fill the ceiling") || err.contains("process census is unreadable"),
            "{err}"
        ),
        other => panic!("neither census arm: {other:?}: {err}"),
    }
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
