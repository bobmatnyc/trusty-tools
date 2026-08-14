//! End-to-end coverage for the analyze preflight against the REAL
//! `trusty-analyze` binary (#5670).
//!
//! Why: every other test of [`super::ensure_analyze_daemon_with`] stands a stub
//! where the daemon should be, so all of them would still pass if
//! `trusty-analyze serve` stopped taking `--port`, stopped exiting when
//! trusty-search is unreachable, or stopped answering 2xx on `/health` when it
//! is up. Those three facts are the whole contract this preflight rests on, and
//! all three live in another crate's binary. This module is the only place they
//! are checked against the article rather than against a copy of the belief.
//!
//! What: two runs of the guard against `target/<profile>/trusty-analyze` — the
//! refusal arm (trusty-search unreachable) and the success arm (trusty-search
//! reachable).
//!
//! Both are `#[ignore]`d because they need that binary built, which
//! `cargo test -p tga` does not do, and the success arm additionally needs a
//! running trusty-search — see [`reachable_trusty_search`] for why that one
//! cannot be stubbed:
//!
//! ```text
//! cargo build -p trusty-analyze
//! trusty-search start
//! cargo test -p tga -- --include-ignored
//! ```
//!
//! Nothing here touches the operator's analyze daemon or data: a wrapper script
//! points the child's facts store and data directory at a temp directory, and
//! the success arm kills the daemon it started. trusty-search is read-only to
//! these tests — one `GET /health`, plus whatever the daemon asks it during
//! boot.
//! Test: this module.
//!
//! # Spec References
//! - [`SPEC-TGAUDIT-06~draft`](../../../../docs/specs/DOC-67-tga-audit-mode.md#SPEC-TGAUDIT-06~draft)

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::audit::AnalyzeGuard;

/// The built `trusty-analyze` binary, or a panic naming the command that builds
/// it.
///
/// `TRUSTY_ANALYZE_BIN` wins when set — the same override `trusty-audit` exports
/// onto its `tga audit` children. Otherwise the sibling binary is resolved
/// beside this test binary, which is where the workspace target directory puts
/// it for whichever profile the run is using.
fn real_analyze_binary() -> PathBuf {
    if let Some(pinned) = std::env::var_os("TRUSTY_ANALYZE_BIN") {
        let pinned = PathBuf::from(pinned);
        assert!(
            pinned.exists(),
            "TRUSTY_ANALYZE_BIN={} does not exist",
            pinned.display()
        );
        return pinned;
    }
    // target/<profile>/deps/<test binary> → target/<profile>/trusty-analyze
    let exe = std::env::current_exe().expect("resolve the test executable");
    let candidate = exe
        .parent()
        .and_then(Path::parent)
        .map(|profile| profile.join("trusty-analyze"))
        .expect("a target/<profile>/deps layout");
    assert!(
        candidate.exists(),
        "{} is not built — run `cargo build -p trusty-analyze` before \
         `cargo test -p tga -- --include-ignored`, or point TRUSTY_ANALYZE_BIN at a copy",
        candidate.display()
    );
    candidate
}

/// A reachable trusty-search address, or a panic naming what to start.
///
/// This one dependency cannot be stubbed. `TrustySearchClient` builds its
/// reqwest client with `http2_prior_knowledge()`
/// (`crates/trusty-analyze/src/core/client.rs`), so it opens h2c and never
/// negotiates down — a one-line HTTP/1.1 listener, which is what every other
/// test here uses for a daemon, is refused as unreachable and the binary exits
/// before it binds anything. Standing up an h2c server by hand to avoid the
/// prerequisite would be testing the stub.
///
/// So the success arm runs against the real trusty-search, which is also the
/// more honest end of the chain: real search → real analyze → the guard.
async fn reachable_trusty_search() -> String {
    let url =
        std::env::var("TRUSTY_SEARCH_URL").unwrap_or_else(|_| "http://127.0.0.1:7878".to_string());
    let reachable =
        trusty_common::daemon_guard::probe_once(&format!("{}/health", url.trim_end_matches('/')))
            .await;
    assert!(
        reachable,
        "trusty-search is not answering at {url} — start it (`trusty-search start`) before \
         `cargo test -p tga -- --include-ignored`, or point TRUSTY_SEARCH_URL at a running one. \
         It cannot be stubbed: trusty-analyze speaks h2c to it with prior knowledge."
    );
    url
}

/// A wrapper that gives the real binary a private environment, then `exec`s it.
///
/// Why a wrapper rather than setting the variables here: `std::env::set_var` is
/// `unsafe` in edition 2024 and unsound under parallel tests, and the guard
/// spawns with the parent's environment inherited. It is also what production
/// does — `trusty-audit` hands its children a pinned environment
/// (`crates/trusty-audit/src/run.rs`).
///
/// `exec` keeps the PID, so the `$$` recorded before it is the daemon's own PID
/// and the success arm can kill exactly what it started.
fn wrapper_for(dir: &Path, binary: &Path, search_url: &str, pid_file: &Path) -> String {
    use std::os::unix::fs::PermissionsExt as _;

    let script = format!(
        "#!/bin/sh\n\
         echo $$ > {pid_file}\n\
         export TRUSTY_SEARCH_URL={search_url}\n\
         export TRUSTY_ANALYZER_FACTS={dir}/facts.redb\n\
         export TRUSTY_DATA_DIR_OVERRIDE={dir}\n\
         exec {binary} \"$@\" > {dir}/analyze.log 2>&1\n",
        pid_file = pid_file.display(),
        dir = dir.display(),
        binary = binary.display(),
    );
    let path = dir.join("analyze-wrapper");
    std::fs::write(&path, script).expect("write the wrapper");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make the wrapper executable");
    path.to_str().expect("a UTF-8 temp path").to_string()
}

/// Kills the daemon named by the pid file on drop, so a panicking assertion
/// cannot leave a real `trusty-analyze` running on the machine.
struct KillOnDrop(PathBuf);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let Ok(raw) = std::fs::read_to_string(&self.0) else {
            return;
        };
        let Ok(pid) = raw.trim().parse::<u32>() else {
            return;
        };
        // SIGTERM, not SIGKILL: the daemon's own shutdown path, so it clears its
        // discovery file the way an operator's `stop` would.
        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status();
    }
}

/// The refusal arm, end to end: the real binary cannot come up without
/// trusty-search, and the guard turns that into a refusal.
///
/// This is the claim the error message makes to the operator — "`trusty-analyze
/// serve` exits immediately when trusty-search is unreachable, so trusty-search
/// goes first". Nothing inside this crate can check that claim. Point the real
/// binary at a dead trusty-search and the guard must spend its readiness budget
/// and then refuse.
#[ignore = "needs `cargo build -p trusty-analyze`; run with --include-ignored"]
#[tokio::test]
async fn the_real_analyze_binary_refuses_the_audit_when_trusty_search_is_down() {
    let binary = real_analyze_binary();
    let dir = tempfile::tempdir().expect("create a temp dir");
    let pid_file = dir.path().join("analyze.pid");
    let _reaper = KillOnDrop(pid_file.clone());
    // A trusty-search port with nothing behind it: the dependency is genuinely
    // absent rather than merely slow.
    let dead_search = format!("http://127.0.0.1:{}", super::tests::free_port());
    let wrapper = wrapper_for(dir.path(), &binary, &dead_search, &pid_file);

    let guard = AnalyzeGuard {
        url: format!("http://127.0.0.1:{}", super::tests::free_port()),
        binary: wrapper,
        startup_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(200),
    };
    let err = crate::audit::ensure_analyze_daemon_with(&guard)
        .await
        .expect_err("the real binary cannot serve without trusty-search");

    assert!(
        err.cause.contains("did not become ready"),
        "the spawn succeeded and the readiness poll is what refused; got: {}",
        err.cause
    );
    assert!(
        err.to_string().contains("trusty-search start"),
        "the refusal must name the fix: {err}"
    );
}

/// The success arm, end to end: with its dependency satisfied, the real binary
/// answers on the port the guard chose and the preflight passes.
///
/// This is the half no stub can stand in for. It checks that `serve --port
/// <port>` is still the argument vector the binary accepts, that it binds THAT
/// port rather than its own default, and that its `/health` answers 2xx — the
/// three facts `serve_args`, `port_of` and `probe_once` each assume separately.
///
/// Needs a running trusty-search as well as the built binary; see
/// [`reachable_trusty_search`] for why that one cannot be stubbed away.
#[ignore = "needs `cargo build -p trusty-analyze` and a running trusty-search; run with --include-ignored"]
#[tokio::test]
async fn the_real_analyze_binary_satisfies_the_preflight_end_to_end() {
    let binary = real_analyze_binary();
    let search = reachable_trusty_search().await;
    let dir = tempfile::tempdir().expect("create a temp dir");
    let pid_file = dir.path().join("analyze.pid");
    let _reaper = KillOnDrop(pid_file.clone());
    let wrapper = wrapper_for(dir.path(), &binary, &search, &pid_file);

    let port = super::tests::free_port();
    let guard = AnalyzeGuard {
        url: format!("http://127.0.0.1:{port}"),
        binary: wrapper,
        // Generous: a debug-profile daemon opening two redb stores on a cold
        // filesystem is slower than anything the stub tests model.
        startup_timeout: Duration::from_secs(60),
        poll_interval: Duration::from_millis(250),
    };
    if let Err(e) = crate::audit::ensure_analyze_daemon_with(&guard).await {
        // The daemon's own stdio is null-ed by `spawn_detached`, so without this
        // the failure says only "did not become ready" and the reason is lost.
        let log = std::fs::read_to_string(dir.path().join("analyze.log")).unwrap_or_default();
        panic!("the real binary must satisfy the preflight: {e}\n--- daemon log ---\n{log}");
    }

    // The daemon is now up, so a second call must take the fast path — the path
    // that keeps `tga audit` from starting a second daemon beside an operator's
    // own. An unspawnable binary is what proves it: any spawn attempt fails.
    let already_up = AnalyzeGuard {
        binary: "/nonexistent/trusty-analyze".to_string(),
        ..guard
    };
    crate::audit::ensure_analyze_daemon_with(&already_up)
        .await
        .expect("a daemon this guard just started must satisfy the next run without a spawn");
}

// ─── The trusty-search CLI facts the indexing pass rests on (#5670) ───────────

/// The built `trusty-search` binary, or a panic naming the command that builds
/// it. `TRUSTY_SEARCH_BIN` wins when set, mirroring [`real_analyze_binary`].
fn real_search_binary() -> PathBuf {
    if let Some(pinned) = std::env::var_os(crate::audit::ENV_SEARCH_BIN) {
        let pinned = PathBuf::from(pinned);
        assert!(
            pinned.exists(),
            "TRUSTY_SEARCH_BIN={} does not exist",
            pinned.display()
        );
        return pinned;
    }
    let exe = std::env::current_exe().expect("resolve the test executable");
    let candidate = exe
        .parent()
        .and_then(Path::parent)
        .map(|profile| profile.join("trusty-search"))
        .expect("a target/<profile>/deps layout");
    assert!(
        candidate.exists(),
        "{} is not built — run `cargo build -p trusty-search` before \
         `cargo test -p tga -- --include-ignored`, or point TRUSTY_SEARCH_BIN at a copy",
        candidate.display()
    );
    candidate
}

/// `index <path> --name <id>` is still the CLI this module builds.
///
/// Why this cannot be checked in-crate: `super::repo_index::index_args` is a
/// `Vec<OsString>` a stub happily accepts whatever it contains, so every stub
/// test would still pass if trusty-search renamed the flag or dropped the
/// positional path. Asking the real binary's own parser is the only check that
/// fails when the contract moves.
///
/// `--help` mutates nothing and needs no daemon, so this arm costs one process
/// spawn.
#[ignore = "needs `cargo build -p trusty-search`; run with --include-ignored"]
#[test]
fn the_real_search_binary_still_takes_index_path_and_name() {
    let binary = real_search_binary();
    let output = std::process::Command::new(&binary)
        .args(["index", "--help"])
        .output()
        .expect("run `trusty-search index --help`");
    assert!(output.status.success(), "`index --help` exited non-zero");

    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("--name"),
        "`--name` is what binds the index to the id trusty-review looks up:\n{help}"
    );
    assert!(
        help.contains("[PATH]") || help.contains("<PATH>"),
        "the positional checkout path is still the first argument:\n{help}"
    );
}

/// An unknown index id exits non-zero — the membership signal the cheap path
/// reads.
///
/// Why this cannot be checked in-crate: the stub answers by pattern, so it
/// proves only that this module reads the exit status, never that `index-status`
/// still SETS it. The real command 404s at the daemon and turns that into a
/// non-zero exit; if it ever started exiting 0 with a "not found" message, every
/// repository would be treated as already served and the audit would render
/// exactly the hollow report #5670 is about.
///
/// Read-only against the operator's daemon: one `GET
/// /indexes/<random>/status` for an id that cannot exist.
#[ignore = "needs `cargo build -p trusty-search` and a running trusty-search; run with --include-ignored"]
#[tokio::test]
async fn the_real_search_binary_exits_non_zero_for_an_unknown_index() {
    let binary = real_search_binary();
    let _ = reachable_trusty_search().await;

    let unknown = format!("tga-audit-no-such-index-{}", std::process::id());
    let args = super::repo_index::probe_args(&unknown);
    let output = std::process::Command::new(&binary)
        .args(&args)
        .output()
        .expect("run `trusty-search index-status <unknown>`");

    assert!(
        !output.status.success(),
        "an unknown index must not report success — stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
