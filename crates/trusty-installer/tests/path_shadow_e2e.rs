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
