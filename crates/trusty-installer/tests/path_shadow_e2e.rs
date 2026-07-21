//! End-to-end regression test for #3554 — PATH shadowing must not fool the
//! installer's health gate or its shadow-detection warning.
//!
//! Why: the prior isolated-`$HOME` E2E for the web installer passed even
//! though the real #3554 bug was present, precisely because a throwaway
//! `$HOME` has no pre-existing `~/.cargo/bin/tm` to shadow the fresh install
//! — there is nothing earlier on `$PATH` to read stale in that setup. That
//! is a false-negative class isolated/sandboxed testing structurally cannot
//! catch unless it deliberately reproduces the shadow precondition. This
//! test builds the REAL shape: an isolated home directory that ALSO
//! contains a stale, OLDER `tm` in a directory placed EARLIER on a synthetic
//! `$PATH` than the install prefix — exactly Bob's #3554 repro
//! (`~/.cargo/bin` 0.19.26 preceding `~/.local/bin` 0.19.29).
//!
//! What: exercises the two primitives `tctl install` composes to fix #3554
//! (see `crates/trusty-installer/src/commands/install.rs::install_one` /
//! `install_all`): [`trusty_common::update::verify_installed_binary_at_path`]
//! (the health gate, now pointed at the concrete install path) and
//! [`trusty_installer::commands::shadow_check::detect`] (the loud
//! PATH-shadow warning). Neither touches the real filesystem outside a
//! `tempfile` tempdir, the real `$PATH`, or any launchd/network resource —
//! safe to run on a live developer machine.
//!
//! `#3551` addition:
//! `supervisor_bootstrap_runs_fully_isolated_alongside_the_path_shadow_precondition`
//! adds the THIRD primitive `install_all` composes for a trusty-mpm member —
//! `plist_bootstrap::install_mpm_supervisor_for` (the supervisor bootstrap) —
//! run in the SAME fully-isolated flow, on the SAME PATH-shadow precondition.
//! Before #3551 this combination was impossible to exercise safely: the
//! supervisor bootstrap always resolved the real, live `gui/<real-uid>`
//! launchd domain regardless of `$HOME`, so a genuine end-to-end run had no
//! isolated path that also covered the supervisor step (the #3551 issue's
//! own E2E had to PATH-shadow the `launchctl` BINARY as an external
//! workaround — which would have collided with this file's PATH-shadow
//! precondition for `tm` itself). `install_mpm_supervisor_for`'s injected
//! [`trusty_installer::commands::plist_bootstrap::SupervisorTarget`] /
//! [`trusty_installer::commands::plist_bootstrap::StubLaunchctl`] removes
//! that workaround entirely.
//!
//! Full-flow caveat (documented, not papered over): none of the tests in
//! this file invoke `install_all` / `install_one` directly. `install_one`'s
//! prebuilt path (`download::try_install_prebuilt`) performs a real network
//! round-trip to GitHub Releases with no test seam (see
//! `install.rs::select_prebuilt_bin_path`'s doc, which already flags this
//! same constraint for #3554's coverage) — that is a separate, pre-existing
//! limitation of the download layer, unrelated to #3551's launchd-domain
//! defect, and out of scope here. This file instead calls the exact
//! functions `install_all`'s trusty-mpm branch composes
//! (`install_mpm_supervisor_for` and `shadow_check::detect`) directly, on a
//! synthetic `InstalledBinary`-shaped path standing in for what a real
//! `install_one` call would have returned — the same substitution
//! `install.rs`'s own `select_prebuilt_bin_path` doc uses to justify testing
//! below the network boundary.
//!
//! Test: this file IS the test.

use std::ffi::OsStr;

#[cfg(unix)]
fn write_versioned_binary(path: &std::path::Path, version_line: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\necho '{version_line}'\nexit 0\n"))
        .expect("write fake binary");
    let mut perms = std::fs::metadata(path)
        .expect("stat fake binary")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod fake binary");
}

/// THE #3554 regression shape, end to end: an isolated home directory that
/// ALSO contains a stale, OLDER `tm` on a synthetic `$PATH` entry placed
/// EARLIER than the install prefix.
///
/// Asserts:
/// 1. The health gate (`verify_installed_binary_at_path`, pointed at the
///    concrete install path) reads the NEW version — never the stale
///    shadowing copy — even though this exact synthetic `$PATH` would
///    resolve a bare `tm` lookup to the OLD one (demonstrated explicitly
///    below, proving this test's shape actually reproduces #3554's
///    mechanism rather than a clean-`$HOME` false pass).
/// 2. The shadow condition is surfaced, not silently swallowed
///    (`shadow_check::detect` fires, naming both paths and both versions).
#[cfg(unix)]
#[tokio::test]
async fn health_gate_reads_new_version_and_shadow_is_surfaced_despite_earlier_path_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // The stale, earlier-PATH copy — stands in for `~/.cargo/bin/tm` 0.19.26.
    let early_dir = tmp.path().join("early-cargo-bin");
    std::fs::create_dir_all(&early_dir).expect("mkdir early");
    write_versioned_binary(&early_dir.join("tm"), "trusty-mpm 0.19.26");

    // The just-installed copy — stands in for `~/.local/bin/tm` 0.19.29,
    // exactly where `tctl install`'s prebuilt path places it.
    let install_dir = tmp.path().join("local-bin");
    std::fs::create_dir_all(&install_dir).expect("mkdir install");
    let install_path = install_dir.join("tm");
    write_versioned_binary(&install_path, "trusty-mpm 0.19.29");

    // Synthetic PATH: the stale dir BEFORE the install dir — the exact
    // #3554 precondition (`~/.cargo/bin` precedes `~/.local/bin`).
    let synthetic_path = format!("{}:{}", early_dir.display(), install_dir.display());

    // Prerequisite check: prove a bare-name PATH lookup through this EXACT
    // synthetic PATH resolves to the STALE binary — otherwise this test
    // would not actually be reproducing #3554's shape (see module doc on
    // why the prior clean-HOME E2E gave a false pass).
    let bare_name_resolution = which::which_in("tm", Some(&synthetic_path), tmp.path())
        .expect("which_in must resolve `tm` somewhere on the synthetic PATH");
    assert_eq!(
        bare_name_resolution,
        early_dir.join("tm"),
        "precondition failed: a bare-name PATH lookup must resolve to the STALE \
         (earlier-PATH) binary for this test to reproduce #3554's mechanism"
    );

    // 1) THE health gate: pointed at the concrete install path, it must read
    // the NEW version — never whatever a bare `tm` PATH lookup would find.
    let reported = trusty_common::update::verify_installed_binary_at_path(&install_path)
        .await
        .expect("health gate must pass against the concrete install path");
    assert!(
        reported.contains("0.19.29"),
        "health gate must read the NEW binary's version even though an OLDER \
         copy shadows it earlier on PATH; got: {reported:?}"
    );
    assert!(
        !reported.contains("0.19.26"),
        "health gate must NOT read the stale shadowed binary; got: {reported:?}"
    );

    // 2) THE #3554 shadow-detection warning: must fire, naming both paths
    // and both versions — never a silent success.
    let report = trusty_installer::commands::shadow_check::detect(
        "tm",
        &install_path,
        Some("0.19.29"),
        OsStr::new(&synthetic_path),
    )
    .await
    .expect("shadow warning must fire — an older copy shadows the new install on PATH");
    assert_eq!(report.shadowing_path, early_dir.join("tm"));
    assert_eq!(report.shadowing_version.as_deref(), Some("0.19.26"));
    assert_eq!(report.install_path, install_path);
    assert_eq!(report.install_version.as_deref(), Some("0.19.29"));

    let msg = report.message();
    assert!(
        msg.contains("0.19.29"),
        "message must name the install version: {msg}"
    );
    assert!(
        msg.contains("0.19.26"),
        "message must name the shadowing version: {msg}"
    );
    assert!(
        msg.contains(&early_dir.join("tm").display().to_string()),
        "message must name the shadowing path: {msg}"
    );
    assert!(
        msg.contains(&install_path.display().to_string()),
        "message must name the install path: {msg}"
    );
}

/// Negative/sanity companion: when the install directory is the ONLY (or
/// first) entry resolving the binary name on PATH, no shadow warning fires
/// — a clean install, or a correctly-ordered PATH, must not be flagged.
#[cfg(unix)]
#[tokio::test]
async fn no_shadow_warning_when_path_resolves_to_the_installed_binary() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let install_dir = tmp.path().join("local-bin");
    std::fs::create_dir_all(&install_dir).expect("mkdir");
    let install_path = install_dir.join("tm");
    write_versioned_binary(&install_path, "trusty-mpm 0.19.29");

    let report = trusty_installer::commands::shadow_check::detect(
        "tm",
        &install_path,
        Some("0.19.29"),
        OsStr::new(&install_dir.display().to_string()),
    )
    .await;
    assert!(report.is_none(), "no shadow should be reported: {report:?}");
}

/// THE #3551 payoff, combined with the #3554 shape above: the trusty-mpm
/// supervisor bootstrap (`install_all`'s OTHER post-install step for a
/// trusty-mpm member, alongside the shadow check exercised above) now runs
/// through the SAME fully-isolated home + PATH-shadow precondition, with
/// zero risk to the real host.
///
/// Why this specific combination matters: before #3551, `install_all`'s
/// trusty-mpm branch called `plist_bootstrap::install_mpm_supervisor`, which
/// ALWAYS resolved the real, live `gui/<real-uid>` launchd domain — an
/// isolated `$HOME` could not redirect it (see #3551's issue body: the
/// E2E's only way to avoid mutating the live supervisor was to PATH-shadow
/// the `launchctl` BINARY ITSELF with a no-op stub, external to the process
/// under test). That workaround defeated its own purpose for #3554's shape:
/// a synthetic `$PATH` earlier-binary shadow is exactly the precondition
/// this test also needs for `tm`, so shadowing `launchctl` too would have
/// made "is a bare-name PATH lookup shadowable" indistinguishable from "did
/// the test harness intercept the subprocess" — an unfalsifiable test.
///
/// `install_mpm_supervisor_for` (#3551) removes the workaround entirely:
/// [`trusty_installer::commands::plist_bootstrap::StubLaunchctl`] is passed
/// in directly as a normal parameter — no PATH tricks, no env vars, and the
/// type system makes "this call can never spawn a real `launchctl`" visible
/// at the call site rather than inferred from a lack of host corruption.
///
/// What: runs the REAL `install_mpm_supervisor_for` — the exact function
/// `install.rs::install_mpm_supervisor` (which `install_all` calls for a
/// trusty-mpm member) delegates every bit of its logic to — against an
/// isolated home and a `StubLaunchctl`, using the SAME just-installed binary
/// this file's #3554 shape test above health-gates and shadow-checks.
/// Asserts: the plist lands under the isolated home (never the real one);
/// every `launchctl` call the stub recorded targets the injected synthetic
/// domain and NEVER the real, live `gui/<real uid>` domain this same process
/// would otherwise resolve; and, on the identical PATH-shadow precondition,
/// `shadow_check::detect` still fires — the two real checks `install_all`
/// performs after a trusty-mpm install, run together, fully isolated.
#[cfg(unix)]
#[tokio::test]
async fn supervisor_bootstrap_runs_fully_isolated_alongside_the_path_shadow_precondition() {
    use trusty_installer::commands::plist_bootstrap::{
        install_mpm_supervisor_for, resolve_uid, StubLaunchctl, SupervisorTarget, PLIST_LABEL,
    };

    let tmp = tempfile::tempdir().expect("tempdir");

    // The same #3554 PATH-shadow precondition as the test above: a stale,
    // older `tm` earlier on a synthetic `$PATH` than the fresh install.
    let early_dir = tmp.path().join("early-cargo-bin");
    std::fs::create_dir_all(&early_dir).expect("mkdir early");
    write_versioned_binary(&early_dir.join("tm"), "trusty-mpm 0.19.26");

    let install_dir = tmp.path().join("local-bin");
    std::fs::create_dir_all(&install_dir).expect("mkdir install");
    let install_path = install_dir.join("tm");
    write_versioned_binary(&install_path, "trusty-mpm 0.19.29");
    let synthetic_path = format!("{}:{}", early_dir.display(), install_dir.display());

    // Isolated HOME — a directory that is provably NOT the real user's home
    // (a fresh tempdir), where the supervisor plist / log dir / LaunchAgents
    // directory land.
    let isolated_home = tmp.path().join("isolated-home");
    std::fs::create_dir_all(&isolated_home).expect("mkdir isolated home");

    // 1) THE supervisor bootstrap: the real `install_mpm_supervisor_for`,
    // injected with an isolated home + a domain that provably differs from
    // the real live one + a `StubLaunchctl` that can never spawn a process.
    let real_domain = format!("gui/{}", resolve_uid());
    let synthetic_domain = "gui/999999-e2e-isolated-domain".to_owned();
    assert_ne!(
        synthetic_domain, real_domain,
        "precondition: the synthetic domain must differ from this host's real live domain"
    );
    let stub = StubLaunchctl::new();
    let target = SupervisorTarget {
        home: isolated_home.clone(),
        domain: synthetic_domain.clone(),
        launchctl: &stub,
    };

    let result = install_mpm_supervisor_for(&target, false, &install_path);

    assert!(
        result.is_ok(),
        "supervisor bootstrap must succeed against the isolated target: {result:?}"
    );
    let plist_path = isolated_home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{PLIST_LABEL}.plist"));
    assert!(
        plist_path.exists(),
        "plist must land under the ISOLATED home, never the real one"
    );
    let calls = stub.calls();
    assert!(
        !calls.is_empty(),
        "expected bootout + bootstrap calls to be recorded on the stub"
    );
    assert!(
        calls.iter().all(|c| c.contains(&synthetic_domain)),
        "every launchctl call must target the injected synthetic domain: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.contains(&real_domain)),
        "no launchctl call may reference the real, live domain this host would \
         otherwise resolve — this is the causal #3551 isolation proof: {calls:?}"
    );

    // 2) THE #3554 shadow-detection warning, on the SAME just-installed
    // binary and the SAME PATH-shadow precondition, run in the SAME
    // fully-isolated flow as the supervisor bootstrap above — exactly the
    // combination #3551 made impossible to exercise safely before.
    let report = trusty_installer::commands::shadow_check::detect(
        "tm",
        &install_path,
        Some("0.19.29"),
        OsStr::new(&synthetic_path),
    )
    .await
    .expect("shadow warning must fire — an older copy shadows the new install on PATH");
    assert_eq!(report.shadowing_path, early_dir.join("tm"));
    assert_eq!(report.shadowing_version.as_deref(), Some("0.19.26"));
    assert_eq!(report.install_path, install_path);
    assert_eq!(report.install_version.as_deref(), Some("0.19.29"));
}
