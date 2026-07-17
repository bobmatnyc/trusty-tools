//! `tm doctor` probe for agent-bundled skills (DOC-42, issue #2889).
//!
//! Why: `docs/specs/agent-bundled-skills.md` §SPEC-AGENTSKILLS-03 requires two
//! validations over the deployed agent roster: (1) every `skills:` frontmatter
//! entry must resolve through the 3-tier precedence (project/user/bundled), or
//! it is a dangling reference; (2) an agent body that mentions a known skill
//! name in prose ("load the `X` skill") without declaring it in `skills:` is a
//! best-effort, informational signal that the declaration may be missing.
//! Split out from `doctor.rs` (mirroring the existing `doctor_output_style.rs`
//! / `doctor_fs_checks.rs` / `doctor_deploy_validate.rs` / `doctor_staleness.rs`
//! splits) to keep it under the 500-SLOC production cap.
//! What: [`check_agent_skills`] scans every deployed agent file
//! (`~/.claude/agents/*.md` or the workspace-scoped equivalent), resolves each
//! declared skill via [`resolve_skill_tier`], and scans the body for
//! undeclared prose mentions of a KNOWN skill name via a conservative regex.
//! Test: the `tests` module below covers no-agents-dir, all-resolved,
//! dangling-reference, and prose-mention-without-declaration.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::core::agent_metadata::read_agent_metadata;
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_tiers::{list_project_custom_stems, list_source_stems, resolve_skill_tier};

/// Conservative prose-mention pattern: a backticked identifier immediately
/// before or after the word "skill" (case-insensitive), tolerating Markdown
/// bold markers (`**`) between the closing backtick and "skill" — the exact
/// style trusty-mpm's own agent bodies use, e.g. `` **`toolchains-rust-core`**
/// skill ``.
///
/// Why: matching ONLY backticked names adjacent to the literal word "skill"
/// (rather than a looser "load X" pattern) keeps false positives low — per
/// §SPEC-AGENTSKILLS-03 this is explicitly "a best-effort heuristic, not a
/// hard rule". `LazyLock` (not `once_cell`, per project convention) compiles
/// the pattern once.
static PROSE_SKILL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)`([A-Za-z0-9_-]+)`\**\s*skill|\bskill\s*\**`([A-Za-z0-9_-]+)`")
        .expect("prose skill regex is valid")
});

/// Scan `body` for backticked mentions of a name in `known_skills`.
///
/// Why: only names that are ACTUAL deployed skills are worth flagging — a
/// backticked word that happens to precede "skill" but isn't a real skill
/// name is almost certainly unrelated prose, not a missed declaration.
/// What: returns the set of matched names (deduplicated) that also appear in
/// `known_skills`.
/// Test: `prose_mentions_matches_known_skill`,
/// `prose_mentions_ignores_unknown_backticked_name`.
fn prose_skill_mentions(body: &str, known_skills: &BTreeSet<String>) -> BTreeSet<String> {
    PROSE_SKILL_RE
        .captures_iter(body)
        .filter_map(|caps| caps.get(1).or_else(|| caps.get(2)))
        .map(|m| m.as_str().to_string())
        .filter(|name| known_skills.contains(name))
        .collect()
}

/// Probe the deployed agent roster's `skills:` declarations for dangling
/// references and undeclared prose mentions (DOC-42 §SPEC-AGENTSKILLS-03).
///
/// Why: a `skills:` entry naming a skill absent from every tier silently
/// produces a "skill not found" error at session-launch time (or worse, a
/// silently missing capability) — this probe catches it proactively. The
/// prose heuristic catches the EXACT failure mode issue #2889 documents:
/// `rust-engineer.md` told the agent to "load … `toolchains-rust-core`" in
/// prose with no `skills:` declaration, and that skill was not bundled
/// anywhere in the repo.
/// What: `Ok` when the agents directory is absent (nothing to validate — a
/// fresh/empty deploy is not this probe's concern; `check_agents` already
/// covers that) or every declared skill resolves with no undeclared prose
/// mentions; `Warn` (never `Fail` — this is advisory, per the spec's "no
/// deployment is blocked" contract) naming every dangling reference and
/// prose mention found.
/// Test: `no_agents_dir_is_ok`, `all_skills_resolve_is_ok`,
/// `dangling_skill_reference_is_warn`, `prose_mention_without_declaration_is_warn`,
/// `declared_skill_suppresses_prose_warning`.
pub(super) fn check_agent_skills(paths: &FrameworkPaths) -> DoctorCheck {
    let agents_dir = paths.claude_agents_dir();
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return DoctorCheck::new(
            "agent_skills",
            CheckStatus::Ok,
            format!("{} not present — nothing to validate", agents_dir.display()),
        );
    };

    let bundled = list_source_stems(&paths.skill_source_dir()).unwrap_or_default();
    let user = list_source_stems(&paths.user_skill_source_dir()).unwrap_or_default();
    let project = list_project_custom_stems(&paths.claude_skills_dir()).unwrap_or_default();
    let known_skills: BTreeSet<String> = bundled
        .iter()
        .chain(user.iter())
        .chain(project.iter())
        .cloned()
        .collect();

    let mut dangling: Vec<String> = Vec::new();
    let mut prose_hints: Vec<String> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };

        let meta = read_agent_metadata(&path);
        for skill in &meta.skills {
            if resolve_skill_tier(skill, &project, &user, &bundled).is_none() {
                dangling.push(format!("{stem} declares `{skill}` (not found in any tier)"));
            }
        }

        if let Ok(body) = std::fs::read_to_string(&path) {
            for mention in prose_skill_mentions(&body, &known_skills) {
                if !meta.skills.contains(&mention) {
                    prose_hints.push(format!(
                        "{stem} mentions `{mention}` in prose but does not declare it in `skills:`"
                    ));
                }
            }
        }
    }

    if dangling.is_empty() && prose_hints.is_empty() {
        return DoctorCheck::new(
            "agent_skills",
            CheckStatus::Ok,
            format!("every declared skill in {} resolves", agents_dir.display()),
        );
    }

    let mut message = String::new();
    if !dangling.is_empty() {
        message.push_str(&format!(
            "{} dangling skill reference(s): {}",
            dangling.len(),
            dangling.join("; ")
        ));
    }
    if !prose_hints.is_empty() {
        if !message.is_empty() {
            message.push_str(" | ");
        }
        message.push_str(&format!(
            "{} undeclared prose mention(s): {}",
            prose_hints.len(),
            prose_hints.join("; ")
        ));
    }
    DoctorCheck::new("agent_skills", CheckStatus::Warn, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn write_agent(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(format!("{name}.md")), content).unwrap();
    }

    #[test]
    fn prose_mentions_matches_known_skill() {
        let body = "load and apply the **`toolchains-rust-core`** skill.";
        let hits = prose_skill_mentions(body, &set(&["toolchains-rust-core"]));
        assert!(hits.contains("toolchains-rust-core"));
    }

    #[test]
    fn prose_mentions_ignores_unknown_backticked_name() {
        let body = "run the `cargo test` skill check.";
        let hits = prose_skill_mentions(body, &set(&["toolchains-rust-core"]));
        assert!(hits.is_empty());
    }

    #[test]
    fn prose_mentions_reverse_order() {
        let body = "load skill `code-review-standards` before reviewing.";
        let hits = prose_skill_mentions(body, &set(&["code-review-standards"]));
        assert!(hits.contains("code-review-standards"));
    }

    #[test]
    fn no_agents_dir_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let check = check_agent_skills(&paths);
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn all_skills_resolve_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let agents = paths.claude_agents_dir();
        std::fs::create_dir_all(&agents).unwrap();
        write_agent(
            &agents,
            "critic",
            "---\nname: critic\nskills: [code-review-standards]\n---\n\nBody.\n",
        );
        let skill_source = paths.skill_source_dir();
        std::fs::create_dir_all(&skill_source).unwrap();
        std::fs::write(
            skill_source.join("code-review-standards.md"),
            "---\nname: code-review-standards\n---\n\nSkill body.\n",
        )
        .unwrap();

        let check = check_agent_skills(&paths);
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    }

    #[test]
    fn dangling_skill_reference_is_warn() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let agents = paths.claude_agents_dir();
        std::fs::create_dir_all(&agents).unwrap();
        write_agent(
            &agents,
            "critic",
            "---\nname: critic\nskills: [missing-skill]\n---\n\nBody.\n",
        );

        let check = check_agent_skills(&paths);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("missing-skill"));
        assert!(check.message.contains("critic"));
    }

    #[test]
    fn prose_mention_without_declaration_is_warn() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let agents = paths.claude_agents_dir();
        std::fs::create_dir_all(&agents).unwrap();
        write_agent(
            &agents,
            "rust-engineer",
            "---\nname: rust-engineer\n---\n\nload and apply the **`toolchains-rust-core`** skill.\n",
        );
        let skill_source = paths.skill_source_dir();
        std::fs::create_dir_all(&skill_source).unwrap();
        std::fs::write(
            skill_source.join("toolchains-rust-core.md"),
            "---\nname: toolchains-rust-core\n---\n\nSkill body.\n",
        )
        .unwrap();

        let check = check_agent_skills(&paths);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("toolchains-rust-core"));
    }

    #[test]
    fn declared_skill_suppresses_prose_warning() {
        // The same prose mention, but ALSO declared in `skills:`, must not be
        // flagged as an undeclared reference.
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let agents = paths.claude_agents_dir();
        std::fs::create_dir_all(&agents).unwrap();
        write_agent(
            &agents,
            "rust-engineer",
            "---\nname: rust-engineer\nskills: [toolchains-rust-core]\n---\n\nload the **`toolchains-rust-core`** skill.\n",
        );
        let skill_source = paths.skill_source_dir();
        std::fs::create_dir_all(&skill_source).unwrap();
        std::fs::write(
            skill_source.join("toolchains-rust-core.md"),
            "---\nname: toolchains-rust-core\n---\n\nSkill body.\n",
        )
        .unwrap();

        let check = check_agent_skills(&paths);
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    }
}
