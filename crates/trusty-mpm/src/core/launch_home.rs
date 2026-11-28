//! The home every launch-time write and sandbox policy names (#11077, #10102).
//!
//! Why: the `tm` binary's launch files and the daemon's managed-spawn sandbox
//! must resolve the same, sandbox-aware home, independent of the config base
//! that `TRUSTY_MPM_CONFIG_DIR` or `install.toml` can move off `$HOME`.
//! What: [`resolve_launch_home`] is the pure core; [`sandbox_launch_home`]
//! reads the process env and refuses when no home resolves.
//! Test: `a_split_config_base_launch_writes_no_sandbox_12077`,
//! `sandboxed_launch_home_is_the_sandbox_home_not_the_process_home`.

use std::path::{Path, PathBuf};

/// The user home a launch's sandbox policy names (#10102).
///
/// Why: since #11821 P3 the framework root sits under the config base, which
/// `TRUSTY_MPM_CONFIG_DIR` or `install.toml` can move off `$HOME`; a home
/// derived from that root left the real `~/.ssh` and `~/.aws` readable.
/// What: [`resolve_launch_home`] over the process env and `dirs::home_dir()`.
/// FAIL-CLOSED: no resolvable home is an error, never a fallback.
/// Test: `a_split_config_base_launch_writes_no_sandbox_12077`.
pub fn sandbox_launch_home() -> Result<PathBuf, String> {
    resolve_launch_home(
        std::env::var_os("TRUSTY_SANDBOX").as_deref(),
        std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).as_deref(),
        dirs::home_dir(),
    )?
    .ok_or_else(|| {
        "#10102 sandbox settings: no home directory resolves, so the sandbox cannot name \
         `~/.ssh` or `~/.aws`; refusing the launch"
            .to_owned()
    })
}

/// The home every launch-time write goes under, with the env passed in.
///
/// Why (#11077): a sandboxed client (`TRUSTY_SANDBOX=1`, the env
/// `scripts/sandbox_daemon.sh` sets up) must never write or name its own
/// `$HOME`; tests must not mutate process-global env.
/// What: `sandbox` exactly `1` gives `<parent of data_override>/home`, the
/// layout `sandbox_daemon.sh` creates; an unset, relative or rootless override
/// is an error, never a fall back to `home` (Fail-Open Check). Otherwise `home`.
/// Test: `sandboxed_launch_home_is_the_sandbox_home_not_the_process_home`,
/// `sandboxed_launch_home_refuses_without_a_data_override`,
/// `unsandboxed_launch_home_is_the_process_home`.
pub fn resolve_launch_home(
    sandbox: Option<&std::ffi::OsStr>,
    data_override: Option<&std::ffi::OsStr>,
    home: Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    // #11077: the sandbox flag routes every launch-time write into the sandbox.
    if sandbox != Some(std::ffi::OsStr::new("1")) {
        return Ok(home);
    }
    let data = data_override
        .filter(|raw| !raw.is_empty())
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            "TRUSTY_SANDBOX=1 but TRUSTY_DATA_DIR_OVERRIDE is not an absolute path; \
             refusing to write launch files under the real home (#11077)"
                .to_owned()
        })?;
    let root = data
        .parent()
        .filter(|parent| parent.parent().is_some())
        .ok_or_else(|| {
            format!(
                "TRUSTY_DATA_DIR_OVERRIDE={} has no sandbox dir above it; refusing to \
                 write launch files under the real home (#11077)",
                data.display()
            )
        })?;
    Ok(Some(root.join("home")))
}
