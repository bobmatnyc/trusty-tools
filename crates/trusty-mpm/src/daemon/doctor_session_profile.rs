//! `tm doctor` session-profile probe (#8453).
//!
//! Why: a project that asks for the supervisor profile without the operator's
//! allowlist entry runs as a PM, and the only other signal is a launch-time
//! `warn` line. The operator needs the reason where they look.
//! What: [`check_session_profile`] `Warn`s with
//! [`crate::core::session_profile::NOT_ALLOW_LISTED`] when the project's
//! `.trusty-mpm.toml` says `profile = "supervisor"` and `~/.trusty-mpm/config.toml`
//! does not list the project under `[supervisor] projects`; otherwise `Ok`,
//! naming the profile a launch would resolve. `Warn`s too when the config has
//! both `[architect]` and the legacy `[supervisor]` (#9755, Q7). Read-only.
//! Test: `a_project_only_architect_switch_warns`,
//! `an_allow_listed_architect_is_ok`, `both_architect_tables_warn`.

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
/// Test: `a_project_only_architect_switch_warns`,
/// `an_allow_listed_architect_is_ok`, `both_architect_tables_warn`.
pub(super) fn check_session_profile(project_dir: Option<&Path>, home: &Path) -> DoctorCheck {
    let config = MpmConfig::load(&home.join(".trusty-mpm"));
    let (status, message) = profile_verdict(project_dir, &config);
    // #9755 (Q7): the legacy table is ignored once `[architect]` exists.
    if config.has_both_architect_tables() {
        return DoctorCheck::new(NAME, CheckStatus::Warn, format!("{BOTH_TABLES}; {message}"));
    }
    DoctorCheck::new(NAME, status, message)
}

/// The warning when the user config carries both grant tables.
const BOTH_TABLES: &str = "~/.trusty-mpm/config.toml has both `[architect]` and the legacy \
                           `[supervisor]`; `[architect]` is used and `[supervisor]` is ignored — \
                           merge it into `[architect]` and delete it";

/// The profile row's status and message, before the both-tables warning.
fn profile_verdict(project_dir: Option<&Path>, config: &MpmConfig) -> (CheckStatus, String) {
    let Some(project) = project_dir else {
        let none = "no project directory supplied — nothing to resolve";
        return (CheckStatus::Ok, none.to_owned());
    };
    if let Some(reason) = session_profile::refusal(project, config) {
        return (CheckStatus::Warn, reason.to_owned());
    }
    let profile = session_profile::resolve(project, config);
    let ok = format!("a launch here runs the `{}` profile", profile.id());
    (CheckStatus::Ok, ok)
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
    fn a_project_only_architect_switch_warns() {
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
    fn an_allow_listed_architect_is_ok() {
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

    /// #9755 (Q7): both tables warn, and the row still names the profile
    /// `[architect]` grants — here none, so the legacy grant is ignored.
    #[test]
    fn both_architect_tables_warn() {
        let (project, home) = fixture();
        let root = home.path().join(".trusty-mpm");
        std::fs::create_dir_all(&root).unwrap();
        let listed = format!("{:?}", project.path().display().to_string());
        std::fs::write(
            root.join("config.toml"),
            format!("[architect]\nprojects = []\n\n[supervisor]\nprojects = [{listed}]\n"),
        )
        .unwrap();
        let check = check_session_profile(Some(project.path()), home.path());
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(
            check.message.contains("`[supervisor]` is ignored"),
            "{}",
            check.message
        );
        assert!(
            check.message.contains("not allow-listed"),
            "{}",
            check.message
        );
        // Without a project the warning still shows.
        let check = check_session_profile(None, home.path());
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    }
}
