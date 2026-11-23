//! The one way an integration test runs this crate's binaries (#11632).
//!
//! Why: a spawned `ta` inherits the test's environment. One spawn without a
//! `$HOME` override reached the operator's real home, so every spawn now goes
//! through here and `tests/bin_spawn_guard.rs` fails on any that does not.
//! What: [`ta`] and [`tagent`] return a `Command` whose `$HOME` and working
//! directory are the caller's `TempDir`, with every variable in
//! `SANDBOX_SCRUB` removed — the same list the process-start hook removes. Taking a `TempDir`, not a
//! path, is what keeps the home temporary. Including this module also installs
//! the process-start HOME sandbox (`src/test_home_sandbox.rs`) in the test
//! binary itself.
//! Test: `tests/bin_spawn_guard.rs`.

#![allow(dead_code)]

use std::process::Command;

use tempfile::TempDir;

include!("../../src/test_home_sandbox.rs");

/// `ta` under the temporary home `home`.
pub fn ta(home: &TempDir) -> Command {
    sandboxed(env!("CARGO_BIN_EXE_ta"), home)
}

/// `tagent`, the one-release alias, under the temporary home `home`.
pub fn tagent(home: &TempDir) -> Command {
    sandboxed(env!("CARGO_BIN_EXE_tagent"), home)
}

fn sandboxed(bin: &str, home: &TempDir) -> Command {
    let mut cmd = Command::new(bin);
    // #9617: stamp the child so a trusty-search daemon it starts dies with this test.
    trusty_common::parent_death::exit_with_parent(&mut cmd);
    cmd.current_dir(home.path()).env("HOME", home.path());
    for key in SANDBOX_SCRUB {
        cmd.env_remove(key);
    }
    cmd
}
