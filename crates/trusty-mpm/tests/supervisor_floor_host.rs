//! #8942 critic MEDIUM: the production `SupervisorFloor::host()` line.
//!
//! Why: in the lib's own `cfg(test)` build `host()` is unguarded, so no unit
//! test runs `from_home(dirs::home_dir())`. This target links the lib without
//! `cfg(test)`, so here it is the real line.
//! What: points `$HOME` at a scratch directory holding one Architect sidecar
//! and asks the host floor, and the discovered `TmuxDriver`'s floor when tmux
//! is installed. `env_serial` runs one test at a time, so the `$HOME` change
//! is invisible to every other test.
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test env_serial supervisor_floor_host::`.

use trusty_mpm::session_manager::{KillVerdict, SupervisorFloor};

/// Restores `$HOME` on drop, so a failed assertion cannot leak the override.
struct HomeOverride(Option<std::ffi::OsString>);

impl Drop for HomeOverride {
    fn drop(&mut self) {
        // SAFETY: `env_serial` runs one test at a time.
        match self.0.take() {
            Some(home) => unsafe { std::env::set_var("HOME", home) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

#[test]
fn the_host_floor_reads_the_sidecars_under_home() {
    let home = tempfile::tempdir().expect("scratch home");
    let sidecars = home.path().join(".trusty-mpm").join("architect-launch");
    std::fs::create_dir_all(&sidecars).expect("sidecar dir");
    let body = serde_json::json!({"pid": 111, "start_time": 1, "session": "tm-arch"});
    std::fs::write(sidecars.join("111.architect-session"), body.to_string()).expect("sidecar");
    let _restore = HomeOverride(std::env::var_os("HOME"));
    // SAFETY: `env_serial` runs one test at a time.
    unsafe { std::env::set_var("HOME", home.path()) };

    let floor = SupervisorFloor::host();
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
        let verdict = driver.supervisor_floor().verdict("tm-arch");
        assert!(matches!(verdict, KillVerdict::Protected(_)), "{verdict:?}");
    }
}
