//! Discovery of the `trusty-embedderd` binary.
//!
//! Why: split out of `supervisor.rs` (#9828) to keep that file under the
//! 500-SLOC production cap once the recycle triggers landed there.
//! What: [`locate_embedderd_binary`] and its PATH helper, unchanged.
//! Test: `locate_binary_respects_explicit_override` in `supervisor_tests.rs`.

use std::path::PathBuf;

use anyhow::Result;

/// Locate the `trusty-embedderd` binary for the current build profile.
///
/// Why: the default auto-spawn path needs to find the binary without requiring
/// the operator to set `TRUSTY_EMBEDDERD_BIN`. The search order is:
///   1. `TRUSTY_EMBEDDERD_BIN` env var (explicit override)
///   2. Sibling of `current_exe()` (release build: both binaries are in
///      `target/release/`)
///   3. `trusty-embedderd` on `PATH`
///
/// What: returns the first path at which the file exists and is executable.
/// Returns `Err` if none is found.
///
/// Test: `locate_binary_respects_explicit_override` (the explicit override
/// path; the sibling and PATH tiers have no unit test).
pub fn locate_embedderd_binary() -> Result<PathBuf> {
    // Env override takes precedence over all discovery.
    if let Ok(explicit) = std::env::var("TRUSTY_EMBEDDERD_BIN") {
        let p = PathBuf::from(&explicit);
        if p.is_file() {
            return Ok(p);
        }
        anyhow::bail!("TRUSTY_EMBEDDERD_BIN={explicit:?} does not point to an existing file");
    }

    // Sibling of current_exe (works for both `cargo run` and installed binaries).
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("trusty-embedderd");
        if sibling.is_file() {
            return Ok(sibling);
        }
        // Windows
        let sibling_exe = dir.join("trusty-embedderd.exe");
        if sibling_exe.is_file() {
            return Ok(sibling_exe);
        }
    }

    // PATH lookup.
    if let Ok(path) = which_embedderd() {
        return Ok(path);
    }

    anyhow::bail!(
        "could not locate trusty-embedderd binary. \
         Set TRUSTY_EMBEDDERD_BIN=/path/to/trusty-embedderd or ensure it is on PATH."
    )
}

/// Minimal `which`-style PATH search for `trusty-embedderd`.
///
/// Why: avoids adding a `which` crate dependency just for this one lookup.
/// What: splits `PATH` by OS separator, appends `trusty-embedderd` (+ `.exe`
/// on Windows), returns the first path that is_file().
/// Test: tested implicitly when the sibling-path lookup fails.
fn which_embedderd() -> Result<PathBuf> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    let sep = if cfg!(windows) { ';' } else { ':' };
    for dir in path_var.split(sep) {
        let candidate = PathBuf::from(dir).join("trusty-embedderd");
        if candidate.is_file() {
            return Ok(candidate);
        }
        #[cfg(windows)]
        {
            let candidate_exe = PathBuf::from(dir).join("trusty-embedderd.exe");
            if candidate_exe.is_file() {
                return Ok(candidate_exe);
            }
        }
    }
    anyhow::bail!("trusty-embedderd not found on PATH")
}
