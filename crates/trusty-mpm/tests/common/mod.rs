//! Per-process `$HOME` isolation for trusty-mpm's integration targets (#6671).
//!
//! Why: `DaemonState::project_registry()` seeds itself from
//! `TrustyToolsConfig::load()`, which resolves
//! `~/.trusty-tools/trusty-mpm/config.yaml` through `dirs::home_dir()` — i.e.
//! `$HOME`. A target that isolates only the daemon's framework root therefore
//! still reads the DEVELOPER's registered projects, so assertions on portfolio
//! counts describe that machine rather than the fixture (#6671 observed
//! `left Number(22) right 0`). #4120 established the per-test `$HOME` redirect;
//! these five targets never adopted it.
//!
//! What: [`scratch_home`] points the whole test PROCESS at a fresh scratch
//! directory exactly once, inside a [`std::sync::OnceLock`] initialiser. A test
//! that calls it either performs the redirect or blocks until another thread's
//! redirect is visible, so no test can observe the real `$HOME`. Exactly one
//! value is ever written, so — unlike the save-and-restore `HomeGuard` pattern
//! used elsewhere in this suite — there is no window in which one test's
//! restore un-isolates another test's read.
//!
//! Test: every test in `manager_routes`, `project_registry_routes`,
//! `manager_cli_client`, `manager_inference` and `mcp_spawn_gate`. Each now
//! asserts against a registry the host machine cannot reach.
//!
//! #7568 adds the second half of the same concern: a target that spawns the
//! built `tm` binary hands the CHILD an environment, and a child that inherits
//! `$HOME` resolves the operator's own `~/.trusty-mpm`. [`tm_command`],
//! [`tm_command_in`] and [`isolate_spawned_tm`] are the one seam every such
//! spawn goes through; `tests/spawned_tm_home_isolation.rs` is the ratchet that
//! keeps them the only one.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Arm `core::home_write_fence` for this integration target, before `main`
/// (#8545).
///
/// Why: every integration target declares `mod common;` (ratcheted by
/// `every_integration_target_arms_the_home_write_fence`), so this one
/// constructor fences them all. It runs before [`scratch_home`] can repoint
/// `$HOME`, so the fenced roots are the harness's home and the password-database
/// home, never the scratch dir.
/// What: records the fenced roots; an in-process writer that reaches one panics.
/// Also records [`operator_home`] while `$HOME` is still the harness's own.
/// A spawned `tm` child is not fenced — [`isolate_spawned_tm`] confines it.
/// Test: `the_home_write_fence_is_armed_for_integration_targets`.
#[ctor::ctor]
fn arm_home_write_fence() {
    OPERATOR_HOME.get_or_init(|| std::env::var_os("HOME").map(PathBuf::from));
    trusty_mpm::core::home_write_fence::arm_for_this_process();
}

/// `$HOME` as the harness started this process, before any test repointed it.
static OPERATOR_HOME: OnceLock<Option<PathBuf>> = OnceLock::new();

/// The operator's own `$HOME`, as it was before `main` (#8345).
///
/// Why: a guard that asserts the operator's files stay untouched must name the
/// real ones. Since #8345 a module shares its process with [`scratch_home`]'s
/// callers, so reading `$HOME` mid-run can return the scratch dir and turn that
/// guard vacuous.
/// What: the value [`arm_home_write_fence`] captured; `None` when the harness
/// ran with no `$HOME`, which is the stripped-CI case.
/// Test: `a_spawned_tm_resolves_the_helper_home_and_leaves_the_operators_alone`.
pub fn operator_home() -> Option<&'static Path> {
    OPERATOR_HOME
        .get_or_init(|| std::env::var_os("HOME").map(PathBuf::from))
        .as_deref()
}

/// Give this integration target its own default tmux server, before `main`
/// (#6542). Its spawned children inherit it: [`CHILD_STATE_ENV`] clears `TMUX`
/// but keeps `TMUX_TMPDIR`. Aborts rather than let a test reach the operator's
/// server. See `trusty_mpm::core::tmux_test_isolation`.
#[ctor::ctor]
fn isolate_tmux_server() {
    trusty_mpm::core::tmux_test_isolation::isolate_for_this_process()
        .expect("#6542: create this test binary's private tmux directory");
}

/// Kill the private tmux servers and remove their directory at exit (#6542).
#[ctor::dtor]
fn teardown_tmux_server() {
    trusty_mpm::core::tmux_test_isolation::teardown_for_this_process();
}

/// Redirect this test process's `$HOME` to a scratch directory, once (#6671).
///
/// Returns the scratch home, so a caller may plant fixtures under it.
///
/// The directory is deliberately `keep()`-ed rather than dropped: it must
/// outlive every test in the process, and a `static` is never dropped anyway.
/// It carries the crate's `tm-test-` prefix under `/tmp`, so
/// `test_support::sweep_stale_test_dirs` — which runs in the lib target on
/// every `cargo test -p trusty-mpm` — reaps it after a day.
pub fn scratch_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::Builder::new()
            .prefix("tm-test-home-")
            .tempdir_in("/tmp")
            .expect("create scratch $HOME")
            .keep();
        // SAFETY: this runs inside `OnceLock::get_or_init`, so exactly one
        // thread ever writes `HOME` in this process and every other thread is
        // blocked until that write is visible. In the parallel `integration`
        // target nothing else mutates `HOME` (ratcheted by
        // `every_integration_target_arms_the_home_write_fence`). `env_serial`
        // modules do rewrite it, which is safe only because that target runs
        // one test at a time (#8345).
        unsafe { std::env::set_var("HOME", &dir) };
        dir
    })
    .as_path()
}

/// The `tm` binary cargo built for this integration target (#7568).
///
/// Why: this is the ONE place `CARGO_BIN_EXE_tm` is named. Every other spawn
/// site reaches the binary through [`tm_command`] / [`tm_command_in`], which
/// also isolate the child; the two targets that need the raw PATH (a `sh -c`
/// pipeline, a `PATH` shim) are ratcheted by
/// `tests/spawned_tm_home_isolation.rs` and isolate the outer process
/// themselves with [`isolate_spawned_tm`].
pub fn tm_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tm")
}

/// Variables that would point a spawned `tm` back at the operator's own state.
///
/// Why (#7568): redirecting `$HOME` alone is not enough. `TRUSTY_MPM_ROOT` and
/// the XDG config file both outrank the home-relative default in the framework
/// root chain (`--root` > `TRUSTY_MPM_ROOT` > XDG config > `~/.trusty-mpm`),
/// `CLAUDE_CONFIG_DIR` is absolute and survives a `$HOME` repoint (#5544), and
/// `CLAUDE_CODE_SESSION_ID` is what made a spawned child append a real row to
/// the operator's savings ledger (#7514). `TRUSTY_MPM_URL`, `TMUX` and the
/// managed-session ids make a child adopt the developer's live daemon, tmux
/// server and session rather than the fixture's. #6288: `TRUSTY_MPM_SOCKET`,
/// `TRUSTY_DATA_DIR_OVERRIDE` and `XDG_DATA_HOME` pick the daemon socket the
/// socket-only commands dial, so each points a child at the developer's daemon.
///
/// What: cleared on the CHILD only. Nothing here touches this process's
/// environment, so the `#5544` hazard — a `set_var` visible to every test
/// running in parallel in the `integration` target — does not arise.
/// Test: `the_helper_clears_every_state_pointing_var`.
const CHILD_STATE_ENV: &[&str] = &[
    "TRUSTY_MPM_ROOT",
    "TRUSTY_MPM_URL",
    "TRUSTY_MPM_SOCKET",
    "TRUSTY_DATA_DIR_OVERRIDE",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "CLAUDE_CONFIG_DIR",
    trusty_mpm::core::savings::CLAUDE_CODE_SESSION_ID_ENV,
    "CLAUDE_SESSION_ID",
    "TM_MANAGED_SESSION_ID",
    "TRUSTY_MPM_MANAGED_SESSION_ID",
    "TMUX",
    "TMUX_PANE",
    "REPOS_ROOT",
    "GH_CONFIG_DIR",
];

/// Point `cmd`'s child at `home` and strip every other state-pointing variable.
///
/// Why (#7568): a spawned `tm` resolves `~/.trusty-mpm` from the `$HOME` it
/// inherits, so an integration target that spawns the binary writes into the
/// operator's own framework root — observed as `migrations.json`,
/// `compression.jsonl` and `usage/` markers appearing under a real `$HOME`
/// during `cargo test -p trusty-mpm`.
/// What: one `env("HOME", …)`, `TRUSTY_CONTENT_OFFLINE=1` (#9396: no
/// first-use content fetch), plus an `env_remove` per [`CHILD_STATE_ENV`]
/// entry, applied to the child's environment block. A caller that needs its own
/// value for one of these chains `.env(…)` afterwards — the later call for a key
/// wins — and a caller that needs `$HOME` absent chains `.env_remove("HOME")`.
/// Test: `a_spawned_tm_resolves_the_helper_home_and_leaves_the_operators_alone`,
/// `the_helper_clears_every_state_pointing_var`.
pub fn isolate_spawned_tm<'a>(cmd: &'a mut Command, home: &Path) -> &'a mut Command {
    cmd.env("HOME", home);
    // #9396: a child under a scratch `$HOME` has no content installed; it
    // must report that, never fetch the release from GitHub.
    cmd.env(trusty_mpm::content::first_use::OFFLINE_ENV, "1");
    for key in CHILD_STATE_ENV {
        cmd.env_remove(key);
    }
    cmd
}

/// A `tm` command whose child is confined to `home`.
pub fn tm_command_in(home: &Path) -> Command {
    let mut cmd = Command::new(tm_bin());
    isolate_spawned_tm(&mut cmd, home);
    cmd
}

/// Variables a spawned `tm daemon` copies from this process, and nothing else.
///
/// Why (#9121): a daemon that inherits the operator's shell inherits its
/// secrets, and one holding `TELEGRAM_BOT_TOKEN` polls the real bot. Stripping
/// names one at a time ([`CHILD_STATE_ENV`]) misses every secret nobody listed.
/// What: `PATH` so the daemon finds `git` and `tmux`; `TMUX_TMPDIR` because the
/// `isolate_tmux_server` constructor points it at this binary's private tmux
/// directory, and a daemon without it would reach the operator's tmux server.
const DAEMON_PASSTHROUGH_ENV: &[&str] = &["PATH", "TMUX_TMPDIR"];

/// Give `cmd` the cleared, allowlisted environment of a test `tm daemon` (#9121).
///
/// Why: see [`DAEMON_PASSTHROUGH_ENV`]. Split from [`daemon_command`] so a test
/// can run the same environment under a probe program instead of the daemon.
/// What: `env_clear`, then `HOME` = `home` (confines the framework root and
/// config), `TRUSTY_MPM_WORKSPACE_ROOT` = `workspace_root` (the tree the disk
/// survey walks; without it the daemon falls back to a home-relative default),
/// `TRUSTY_MPM_ORPHAN_GC=0` (a test daemon must not reap processes it did not
/// start), `TRUSTY_CONTENT_OFFLINE=1` (#9396: no first-use content fetch), and
/// each [`DAEMON_PASSTHROUGH_ENV`] name this process has set.
/// Test: `a_test_daemon_env_carries_no_secret_shaped_variable`.
pub fn apply_daemon_env<'a>(
    cmd: &'a mut Command,
    home: &Path,
    workspace_root: &Path,
) -> &'a mut Command {
    cmd.env_clear()
        .env("HOME", home)
        .env("TRUSTY_MPM_WORKSPACE_ROOT", workspace_root)
        .env("TRUSTY_MPM_ORPHAN_GC", "0")
        // #9396: a test daemon never fetches the content release.
        .env(trusty_mpm::content::first_use::OFFLINE_ENV, "1");
    for key in DAEMON_PASSTHROUGH_ENV {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd
}

/// `tm daemon --force`, run in `home` with only [`apply_daemon_env`]'s variables.
///
/// `--force` because no launchd supervises a test daemon. The caller appends
/// `--addr` and any further flags.
pub fn daemon_command(home: &Path, workspace_root: &Path) -> Command {
    let mut cmd = Command::new(tm_bin());
    apply_daemon_env(&mut cmd, home, workspace_root)
        .current_dir(home)
        .args(["daemon", "--force"]);
    cmd
}

/// The scratch `$HOME` this test process hands its spawned children (#7568).
///
/// Why: most spawn sites never inspect what the child wrote — they assert on
/// stdout — so one scratch home per PROCESS is enough isolation and avoids
/// leaking a directory per spawn. A test that reads the child's writes back
/// calls [`tm_command_in`] with a home it owns.
/// What: a `OnceLock` scratch directory under `/tmp`, `keep()`-ed for the same
/// reason [`scratch_home`] keeps its own and carrying the same `tm-test-`
/// prefix, so `test_support::sweep_stale_test_dirs` reaps it after a day.
/// Unlike [`scratch_home`] this NEVER calls `set_var` — the path is handed to
/// spawned children and to nothing else.
pub fn tm_spawn_home() -> &'static Path {
    static SPAWN_HOME: OnceLock<PathBuf> = OnceLock::new();
    SPAWN_HOME
        .get_or_init(|| {
            tempfile::Builder::new()
                .prefix("tm-test-spawn-home-")
                .tempdir_in("/tmp")
                .expect("create scratch spawn $HOME")
                .keep()
        })
        .as_path()
}

/// A `tm` command confined to [`tm_spawn_home`].
pub fn tm_command() -> Command {
    tm_command_in(tm_spawn_home())
}

/// A `trusty-mpm` alias command confined to [`tm_spawn_home`].
///
/// The alias execs the `tm` beside it, so the child needs the same isolation.
pub fn alias_command() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trusty-mpm"));
    isolate_spawned_tm(&mut cmd, tm_spawn_home());
    cmd
}

/// Write `disk.max_usage_pct: <pct>` into `home`'s trusty-mpm config (#7497).
///
/// Why: a spawned `tm hook --pm-guard` gates every `git worktree add` on the
/// REAL volume's usage, and with no configured value the shipped 90% default
/// decides the verdict — so a test's outcome would track how full the host
/// disk is. An explicit threshold is the only seam; the gate has no env-var off
/// switch by design (`core::disk_usage_guard` module doc).
/// What: creates `<home>/.trusty-tools/trusty-mpm/config.yaml` holding only the
/// `disk:` section.
pub fn write_disk_threshold(home: &Path, pct: u8) {
    let dir = home.join(".trusty-tools").join("trusty-mpm");
    std::fs::create_dir_all(&dir).expect("create config dir");
    std::fs::write(
        dir.join("config.yaml"),
        format!("disk:\n  max_usage_pct: {pct}\n"),
    )
    .expect("write config");
}

/// The prefix every `tm hook --pm-guard` refusal starts with (#8546).
///
/// Mirrors `PM_GUARD_REFUSAL_PREFIX` in the `tm` binary, which an integration
/// target cannot import.
pub const PM_GUARD_REFUSAL_PREFIX: &str = "tm pm-guard: ";

/// Assert that every deny object in a `tm hook --pm-guard` stdout names its
/// layer: the reason starts with [`PM_GUARD_REFUSAL_PREFIX`] and carries it
/// exactly once (#8546).
///
/// Why: every pm-guard integration collector calls this, so a refusal path
/// added later without the prefix fails the first test that exercises it —
/// the test author does not have to remember to assert it.
/// What: parses each stdout line; a line that is not a JSON deny is ignored,
/// because an ALLOW prints nothing and a grant prints `updatedInput`.
pub fn assert_pm_guard_refusals_prefixed(stdout: &str) {
    for line in stdout.lines() {
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let output = &parsed["hookSpecificOutput"];
        if output["permissionDecision"] != "deny" {
            continue;
        }
        let reason = output["permissionDecisionReason"]
            .as_str()
            .unwrap_or_default();
        assert!(
            reason.starts_with(PM_GUARD_REFUSAL_PREFIX)
                && reason.matches(PM_GUARD_REFUSAL_PREFIX.trim_end()).count() == 1,
            "a tm pm-guard refusal must start with {PM_GUARD_REFUSAL_PREFIX:?} exactly once: {reason:?}"
        );
    }
}

/// Install the checkout's instructional content into `home` as a verified
/// content bundle (#9011, #9012).
///
/// Why: since #9011 a spawned `tm` reads its agents from instructional
/// content — a checkout above its cwd, else the bundle pinned under
/// `<home>/.trusty-mpm/content`. A test that pins the child OUTSIDE any
/// checkout (the ADR-0048 rule would otherwise rewrite its dispatch) has
/// neither, and pm-guard refuses every dispatch it cannot classify (owner
/// ruling 09(a)). This stages what `tm content install` would have.
/// What: a gzip tar of every file under `content/{agents,skills,instructions}`
/// (#9012: skills, PM sections, output styles and SM instructions are content
/// too) under the bundle paths the packager uses, plus `bundle-manifest.toml`,
/// written as `content-v0.0.1.tar.gz` with a `content-lock.toml` pinning its
/// sha256. Idempotent per `home`.
pub fn stage_repo_content(home: &Path) {
    use trusty_common::content::{ContentLock, LOCK_FILE_NAME};
    use trusty_common::integrity::Sha256Digest;

    const TAG: &str = "content-v0.0.1";
    let cache = home.join(".trusty-mpm").join("content");
    if cache.join(LOCK_FILE_NAME).exists() {
        return;
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut entries: Vec<(String, Vec<u8>)> = vec![(
        "bundle-manifest.toml".to_string(),
        format!("tag = \"{TAG}\"\nschema_major = 1\n").into_bytes(),
    )];
    // #9012: every class, recursively; dot-files never reach a bundle.
    fn walk(dir: &Path, key: &str, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).expect("content dir").flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let child = format!("{key}/{name}");
            if entry.path().is_dir() {
                walk(&entry.path(), &child, out);
            } else {
                out.push((child, std::fs::read(entry.path()).expect("content file")));
            }
        }
    }
    for class in ["agents", "skills", "instructions"] {
        walk(&repo.join("content").join(class), class, &mut entries);
    }
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    for (path, data) in &entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, path, data.as_slice())
            .expect("append");
    }
    let bytes = tar.into_inner().expect("tar").finish().expect("gzip");
    std::fs::create_dir_all(&cache).expect("content cache");
    let lock = ContentLock::new(TAG, Sha256Digest::of_bytes(&bytes)).expect("lock");
    std::fs::write(cache.join(lock.bundle_file_name()), &bytes).expect("bundle");
    lock.store(&cache.join(LOCK_FILE_NAME)).expect("store lock");
}
