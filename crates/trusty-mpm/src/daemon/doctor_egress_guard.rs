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
//! malformed, or when `claude-config/` cannot be stat'ed; a `claude-config/`
//! whose `settings.json` is missing fails. `Ok` otherwise — a foreign
//! project owes tm nothing. It never reads `[pm_guard]`: the entry has no off
//! switch (Architect D3). Read-only.
//! Test: the `egress_doctor_*` tests below.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::doctor_hooks_hygiene::candidate_settings_files;
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::standalone::hooks::build_tree::PM_GUARD_SUFFIX;
use crate::core::standalone::hooks::cleanup::event_names_matching;
use crate::core::standalone::hooks::egress::{
    EgressGuardState, disables_all_hooks, egress_guard_state, is_egress_guard_hook_command,
};
use crate::core::standalone::hooks::is_mpm_hook_command;
use crate::core::trusty_tools_config::managed_claude_config_dir_at;

/// The check's name.
const NAME: &str = "egress_guard";

/// How many findings the message names before counting the rest.
const SHOWN: usize = 3;

/// The settings files in one directory, and whether tm owns it outright.
struct Tier {
    files: Vec<PathBuf>,
    managed: bool,
}

/// Report whether every tm-provisioned settings file registers the egress
/// guard. See the module doc.
pub(super) fn check_egress_guard(
    project_dir: Option<&Path>,
    active_workspace_paths: &[PathBuf],
    home: &Path,
) -> DoctorCheck {
    let mut tiers: BTreeMap<PathBuf, Tier> = BTreeMap::new();
    for file in candidate_settings_files(project_dir, active_workspace_paths) {
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        tiers
            .entry(dir)
            .or_insert_with(|| Tier {
                files: Vec::new(),
                managed: false,
            })
            .files
            .push(file);
    }
    let (mut fails, mut unknowns, mut guarded) = (Vec::new(), Vec::new(), 0usize);
    // #8454 fix round: the managed tier is judged whenever `claude-config/`
    // exists, so a deleted or dangling `settings.json` fails rather than
    // hiding the tier; a directory that cannot be stat'ed is Unknown.
    let managed = managed_claude_config_dir_at(home);
    match std::fs::symlink_metadata(&managed) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => unknowns.push(format!("{} cannot be read ({e})", managed.display())),
        Ok(_) => {
            let local = managed.join("settings.local.json");
            let mut files = vec![managed.join("settings.json")];
            if std::fs::symlink_metadata(&local)
                .map_or_else(|e| e.kind() != std::io::ErrorKind::NotFound, |_| true)
            {
                files.push(local);
            }
            tiers.insert(
                managed,
                Tier {
                    files,
                    managed: true,
                },
            );
        }
    }
    for (dir, tier) in &tiers {
        match judge_tier(dir, tier) {
            Verdict::Fine(true) => guarded += 1,
            Verdict::Fine(false) => {}
            Verdict::Unknown(why) => unknowns.extend(why),
            Verdict::Fail(why) => fails.extend(why),
        }
    }
    let remedy = "Relaunch the session with tm, or run `tm install` for the managed tier; a \
         running Claude Code session keeps the hooks it loaded at startup.";
    if !fails.is_empty() {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Fail,
            format!("{} {remedy}", listed(&fails)),
        );
    }
    if !unknowns.is_empty() {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Unknown,
            format!(
                "cannot tell whether the egress guard is registered: {}",
                listed(&unknowns)
            ),
        );
    }
    DoctorCheck::new(
        NAME,
        CheckStatus::Ok,
        format!(
            "`hook --egress-guard` registered in {guarded} tm-provisioned settings director{}",
            if guarded == 1 { "y" } else { "ies" }
        ),
    )
}

/// The outcome for one settings directory.
enum Verdict {
    /// Nothing wrong; `true` when the directory is tm-provisioned.
    Fine(bool),
    /// A file could not be read or parsed.
    Unknown(Vec<String>),
    /// The guard is missing, weakened or switched off.
    Fail(Vec<String>),
}

/// Judge one directory's settings files. See the module doc.
fn judge_tier(dir: &Path, tier: &Tier) -> Verdict {
    let mut values = Vec::new();
    let mut unknown = Vec::new();
    let mut missing = Vec::new();
    for file in &tier.files {
        match std::fs::read_to_string(file) {
            // #8454 fix round: the managed tier's own file is owed; a missing
            // one (or a dangling link) is a finding, not an unknown.
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && tier.managed
                    && file.file_name().is_some_and(|n| n == "settings.json") =>
            {
                missing.push(format!("{} is missing", file.display()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => unknown.push(format!("{} is unreadable ({e})", file.display())),
            Ok(text) if text.trim().is_empty() => {
                values.push((file, Value::Object(Default::default())))
            }
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(value) if value.is_object() => values.push((file, value)),
                _ => unknown.push(format!("{} is not a JSON object", file.display())),
            },
        }
    }
    let provisioned = tier.managed
        || values
            .iter()
            .any(|(_, value)| !event_names_matching(value, is_tm_hook_command).is_empty());
    let mut fails = missing;
    let (mut present, mut weakened) = (false, false);
    for (file, value) in &values {
        if disables_all_hooks(value) {
            fails.push(format!("{} sets disableAllHooks", file.display()));
        }
        match egress_guard_state(value) {
            EgressGuardState::Present => present = true,
            EgressGuardState::Weakened(why) => {
                weakened = true;
                fails.push(format!(
                    "{}: the egress entry is weakened — {why}",
                    file.display()
                ));
            }
            EgressGuardState::Missing => {}
        }
    }
    if provisioned && !present && !weakened && unknown.is_empty() {
        fails.push(format!(
            "{} carries no `hook --egress-guard` PreToolUse group",
            dir.display()
        ));
    }
    if provisioned && !fails.is_empty() {
        Verdict::Fail(fails)
    } else if !unknown.is_empty() {
        Verdict::Unknown(unknown)
    } else {
        Verdict::Fine(provisioned)
    }
}

/// A tm hook command: the lifecycle `tm hook` (including a build-tree one),
/// the pm-guard or the egress guard. Not `trusty-memory`, which writes its
/// own project hooks outside tm.
fn is_tm_hook_command(cmd: &str) -> bool {
    is_mpm_hook_command(cmd) || cmd.ends_with(PM_GUARD_SUFFIX) || is_egress_guard_hook_command(cmd)
}

/// The first [`SHOWN`] findings joined, plus a count of the rest.
fn listed(findings: &[String]) -> String {
    let mut text = findings
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if findings.len() > SHOWN {
        text.push_str(&format!(" (and {} more)", findings.len() - SHOWN));
    }
    text
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

    /// 🔴 #8454 S2 fix round (critic LOW): a `claude-config/` directory whose
    /// `settings.json` is gone, or a dangling link, fails rather than reading
    /// as no managed tier at all.
    #[test]
    fn egress_doctor_fails_a_managed_tier_whose_settings_file_is_missing() {
        let fx = fixture(&[]);
        let managed = crate::core::trusty_tools_config::managed_claude_config_dir_at(&fx.home);
        std::fs::create_dir_all(&managed).expect("mkdir managed");
        let row = check(&fx);
        assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
        assert!(row.message.contains("settings.json"), "{}", row.message);
        std::os::unix::fs::symlink(managed.join("gone.json"), managed.join("settings.json"))
            .expect("dangling link");
        let row = check(&fx);
        assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    }

    /// 🔴 #8454 S2 fix round (critic LOW): a managed tier the probe cannot
    /// stat or read is `Unknown`, never `Ok`.
    #[test]
    fn egress_doctor_reports_unknown_when_the_managed_tier_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        let fx = fixture(&[]);
        let managed = crate::core::trusty_tools_config::managed_claude_config_dir_at(&fx.home);
        std::fs::create_dir_all(&managed).expect("mkdir managed");
        std::fs::write(managed.join("settings.json"), provisioned(Some(10))).expect("write");
        let parent = managed.parent().expect("parent").to_path_buf();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        // Root stats any path; the arm is only reachable for another user.
        let blocked = std::fs::symlink_metadata(&managed).is_err();
        let row = check(&fx);
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        if blocked {
            assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
        }
    }
}
