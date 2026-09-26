//! A process-wide fence around the operator's home config paths (#8545).
//!
//! Why: the `tm` bin target's `guided_fallback_*` tests drove the real launch
//! pipeline, which resolved its deploy destinations from `$HOME` and deployed
//! the full skill and agent roster into `~/.trusty-tools/trusty-mpm/claude-config`
//! and `~/.trusty-mpm/framework`. Repointing `$HOME` cannot contain that class in
//! the bin target: `env_isolation_tests` bans the write there (#5544), and a
//! redirected `$HOME` makes [`crate::core::host_state_gate`] refuse tmux for the
//! whole binary (#5784). A fence needs no environment write at all.
//! What: [`arm`] records the fenced roots once per process; [`check`] panics when
//! a write destination sits under one of them. Production never arms the fence,
//! so there [`check`] is one `OnceLock` read that finds nothing. A test binary
//! arms it before `main`, so a test that reaches a home-config writer fails by
//! name, at the writer, before anything is written.
//! Test: `home_write_fence::tests`; the `tm` bin target arms it in
//! `test_support` and asserts that in `the_home_write_fence_is_armed_for_this_binary`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Home entries a test must never write: Claude Code's user config, and every
/// trusty-* install and config root.
const FENCED_HOME_ENTRIES: &[&str] = &[".claude", ".claude.json", ".trusty-mpm", ".trusty-tools"];

static FENCED_ROOTS: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// The fenced roots under one home directory.
pub fn fenced_roots_under(home: &Path) -> Vec<PathBuf> {
    FENCED_HOME_ENTRIES.iter().map(|e| home.join(e)).collect()
}

/// Arm the fence for this process, once.
///
/// Why: arming must happen before any test thread exists, so the caller is a
/// pre-`main` constructor; a second call is a no-op rather than a widening.
/// What: fences [`fenced_roots_under`] each of `homes`. Returns `true` when this
/// call armed the fence.
/// Test: `tests::arm_is_first_writer_wins`.
pub fn arm(homes: &[&Path]) -> bool {
    let mut roots: Vec<PathBuf> = homes.iter().flat_map(|h| fenced_roots_under(h)).collect();
    roots.dedup();
    FENCED_ROOTS.set(roots).is_ok()
}

/// Arm the fence over this process's `$HOME` and the password-database home.
///
/// Why: `$HOME` is where a home-resolving writer goes; the password-database
/// home is the operator's real one even when `$HOME` was repointed (#5784).
/// What: [`arm`] over whichever of the two resolve.
/// Test: `the_home_write_fence_is_armed_for_this_binary` (tm bin target).
pub fn arm_for_this_process() -> bool {
    let env_home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let passwd_home = crate::core::host_state_gate::passwd_home_dir();
    let homes: Vec<&Path> = env_home
        .iter()
        .chain(passwd_home.iter())
        .map(PathBuf::as_path)
        .collect();
    arm(&homes)
}

/// The roots this process fences; empty when the fence is not armed.
pub fn armed_roots() -> &'static [PathBuf] {
    FENCED_ROOTS.get().map_or(&[], Vec::as_slice)
}

/// The fenced root `dest` sits under, if any. Pure; compares lexically.
pub fn fenced_root<'a>(dest: &Path, roots: &'a [PathBuf]) -> Option<&'a Path> {
    roots
        .iter()
        .find(|root| dest.starts_with(root))
        .map(PathBuf::as_path)
}

/// Panic when the armed fence covers `dest`.
///
/// Why: every home-config writer here is best-effort — it logs a failure and
/// carries on — so an `Err` would be swallowed and the test would pass. Only a
/// panic fails the offending test by name.
/// What: no-op unless armed; otherwise panics naming `dest` and its root.
/// Test: `tests::check_panics_only_under_a_fenced_root`.
pub fn check(dest: &Path) {
    check_against(dest, armed_roots());
}

/// [`check`] against explicit roots, so the panic is testable without arming.
fn check_against(dest: &Path, roots: &[PathBuf]) {
    if let Some(root) = fenced_root(dest, roots) {
        panic!(
            "#8545: a test reached a write to {} under the fenced home path {}. \
             Pass the writer an explicit temp root (FrameworkPaths::under, \
             ManagedPaths::from_root, a `home` argument) instead of resolving \
             the operator's home.",
            dest.display(),
            root.display()
        );
    }
}

#[cfg(test)]
#[path = "home_write_fence_tests.rs"]
mod tests;
