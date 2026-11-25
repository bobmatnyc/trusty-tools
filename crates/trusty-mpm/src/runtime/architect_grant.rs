//! The one-use grant a binding launch path issues for an Architect launch
//! spec, and the launch shim redeems before it execs (#11270).
//!
//! Why: the shim (`tm internal-spawn-disclaimed --launch-spec`) is what
//! execs `claude`, and it runs whatever spec file it is handed. A spec the
//! shim cannot tie to a launch path that binds would start an Architect
//! `claude` with no launch record. The spec file alone cannot prove that:
//! whoever writes the spec controls every byte of it.
//! What: a binding caller ([`crate::runtime::cli_launch::send_spec_launch`],
//! `tm fleet init|relaunch`, the daemon's managed delivery) calls [`issue`]
//! right before it types the launch line. The grant is a file named by the
//! SHA-256 of the spec as written, under
//! `~/.trusty-mpm/architect-launch/launch-grants/`; everything under
//! `architect-launch/` is a pm-guard trust anchor, so no agent tool call can
//! write one. The shim calls [`redeem`], which deletes the grant for the
//! exact spec it read and refuses a missing or expired one. A grant never
//! creates a launch record; the caller still binds through
//! [`crate::core::architect_session::bind_architect`].
//! Test: `architect_grant_tests.rs`;
//! `an_architect_spec_without_a_grant_is_refused_before_exec_11270`
//! (the `tm` binary's shim).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::core::architect_compat::is_architect_stamp;
use crate::core::architect_launch::ARCHITECT_DIR;
use crate::core::session_profile::SESSION_PROFILE_ENV;
use crate::runtime::launch_spec::LaunchSpec;

/// The grant directory under `architect-launch/`.
pub const GRANT_DIR: &str = "launch-grants";

/// How long a grant stays redeemable: the pane runs the line within seconds.
pub const GRANT_TTL: Duration = Duration::from_secs(120);

/// Whether the `claude` the shim would exec for `spec` is an Architect.
///
/// Why: the child sees the spec's own stamp when it sets one, and otherwise
/// the stamp it inherits from the pane, unless the spec unsets it.
/// What: the spec's last stamp when `env_set` names one; `false` when
/// `env_unset` removes it; else `inherited`, the shim's own value.
/// Test: `the_child_stamp_decides_whether_a_launch_is_an_architect_11270`.
pub fn launch_is_architect(spec: &LaunchSpec, inherited: Option<&OsStr>) -> bool {
    if spec
        .env_set
        .iter()
        .any(|(key, _)| key == SESSION_PROFILE_ENV)
    {
        return crate::runtime::architect_bind::spec_is_architect(spec);
    }
    if spec.env_unset.iter().any(|key| key == SESSION_PROFILE_ENV) {
        return false;
    }
    is_architect_stamp(inherited)
}

/// `root/architect-launch/launch-grants`.
pub fn grant_dir(root: &Path) -> PathBuf {
    root.join(ARCHITECT_DIR).join(GRANT_DIR)
}

/// The grant path for `spec` under `root`: its SHA-256 as hex.
fn grant_path(root: &Path, spec: &LaunchSpec) -> Result<PathBuf, String> {
    let bytes = serde_json::to_vec(spec).map_err(|e| format!("the spec cannot be encoded: {e}"))?;
    Ok(grant_dir(root).join(format!("{:x}", Sha256::digest(bytes))))
}

/// Issue the grant for `spec` under `root` (`~/.trusty-mpm`).
///
/// What: writes the empty, mode-0600 grant file; the caller types the line
/// next. Only a launch path that binds the `claude` it starts calls this.
/// Test: `a_grant_is_redeemed_once_for_its_own_spec_11270`.
pub fn issue(root: &Path, spec: &LaunchSpec) -> Result<PathBuf, String> {
    let path = grant_path(root, spec)?;
    crate::core::architect_session::write_owner_only(&path, b"")
        .map_err(|e| format!("the Architect launch grant could not be written: {e}"))?;
    Ok(path)
}

/// Remove the grant for `spec`, best-effort, when its line was never typed.
pub fn revoke(root: &Path, spec: &LaunchSpec) {
    if let Ok(path) = grant_path(root, spec) {
        let _ = std::fs::remove_file(path);
    }
}

/// Redeem the grant for exactly `spec` under `root`, deleting it.
///
/// What: `Ok` when the grant existed, this call removed it, and it is younger
/// than [`GRANT_TTL`]; `Err` naming why otherwise. A second redeem of the same
/// grant fails, so one grant starts one `claude`.
/// Test: `a_grant_is_redeemed_once_for_its_own_spec_11270`,
/// `an_expired_or_foreign_grant_is_refused_11270`.
pub fn redeem(root: &Path, spec: &LaunchSpec) -> Result<(), String> {
    redeem_within(root, spec, GRANT_TTL)
}

/// [`redeem`] with the age limit named (test seam).
pub(crate) fn redeem_within(root: &Path, spec: &LaunchSpec, ttl: Duration) -> Result<(), String> {
    let path = grant_path(root, spec)?;
    let age = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .map_err(|_| "no tm launch path that binds the Architect issued this launch".to_owned())?
        .elapsed()
        .unwrap_or(Duration::ZERO);
    std::fs::remove_file(&path)
        .map_err(|_| "the launch grant was already used or removed".to_owned())?;
    if age > ttl {
        return Err(format!(
            "the launch grant expired ({} s old)",
            age.as_secs()
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "architect_grant_tests.rs"]
mod tests;
