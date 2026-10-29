//! #8942 critic MEDIUM: the production `ArchitectFloor::host()` line.
//!
//! Why: in the lib's own `cfg(test)` build `host()` is unguarded, so no unit
//! test runs `from_home(dirs::home_dir())`. This target links the lib without
//! `cfg(test)`, so here it is the real line.
//! What: points `$HOME` at a scratch directory holding one Architect sidecar
//! and asks the host floor, and the discovered `TmuxDriver`'s floor when tmux
//! is installed. `env_serial` runs one test at a time, so the `$HOME` change
//! is invisible to every other test.
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test env_serial architect_floor_host::`.

use trusty_mpm::session_manager::{ArchitectFloor, KillVerdict};

/// Restores the named variables on drop, so a failed assertion cannot leak
/// an override.
struct EnvOverride(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvOverride {
    /// Set each `(name, value)`, remembering the prior values.
    fn set(vars: &[(&'static str, &std::ffi::OsStr)]) -> Self {
        let saved = vars
            .iter()
            .map(|(name, _)| (*name, std::env::var_os(name)))
            .collect();
        for (name, value) in vars {
            // SAFETY: `env_serial` runs one test at a time.
            unsafe { std::env::set_var(name, value) };
        }
        Self(saved)
    }
}

impl Drop for EnvOverride {
    fn drop(&mut self) {
        for (name, value) in self.0.drain(..) {
            // SAFETY: `env_serial` runs one test at a time.
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }
}

/// A scratch home holding one Architect sidecar for session `tm-arch`.
fn home_with_sidecar() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("scratch home");
    let sidecars = home.path().join(".trusty-mpm").join("architect-launch");
    std::fs::create_dir_all(&sidecars).expect("sidecar dir");
    let body = serde_json::json!({"pid": 111, "start_time": 1, "session": "tm-arch"});
    std::fs::write(sidecars.join("111.architect-session"), body.to_string()).expect("sidecar");
    home
}

#[test]
fn the_host_floor_reads_the_sidecars_under_home() {
    let home = home_with_sidecar();
    let _restore = EnvOverride::set(&[("HOME", home.path().as_os_str())]);

    let floor = ArchitectFloor::host();
    assert!(matches!(
        floor.verdict("tm-arch"),
        KillVerdict::Protected(_)
    ));
    assert!(matches!(
        floor.verdict("tm-arch-poll"),
        KillVerdict::Protected(_)
    ));
    assert_eq!(floor.verdict("tm-work"), KillVerdict::Permit);

    // The `discover()` wiring hands the same floor to the kill primitive.
    #[cfg(feature = "daemon")]
    if let Ok(driver) = trusty_mpm::daemon::tmux::TmuxDriver::discover() {
        let verdict = driver.architect_floor().verdict("tm-arch");
        assert!(matches!(verdict, KillVerdict::Protected(_)), "{verdict:?}");
    }
}

/// #9017 row 2d: the legacy `DELETE /sessions/{id}` kills by name through
/// `TmuxService::kill_best_effort`, which discovers the production driver and
/// its host floor. A name an Architect sidecar records, or one of its helper
/// names, never reaches tmux; an unrelated name does (the positive control,
/// which also proves `discover()` reached the fake rather than being gated).
#[cfg(feature = "daemon")]
#[tokio::test]
async fn the_legacy_delete_kill_never_reaches_a_sidecar_named_session() {
    use trusty_mpm::daemon::services::TmuxService;

    let home = home_with_sidecar();
    let _restore = EnvOverride::set(&[
        ("HOME", home.path().as_os_str()),
        ("TRUSTY_MPM_ALLOW_HOST_STATE", "1".as_ref()),
    ]);
    let log = home.path().join("calls.log");
    let bin = home.path().join("fake-tmux");
    let script = format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n", log.display());
    // See #10248
    trusty_common::exec_stub::write_executable(&bin, script).expect("write fake tmux");
    let calls = || std::fs::read_to_string(&log).unwrap_or_default();

    trusty_mpm::core::tmux::with_tmux_binary(bin.clone(), async {
        TmuxService::kill_best_effort("tm-arch");
        TmuxService::kill_best_effort("tm-arch-poll");
    })
    .await;
    assert!(
        !calls().contains("kill-session"),
        "a sidecar-named session was killed: {}",
        calls()
    );

    trusty_mpm::core::tmux::with_tmux_binary(bin, async {
        TmuxService::kill_best_effort("tm-work");
    })
    .await;
    assert!(
        calls()
            .lines()
            .any(|l| l.contains("kill-session") && l.contains("tm-work")),
        "the fake was never reached: {}",
        calls()
    );
}
