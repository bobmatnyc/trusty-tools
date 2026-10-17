//! The user-tier Claude config the daemon writes into (#9999).
//!
//! Why: the daemon's deployment repair and resume statusLine write named only a
//! home, so with `CLAUDE_CONFIG_DIR` set elsewhere they still wrote
//! `<home>/.claude/settings.json`, a file no session reads (#9846 fixed the CLI
//! repair path only). Split out of `core.rs`, which sits at the SLOC cap.
//! What: [`DaemonState::user_claude_config_dir`] and
//! [`DaemonState::ensure_status_line`], both resolved through
//! `session_launch::user_tier_config_dir` and `user_settings_path_in`.
//! Test: `the_resume_status_line_lands_under_the_config_dir_9999`.

use std::path::PathBuf;

use super::core::DaemonState;

impl DaemonState {
    /// The config dir relocating this daemon's user tier, if any (#9999).
    ///
    /// What: the test pin while [`Self::user_home`] is pinned, otherwise
    /// `CLAUDE_CONFIG_DIR` — see `session_launch::user_tier_config_dir`.
    /// Test: `the_daemon_repair_writes_the_user_tier_under_the_config_dir_9999`.
    pub fn user_claude_config_dir(&self) -> Option<PathBuf> {
        crate::core::session_launch::user_tier_config_dir(
            self.user_home.as_deref(),
            self.user_claude_config_dir.as_deref(),
        )
    }

    /// Pin this state's user home and user-tier config dir (#9999).
    #[cfg(test)]
    pub(crate) fn pin_user_tier(&mut self, home: PathBuf, config_dir: PathBuf) {
        self.user_home = Some(home);
        self.user_claude_config_dir = Some(config_dir);
    }

    /// Self-heal `workspace`'s statusLine and the user tier's.
    ///
    /// Why (#8545): the resume route used the ambient `$HOME`, so an isolated
    /// daemon test wrote the operator's `~/.claude/settings.json`. #9999: the
    /// user tier is the one `CLAUDE_CONFIG_DIR` names, when set.
    /// What: `session_launch::ensure_status_line_in` with the user-tier
    /// `settings.json` under [`Self::user_claude_config_dir`], else under
    /// `<home>/.claude`; a relative location seeds no user tier.
    /// Test: `the_claim_is_still_held_when_the_route_types_into_the_pane`,
    /// `the_resume_status_line_lands_under_the_config_dir_9999`.
    pub fn ensure_status_line(
        &self,
        workspace: &std::path::Path,
    ) -> Result<(), crate::core::session_launch::PrepError> {
        let home = self.user_home();
        let config_dir = self.user_claude_config_dir();
        let user = crate::core::session_launch::user_settings_path_in(
            home.as_deref(),
            config_dir.as_deref(),
        );
        crate::core::session_launch::ensure_status_line_in(workspace, user.as_deref())
    }
}

#[cfg(test)]
mod tests {
    /// #9999: the resume route's statusLine write follows the pinned config dir
    /// and leaves `<home>/.claude` untouched.
    #[test]
    fn the_resume_status_line_lands_under_the_config_dir_9999() {
        let root = tempfile::tempdir().expect("tempdir");
        let config_dir = tempfile::tempdir().expect("config dir");
        let workspace = tempfile::tempdir().expect("workspace");
        let mut state = super::DaemonState::with_root(root.path().to_path_buf());
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        state.pin_user_tier(home.clone(), config_dir.path().to_path_buf());

        state
            .ensure_status_line(workspace.path())
            .expect("statusLine seeded");

        let user = std::fs::read_to_string(config_dir.path().join("settings.json"))
            .expect("the user-tier settings.json lands in the config dir");
        assert!(user.contains("statusLine"), "{user}");
        assert!(
            !home.join(".claude").exists(),
            "nothing may be written under <home>/.claude when a config dir is named"
        );
    }
}
