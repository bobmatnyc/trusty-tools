//! `tm doctor` session-profile probe (#8453).
//!
//! Why: a project that asks for the supervisor profile without the operator's
//! allowlist entry runs as a PM, and the only other signal is a launch-time
//! `warn` line. The operator needs the reason where they look.
//! What: [`check_session_profile`] `Warn`s with
//! [`crate::core::session_profile::NOT_ALLOW_LISTED`] when the project's
//! `.trusty-mpm.toml` says `profile = "supervisor"` and `~/.trusty-mpm/config.toml`
//! does not list the project under `[supervisor] projects`; otherwise `Ok`,
//! naming the profile a launch would resolve. Read-only.
//! Test: `a_project_only_supervisor_switch_warns`,
//! `an_allow_listed_supervisor_is_ok`.

use std::path::Path;

use crate::core::config::MpmConfig;
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::session_profile;

/// The check's name.
const NAME: &str = "session_profile";

/// Probe which instruction profile a launch in `project_dir` resolves.
///
/// Why: see the module doc.
/// What: loads the user config under `<home>/.trusty-mpm`; `Warn` with the
/// refusal reason, else `Ok` naming the profile. No project → `Ok`, nothing to
/// resolve.
/// Test: `a_project_only_supervisor_switch_warns`,
/// `an_allow_listed_supervisor_is_ok`.
pub(super) fn check_session_profile(project_dir: Option<&Path>, home: &Path) -> DoctorCheck {
    let Some(project) = project_dir else {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Ok,
            "no project directory supplied — nothing to resolve",
        );
    };
    let config = MpmConfig::load(&home.join(".trusty-mpm"));
    if let Some(reason) = session_profile::refusal(project, &config) {
        return DoctorCheck::new(NAME, CheckStatus::Warn, reason);
    }
    let profile = session_profile::resolve(project, &config);
    DoctorCheck::new(
        NAME,
        CheckStatus::Ok,
        format!("a launch here runs the `{}` profile", profile.id()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project asking for the supervisor profile, and a scratch home.
    fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join(".trusty-mpm.toml"),
            "profile = \"supervisor\"\n",
        )
        .unwrap();
        (project, tempfile::tempdir().unwrap())
    }

    #[test]
    fn a_project_only_supervisor_switch_warns() {
        let (project, home) = fixture();
        let check = check_session_profile(Some(project.path()), home.path());
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(
            check
                .message
                .contains("not allow-listed in ~/.trusty-mpm/config.toml"),
            "{}",
            check.message
        );
    }

    #[test]
    fn an_allow_listed_supervisor_is_ok() {
        let (project, home) = fixture();
        let root = home.path().join(".trusty-mpm");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("config.toml"),
            format!(
                "[supervisor]\nprojects = [{:?}]\n",
                project.path().display().to_string()
            ),
        )
        .unwrap();
        let check = check_session_profile(Some(project.path()), home.path());
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        assert!(check.message.contains("`supervisor`"), "{}", check.message);
    }
}
