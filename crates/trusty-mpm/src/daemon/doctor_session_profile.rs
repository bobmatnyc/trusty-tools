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
//! both `[architect]` and the legacy `[supervisor]` (#9755, Q7). #8436: for
//! an Architect it also names the instruction source (`architect.md` or the
//! legacy sections, and the content origin), the `AGENTS.md` state, and
//! `Warn`s on a refused or unapplied `AGENTS.md`, a core rule the content
//! lacks, a `CLAUDE.md` that `@`-imports what tm composes, or leftover PM
//! marker blocks. Read-only; never fetches content.
//! Test: `a_project_only_architect_switch_warns`,
//! `an_allow_listed_architect_is_ok`, `both_architect_tables_warn`,
//! `an_architect_row_names_its_instructions_and_agents_md`,
//! `an_architect_claude_md_with_a_double_load_or_markers_warns`,
//! `a_legacy_content_source_is_named_and_agents_md_warns`.

use std::path::Path;

use crate::core::architect_prompt::{self, AgentsMd};
use crate::core::config::MpmConfig;
use crate::core::content_source::{Fetch, FrameworkContent, resolve_for_in};
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::framework_content::AgentContentError;
use crate::core::session_profile;

/// The check's name.
const NAME: &str = "session_profile";

/// Probe which instruction profile a launch in `project_dir` resolves.
///
/// Why: see the module doc.
/// What: loads the user config under `<home>/.trusty-mpm`; `Warn` with the
/// refusal reason, else `Ok` naming the profile. No project → `Ok`, nothing to
/// resolve. An Architect's content is resolved as a launch would, with
/// [`Fetch::Never`].
/// Test: `a_project_only_architect_switch_warns`,
/// `an_allow_listed_architect_is_ok`, `both_architect_tables_warn`.
pub(super) fn check_session_profile(project_dir: Option<&Path>, home: &Path) -> DoctorCheck {
    let load = |project: &Path| {
        let cwd = std::env::current_dir().ok();
        let cache = trusty_common::content::default_cache_dir();
        resolve_for_in(
            Some(project),
            cwd.as_deref(),
            cache.as_deref(),
            Fetch::Never,
        )
        .and_then(|content| FrameworkContent::load(&content))
    };
    check_session_profile_with(project_dir, home, load)
}

/// [`check_session_profile`] with the content loader supplied.
fn check_session_profile_with(
    project_dir: Option<&Path>,
    home: &Path,
    load: impl FnOnce(&Path) -> Result<FrameworkContent, AgentContentError>,
) -> DoctorCheck {
    let config = MpmConfig::load(&home.join(".trusty-mpm"));
    let (mut status, mut message) = profile_verdict(project_dir, &config);
    if let Some(project) = project_dir
        && status == CheckStatus::Ok
        && session_profile::resolve(project, &config).is_architect()
    {
        let (warn, lines) = architect_report(project, load(project));
        if warn {
            status = CheckStatus::Warn;
        }
        message = std::iter::once(message)
            .chain(lines)
            .collect::<Vec<_>>()
            .join("; ");
    }
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

/// The Architect half of the row: whether to `Warn`, and its lines (#8436).
///
/// Why: plan r2 §4 — the operator sees which instructions an Architect gets
/// and why an override does not apply, in the row they already read.
/// What: the instruction source and origin, the core rules `core.md` lacks (content and binary out of step), the `AGENTS.md` state, and
/// the `CLAUDE.md` double-load imports and marker blocks. A content load
/// error is itself a warning; the `content` row grades it.
/// Test: `an_architect_row_names_its_instructions_and_agents_md`,
/// `a_legacy_content_source_is_named_and_agents_md_warns`,
/// `an_agents_md_carrying_a_dropped_core_marker_does_not_hide_it`.
fn architect_report(
    project: &Path,
    content: Result<FrameworkContent, AgentContentError>,
) -> (bool, Vec<String>) {
    let agents = architect_prompt::read_agents_md(project);
    let mut warn = matches!(agents, AgentsMd::Refused(_));
    let mut lines = Vec::new();
    match &content {
        Err(err) => {
            warn = true;
            lines.push(format!(
                "instructions unknown: the content does not load ({err})"
            ));
            lines.push(format!("AGENTS.md: {}", agents.summary()));
        }
        Ok(content) if architect_prompt::has_architect_md(content) => {
            lines.push(format!(
                "instructions: architect.md + core.md from {}",
                content.origin()
            ));
            // #8436: core.md alone — an AGENTS.md copy of a dropped rule's
            // marker must not make the core look complete.
            let core = content
                .instruction(architect_prompt::ARCHITECT_CORE_MD)
                .unwrap_or_default();
            let missing = crate::core::architect_core::missing_architect_core_members(
                &crate::core::instruction_fold::fold_block(core),
            );
            if !missing.is_empty() {
                warn = true;
                lines.push(format!(
                    "the Architect core lacks {} — this binary and the content release are out \
                     of step; run `tm content update` or update tm",
                    missing.join(", ")
                ));
            }
            let applied = if matches!(agents, AgentsMd::Read(_)) {
                ", applied"
            } else {
                ""
            };
            lines.push(format!("AGENTS.md: {}{applied}", agents.summary()));
        }
        Ok(content) => {
            lines.push(format!(
                "instructions: the 8 legacy supervisor sections from {} (it has no \
                 instructions/{}; `tm content update` brings it)",
                content.origin(),
                architect_prompt::ARCHITECT_MD
            ));
            let present = !matches!(agents, AgentsMd::Absent | AgentsMd::Empty);
            warn |= present;
            let note = if present {
                ", not applied by the legacy sections"
            } else {
                ""
            };
            lines.push(format!("AGENTS.md: {}{note}", agents.summary()));
        }
    }
    let claude_md = std::fs::read_to_string(project.join("CLAUDE.md")).unwrap_or_default();
    for import in double_load_imports(&claude_md) {
        warn = true;
        lines.push(format!(
            "CLAUDE.md imports `{import}`, which tm already composes into the prompt — drop the \
             import (double load)"
        ));
    }
    let blocks = crate::core::claude_md_sections::scan_project(project)
        .overrides
        .len();
    if blocks > 0 {
        warn = true;
        lines.push(format!(
            "CLAUDE.md carries {blocks} TRUSTY-MPM marker block(s); the Architect profile ignores \
             them — delete them"
        ));
    }
    (warn, lines)
}

/// The `@` imports in `claude_md` of a file tm composes for the Architect.
///
/// Why: Claude Code expands `@<path>` in `CLAUDE.md` every turn, so an import
/// of `AGENTS.md` or `ARCHITECT.md` loads that text twice (plan r2 §2).
/// What: each whitespace-separated `@<path>` token whose file name is
/// `AGENTS.md` or `ARCHITECT.md`, case-insensitive, in order.
/// Test: `an_architect_claude_md_with_a_double_load_or_markers_warns`.
fn double_load_imports(claude_md: &str) -> Vec<&str> {
    claude_md
        .split_whitespace()
        .filter(|token| {
            let Some(path) = token.strip_prefix('@') else {
                return false;
            };
            let name = path.rsplit('/').next().unwrap_or(path);
            ["AGENTS.md", "ARCHITECT.md"]
                .iter()
                .any(|f| name.eq_ignore_ascii_case(f))
        })
        .collect()
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

    /// An allow-listed Architect project and its scratch home.
    fn allow_listed() -> (tempfile::TempDir, tempfile::TempDir) {
        let (project, home) = fixture();
        let root = home.path().join(".trusty-mpm");
        std::fs::create_dir_all(&root).unwrap();
        let listed = format!("{:?}", project.path().display().to_string());
        std::fs::write(
            root.join("config.toml"),
            format!("[supervisor]\nprojects = [{listed}]\n"),
        )
        .unwrap();
        (project, home)
    }

    /// #8436: the Architect row names its instruction source and `AGENTS.md`.
    #[test]
    fn an_architect_row_names_its_instructions_and_agents_md() {
        let (project, home) = allow_listed();
        std::fs::write(project.path().join("AGENTS.md"), "hello\n").unwrap();
        let check = check_session_profile(Some(project.path()), home.path());
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        for needle in ["architect.md", "AGENTS.md: 6 bytes"] {
            assert!(
                check.message.contains(needle),
                "{needle}: {}",
                check.message
            );
        }
    }

    /// #8436: the core check reads `core.md` alone. An `AGENTS.md` that copies
    /// a dropped rule's marker sentence must not make the core look complete.
    #[test]
    fn an_agents_md_carrying_a_dropped_core_marker_does_not_hide_it() {
        let (project, home) = allow_listed();
        let marker = crate::core::architect_core::ARCHITECT_CORE[0].marker;
        let repo = crate::core::content_source::test_support::rc();
        let core = repo
            .instruction(architect_prompt::ARCHITECT_CORE_MD)
            .unwrap();
        assert!(
            core.contains(marker),
            "the marker is in the repository core"
        );
        let content =
            crate::core::framework_content::tests::content_with_core(&core.replace(marker, ""));
        std::fs::write(project.path().join("AGENTS.md"), format!("{marker}\n")).unwrap();
        let check =
            check_session_profile_with(Some(project.path()), home.path(), |_| Ok(content.clone()));
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(
            check.message.contains("the Architect core lacks"),
            "{}",
            check.message
        );
    }

    /// #8436 (Q7a): a content source without `architect.md` is named, and an
    /// `AGENTS.md` it cannot apply warns; an unloadable source warns too.
    #[test]
    fn a_legacy_content_source_is_named_and_agents_md_warns() {
        let (project, home) = allow_listed();
        let legacy = || Ok(crate::core::framework_content::tests::legacy_content());
        let check = check_session_profile_with(Some(project.path()), home.path(), |_| legacy());
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        assert!(
            check.message.contains("8 legacy supervisor sections"),
            "{}",
            check.message
        );
        std::fs::write(project.path().join("AGENTS.md"), "rules\n").unwrap();
        let check = check_session_profile_with(Some(project.path()), home.path(), |_| legacy());
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(check.message.contains("not applied"), "{}", check.message);
        let check = check_session_profile_with(Some(project.path()), home.path(), |_| {
            Err(AgentContentError::NoCacheDir)
        });
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    }

    /// #8436: a CLAUDE.md that `@`-imports what tm composes, or that carries
    /// PM marker blocks the Architect ignores, warns.
    #[test]
    fn an_architect_claude_md_with_a_double_load_or_markers_warns() {
        let block =
            "<!-- TRUSTY-MPM: WORKFLOW START v=1 -->\nx\n<!-- TRUSTY-MPM: WORKFLOW END -->\n";
        for (claude_md, needle) in [
            ("# Fleet\n\n@AGENTS.md\n", "@AGENTS.md"),
            ("See @./ARCHITECT.md for the manual.\n", "@./ARCHITECT.md"),
            (block, "marker block"),
        ] {
            let (project, home) = allow_listed();
            std::fs::write(project.path().join("CLAUDE.md"), claude_md).unwrap();
            let check = check_session_profile(Some(project.path()), home.path());
            assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
            assert!(
                check.message.contains(needle),
                "{needle}: {}",
                check.message
            );
        }
    }
}
