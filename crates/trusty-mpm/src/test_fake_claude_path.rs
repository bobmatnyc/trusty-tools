//! A fake, executable `claude` first on `PATH` (#7862, #12077 part 4c).
//!
//! This file compiles into two targets through a `#[path]` include: the lib's
//! `test_support` and the `env_serial` module
//! `launch_files_sandbox_architect_12077`. It lives under `src/`, not
//! `tests/common/`, because the parallel `integration` target mounts `common`
//! and must hold no `PATH` write. Each includer provides `hermetic_temp_dir()`
//! and `ARCHITECT_MIN_CLAUDE_VERSION`: the lib spells the constant `crate::…`
//! and an integration target `trusty_mpm::…`, and no single spelling compiles
//! in both.
//! Test: `the_claim_is_still_held_when_the_route_types_into_the_pane`,
//! `an_architect_launch_carries_the_sandbox_only_when_opted_in_12077`.

use tempfile::TempDir;

use super::{ARCHITECT_MIN_CLAUDE_VERSION, hermetic_temp_dir};

/// A fake, executable `claude` first on `PATH`, plus the guard that restores
/// `PATH` when the test ends (#7862).
///
/// Why: every route that reaches `ClaudeCodeAdapter::spawn_resume` resolves the
/// `claude` binary before it types anything into the pane, so a test driving
/// that route on a machine with no Claude Code install stops at the adapter and
/// observes nothing past it — green on a developer laptop, red on every CI
/// runner. Winning the lookup outright also stops a machine that DOES have
/// `claude` from launching the operator's real one. #12077 part 4c: an
/// Architect launch through the carrier probes `claude --version`, so the stub
/// reports the Architect floor version.
/// What: writes a `#!/bin/sh` stub that prints the floor version and exits 0
/// at `<tempdir>/claude`, mode 0755, and prepends that directory to `PATH` —
/// `bin_resolve::resolve_binary` consults the live `PATH` before its
/// well-known-dirs fallback. Both the directory and the previous `PATH` live
/// in the returned guard, so the stub survives exactly as long as the test and
/// `Drop` restores the environment on the unwind too.
///
/// Callers MUST hold `PATH` alone: `#[serial_test::serial]` in the lib, the
/// one-thread `env_serial` target otherwise. `PATH` is process-global.
/// Test: `daemon::managed_routes::resume_claim_tests::the_claim_is_still_held_when_the_route_types_into_the_pane`,
/// `an_architect_launch_carries_the_sandbox_only_when_opted_in_12077`.
pub(crate) fn fake_claude_on_path() -> FakeClaudeOnPath {
    let dir = hermetic_temp_dir();
    let exe = dir.path().join("claude");
    let (major, minor, patch) = ARCHITECT_MIN_CLAUDE_VERSION;
    // See #10248
    trusty_common::exec_stub::write_executable(
        &exe,
        format!("#!/bin/sh\necho '{major}.{minor}.{patch} (Claude Code)'\n"),
    )
    .expect("write the fake claude");
    let prev = std::env::var_os("PATH");
    let mut entries = vec![dir.path().to_path_buf()];
    if let Some(ref p) = prev {
        entries.extend(std::env::split_paths(p));
    }
    let joined = std::env::join_paths(entries).expect("join PATH");
    // SAFETY: callers hold `PATH` alone (see above), and `Drop` restores it.
    unsafe { std::env::set_var("PATH", joined) };
    assert!(
        trusty_common::bin_resolve::resolve_binary("claude").is_some_and(|found| found == exe),
        "the planted stub must WIN the lookup, else the test it guards still \
         depends on the host's own Claude Code install"
    );
    FakeClaudeOnPath { _dir: dir, prev }
}

/// The live half of [`fake_claude_on_path`] — see its doc.
pub(crate) struct FakeClaudeOnPath {
    /// Holds the stub's directory alive for the guard's lifetime.
    _dir: TempDir,
    prev: Option<std::ffi::OsString>,
}

impl Drop for FakeClaudeOnPath {
    fn drop(&mut self) {
        // SAFETY: as in `fake_claude_on_path`.
        unsafe {
            match self.prev.take() {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}
