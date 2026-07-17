//! `tm doctor` probes for agent-bundled skills (DOC-42, issue #2889).
//!
//! Why: `docs/specs/agent-bundled-skills.md` §SPEC-AGENTSKILLS-03 requires two
//! validations over the deployed agent roster: (1) every `skills:` frontmatter
//! entry must resolve through the 3-tier precedence (project/user/bundled), or
//! it is a dangling reference; (2) an agent body that mentions a known skill
//! name in prose ("load the `X` skill") without declaring it in `skills:` is a
//! best-effort, INFORMATIONAL signal that the declaration may be missing —
//! the spec explicitly grades this INFO, not the same severity as a real
//! dangling reference. Issue #2906 review (MEDIUM finding): folding both into
//! one `Warn`-on-either check caused alert fatigue (a purely informational
//! prose hint escalated the whole report the same as a real broken
//! reference), so they are now two independent checks — `agent_skills`
//! (dangling references, can `Warn`) and `agent_skills_prose_hints` (always
//! `Ok`, hints carried in the message) — sharing one scan via
//! [`scan_agent_skills`] rather than re-walking the roster twice. `CheckStatus`
//! deliberately gains no new `Info` variant here (too cross-cutting a change
//! for this PR); "informational" is expressed as an `Ok`-status check whose
//! message still surfaces the hints.
//! Split out from `doctor.rs` (mirroring the existing `doctor_output_style.rs`
//! / `doctor_fs_checks.rs` / `doctor_deploy_validate.rs` / `doctor_staleness.rs`
//! splits) to keep it under the 500-SLOC production cap.
//! What: [`check_agent_skills`] returns both `DoctorCheck`s from one scan of
//! every deployed agent file (`~/.claude/agents/*.md` or the workspace-scoped
//! equivalent) — resolving each declared skill via [`resolve_skill_tier`] and
//! scanning the body for undeclared prose mentions of a KNOWN skill name via
//! a conservative regex.
//! Test: the `tests` module below covers no-agents-dir, all-resolved,
//! dangling-reference, and prose-mention-without-declaration for both checks.

use std::collections::BTreeSet;
use std::path::PathBuf;
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

/// The result of one scan over the deployed agent roster.
///
/// Why: [`check_agent_skills`] produces TWO independent [`DoctorCheck`]s from
/// the SAME directory walk / regex scan — separating the scan from the
/// verdict-rendering avoids either duplicating the walk or entangling two
/// different severity policies in one function.
/// What: the agents directory (for display), whether it was absent at all,
/// and the raw dangling-reference / prose-hint message lists.
/// Test: exercised indirectly via `check_agent_skills`'s own tests.
struct AgentSkillsScan {
    agents_dir: PathBuf,
    /// `true` when `agents_dir` could not be read (nothing deployed yet) —
    /// both checks short-circuit to `Ok` in that case; `check_agents`
    /// already owns the "roster is empty" diagnosis.
    no_agents_dir: bool,
    dangling: Vec<String>,
    prose_hints: Vec<String>,
}

/// Scan the deployed agent roster once for both dangling `skills:` references
/// and undeclared prose mentions.
///
/// Why: a `skills:` entry naming a skill absent from every tier silently
/// produces a "skill not found" error at session-launch time (or worse, a
/// silently missing capability). The prose heuristic catches the EXACT
/// failure mode issue #2889 documents: `rust-engineer.md` told the agent to
/// "load … `toolchains-rust-core`" in prose with no `skills:` declaration,
/// and that skill was not bundled anywhere in the repo.
/// What: resolves the 3-tier stem sets once, then for every deployed
/// `*.md` agent file resolves each declared skill via [`resolve_skill_tier`]
/// (recording a dangling-reference message on `None`) and scans the body for
/// [`prose_skill_mentions`] not already covered by `skills:`.
/// Test: covered indirectly by every `check_agent_skills` test.
fn scan_agent_skills(paths: &FrameworkPaths) -> AgentSkillsScan {
    let agents_dir = paths.claude_agents_dir();
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return AgentSkillsScan {
            agents_dir,
            no_agents_dir: true,
            dangling: Vec::new(),
            prose_hints: Vec::new(),
        };
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

    AgentSkillsScan {
        agents_dir,
        no_agents_dir: false,
        dangling,
        prose_hints,
    }
}

/// Probe the deployed agent roster's `skills:` declarations for dangling
/// references and undeclared prose mentions (DOC-42 §SPEC-AGENTSKILLS-03).
///
/// Why: dangling references and prose hints carry DIFFERENT severities per
/// the spec — a dangling reference is an actionable, real gap; a prose hint
/// is a best-effort suggestion. Issue #2906 review (MEDIUM finding): folding
/// both into one `Warn`-on-either check caused alert fatigue. Returning two
/// checks from one scan keeps that severity split without re-walking the
/// roster twice.
/// What: returns `(agent_skills, agent_skills_prose_hints)`. `agent_skills`
/// is `Ok` when the agents directory is absent or every declared skill
/// resolves; `Warn` (never `Fail` — advisory, per the spec's "no deployment
/// is blocked" contract) naming every dangling reference otherwise.
/// `agent_skills_prose_hints` is ALWAYS `Ok` — informational per the spec —
/// but its message names every undeclared prose mention found, or reports
/// none.
/// Test: `no_agents_dir_is_ok`, `all_skills_resolve_is_ok`,
/// `dangling_skill_reference_is_warn`, `prose_mention_without_declaration_is_ok_but_reported`,
/// `declared_skill_suppresses_prose_hint`,
/// `prose_hints_never_escalate_agent_skills_status`.
pub(super) fn check_agent_skills(paths: &FrameworkPaths) -> (DoctorCheck, DoctorCheck) {
    let scan = scan_agent_skills(paths);

    let agent_skills = if scan.no_agents_dir {
        DoctorCheck::new(
            "agent_skills",
            CheckStatus::Ok,
            format!(
                "{} not present — nothing to validate",
                scan.agents_dir.display()
            ),
        )
    } else if scan.dangling.is_empty() {
        DoctorCheck::new(
            "agent_skills",
            CheckStatus::Ok,
            format!(
                "every declared skill in {} resolves",
                scan.agents_dir.display()
            ),
        )
    } else {
        DoctorCheck::new(
            "agent_skills",
            CheckStatus::Warn,
            format!(
                "{} dangling skill reference(s): {}",
                scan.dangling.len(),
                scan.dangling.join("; ")
            ),
        )
    };

    let agent_skills_prose_hints = if scan.no_agents_dir || scan.prose_hints.is_empty() {
        DoctorCheck::new(
            "agent_skills_prose_hints",
            CheckStatus::Ok,
            "no undeclared prose skill mentions found",
        )
    } else {
        DoctorCheck::new(
            "agent_skills_prose_hints",
            CheckStatus::Ok,
            format!(
                "{} undeclared prose mention(s) (informational — consider adding a `skills:` \
                 declaration): {}",
                scan.prose_hints.len(),
                scan.prose_hints.join("; ")
            ),
        )
    };

    (agent_skills, agent_skills_prose_hints)
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
        let (agent_skills, prose_hints) = check_agent_skills(&paths);
        assert_eq!(agent_skills.status, CheckStatus::Ok);
        assert_eq!(prose_hints.status, CheckStatus::Ok);
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

        let (agent_skills, prose_hints) = check_agent_skills(&paths);
        assert_eq!(
            agent_skills.status,
            CheckStatus::Ok,
            "{}",
            agent_skills.message
        );
        assert_eq!(prose_hints.status, CheckStatus::Ok);
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

        let (agent_skills, prose_hints) = check_agent_skills(&paths);
        assert_eq!(agent_skills.status, CheckStatus::Warn);
        assert!(agent_skills.message.contains("missing-skill"));
        assert!(agent_skills.message.contains("critic"));
        // A dangling reference is NOT a prose hint — the prose check must
        // stay Ok and unaffected.
        assert_eq!(prose_hints.status, CheckStatus::Ok);
    }

    #[test]
    fn prose_mention_without_declaration_is_ok_but_reported() {
        // Issue #2906 review (MEDIUM finding): a prose-only mention is
        // informational — `agent_skills_prose_hints` must stay `Ok`, but its
        // message must still name the mention so it's discoverable.
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

        let (agent_skills, prose_hints) = check_agent_skills(&paths);
        assert_eq!(prose_hints.status, CheckStatus::Ok);
        assert!(prose_hints.message.contains("toolchains-rust-core"));
        // No `skills:` was declared at all, so there is nothing dangling —
        // `agent_skills` must stay Ok too.
        assert_eq!(agent_skills.status, CheckStatus::Ok);
    }

    #[test]
    fn prose_hints_never_escalate_agent_skills_status() {
        // A prose hint alone (no dangling reference anywhere) must never
        // push `agent_skills` to `Warn` — that would reintroduce the alert
        // fatigue this split fixes.
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let agents = paths.claude_agents_dir();
        std::fs::create_dir_all(&agents).unwrap();
        write_agent(
            &agents,
            "rust-engineer",
            "---\nname: rust-engineer\n---\n\nload the **`toolchains-rust-core`** skill.\n",
        );
        let skill_source = paths.skill_source_dir();
        std::fs::create_dir_all(&skill_source).unwrap();
        std::fs::write(
            skill_source.join("toolchains-rust-core.md"),
            "---\nname: toolchains-rust-core\n---\n\nSkill body.\n",
        )
        .unwrap();

        let (agent_skills, _prose_hints) = check_agent_skills(&paths);
        assert_eq!(agent_skills.status, CheckStatus::Ok);
    }

    #[test]
    fn declared_skill_suppresses_prose_hint() {
        // The same prose mention, but ALSO declared in `skills:`, must not be
        // flagged as an undeclared reference — the prose-hints message must
        // report zero mentions.
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

        let (agent_skills, prose_hints) = check_agent_skills(&paths);
        assert_eq!(
            agent_skills.status,
            CheckStatus::Ok,
            "{}",
            agent_skills.message
        );
        assert_eq!(prose_hints.status, CheckStatus::Ok);
        assert!(!prose_hints.message.contains("toolchains-rust-core"));
    }
}
