//! `tm doctor` egress-guard probe (#8454 G3, slice S2).
//!
//! Why: the `tm hook --egress-guard` entry is the only thing between a
//! tm-launched session and a Slack, Gmail, Notion or Google Chat MCP tool. A
//! settings file that lost it, shortened its timeout or set `disableAllHooks`
//! runs every session there unguarded, and nothing else on screen says so.
//! What: [`check_egress_guard`] reads every project-tier settings file the
//! hygiene probe reaches ([`candidate_settings_files`]) and the managed
//! user-tier `claude-config/settings{,.local}.json`, grouped by the directory
//! that holds them. A group is tm-provisioned when one of its files carries a
//! tm hook command (the lifecycle `tm hook`, the pm-guard or the egress
//! guard); the managed tier always is. `Fail` when a provisioned group carries
//! no egress group, a weakened one (a timeout under 10 or a narrowed matcher),
//! or sets `disableAllHooks`. `Unknown` when a file is unreadable or
//! malformed. `Ok` otherwise — a foreign project owes tm nothing. It never
//! reads `[pm_guard]`: the entry has no off switch (Architect D3). Read-only.
//! Test: the `egress_doctor_*` tests below.

use std::path::{Path, PathBuf};

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The check's name.
const NAME: &str = "egress_guard";

/// Report whether every tm-provisioned settings file registers the egress
/// guard. See the module doc.
pub(super) fn check_egress_guard(
    _project_dir: Option<&Path>,
    _active_workspace_paths: &[PathBuf],
    _home: &Path,
) -> DoctorCheck {
    DoctorCheck::new(NAME, CheckStatus::Ok, "egress guard registered")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A scratch home and project; the project's `.claude/` holds `files`.
    struct Fixture {
        _dir: tempfile::TempDir,
        home: PathBuf,
        project: PathBuf,
    }

    fn fixture(files: &[(&str, String)]) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&home).expect("mkdir home");
        std::fs::create_dir_all(project.join(".claude")).expect("mkdir .claude");
        for (name, body) in files {
            std::fs::write(project.join(".claude").join(name), body).expect("write settings");
        }
        Fixture {
            _dir: dir,
            home,
            project,
        }
    }

    fn check(fx: &Fixture) -> DoctorCheck {
        check_egress_guard(Some(fx.project.as_path()), &[], &fx.home)
    }

    /// A tm-provisioned settings body: the lifecycle `tm hook` group, plus the
    /// egress group with `timeout` when it is `Some`.
    fn provisioned(timeout: Option<u64>) -> String {
        let mut pre = vec![json!({ "matcher": "*", "hooks": [
            { "type": "command", "command": "/usr/local/bin/tm hook", "timeout": 5 }
        ]})];
        if let Some(timeout) = timeout {
            pre.push(json!({ "matcher": "", "hooks": [
                { "type": "command", "command": "/usr/local/bin/tm hook --egress-guard",
                  "timeout": timeout }
            ]}));
        }
        json!({ "hooks": { "PreToolUse": pre } }).to_string()
    }

    /// 🔴 #8454 S2: a provisioned project carrying the group is `Ok`.
    #[test]
    fn egress_doctor_reports_ok_for_a_registered_project() {
        let fx = fixture(&[("settings.json", provisioned(Some(10)))]);
        let row = check(&fx);
        assert_eq!(row.name, NAME);
        assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    }

    /// 🔴 #8454 S2 (plan §6 `doctor_flags_missing_egress_entry`): a
    /// provisioned project file with no egress group fails — even with
    /// `[pm_guard] enabled = false`, which this row never reads.
    #[test]
    fn egress_doctor_flags_missing_egress_entry() {
        let fx = fixture(&[("settings.json", provisioned(None))]);
        std::fs::create_dir_all(fx.home.join(".trusty-mpm")).expect("mkdir");
        std::fs::write(
            fx.home.join(".trusty-mpm/config.toml"),
            "[pm_guard]\nenabled = false\n",
        )
        .expect("write config");
        let row = check(&fx);
        assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
        assert!(row.message.contains("--egress-guard"), "{}", row.message);
    }

    /// 🔴 #8454 S2: a timeout under 10 lets Claude Code kill the hook into an
    /// allow, so it fails.
    #[test]
    fn egress_doctor_fails_a_timeout_under_10() {
        let fx = fixture(&[("settings.json", provisioned(Some(5)))]);
        let row = check(&fx);
        assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
        assert!(row.message.contains("timeout"), "{}", row.message);
    }

    /// 🔴 #8454 S2: `disableAllHooks` in either file of a provisioned project
    /// fails, though the group itself is intact.
    #[test]
    fn egress_doctor_fails_disable_all_hooks() {
        let fx = fixture(&[
            ("settings.json", provisioned(Some(10))),
            (
                "settings.local.json",
                json!({ "disableAllHooks": true }).to_string(),
            ),
        ]);
        let row = check(&fx);
        assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
        assert!(row.message.contains("disableAllHooks"), "{}", row.message);
    }

    /// 🔴 #8454 S2: a file the probe cannot read or parse is `Unknown`, never
    /// `Ok`.
    #[test]
    fn egress_doctor_reports_unknown_for_a_malformed_or_unreadable_file() {
        let fx = fixture(&[("settings.json", "{ not json".to_string())]);
        let row = check(&fx);
        assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);

        use std::os::unix::fs::PermissionsExt;
        let fx = fixture(&[("settings.json", provisioned(Some(10)))]);
        let path = fx.project.join(".claude/settings.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        // Root reads any file; the arm is only reachable for another user.
        if std::fs::read(&path).is_err() {
            let row = check(&fx);
            assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }

    /// 🔴 #8454 S2: a foreign project — no tm hook anywhere — owes tm nothing.
    #[test]
    fn egress_doctor_reports_ok_for_a_foreign_project() {
        let foreign = json!({ "hooks": { "PreToolUse": [{ "matcher": "", "hooks": [
            { "type": "command", "command": "trusty-memory prompt-context" }
        ]}]}});
        let fx = fixture(&[("settings.json", foreign.to_string())]);
        let row = check(&fx);
        assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    }

    /// 🔴 #8454 S2 (Architect D4): the managed user tier is tm-provisioned by
    /// definition, so its settings file must carry the group too.
    #[test]
    fn egress_doctor_fails_a_managed_tier_missing_the_group() {
        let fx = fixture(&[]);
        let managed = crate::core::trusty_tools_config::managed_claude_config_dir_at(&fx.home);
        std::fs::create_dir_all(&managed).expect("mkdir managed");
        std::fs::write(managed.join("settings.json"), "{}").expect("write managed");
        let row = check(&fx);
        assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
        std::fs::write(managed.join("settings.json"), provisioned(Some(10))).expect("rewrite");
        assert_eq!(check(&fx).status, CheckStatus::Ok);
    }
}
