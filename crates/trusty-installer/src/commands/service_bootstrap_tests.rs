//! Tests for `service_bootstrap.rs`.
//!
//! Why a sibling file: `service_bootstrap.rs` is production source under the
//! 500-SLOC cap and these test bodies alone are most of its budget. Moving them
//! to a `_tests.rs` sibling (3000-SLOC test cap) is the pattern
//! `plist_bootstrap.rs` / `plist_bootstrap_tests.rs` already uses here, and it
//! is what left room for the #4470 port guard.
//!
//! What: the member predicate, the opt-out truth table, every `bootstrap_one`
//! branch (including the #3836/#3841 defensive-fallback branches and the #4470
//! port-guard refusals), and the `run_captured` stdio-capture proofs.

use super::*;
use std::cell::RefCell;

/// In-memory [`ServiceEnv`] fake — records `service install` /
/// `bootstrap_fallback` calls and simulates plist presence + launchd load
/// state, so tests never touch launchd or the real
/// `~/Library/LaunchAgents`.
///
/// Why the `loaded: true` default (#3836): the pre-existing
/// `bootstrap_one_*` tests below construct this via `new(present, fail)`
/// and assert the OLD `Installed` outcome — defaulting `loaded` to `true`
/// (the honest, common case: `service install` DID load the agent) keeps
/// those tests passing unchanged and expresses the #3832 failure mode
/// (loaded: false) as an opt-in builder instead.
struct FakeServiceEnv {
    present: bool,
    fail: bool,
    loaded: bool,
    fail_fallback: bool,
    /// #4470: when `Some`, the port guard refuses with this reason.
    port_guard_refusal: Option<String>,
    installed: RefCell<Vec<String>>,
    /// #4964: the `exe_path` each `run_service_install` call received, so a
    /// test can assert the CONCRETE just-installed path reaches the spawn
    /// rather than being dropped on the way.
    installed_exe_paths: RefCell<Vec<Option<std::path::PathBuf>>>,
    fallback_calls: RefCell<Vec<String>>,
    /// #6290: the members `evict_retired` was called for.
    evicted: RefCell<Vec<String>>,
    /// #6290: the labels the fake pretends were loaded and got booted out.
    retired_loaded: Vec<String>,
}

impl FakeServiceEnv {
    fn new(present: bool, fail: bool) -> Self {
        Self {
            present,
            fail,
            loaded: true,
            fail_fallback: false,
            // #4470: default to a CLEAR port, so every pre-existing test still
            // exercises the branch it was written for rather than silently
            // becoming a port-guard test.
            port_guard_refusal: None,
            installed: RefCell::new(Vec::new()),
            installed_exe_paths: RefCell::new(Vec::new()),
            fallback_calls: RefCell::new(Vec::new()),
            evicted: RefCell::new(Vec::new()),
            retired_loaded: Vec::new(),
        }
    }

    /// Builder (#6290): simulate a host that still has the retired unit loaded.
    fn with_retired_unit_loaded(mut self, labels: &[&str]) -> Self {
        self.retired_loaded = labels.iter().map(|s| (*s).to_owned()).collect();
        self
    }

    /// Builder (#4470): simulate a foreign, unsupervised process already
    /// holding this member's port — the #4230 orphan state.
    fn foreign_port_holder(mut self) -> Self {
        self.port_guard_refusal = Some(
            "refusing to bootstrap trusty-search: port 7878 is held by pid 9931, which \
             launchd does not supervise"
                .to_owned(),
        );
        self
    }

    /// Builder (#3836): simulate `service install` exiting 0 WITHOUT
    /// launchd ever loading the label — #3832's exact failure mode.
    fn not_loaded(mut self) -> Self {
        self.loaded = false;
        self
    }

    /// Builder (#3836): simulate the installer's own fallback
    /// `launchctl bootstrap` also failing.
    fn failing_fallback(mut self) -> Self {
        self.fail_fallback = true;
        self
    }
}

impl ServiceEnv for FakeServiceEnv {
    fn plist_present(&self, _binary: &str) -> bool {
        self.present
    }
    fn run_service_install(
        &self,
        binary: &str,
        exe_path: Option<&std::path::Path>,
    ) -> anyhow::Result<()> {
        self.installed.borrow_mut().push(binary.to_string());
        self.installed_exe_paths
            .borrow_mut()
            .push(exe_path.map(std::path::Path::to_path_buf));
        if self.fail {
            anyhow::bail!("simulated failure");
        }
        Ok(())
    }
    fn is_loaded(&self, _binary: &str) -> bool {
        self.loaded
    }
    fn port_guard(&self, _binary: &str) -> Result<(), String> {
        match &self.port_guard_refusal {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        }
    }
    fn bootstrap_fallback(&self, binary: &str) -> anyhow::Result<()> {
        self.fallback_calls.borrow_mut().push(binary.to_string());
        if self.fail_fallback {
            anyhow::bail!("simulated fallback failure");
        }
        Ok(())
    }
    // #6290: stands in for `launchctl bootout` + plist deletion. Records the
    // member so a test can prove the eviction was ATTEMPTED, and returns the
    // labels the fake was told are loaded.
    fn evict_retired(&self, binary: &str) -> Vec<String> {
        self.evicted.borrow_mut().push(binary.to_string());
        self.retired_loaded.clone()
    }
}

/// REGRESSION (#6290): `tctl install` / `tctl up` must never shell out to
/// `trusty-review service install`, and must boot out the unit an earlier
/// install left loaded.
///
/// Why: trusty-review has no `service` subcommand any more, so the shell-out
/// would exit 2 and be reported as a bootstrap failure on every install. The
/// second half matters more: `com.trusty.review` is loaded with
/// `KeepAlive::Always` on every host that ran the old binary, pointed at a
/// `serve` subcommand that no longer exists, and this pass is the only one that
/// visits the member. A machine that ends this pass with the unit still loaded
/// is the failure this test exists to catch.
/// What: asserts the predicates, then drives `bootstrap_one` over a fake host
/// carrying both labels and asserts the eviction ran, nothing was installed,
/// and the evicted labels are named in the operator-facing note.
/// Test: this is the test.
#[test]
fn retired_review_has_no_service_install() {
    assert!(
        !member_has_service_install("trusty-review"),
        "shelling out to `trusty-review service install` would exit 2 — the \
         subcommand is gone"
    );
    assert!(
        super::member_has_retired_service("trusty-review"),
        "trusty-review must still be visited, to clear its unit"
    );

    let env = FakeServiceEnv::new(true, false)
        .with_retired_unit_loaded(&["com.trusty.review", "com.trusty.trusty-review"]);
    let action = bootstrap_one(&env, "trusty-review", None);

    assert_eq!(
        *env.evicted.borrow(),
        vec!["trusty-review".to_owned()],
        "the retired unit must be booted out, not merely skipped"
    );
    assert!(
        env.installed.borrow().is_empty(),
        "nothing may be installed for a retired member: {:?}",
        env.installed.borrow()
    );
    assert!(
        env.fallback_calls.borrow().is_empty(),
        "the bootstrap fallback must never fire for a retired member"
    );
    assert!(
        !action.is_failure(),
        "clearing a retired unit is routine, never an install failure"
    );
    match &action {
        BootstrapAction::Skipped(reason) => {
            assert!(reason.contains("com.trusty.review"), "reason: {reason}");
            assert!(
                reason.contains("com.trusty.trusty-review"),
                "both labels exist on real hosts and both must be named: {reason}"
            );
        }
        other => panic!("a retired member must be Skipped, got {other:?}"),
    }
}

/// Why (#6290): the eviction is idempotent, and the second run is the common
/// case — every install after the first finds nothing loaded. Reporting that as
/// anything but a quiet skip would put a permanent line in the install output
/// about a unit that has been gone for months.
/// What: a host with no retired unit loaded gets a Skipped with no label list.
/// Test: this is the test.
#[test]
fn evicting_a_retired_member_twice_is_quiet() {
    let env = FakeServiceEnv::new(false, false);
    match bootstrap_one(&env, "trusty-review", None) {
        BootstrapAction::Skipped(reason) => {
            assert!(reason.contains("retired"), "reason: {reason}");
            assert!(
                !reason.contains("evicted"),
                "nothing was there to evict: {reason}"
            );
        }
        other => panic!("expected a quiet skip, got {other:?}"),
    }
}

/// Why: the member predicate gates which daemons get a service bootstrap;
/// every launchd-managed shared daemon must be recognised.
/// What: asserts the five service members return `true`.
/// Test: this is the test.
#[test]
fn service_members_recognised() {
    for b in [
        "trusty-search",
        "trusty-memory",
        "trusty-analyze",
        "trusty-console",
    ] {
        assert!(
            member_has_service_install(b),
            "{b} should be a service member"
        );
    }
}

/// Why: process-managed / non-daemon members must NOT be shelled out to a
/// non-existent `service install` subcommand.
/// What: asserts trusty-mpm and tga are excluded.
/// Test: this is the test.
#[test]
fn non_service_members_excluded() {
    assert!(!member_has_service_install("trusty-mpm"));
    assert!(!member_has_service_install("tga"));
    assert!(!member_has_service_install("nope"));
}

/// Why: both opt-out paths must disable the step; neither set must enable it.
/// What: exercises the four-way truth table.
/// Test: this is the test.
#[test]
fn bootstrap_enabled_truth_table() {
    assert!(bootstrap_enabled(false, false));
    assert!(!bootstrap_enabled(true, false));
    assert!(!bootstrap_enabled(false, true));
    assert!(!bootstrap_enabled(true, true));
}

/// Why: the env opt-out is the automation escape hatch; pin that a present
/// env value disables the step (flag unset).
/// What: `bootstrap_enabled(false, true)` is `false`.
/// Test: this is the test.
#[test]
fn bootstrap_enabled_respects_env() {
    assert!(!bootstrap_enabled(false, true));
    assert!(bootstrap_enabled(false, false));
}

/// Why: a non-service member must be skipped without any install attempt.
/// What: asserts `Skipped` and that no install was recorded.
/// Test: this is the test.
#[test]
fn bootstrap_one_skips_non_service_member() {
    let env = FakeServiceEnv::new(false, false);
    let action = bootstrap_one(&env, "trusty-mpm", None);
    assert!(matches!(action, BootstrapAction::Skipped(_)));
    assert!(env.installed.borrow().is_empty());
}

/// Why: idempotency + non-clobber — an existing, ALREADY LOADED plist
/// must be left untouched with NO `service install` call and NO fallback
/// bootstrap (no needless restart, no overwrite).
/// What: with `present = true` and `loaded` at its default (`true`),
/// asserts `Skipped`, no recorded install, and no recorded fallback call.
/// Test: this is the test.
#[test]
fn bootstrap_one_skips_when_plist_present_and_loaded() {
    let env = FakeServiceEnv::new(true, false);
    let action = bootstrap_one(&env, "trusty-search", None);
    assert!(matches!(action, BootstrapAction::Skipped(_)));
    assert!(
        env.installed.borrow().is_empty(),
        "must not re-install over an existing plist"
    );
    assert!(
        env.fallback_calls.borrow().is_empty(),
        "must not force-bootstrap an already-loaded label"
    );
}

/// Why: THE #3841 root-cause fix — a plist already present on disk is NOT
/// proof launchd has it loaded (a prior run can leave exactly this state,
/// #3832's failure signature). Before the fix, `bootstrap_one` returned
/// `Skipped` here WITHOUT ever checking `is_loaded`, so the #3836
/// defensive postcondition never ran for exactly the damaged machines it
/// exists to repair. This test fails against the pre-fix code (it always
/// short-circuited to `Skipped` the instant `plist_present` was `true`).
/// What: with `present = true` AND `loaded = false` (via `.not_loaded()`),
/// asserts `LoadedByFallback`, exactly one recorded `bootstrap_fallback`
/// call, and NO `service install` call (non-clobbering — the plist itself
/// is never rewritten, only loaded).
/// Test: this is the test.
#[test]
fn bootstrap_one_loads_via_fallback_when_plist_present_but_not_loaded() {
    let env = FakeServiceEnv::new(true, false).not_loaded();
    let action = bootstrap_one(&env, "trusty-memory", None);
    assert_eq!(action, BootstrapAction::LoadedByFallback);
    assert_eq!(env.fallback_calls.borrow().as_slice(), ["trusty-memory"]);
    assert!(
        env.installed.borrow().is_empty(),
        "an already-present plist must never be re-installed, only loaded"
    );
}

/// Why: even on the already-installed path, a fallback failure must be
/// surfaced loudly (`Failed`) with a message naming BOTH the plist-not-
/// loaded state and the fallback's own failure reason — mirrors
/// `bootstrap_one_reports_failure_when_fallback_also_fails` for the
/// fresh-install branch.
/// What: with `present = true`, `loaded = false`, and the fallback set to
/// fail, asserts `Failed` whose message names both facts.
/// Test: this is the test.
#[test]
fn bootstrap_one_reports_failure_when_present_but_fallback_fails() {
    let env = FakeServiceEnv::new(true, false)
        .not_loaded()
        .failing_fallback();
    let action = bootstrap_one(&env, "trusty-console", None);
    match action {
        BootstrapAction::Failed(e) => {
            assert!(e.contains("not loaded"), "message: {e}");
            assert!(e.contains("simulated fallback failure"), "message: {e}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// Why: the core happy path — a service member with no plist yet gets
/// `service install` run exactly once.
/// What: with `present = false`, asserts `Installed` and one recorded call.
/// Test: this is the test.
#[test]
fn bootstrap_one_installs_when_absent() {
    let env = FakeServiceEnv::new(false, false);
    let action = bootstrap_one(&env, "trusty-memory", None);
    assert_eq!(action, BootstrapAction::Installed);
    assert_eq!(env.installed.borrow().as_slice(), ["trusty-memory"]);
}

/// Why: a `service install` failure must be fail-soft — surfaced as
/// `Failed`, not a panic or an aborted install.
/// What: with the fake set to fail, asserts a `Failed` carrying the error.
/// Test: this is the test.
#[test]
fn bootstrap_one_reports_failure() {
    let env = FakeServiceEnv::new(false, true);
    let action = bootstrap_one(&env, "trusty-analyze", None);
    match action {
        BootstrapAction::Failed(e) => assert!(e.contains("simulated failure")),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(
        env.fallback_calls.borrow().is_empty(),
        "a genuine service-install failure must never trigger the fallback \
         bootstrap — that's only for a misleadingly-successful exit code"
    );
}

/// Why: #3836 HIGH fix — the common, honest case: `service install`
/// exited 0 AND launchd actually loaded the label. Must NOT invoke the
/// fallback bootstrap (it would be a needless extra `launchctl` call /
/// redundant restart).
/// What: with `loaded: true` (the default), asserts plain `Installed` and
/// zero fallback calls.
/// Test: this is the test.
#[test]
fn bootstrap_one_installed_directly_when_loaded() {
    let env = FakeServiceEnv::new(false, false);
    let action = bootstrap_one(&env, "trusty-search", None);
    assert_eq!(action, BootstrapAction::Installed);
    assert!(
        env.fallback_calls.borrow().is_empty(),
        "must not force-bootstrap when launchd already loaded the label"
    );
}

/// Why: THE #3836 HIGH fix's core safety property — #3832's exact
/// failure signature (`service install` exits 0 but launchd never loads
/// the label) must be caught and repaired, not silently reported as a
/// clean `Installed`.
/// What: with `loaded: false`, asserts `InstalledByFallback` and exactly
/// one recorded `bootstrap_fallback` call for the right binary.
/// Test: this is the test.
#[test]
fn bootstrap_one_falls_back_when_not_loaded() {
    let env = FakeServiceEnv::new(false, false).not_loaded();
    let action = bootstrap_one(&env, "trusty-memory", None);
    assert_eq!(action, BootstrapAction::InstalledByFallback);
    assert_eq!(env.fallback_calls.borrow().as_slice(), ["trusty-memory"]);
}

/// Why: if EVEN the installer's own direct `launchctl bootstrap` fallback
/// fails, that must be surfaced loudly (`Failed`) — never silently
/// swallowed — and the error text must explain BOTH what happened (a
/// misleading exit code) and why the recovery attempt itself failed, so
/// an operator isn't left staring at a bare "Failed" for a
/// non-obvious reason.
/// What: with `loaded: false` AND the fallback set to fail, asserts
/// `Failed` whose message names the fallback failure.
/// Test: this is the test.
#[test]
fn bootstrap_one_reports_failure_when_fallback_also_fails() {
    let env = FakeServiceEnv::new(false, false)
        .not_loaded()
        .failing_fallback();
    let action = bootstrap_one(&env, "trusty-console", None);
    match action {
        BootstrapAction::Failed(e) => {
            assert!(e.contains("never loaded"), "message: {e}");
            assert!(e.contains("simulated fallback failure"), "message: {e}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// Why: THE #4470 fix on the fresh-install branch. A foreign, unsupervised
/// process holding the member's port makes `<binary> service install` (which
/// bootstraps) exit 0 while that process keeps serving the port — a signed
/// install can then verify green against the WRONG binary (#4230). The guard
/// must refuse BEFORE the install runs.
///
/// This test fails if the guard is removed: without it `bootstrap_one` takes
/// the ordinary path and returns `Installed` with `trusty-search` recorded in
/// `installed`, so BOTH assertions below flip.
///
/// What: with no plist present and the port held by a foreign process, asserts
/// `RefusedForeignPort` carrying the reason, and that NOTHING was installed or
/// bootstrapped.
/// Test: this is the test.
#[test]
fn bootstrap_one_refuses_when_foreign_process_holds_port() {
    let env = FakeServiceEnv::new(false, false).foreign_port_holder();
    let action = bootstrap_one(&env, "trusty-search", None);
    match &action {
        BootstrapAction::RefusedForeignPort(reason) => {
            assert!(reason.contains("9931"), "reason: {reason}");
            assert!(reason.contains("does not supervise"), "reason: {reason}");
        }
        other => panic!("expected RefusedForeignPort, got {other:?}"),
    }
    assert!(
        env.installed.borrow().is_empty(),
        "a refusal must not run `service install` — it must change nothing"
    );
    assert!(
        env.fallback_calls.borrow().is_empty(),
        "a refusal must not issue a `launchctl bootstrap`"
    );
}

/// Why: the #3841 repair path (plist on disk, launchd has not loaded it) ends
/// in a direct `launchctl bootstrap`, which is exactly the call #4470 says
/// lies when a foreign process owns the port. The guard must cover that branch
/// too, or the fix has a hole precisely on the machines #3841 exists to repair.
///
/// This test fails if the guard is removed from that branch: `bootstrap_one`
/// would return `LoadedByFallback` with a recorded fallback call.
///
/// What: with a plist present, launchd NOT having it loaded, and the port held
/// by a foreign process, asserts `RefusedForeignPort` and zero fallback calls.
/// Test: this is the test.
#[test]
fn bootstrap_one_refuses_existing_plist_when_foreign_process_holds_port() {
    let env = FakeServiceEnv::new(true, false)
        .not_loaded()
        .foreign_port_holder();
    let action = bootstrap_one(&env, "trusty-memory", None);
    assert!(
        matches!(action, BootstrapAction::RefusedForeignPort(_)),
        "expected RefusedForeignPort, got {action:?}"
    );
    assert!(
        env.fallback_calls.borrow().is_empty(),
        "a refusal must not issue a `launchctl bootstrap`"
    );
    assert!(env.installed.borrow().is_empty());
}

/// Why: the guard must not gate a member that is already healthy — launchd has
/// the label loaded, so no bootstrap is issued and there is nothing to protect.
/// Refusing here would break `tctl install` over a live stack whenever the port
/// probe was unavailable.
/// What: with the plist present AND loaded, a refusing port guard still yields
/// the ordinary `Skipped`.
/// Test: this is the test.
#[test]
fn bootstrap_one_does_not_gate_an_already_loaded_member_on_the_port_guard() {
    let env = FakeServiceEnv::new(true, false).foreign_port_holder();
    let action = bootstrap_one(&env, "trusty-search", None);
    assert!(
        matches!(action, BootstrapAction::Skipped(_)),
        "expected Skipped, got {action:?}"
    );
}

/// Why (#4470 HIGH-1): `install_all` folds `is_failure` into `service_ok`,
/// which `InstallReport::build` folds into `all_ok`, which drives the exit code
/// and the `--json` payload. The first round of this PR classified failure with
/// an inline `matches!(action, Failed(_))` at the call site and never added the
/// new variant, so a REFUSAL — the guard working, the daemon consequently not
/// running — reported `service_ok: true`. Classifying every variant here, in
/// one exhaustive match next to the definition, is what stops the next variant
/// repeating it.
/// What: pins the failure verdict for all six variants.
/// Test: this is the test.
#[test]
fn bootstrap_action_failure_classification_is_exhaustive() {
    assert!(BootstrapAction::Failed("boom".into()).is_failure());
    assert!(
        BootstrapAction::RefusedForeignPort("port held".into()).is_failure(),
        "a refusal means the daemon is NOT running — the install must not \
         report success"
    );
    assert!(!BootstrapAction::Installed.is_failure());
    assert!(!BootstrapAction::InstalledByFallback.is_failure());
    assert!(!BootstrapAction::LoadedByFallback.is_failure());
    assert!(!BootstrapAction::Skipped("opted out".into()).is_failure());
}

// The end-to-end report-level proof for #4470 HIGH-1 lives in
// `install_tests::refused_foreign_port_drives_all_ok_false_and_a_nonzero_exit_code`,
// where `install_report`'s private `build` / `exit_code` are in scope.

/// Why: the narration note must name the member for a scannable install log.
/// What: asserts each variant's note contains the binary name.
/// Test: this is the test.
#[test]
fn note_mentions_binary() {
    assert!(BootstrapAction::Installed
        .note("trusty-search")
        .contains("trusty-search"));
    assert!(BootstrapAction::InstalledByFallback
        .note("trusty-memory")
        .contains("trusty-memory"));
    assert!(BootstrapAction::LoadedByFallback
        .note("trusty-memory")
        .contains("trusty-memory"));
    assert!(BootstrapAction::Skipped("x".into())
        .note("trusty-memory")
        .contains("trusty-memory"));
    assert!(BootstrapAction::RefusedForeignPort("port 7878 held".into())
        .note("trusty-search")
        .contains("trusty-search"));
    assert!(BootstrapAction::Failed("boom".into())
        .note("trusty-review")
        .contains("trusty-review"));
}

/// Why: the `tctl start` relaxation is the #2556 lifecycle fix; pin the map.
/// What: present → Bootstrap, absent → ServiceInstall.
/// Test: this is the test.
#[test]
fn start_plan_maps_presence() {
    assert_eq!(start_plan(true), StartPlan::Bootstrap);
    assert_eq!(start_plan(false), StartPlan::ServiceInstall);
}

/// Redirect the REAL (OS-level) stdout file descriptor to `tmp` for the
/// duration of `f`, then restore it — regardless of whether `f` panics.
///
/// Why (#3830 regression proof): `Command::status()` "inheriting" stdio
/// means a child writes to whatever fd 1 currently points to; the only
/// way to OBSERVE that from a test is to control what fd 1 points to
/// before spawning, run the command, then read it back. Asserting only
/// `result.is_ok()` (a prior version of this test) proves nothing about
/// where the child's bytes went — it still passed when code-critic
/// reverted `run_captured` to `.status()` on PR #3834.
///
/// CRITICAL: fd 1 is process-global, so this must never run as an
/// ordinary parallel `#[test]` — the default test harness's OWN per-test
/// "test x ... ok" status line is printed by the harness-controller
/// thread through the REAL stdout (not the per-test captured sink), and
/// can land in `tmp` mid-redirect whenever ANY sibling test finishes on
/// another thread (confirmed empirically; mirrors the identical fix in
/// `trusty-common::update::tests`). The `#[test]` wrapper below never
/// calls this on the main invocation — it re-execs the test binary,
/// selecting ONLY the `_inner` test (`--test-threads=1`), so it is the
/// sole test running in a dedicated process.
///
/// What: `dup`s the real stdout fd aside, `dup2`s `tmp`'s fd onto it,
/// runs `f`, restores the saved fd (via a guard so a panic in `f` still
/// restores it), and returns `f`'s result.
/// Test: used by `run_captured_never_leaks_to_parent_stdio_inner` below.
fn with_real_stdout_redirected_to<T>(tmp: &std::fs::File, f: impl FnOnce() -> T) -> T {
    use std::os::unix::io::AsRawFd;

    /// RAII guard: restores the saved real-stdout fd on drop, so a panic
    /// in `f` can never leave the process's stdout permanently redirected.
    struct RestoreStdout(std::os::raw::c_int);
    impl Drop for RestoreStdout {
        fn drop(&mut self) {
            unsafe {
                libc::dup2(self.0, libc::STDOUT_FILENO);
                libc::close(self.0);
            }
        }
    }

    let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
    assert!(saved >= 0, "dup(STDOUT_FILENO) failed");
    let _restore = RestoreStdout(saved);

    let rc = unsafe { libc::dup2(tmp.as_raw_fd(), libc::STDOUT_FILENO) };
    assert!(rc >= 0, "dup2(tmp, STDOUT_FILENO) failed");

    f()
}

/// Re-exec this test binary, running ONLY `inner_test_name` (an
/// `#[ignore]`d test) alone with `--test-threads=1`, and assert it
/// exits 0.
///
/// Why: see [`with_real_stdout_redirected_to`]'s doc — a test that
/// hijacks the process's real stdout fd must run with zero sibling test
/// threads.
/// What: `Command::new(current_exe())` with
/// `[name, "--exact", "--ignored", "--test-threads=1"]`; panics with the
/// child's captured stdout/stderr on a non-zero exit.
/// Test: exercised by `run_captured_never_leaks_to_parent_stdio` below.
fn run_isolated_inner_test(inner_test_name: &str) {
    let exe = std::env::current_exe().expect("resolve current test binary");
    let output = std::process::Command::new(&exe)
        .args([inner_test_name, "--exact", "--ignored", "--test-threads=1"])
        .output()
        .expect("re-exec test binary for isolated fd test");
    assert!(
        output.status.success(),
        "isolated test `{inner_test_name}` failed ({}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The #3830 regression proof: `run_captured` must NEVER let a child's
/// output reach the parent's real (inherited) stdout, no matter how
/// noisy the child is or whether it succeeds.
///
/// Why: see [`with_real_stdout_redirected_to`]'s doc — this replaces a
/// weaker predecessor that only asserted `is_ok()` (code-critic finding
/// on PR #3834). `#[ignore]`d + only ever invoked, alone, via
/// [`run_isolated_inner_test`] from the `#[test]` wrapper immediately
/// below.
/// What: redirects the real stdout fd to a tempfile, runs a noisy,
/// successful `sh -c` command through `run_captured`, restores stdout,
/// then asserts the tempfile received ZERO bytes.
/// Test: this is the test (invoked via
/// `run_captured_never_leaks_to_parent_stdio`).
#[test]
#[ignore]
fn run_captured_never_leaks_to_parent_stdio_inner() {
    use std::io::Read;

    let tmp = tempfile::NamedTempFile::new().expect("create tempfile");
    let mut cmd = std::process::Command::new("sh");
    cmd.args([
        "-c",
        "for i in $(seq 1 20); do echo \"LEAK_MARKER_3830 stdout $i\"; \
         echo \"LEAK_MARKER_3830 stderr $i\" >&2; done; exit 0",
    ]);

    let result =
        with_real_stdout_redirected_to(tmp.as_file(), || run_captured(cmd, "`noisy-capture`"));
    assert!(result.is_ok(), "expected Ok, got {result:?}");

    let mut contents = String::new();
    tmp.reopen()
        .expect("reopen tempfile")
        .read_to_string(&mut contents)
        .expect("read tempfile");
    assert!(
        contents.is_empty(),
        "run_captured leaked to the parent's real stdout fd: {contents:?}"
    );
}

/// `#[test]` entry point for the isolated inner test above — see
/// [`run_isolated_inner_test`]'s doc for why this indirection exists.
/// This is the test `cargo test` actually runs; it re-execs the binary
/// so the real fd-1 hijack happens with zero sibling test threads.
/// Test: this wraps `run_captured_never_leaks_to_parent_stdio_inner`.
#[test]
fn run_captured_never_leaks_to_parent_stdio() {
    run_isolated_inner_test(
        "commands::service_bootstrap::tests::run_captured_never_leaks_to_parent_stdio_inner",
    );
}

/// Why (#3830 regression — the core proof): `run_captured`'s error path
/// must be built from the child's CAPTURED stderr. This is only
/// reachable at all if the command was run via `.output()`; the pre-fix
/// `.status()` call has no `stderr` field to read, so this exact
/// assertion would not compile against that code — pinning the fix at
/// the type level as well as behaviourally.
/// What: runs a command that writes a distinctive marker to stderr and
/// exits non-zero; asserts the returned error's message contains that
/// exact marker.
/// Test: this is the test.
#[test]
fn run_captured_folds_stderr_into_error() {
    let mut cmd = std::process::Command::new("sh");
    cmd.args([
        "-c",
        "echo 'DISTINCTIVE_MARKER_3830: launchd bootstrap failed' >&2; exit 7",
    ]);
    let err = run_captured(cmd, "`fake service install`").expect_err("expected Err");
    let msg = err.to_string();
    assert!(
        msg.contains("DISTINCTIVE_MARKER_3830: launchd bootstrap failed"),
        "error message did not fold in captured stderr: {msg}"
    );
    assert!(msg.contains("fake service install"));
}

// ── #4964 Phase 0.2: the concrete just-installed path reaches the spawn ──────
//
// The stale-daemon respawn loop starts here. `<binary> service install` bakes
// the SPAWNED process's own `current_exe()` into the plist's
// `ProgramArguments[0]`, and launchd's `KeepAlive` then respawns exactly that
// path at every boot. Resolving a bare name through `$PATH` at this point means
// `tctl install` can place a new binary and persist a DIFFERENT, older one into
// launchd — with nothing that ever rewrites the plist.

/// Why: the load-bearing assertion of Phase 0.2. `"sh"` resolves on `$PATH` to
/// `/bin/sh` on every supported host, so a resolver that ignores the supplied
/// path returns `/bin/sh` here — exactly the shadowing this fix removes. No
/// environment mutation, so it is safe under the parallel harness.
/// What: a concrete path is returned verbatim even when the binary NAME also
/// resolves, to somewhere else, on `$PATH`.
/// Test: this is the test.
#[test]
fn service_install_target_prefers_the_concrete_path_over_path_lookup() {
    let concrete = std::path::PathBuf::from("/nowhere/just-installed/sh");
    let got = service_install_target("sh", Some(&concrete)).expect("concrete path always resolves");
    assert_eq!(
        got, concrete,
        "a caller-supplied concrete path must win over the $PATH lookup"
    );
}

/// Why: `tctl start` installs nothing and has no concrete path to offer; the
/// pre-#4964 `$PATH` lookup must survive for it.
/// What: `None` falls back to `which`, which finds `/bin/sh` for `"sh"`.
/// Test: this is the test.
#[test]
fn service_install_target_falls_back_to_path_lookup() {
    let got = service_install_target("sh", None).expect("sh is on PATH on every supported host");
    assert!(
        got.ends_with("sh"),
        "expected a $PATH resolution of `sh`, got {}",
        got.display()
    );
}

/// Why: an unresolvable name must be a clean error, not a spawn of a bare name
/// that fails later with a less useful message.
/// What: `None` plus a name that is on no `$PATH` errors.
/// Test: this is the test.
#[test]
fn service_install_target_errors_when_nothing_resolves() {
    let err = service_install_target("definitely-not-a-real-binary-4964", None)
        .expect_err("expected Err for an unresolvable name");
    assert!(err.to_string().contains("not on PATH"), "{err}");
}

/// Why: `service_install_target` being correct is useless if `bootstrap_one`
/// drops the path on the way to it. This pins the whole thread from the
/// installer's call site down to the spawn.
/// What: `bootstrap_one` with `Some(path)` forwards exactly that path to
/// `run_service_install`.
/// Test: this is the test.
#[test]
fn bootstrap_one_forwards_the_concrete_exe_path_to_service_install() {
    let env = FakeServiceEnv::new(false, false);
    let concrete = std::path::PathBuf::from("/opt/ch/bin/trusty-memory");
    let action = bootstrap_one(&env, "trusty-memory", Some(&concrete));
    assert_eq!(action, BootstrapAction::Installed);
    assert_eq!(
        env.installed_exe_paths.borrow().as_slice(),
        &[Some(concrete)],
        "the concrete just-installed path must reach `run_service_install`"
    );
}
