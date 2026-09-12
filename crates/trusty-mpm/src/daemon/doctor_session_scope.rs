//! `tm doctor` probe for what this project's sessions will NOT load (#7422).
//!
//! Why: default-deny changes what a session sees without changing anything the
//! operator can see. A server that used to connect now silently does not, and
//! the only clue is a tool that is no longer there. This probe is the migration
//! aid: run it in each project and it names every shared MCP server and every
//! installed plugin that project's sessions have stopped loading, plus the exact
//! keys that put one back.
//!
//! What: [`check_session_scope`] is INFORMATIONAL — `Ok` when nothing is
//! excluded, `Warn` when something is, and never `Fail`. An excluded server is
//! the designed outcome, not a fault; failing doctor over it would make a
//! correctly-scoped project look broken.
//!
//! #7678 added the second half, and it is the half that makes the first
//! trustworthy: the probe now also compares the project's `.claude/settings.json`
//! against the `enabledPlugins` map `prepare_session` would write. Until it did,
//! this check reported the DECISION while the plugins it named were still
//! loading — a session paused before that write existed, or resumed across the
//! upgrade that added it, runs against a settings file that never took it. So
//! `Ok` now means "the file matches", not merely "nothing is scoped out", and
//! `tm doctor --fix` re-applies the write.
//! Test: the `tests` module below.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The check's name, as `tm doctor` and the generated catalog print it.
///
/// Why: one literal, shared by the probe and the catalog drift guard.
/// What: `session_scope`.
/// Test: `doctor_checks_match_run_doctor_names`.
pub(super) const CHECK_NAME: &str = "session_scope";

/// Report the MCP servers and plugins this project's sessions will not load.
///
/// Why: see the module doc — this is the one surface that tells an operator why
/// a server stopped connecting, and where to opt it back in.
/// What: `Ok` with no project directory (nothing to scope), `Ok` when nothing
/// is excluded AND the project's `.claude/settings.json` already carries the
/// `enabledPlugins` map a launch would write, otherwise `Warn` naming the
/// excluded servers and plugins, every missing or divergent settings key, and
/// the `.trusty-mpm.toml` keys that restore them. Read-only: it composes the
/// same [`crate::core::session_mcp_scope::resolve_scope`] decision and the same
/// [`crate::core::session_scope_drift::plan_enabled_plugins`] the launch path
/// does, and writes nothing — `tm doctor --fix` owns the write (#7678).
/// Test: `session_scope_ok_when_nothing_is_excluded`,
/// `session_scope_warns_and_names_the_excluded_server`,
/// `session_scope_reports_an_excluded_plugin`,
/// `session_scope_is_ok_without_a_project`.
pub(super) fn check_session_scope(
    project_dir: Option<&Path>,
    config_dir: Option<&Path>,
) -> DoctorCheck {
    let trusted = project_dir
        .map(crate::core::project_trust::is_project_trusted)
        .unwrap_or(false);
    check_session_scope_with_trust(project_dir, config_dir, trusted)
}

/// [`check_session_scope`] against an explicit trust decision (#7678).
///
/// Why: the hermetic seam, mirroring
/// [`crate::core::session_mcp_scope::resolve_scope_with_trust`]. The trust bit
/// lives under the operator's `$HOME`, so without it no test could exercise the
/// one state that must report `Ok` — a project whose settings file already
/// carries the scope AND whose opt-ins leave nothing excluded.
/// What: see [`check_session_scope`]; `trusted` replaces the store lookup for
/// both the server and the plugin halves.
/// Test: `session_scope_warns_when_the_project_settings_lack_the_scope`,
/// `session_scope_is_ok_once_the_settings_carry_the_scope`,
/// `session_scope_names_each_divergent_key`.
pub(super) fn check_session_scope_with_trust(
    project_dir: Option<&Path>,
    config_dir: Option<&Path>,
    trusted: bool,
) -> DoctorCheck {
    let (Some(project), Some(config)) = (project_dir, config_dir) else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            "no project directory supplied — nothing to scope",
        );
    };

    let scope = crate::core::session_mcp_scope::resolve_scope_with_trust(project, config, trusted);
    // #7422: read the same GRANTED list the launch write uses, so an untrusted
    // project's declared plugins report excluded here and are written `false` there.
    let granted = crate::core::session_mcp_scope::granted_plugins_with_trust(project, trusted);
    let plugins = crate::core::session_plugin_scope::excluded_plugins(config, &granted);
    // #7678: and what the project's settings file still owes that write, read
    // through the plan the writer itself is built on.
    let drift =
        crate::core::session_scope_drift::plan_enabled_plugins_with_trust(project, config, trusted)
            .map(|plan| plan.drift)
            .unwrap_or_default();

    if scope.excluded.is_empty()
        && plugins.is_empty()
        && scope.degraded.is_none()
        && drift.is_empty()
    {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "sessions in {} load {} MCP server(s); nothing is scoped out and \
                 .claude/settings.json already carries the scope decision",
                project.display(),
                scope.included.len()
            ),
        );
    }

    let mut parts: Vec<String> = Vec::new();
    if !scope.excluded.is_empty() {
        parts.push(format!(
            "MCP servers NOT loaded: {} — opt in with `[session] mcp_servers = {:?}` in {}/{}",
            scope.excluded.join(", "),
            scope.excluded,
            project.display(),
            crate::core::project_config::PROJECT_CONFIG_FILE,
        ));
    }
    if !plugins.is_empty() {
        parts.push(format!(
            "plugins NOT loaded: {} — opt in with `[session] plugins = [...]`",
            plugins.join(", ")
        ));
    }
    if !drift.is_empty() {
        // #7678: the decision above is only what a LAUNCH would apply. Say so
        // when the file on disk does not carry it — that is the state in which
        // the "NOT loaded" line above is false for a session already running.
        parts.push(format!(
            "{}/.claude/settings.json does NOT carry the scope decision — {} key(s) missing or \
             divergent: {}; sessions launched before that write still load them. Re-apply with \
             `tm doctor --fix --yes`",
            project.display(),
            drift.len(),
            drift
                .iter()
                .map(crate::core::session_scope_drift::PluginKeyDrift::describe)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(reason) = &scope.degraded {
        parts.push(format!(
            "this project's own .mcp.json was skipped: {reason}"
        ));
    }

    DoctorCheck::new(CHECK_NAME, CheckStatus::Warn, parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn shared_config(dir: &Path, names: &[&str]) {
        let mut servers = serde_json::Map::new();
        for name in names {
            servers.insert(
                (*name).to_string(),
                json!({"type": "stdio", "command": name, "args": []}),
            );
        }
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(".claude.json"),
            serde_json::to_string_pretty(&json!({"mcpServers": servers})).unwrap(),
        )
        .unwrap();
    }

    /// Name `keys` in the managed config dir's installed-plugin index.
    fn installed_plugins(config: &Path, keys: &[&str]) {
        let mut plugins = serde_json::Map::new();
        for key in keys {
            plugins.insert((*key).to_string(), json!([]));
        }
        std::fs::create_dir_all(config.join("plugins")).unwrap();
        std::fs::write(
            config.join("plugins").join("installed_plugins.json"),
            serde_json::to_string_pretty(&json!({ "plugins": plugins })).unwrap(),
        )
        .unwrap();
    }

    /// Write a project `.claude/settings.json` holding `body`.
    fn project_settings(project: &Path, body: serde_json::Value) {
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::write(
            project.join(".claude").join("settings.json"),
            serde_json::to_string_pretty(&body).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn session_scope_is_ok_without_a_project() {
        let check = check_session_scope(None, None);
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn session_scope_ok_when_nothing_is_excluded() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("repo");
        let cfg = tmp.path().join("cfg");
        std::fs::create_dir_all(&project).unwrap();
        shared_config(&cfg, &[]);

        let check = check_session_scope(Some(&project), Some(&cfg));

        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        assert!(check.message.contains("nothing is scoped out"));
    }

    #[test]
    fn session_scope_warns_and_names_the_excluded_server() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("repo");
        let cfg = tmp.path().join("cfg");
        std::fs::create_dir_all(&project).unwrap();
        shared_config(&cfg, &["slack-mcp"]);

        let check = check_session_scope(Some(&project), Some(&cfg));

        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(
            check.message.contains("slack-mcp"),
            "the excluded server must be named: {}",
            check.message
        );
        assert!(
            check.message.contains(".trusty-mpm.toml"),
            "the message must say where to opt in: {}",
            check.message
        );
    }

    #[test]
    fn session_scope_reports_an_excluded_plugin() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("repo");
        let cfg = tmp.path().join("cfg");
        std::fs::create_dir_all(&project).unwrap();
        shared_config(&cfg, &[]);
        std::fs::create_dir_all(cfg.join("plugins")).unwrap();
        std::fs::write(
            cfg.join("plugins").join("installed_plugins.json"),
            serde_json::to_string_pretty(&json!({"plugins": {"aws-core@m": []}})).unwrap(),
        )
        .unwrap();

        let check = check_session_scope(Some(&project), Some(&cfg));

        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(
            check.message.contains("aws-core@m"),
            "the excluded plugin must be named: {}",
            check.message
        );
    }

    /// The #7678 defect itself: a settings file with no `enabledPlugins` key.
    ///
    /// Before the fix this reported only "plugins NOT loaded", which was false —
    /// with no key in the project tier, the user-tier `true` still won.
    #[test]
    fn session_scope_warns_when_the_project_settings_lack_the_scope() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("repo");
        let cfg = tmp.path().join("cfg");
        shared_config(&cfg, &[]);
        installed_plugins(&cfg, &["aws-core@m"]);
        project_settings(&project, json!({"outputStyle": "trusty-mpm"}));

        let check = check_session_scope_with_trust(Some(&project), Some(&cfg), false);

        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(
            check.message.contains("does NOT carry the scope decision"),
            "the hint must say the file never took the write: {}",
            check.message
        );
        assert!(
            check.message.contains("tm doctor --fix --yes"),
            "the hint must name the command that re-applies it: {}",
            check.message
        );
    }

    #[test]
    fn session_scope_names_each_divergent_key() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("repo");
        let cfg = tmp.path().join("cfg");
        shared_config(&cfg, &[]);
        installed_plugins(&cfg, &["aws-agents@m", "aws-core@m"]);
        project_settings(&project, json!({"enabledPlugins": {"aws-core@m": true}}));

        let check = check_session_scope_with_trust(Some(&project), Some(&cfg), false);

        assert!(
            check.message.contains("`aws-core@m` is true"),
            "a key set the wrong way must be named with both values: {}",
            check.message
        );
        assert!(
            check.message.contains("`aws-agents@m` absent"),
            "a missing key must be named as absent: {}",
            check.message
        );
    }

    /// `Ok` only once the file matches — the other half of #7678.
    ///
    /// A TRUSTED project that opts its one installed plugin in excludes nothing,
    /// so this is the state in which the settings comparison is the only thing
    /// standing between `Warn` and `Ok`.
    #[test]
    fn session_scope_is_ok_once_the_settings_carry_the_scope() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("repo");
        let cfg = tmp.path().join("cfg");
        std::fs::create_dir_all(&project).unwrap();
        shared_config(&cfg, &[]);
        installed_plugins(&cfg, &["aws-core@m"]);
        std::fs::write(
            project.join(crate::core::project_config::PROJECT_CONFIG_FILE),
            "[session]\nplugins = [\"aws-core\"]\n",
        )
        .unwrap();

        let before = check_session_scope_with_trust(Some(&project), Some(&cfg), true);
        assert_eq!(before.status, CheckStatus::Warn, "{}", before.message);

        project_settings(&project, json!({"enabledPlugins": {"aws-core@m": true}}));
        let after = check_session_scope_with_trust(Some(&project), Some(&cfg), true);

        assert_eq!(after.status, CheckStatus::Ok, "{}", after.message);
        assert!(
            after.message.contains("already carries the scope decision"),
            "{}",
            after.message
        );
    }
}
