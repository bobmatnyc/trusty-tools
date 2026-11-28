//! The launch carrier refuses an Architect on a Claude Code below the floor
//! (#12077 part 4c, #10459).
//!
//! Why: the carrier is the one choke point every launch path reaches, `tm run`
//! and the control-plane worker included; neither checks the floor itself.
//! What: a fake `claude` printing a fixed version goes first on `PATH`, `HOME`
//! points at a scratch dir, and an Architect launch goes through
//! `provision_settings`. Below the floor it refuses; at the floor it passes.
//! Test: this file IS the test (mounted in the `env_serial` target).

use std::ffi::OsString;
use std::path::Path;

use trusty_mpm::core::launch_policy::provision_settings;
use trusty_mpm::core::session_profile::SessionProfile;

/// The env vars the guard rewrites: `PATH`, `HOME` and the root overrides.
const VARS: [&str; 5] = [
    "PATH",
    "HOME",
    "TRUSTY_MPM_WORKSPACE_ROOT",
    "TRUSTY_MPM_REPOS_ROOT",
    "TRUSTY_MPM_CONFIG_DIR",
];

/// A fake `claude` reporting `version` first on `PATH`, under a scratch
/// `HOME`; `Drop` restores every var in [`VARS`].
struct FakeClaude {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl FakeClaude {
    fn install(tmp: &Path, version: &str) -> Self {
        let bin = tmp.join("bin");
        let home = tmp.join("home");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::create_dir_all(&home).expect("home");
        // See #10248
        trusty_common::exec_stub::write_executable(
            &bin.join("claude"),
            format!("#!/bin/sh\necho '{version} (Claude Code)'\n"),
        )
        .expect("fake claude");
        let saved = VARS.iter().map(|k| (*k, std::env::var_os(k))).collect();
        let mut path = vec![bin];
        if let Some(prev) = std::env::var_os("PATH") {
            path.extend(std::env::split_paths(&prev));
        }
        // SAFETY: `env_serial` forces one test thread; Drop restores every var.
        unsafe {
            std::env::set_var("PATH", std::env::join_paths(path).expect("join PATH"));
            std::env::set_var("HOME", &home);
            for k in &VARS[2..] {
                std::env::remove_var(k);
            }
        }
        Self { saved }
    }
}

impl Drop for FakeClaude {
    fn drop(&mut self) {
        // SAFETY: as in `install`.
        unsafe {
            for (k, v) in self.saved.drain(..) {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

/// An Architect launch through the carrier on `version`.
fn architect_launch(version: &str) -> String {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(&cwd).expect("project");
    let _env = FakeClaude::install(tmp.path(), version);
    let outcome = provision_settings(
        &tmp.path().join("state"),
        &cwd,
        None,
        SessionProfile::Architect,
    );
    format!("{outcome:?}")
}

/// #12077 part 4c: a Claude Code one patch below the floor refuses.
#[test]
fn the_carrier_refuses_an_architect_below_the_claude_floor_12077() {
    let shown = architect_launch("2.1.294");
    assert!(
        shown.starts_with("Err(") && shown.contains("TooOld"),
        "a Claude Code below the floor must refuse the launch, got {shown}"
    );
}

/// #12077 part 4c: a Claude Code at the floor launches.
#[test]
fn the_carrier_accepts_an_architect_at_the_claude_floor_12077() {
    let shown = architect_launch("2.1.295");
    assert!(
        shown.starts_with("Ok("),
        "the floor itself must pass, got {shown}"
    );
}
