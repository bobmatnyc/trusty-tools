//! Unit tests for the bootstrap module — no torch, no venv, no network.

use super::*;
use std::cell::{Cell, RefCell};

/// Marker env var identifying the isolated child process. Set by
/// [`isolate_in_child_process`] on the spawned command, never in-process.
const CHILD_MARKER: &str = "TRUSTY_EMBEDDERD_PY_ISOLATED_CHILD";

/// Run this test's body in a dedicated child process that runs this one test
/// alone, so the process-global env vars the body sets have no concurrent
/// reader.
///
/// Why (#4125 follow-up, review finding 2 on PR #4771): every test below that
/// touches `TRUSTY_DATA_DIR_OVERRIDE` or `TRUSTY_UV_BIN` mutates
/// process-global state with `unsafe { std::env::set_var }`. The previous
/// guard was a crate-local `ENV_LOCK` mutex, which — exactly like `#[serial]`,
/// as #4213 and #3954 established — only excludes the OTHER tests that take
/// the same lock. Every non-participating test in the binary kept running
/// inside the window, and `set_var`/`remove_var` are unsound in the presence
/// of any concurrent reader regardless of which tests those are. A lock that
/// does not cover the readers makes a racy test quieter, not correct.
/// What: in the parent, re-executes the current test binary with this test's
/// exact name (`--exact`, `--test-threads=1`) and [`CHILD_MARKER`] supplied
/// through [`Command::env`] at spawn, then asserts the child exited
/// successfully and returns `false` so the parent skips the body. In the child
/// returns `true`: it is the only test in that process, so nothing else can
/// read or clobber what the body sets. The child's output is replayed on the
/// parent's streams, so a failing assertion prints exactly as it would
/// in-process.
///
/// The parent additionally asserts the child ran EXACTLY one test. `--exact`
/// with a stale name (a renamed test) would otherwise match nothing, exit 0,
/// and turn this guard into a silent no-op — a vacuum failure worse than the
/// race it replaces.
/// Test: every env-touching test in this file routes through it;
/// [`assert_isolated`] makes that mechanical.
fn isolate_in_child_process(test_name: &str) -> bool {
    if std::env::var_os(CHILD_MARKER).is_some() {
        return true;
    }
    let exe = std::env::current_exe().expect("current test binary path");
    let out = std::process::Command::new(&exe)
        .args([test_name, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_MARKER, "1")
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
         name passed to isolate_in_child_process is stale (test renamed?), so \
         the body never executed and this guard proved nothing"
    );
    false
}

/// Refuse to mutate the environment outside an isolated child process.
///
/// Why (#4125 follow-up): the isolation above is only as good as its coverage.
/// A test added later that calls [`with_data_override`] or [`deny_uv`] without
/// the guard would silently reintroduce the race. This turns that mistake into
/// an immediate, self-explaining failure instead.
fn assert_isolated() {
    assert!(
        std::env::var_os(CHILD_MARKER).is_some(),
        "this helper mutates process-global env — the calling test must first \
         return early on `!isolate_in_child_process(\"<full::test::path>\")` (#4125)"
    );
}

fn with_data_override<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    assert_isolated();
    unsafe { std::env::set_var(trusty_common::data_dir::DATA_DIR_OVERRIDE_ENV, dir) };
    let out = f();
    unsafe { std::env::remove_var(trusty_common::data_dir::DATA_DIR_OVERRIDE_ENV) };
    out
}

/// Point `TRUSTY_UV_BIN` at nothing, so any rebuild attempt fails immediately
/// instead of starting a real multi-GB torch download.
fn deny_uv() {
    assert_isolated();
    unsafe { std::env::set_var("TRUSTY_UV_BIN", "/nonexistent/uv/binary") };
}

/// Undo [`deny_uv`].
fn allow_uv() {
    assert_isolated();
    unsafe { std::env::remove_var("TRUSTY_UV_BIN") };
}

/// Write a fake, executable "venv python" shell script at `path` with `body`
/// as its script contents (no shebang needed — the caller invokes it directly
/// and the kernel's `#!` handling takes over when a shebang line is present).
/// Real venv interpreters are always executable; a plain `fs::write` is not
/// (mode 0o644), so tests that spawn the fake stub must set the exec bit
/// explicitly or every spawn attempt fails with "permission denied".
fn write_fake_venv_python(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Create the site-packages marker directory `verify_venv_alive` checks for
/// (`<venv>/lib/pythonX.Y/site-packages/sentence_transformers/`) so
/// lightweight-liveness-check tests can simulate "the package is installed"
/// without a real `uv pip sync`.
fn write_site_packages_marker(layout: &VenvLayout) {
    let marker = super::site_packages_dir(layout).join("sentence_transformers");
    fs::create_dir_all(&marker).unwrap();
}

#[test]
fn lockfile_hash_is_stable_and_hex() {
    let h1 = lockfile_hash();
    let h2 = lockfile_hash();
    assert_eq!(h1, h2, "hash must be deterministic");
    assert_eq!(h1.len(), 16, "16 hex chars (8 bytes)");
    assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
}

/// #4125: the pure derivation — no env, no `unsafe`, no isolation needed.
#[test]
fn resolve_layout_derives_paths_under_the_given_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = super::resolve_layout_in(tmp.path());
    assert!(layout.base.starts_with(tmp.path()), "base under data dir");
    assert!(layout.base.ends_with(lockfile_hash()), "keyed by lock hash");
    assert!(layout.venv_dir.ends_with("venv"));
    assert!(layout.project_dir.ends_with("project"));
    assert_eq!(
        layout.venv_dir.join("bin").join("python"),
        layout.venv_python
    );
}

/// The env-honouring half: `resolve_layout()` must feed the override-derived
/// data dir into [`super::resolve_layout_in`].
#[test]
fn resolve_layout_places_venv_under_data_dir_and_hash() {
    // #4125: mutates TRUSTY_DATA_DIR_OVERRIDE — run alone in a child process.
    if !isolate_in_child_process(
        "bootstrap::tests::resolve_layout_places_venv_under_data_dir_and_hash",
    ) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    with_data_override(tmp.path(), || {
        let layout = resolve_layout().unwrap();
        assert!(layout.base.starts_with(tmp.path()), "base under override");
        // The override selects trusty-search's data dir; everything below it is
        // `resolve_layout_in`'s derivation, pinned by that function's own test.
        assert_eq!(
            layout.base,
            super::resolve_layout_in(&tmp.path().join("trusty-search")).base,
            "the override must select the data dir the layout is derived under"
        );
    });
}

#[test]
fn is_ready_requires_both_sentinel_and_python() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = super::resolve_layout_in(tmp.path());
    assert!(!super::is_ready(&layout), "nothing built yet");

    // Sentinel only, no python → not ready.
    std::fs::create_dir_all(&layout.base).unwrap();
    std::fs::write(layout.base.join(".ready"), lockfile_hash()).unwrap();
    assert!(
        !super::is_ready(&layout),
        "sentinel without python is not ready"
    );

    // Add a fake venv python → ready.
    write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 0\n");
    assert!(super::is_ready(&layout), "sentinel + python is ready");

    // Wrong hash in sentinel → not ready.
    std::fs::write(layout.base.join(".ready"), "deadbeefdeadbeef").unwrap();
    assert!(
        !super::is_ready(&layout),
        "stale hash invalidates readiness"
    );
}

#[test]
fn ensure_venv_fast_path_returns_when_ready_without_building() {
    // #4125: `ensure_venv` resolves its layout from the environment — run
    // alone in a child process.
    if !isolate_in_child_process(
        "bootstrap::tests::ensure_venv_fast_path_returns_when_ready_without_building",
    ) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    with_data_override(tmp.path(), || {
        let layout = resolve_layout().unwrap();
        // An executable stub that exits 0, PLUS the site-packages marker dir,
        // satisfies the lightweight `verify_venv_alive` liveness recheck
        // (per-respawn path), so this genuinely hits the fast path rather than
        // falling through to a real (network-dependent) rebuild.
        write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 0\n");
        write_site_packages_marker(&layout);
        std::fs::write(layout.base.join(".ready"), lockfile_hash()).unwrap();
        // Should hit the fast path (no uv, no build) and return Ok.
        let got = ensure_venv().expect("fast path");
        assert_eq!(got.venv_python, layout.venv_python);
    });
}

// ── verify_venv_alive (cheap, torch-free, per-respawn liveness check) ──────
//
// #4125: these take their layout from `resolve_layout_in` rather than
// `resolve_layout()` + a `TRUSTY_DATA_DIR_OVERRIDE` mutation, so there is no
// process-global state to race over and no isolation needed.

#[test]
fn verify_venv_alive_false_when_venv_python_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = super::resolve_layout_in(tmp.path());
    write_site_packages_marker(&layout);
    // Nothing written at `layout.venv_python` at all.
    assert_eq!(super::verify_venv_alive(&layout), RecheckOutcome::Failed);
}

#[test]
fn verify_venv_alive_false_when_site_packages_marker_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = super::resolve_layout_in(tmp.path());
    // A perfectly runnable interpreter, but the package marker is absent
    // (simulates a half-deleted venv) — must fail WITHOUT ever spawning
    // python, since the marker check runs first.
    write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 0\n");
    assert_eq!(super::verify_venv_alive(&layout), RecheckOutcome::Failed);
}

#[test]
fn verify_venv_alive_false_when_interpreter_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = super::resolve_layout_in(tmp.path());
    write_site_packages_marker(&layout);
    // Simulates a broken interpreter binary / missing shared library.
    write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 1\n");
    assert_eq!(super::verify_venv_alive(&layout), RecheckOutcome::Failed);
}

#[test]
fn verify_venv_alive_true_when_marker_present_and_interpreter_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = super::resolve_layout_in(tmp.path());
    write_site_packages_marker(&layout);
    write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 0\n");
    assert_eq!(super::verify_venv_alive(&layout), RecheckOutcome::Passed);
}

// ── verify_full_import_smoke (torch-importing, eager daemon-start check) ──

#[test]
fn verify_full_import_smoke_false_when_venv_python_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = super::resolve_layout_in(tmp.path());
    // Nothing written at `layout.venv_python` at all.
    assert_eq!(
        super::verify_full_import_smoke(&layout),
        RecheckOutcome::Failed
    );
}

#[test]
fn verify_full_import_smoke_false_when_stub_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = super::resolve_layout_in(tmp.path());
    // Simulates a corrupted venv (e.g. a broken native `.so`): the
    // interpreter itself runs but the import fails.
    write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 1\n");
    assert_eq!(
        super::verify_full_import_smoke(&layout),
        RecheckOutcome::Failed
    );
}

#[test]
fn verify_full_import_smoke_true_when_stub_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = super::resolve_layout_in(tmp.path());
    write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 0\n");
    assert_eq!(
        super::verify_full_import_smoke(&layout),
        RecheckOutcome::Passed
    );
}

// ── ensure_venv (per-respawn) vs ensure_venv_eager (daemon-start) ──────────

#[test]
fn ensure_venv_rebuilds_when_ready_sentinel_is_stale_corruption() {
    // A `.ready` sentinel + venv python that FAILS the lightweight liveness
    // recheck must not be trusted: `ensure_venv` (per-respawn path) must fall
    // through past the fast path (verified here via the `uv`-missing error it
    // surfaces once it tries to rebuild, proving the recheck actually gates
    // reuse rather than silently returning the broken venv).
    //
    // #4125: mutates TRUSTY_DATA_DIR_OVERRIDE + TRUSTY_UV_BIN — run alone in a
    // child process.
    if !isolate_in_child_process(
        "bootstrap::tests::ensure_venv_rebuilds_when_ready_sentinel_is_stale_corruption",
    ) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    with_data_override(tmp.path(), || {
        let layout = resolve_layout().unwrap();
        write_site_packages_marker(&layout);
        write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 1\n");
        std::fs::write(layout.base.join(".ready"), lockfile_hash()).unwrap();

        deny_uv();
        let err = ensure_venv().expect_err("stale/corrupt venv must not be reused");
        allow_uv();
        assert!(
            err.to_string()
                .contains("does not point to an existing file")
                || format!("{err:#}").contains("does not point to an existing file"),
            "expected the rebuild attempt to surface the uv-missing error, got: {err:#}"
        );
    });
}

#[test]
fn ensure_venv_eager_fast_path_uses_full_import_recheck() {
    // `ensure_venv_eager` (daemon-start path) must accept a venv that passes
    // the lightweight liveness bar too, since the fake stub here also
    // satisfies `verify_full_import_smoke` (it ignores the `-c "import ..."`
    // args and just exits 0) — this proves the eager variant's fast path
    // wires through `verify_full_import_smoke`, not the cheap check, without
    // requiring a real torch install to prove the DIFFERENCE (see the
    // `_rebuilds_when_stale_corruption` test above for the cheap-path
    // rebuild-on-failure behaviour, which is symmetric here).
    //
    // #4125: mutates TRUSTY_DATA_DIR_OVERRIDE — run alone in a child process.
    if !isolate_in_child_process(
        "bootstrap::tests::ensure_venv_eager_fast_path_uses_full_import_recheck",
    ) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    with_data_override(tmp.path(), || {
        let layout = resolve_layout().unwrap();
        write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 0\n");
        std::fs::write(layout.base.join(".ready"), lockfile_hash()).unwrap();
        // Deliberately NOT writing the site-packages marker: `ensure_venv_eager`
        // must succeed anyway, proving it does NOT go through the
        // marker-checking `verify_venv_alive` path.
        let got = ensure_venv_eager().expect("eager fast path via full import recheck");
        assert_eq!(got.venv_python, layout.venv_python);
    });
}

#[test]
fn ensure_venv_eager_rebuilds_when_full_recheck_fails() {
    // #4125: mutates TRUSTY_DATA_DIR_OVERRIDE + TRUSTY_UV_BIN — run alone in a
    // child process.
    if !isolate_in_child_process(
        "bootstrap::tests::ensure_venv_eager_rebuilds_when_full_recheck_fails",
    ) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    with_data_override(tmp.path(), || {
        let layout = resolve_layout().unwrap();
        write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 1\n");
        std::fs::write(layout.base.join(".ready"), lockfile_hash()).unwrap();

        deny_uv();
        let err = ensure_venv_eager().expect_err("stale/corrupt venv must not be reused");
        allow_uv();
        assert!(
            err.to_string()
                .contains("does not point to an existing file")
                || format!("{err:#}").contains("does not point to an existing file"),
            "expected the rebuild attempt to surface the uv-missing error, got: {err:#}"
        );
    });
}

#[test]
fn locate_uv_rejects_bad_explicit_override() {
    // #4125: mutates TRUSTY_UV_BIN — run alone in a child process.
    if !isolate_in_child_process("bootstrap::tests::locate_uv_rejects_bad_explicit_override") {
        return;
    }
    deny_uv();
    let err = locate_uv().unwrap_err();
    allow_uv();
    assert!(err
        .to_string()
        .contains("does not point to an existing file"));
}

#[test]
fn materialize_project_writes_package_and_lock_skips_tests() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("project");
    super::materialize_project(&dest).unwrap();
    assert!(dest.join("pyproject.toml").is_file());
    assert!(dest.join("uv.lock").is_file());
    assert!(dest
        .join("trusty_embed_sidecar")
        .join("__main__.py")
        .is_file());
    assert!(dest.join("trusty_embed_sidecar").join("model.py").is_file());
    // tests/ dir is intentionally skipped at runtime materialization.
    assert!(
        !dest.join("tests").exists(),
        "tests fixture dir must be skipped"
    );
}

// ── #4125: a timeout is a distinct outcome, never a verification failure ──

/// Why (#4125): `run_bounded_python_check` returned a bare `bool`, so "the
/// check ran out of time" and "the check ran and the venv failed it" were the
/// SAME value — the conflation that made a slow-but-intact venv look broken.
/// What: pins all three classifications at the one site that produces them —
/// a clean exit is `Passed`, a non-zero exit is `Failed`, and outliving the
/// budget is `Indeterminate`. Against the pre-fix code the last case was
/// indistinguishable from the middle one.
/// Test: this test.
#[test]
fn bounded_python_check_classifies_timeout_apart_from_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let ok = tmp.path().join("ok");
    let bad = tmp.path().join("bad");
    let slow = tmp.path().join("slow");
    write_fake_venv_python(&ok, "#!/bin/sh\nexit 0\n");
    write_fake_venv_python(&bad, "#!/bin/sh\nexit 3\n");
    write_fake_venv_python(&slow, "#!/bin/sh\nsleep 30\n");

    // Asymmetric budgets on purpose: the two decided outcomes get a budget no
    // realistic scheduler delay can eat (a loaded CI box can take a surprising
    // while just to fork /bin/sh, and a flaky `Passed` here would be exactly
    // the "slow read as broken" mistake this test exists to forbid), while the
    // `sleep 30` stub is small enough to keep the test fast.
    let generous = Duration::from_secs(30);
    let tight = Duration::from_millis(300);
    assert_eq!(
        super::run_bounded_python_check(&ok, &["-c", "pass"], generous, "t"),
        RecheckOutcome::Passed
    );
    assert_eq!(
        super::run_bounded_python_check(&bad, &["-c", "pass"], generous, "t"),
        RecheckOutcome::Failed
    );
    assert_eq!(
        super::run_bounded_python_check(&slow, &["-c", "pass"], tight, "t"),
        RecheckOutcome::Indeterminate,
        "running out of time says nothing about the venv and must not be reported \
         as a failed check (#4125)"
    );
}

// #5328: the transient-vs-permanent spawn classification and the
// spawn-retry policy that consumes it moved to their own module
// (`spawn_retry.rs`, tested in `spawn_retry_tests.rs`) so `bootstrap.rs`
// stayed under the 500-SLOC production cap.

/// Why (#4125): a single fixed budget under variable startup load is wrong
/// again the moment the load changes, so an over-budget first attempt must buy
/// a second, larger one rather than a verdict.
///
/// #4125 follow-up (review finding 3 on PR #4771): this used to assert the
/// invocation count against a real sleeping subprocess, which made the test bet
/// that a shell stub could fork and append to a counter file inside a wall-clock
/// budget — a race that had already been widened 200 ms → 1500 ms once. What the
/// test actually asserts is ORDERING and BUDGETS, so it now drives
/// [`super::recheck_with_one_retry`] with the check injected: the count is exact
/// and no clock is involved. It also asserts strictly more than before — that
/// the retry receives the LARGER budget, not merely that a retry happened.
/// What: a check that is `Indeterminate` both times runs exactly twice, with the
/// first and retry budgets in that order, and still reports `Indeterminate`,
/// never `Failed`.
/// Test: this test.
#[test]
fn full_import_recheck_retries_once_before_reporting_indeterminate() {
    let seen: RefCell<Vec<(Duration, String)>> = RefCell::new(Vec::new());
    let first = Duration::from_secs(10);
    let retry = Duration::from_secs(60);

    let outcome = super::recheck_with_one_retry(first, retry, &|budget, label| {
        seen.borrow_mut().push((budget, label.to_string()));
        RecheckOutcome::Indeterminate
    });

    assert_eq!(
        outcome,
        RecheckOutcome::Indeterminate,
        "two exhausted budgets still prove nothing about the venv"
    );
    let seen = seen.into_inner();
    assert_eq!(
        seen.len(),
        2,
        "an over-budget first attempt must be retried exactly once"
    );
    assert_eq!(seen[0].0, first, "the first attempt gets the first budget");
    assert_eq!(
        seen[1].0, retry,
        "the retry must get the LARGER budget — retrying with the same one \
         would just move the identical cliff"
    );
    assert!(
        seen[1].1.contains("retry"),
        "the retry's log label must distinguish it, got {:?}",
        seen[1].1
    );
}

/// Why (#4125 follow-up): a decided outcome is real evidence, so spending a
/// second budget on it would be pure latency — and re-running a check that
/// already `Failed` could turn a rebuild-worthy verdict into a flapping one.
/// What: `Passed` and `Failed` are each returned after exactly one call.
/// Test: this test.
#[test]
fn full_import_recheck_does_not_retry_a_decided_outcome() {
    for decided in [RecheckOutcome::Passed, RecheckOutcome::Failed] {
        let calls = Cell::new(0usize);
        let outcome = super::recheck_with_one_retry(
            Duration::from_secs(10),
            Duration::from_secs(60),
            &|_budget, _label| {
                calls.set(calls.get() + 1);
                decided
            },
        );
        assert_eq!(outcome, decided);
        assert_eq!(calls.get(), 1, "{decided:?} is evidence — do not re-run it");
    }
}

/// Why (#4125): this is the real production shape — torch's import is starved
/// past the first budget by the daemon's own warm-boot, then completes fine
/// once given room. That venv must come back `Passed`, not be condemned.
///
/// #4125 follow-up: unlike the old version this drives a REAL bounded
/// subprocess, yet has no race. The stub the first attempt meets is `sleep 30`
/// against a 300 ms budget, which cannot finish; the stub the retry meets is
/// `exit 0` against a 30 s budget, which cannot fail to finish. Nothing depends
/// on how fast the box forks, because the seam — not a counter file written by
/// a process about to be killed — is what distinguishes the two attempts.
/// What: the venv stops being starved between the attempts; the result is
/// `Passed`. A policy that did not retry would return `Indeterminate` here.
/// Test: this test.
#[test]
fn full_import_recheck_retry_lets_a_starved_venv_prove_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let python = tmp.path().join("python");
    // Attempt 1 meets a stub that cannot possibly finish inside its budget.
    write_fake_venv_python(&python, "#!/bin/sh\nsleep 30\n");

    let calls = Cell::new(0usize);
    let outcome = super::recheck_with_one_retry(
        Duration::from_millis(300),
        Duration::from_secs(30),
        &|budget, label| {
            calls.set(calls.get() + 1);
            let got = super::run_bounded_python_check(&python, &["-c", "pass"], budget, label);
            // The contention ends between the attempts — exactly the shape of
            // the daemon finishing its warm-boot restores.
            write_fake_venv_python(&python, "#!/bin/sh\nexit 0\n");
            got
        },
    );

    assert_eq!(
        outcome,
        RecheckOutcome::Passed,
        "a venv that merely needed more time must pass, not be rebuilt"
    );
    assert_eq!(calls.get(), 2);
}

/// Why (#4125 follow-up): the two tests above pin the policy and the bounded
/// check separately; this one pins that the production entry point really
/// composes them — that `verify_full_import_smoke_bounded` spends BOTH budgets
/// on the real subprocess before reporting anything.
/// What: a stub that outlives both (deliberately tight) budgets reports
/// `Indeterminate`. No invocation count is asserted here: that is the policy
/// test's job, and counting it at this level is what made the old version racy.
/// Test: this test.
#[test]
fn verify_full_import_smoke_bounded_reports_indeterminate_when_both_budgets_lapse() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = super::resolve_layout_in(tmp.path());
    // `sleep 30` outlives a 300 ms budget on any machine — the outcome is fixed
    // by construction, not by scheduling luck.
    write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nsleep 30\n");

    assert_eq!(
        super::verify_full_import_smoke_bounded(
            &layout,
            Duration::from_millis(300),
            Duration::from_millis(300),
        ),
        RecheckOutcome::Indeterminate,
        "two exhausted budgets still prove nothing about the venv"
    );
}

/// Why (#4125): THE defect. `ensure_venv_checked` treated a timed-out recheck
/// as proof of a broken venv and tore down a perfectly intact one; the
/// resulting needless rebuild then hard-failed on `uv` discovery and pinned the
/// daemon on the ort backend for its whole lifetime. Against the pre-fix code
/// the `Indeterminate` arm below collapses to `false`, the rebuild runs, and
/// the call returns the uv error instead of the layout.
/// What: with a `.ready` venv in place and any rebuild guaranteed to fail
/// (`TRUSTY_UV_BIN` points at nothing), asserts `Indeterminate` returns the
/// existing layout untouched while `Failed` — the outcome that IS evidence —
/// still rebuilds. Both arms are asserted so the fix cannot be "kept" by
/// trusting every outcome either.
/// Test: this test.
#[test]
fn ensure_venv_does_not_rebuild_on_an_indeterminate_recheck() {
    // #4125: mutates TRUSTY_DATA_DIR_OVERRIDE + TRUSTY_UV_BIN — run alone in a
    // child process. Review finding 2 on PR #4771 named this test specifically;
    // `#[serial]` was NOT used, because it excludes only other `#[serial]`
    // tests and would have made the race quieter rather than absent (#4213,
    // #3954).
    if !isolate_in_child_process(
        "bootstrap::tests::ensure_venv_does_not_rebuild_on_an_indeterminate_recheck",
    ) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    with_data_override(tmp.path(), || {
        let layout = resolve_layout().unwrap();
        write_fake_venv_python(&layout.venv_python, "#!/bin/sh\nexit 0\n");
        write_site_packages_marker(&layout);
        std::fs::write(layout.base.join(".ready"), lockfile_hash()).unwrap();

        // Any rebuild attempt must fail loudly rather than start a real ~3 GB
        // torch download.
        deny_uv();

        let indeterminate =
            super::ensure_venv_verified(&|_| RecheckOutcome::Indeterminate).map(|l| l.venv_python);
        let failed = super::ensure_venv_verified(&|_| RecheckOutcome::Failed);

        allow_uv();

        assert_eq!(
            indeterminate.as_deref().ok(),
            Some(layout.venv_python.as_path()),
            "a timed-out recheck must reuse the `.ready` venv, not rebuild it (#4125)"
        );
        let err = failed.expect_err("a FAILED recheck is real evidence and must still rebuild");
        assert!(
            format!("{err:#}").contains("does not point to an existing file"),
            "expected the rebuild attempt to surface the uv-missing error, got: {err:#}"
        );
    });
}
