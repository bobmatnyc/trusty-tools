//! A fake `claude` process for the fleet tests (#11061).
//!
//! Why: the fleet code finds its `claude` by process name, so the tests need a
//! process whose name contains `claude` and that lives until it is killed. A
//! symlink `claude-sleep` → `/bin/sleep` gave that on macOS, but uutils
//! coreutils (Ubuntu 26.04) is a multicall binary that dispatches on argv[0]:
//! it does not know `claude-sleep` and exits at once. A shell script is no fix
//! either, because macOS names a script process after its interpreter.
//! What: [`sleeper`] symlinks THIS test binary as `claude-sleep`. Both Linux
//! and macOS name the process after the symlink. Run with [`argv`] and
//! [`SECS_ENV`], the binary runs only [`fake_claude_sleeper`], which sleeps
//! that many seconds and exits 0. No host `sleep` is involved.
//!
//! This file compiles into two targets through a `#[path]` include: the `tm`
//! binary's unit tests (`commands::fleet::relaunch::tests`) and the
//! `integration` target (`tm_fleet`). It names only `std`, so one spelling
//! compiles in both.
//! Test: `stop_claude_ends_a_claude_process`, and every `tm_fleet::` test.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The variable that holds how many seconds [`fake_claude_sleeper`] sleeps;
/// unset, the test returns at once, so a normal suite run passes it.
pub(crate) const SECS_ENV: &str = "TM_FAKE_CLAUDE_SECS";

/// Symlink this test binary as `claude-sleep` in `dir` and return the path.
pub(crate) fn sleeper(dir: &Path) -> PathBuf {
    let path = dir.join("claude-sleep");
    let exe = std::env::current_exe().expect("the test binary's path");
    std::os::unix::fs::symlink(exe, &path).expect("fake claude process");
    path
}

/// The arguments that make the test binary run [`fake_claude_sleeper`] alone.
pub(crate) fn argv() -> Vec<String> {
    // libtest names a test by its module path without the crate name.
    let module = module_path!().split_once("::").map_or("", |(_, rest)| rest);
    vec![
        "--exact".to_owned(),
        format!("{module}::fake_claude_sleeper"),
        "--test-threads=1".to_owned(),
        "-q".to_owned(),
    ]
}

/// The fake `claude`'s body: sleep [`SECS_ENV`] seconds, or return at once.
#[test]
fn fake_claude_sleeper() {
    let secs = std::env::var(SECS_ENV).ok().and_then(|s| s.parse().ok());
    if let Some(secs) = secs {
        std::thread::sleep(Duration::from_secs(secs));
    }
}
