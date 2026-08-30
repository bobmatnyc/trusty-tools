//! Deterministic single-test process isolation for env-dependent tests (#4213).
//!
//! Why: a test that needs a specific value of a process-global environment
//! variable cannot get one with `#[serial]`. `serial_test` only excludes other
//! tests carrying the same attribute — every NON-serial test in the binary still
//! runs concurrently and can read (or, via its own `remove_var`, clobber) the
//! variable inside the window. That is how the #2178 root-hijack guard came to
//! pass by luck rather than by correctness, and it is the same defect the #3979
//! resume kill-switch test inherited (#4721). A test that is green by luck is
//! indistinguishable from a test that is green by correctness.
//!
//! What: [`run_isolated`] re-executes the current test binary with the target
//! test's exact name and the required variables supplied through
//! [`std::process::Command::env`] — a per-process value, so there is no
//! in-process `set_var`, no `unsafe`, and nothing for a sibling test to race.
//! The child runs that one test alone, so its environment is inviolate for the
//! whole run.
//!
//! Test: used by `root_hijack_tests`, `checkpoint_tests`, `resume_tests`, and
//! (#6369) the registry-census tests in `rpc::admin_tests`;
//! the guard's own vacuum-failure mode is covered by the "exactly one test"
//! assertion below.

use std::ffi::OsStr;

/// Marker env var identifying the isolated child process. Set by
/// [`run_isolated`] on the spawned command, never in-process.
const CHILD_MARKER: &str = "TRUSTY_SEARCH_ISOLATED_CHILD";

/// `true` when this process IS the isolated child.
///
/// Why: lets a caller skip building spawn-only fixtures (a throwaway data dir)
/// when there is nothing left to spawn.
/// Test: `root_hijack_tests::isolate_in_child_process`.
pub(crate) fn is_isolated_child() -> bool {
    std::env::var_os(CHILD_MARKER).is_some()
}

/// Run `test_name` alone in a child process with a private `TRUSTY_DATA_DIR`,
/// and return `true` only inside that child.
///
/// Why (#6369): the common case of [`run_isolated`] — a test whose correctness
/// depends on process-global state no in-process lock can fence. Two shapes hit
/// it: a registry census, which ~70 non-`#[serial]` tests can write a row into,
/// and a reading of a process-global gauge such as
/// `reindex::deferred_embed_queue`'s `QUEUE_DEPTH`, which a detached task from
/// an already-finished test can still move. A child process starts with the
/// gauge at zero and an empty data dir, so both become exact.
/// What: `false` in the parent (the child has already run the body), `true` in
/// the child. `test_name` is the test's full `module::path::name`; a stale one
/// fails [`run_isolated`]'s "exactly one test" assertion rather than silently
/// skipping the body.
/// Test: `rpc::admin_tests`'s two registry-census tests and
/// `server::tests_stall::health_reports_the_deferred_embed_queue_depth_end_to_end`.
pub(crate) fn isolate_in_child(test_name: &str) -> bool {
    if is_isolated_child() {
        return true;
    }
    let data_dir = tempfile::tempdir().expect("child data dir");
    run_isolated(test_name, &[("TRUSTY_DATA_DIR", data_dir.path())])
}

/// Run the calling test alone in a child process with `envs` applied at spawn.
///
/// Why: see the module docs — this is the replacement for `#[serial]` + an
/// in-process `set_var`, which does not actually exclude the concurrent tests
/// that matter.
/// What: in the child (identified by [`CHILD_MARKER`]) returns `true` so the
/// caller runs its body. In the parent, re-executes the test binary with
/// `test_name --exact --nocapture --test-threads=1`, replays the child's output
/// on the parent's streams (so a failing assertion prints exactly as it would
/// in-process), asserts the child succeeded, and returns `false` so the parent
/// skips the body.
///
/// The parent additionally asserts the child ran EXACTLY one test. `--exact`
/// with a stale name (a renamed test) would otherwise match nothing, exit 0, and
/// turn this guard into a silent no-op — a vacuum failure worse than the race it
/// replaces.
/// Test: every caller routes through it; a stale name fails the "exactly one
/// test" assertion.
pub(crate) fn run_isolated<K, V>(test_name: &str, envs: &[(K, V)]) -> bool
where
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    if std::env::var_os(CHILD_MARKER).is_some() {
        return true;
    }
    let exe = std::env::current_exe().expect("current test binary path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.args([test_name, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_MARKER, "1");
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("spawn isolated child {}: {e}", exe.display()));
    let stdout = String::from_utf8_lossy(&out.stdout);
    eprint!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        out.status.success(),
        "isolated child run of `{test_name}` failed ({}) — see its output \
         above for the actual assertion failure",
        out.status
    );
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "isolated child for `{test_name}` did not run exactly one test — the \
         name passed to run_isolated is stale (test renamed?), so the body never \
         executed and this guard proved nothing"
    );
    false
}
