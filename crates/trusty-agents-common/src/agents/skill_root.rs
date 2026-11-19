//! Deploy-time resolution of the skills-root placeholder in agent bodies (#7727).
//!
//! Why: an agent whose `tools:` omits `Skill` can reach an on-demand skill only
//! by opening the skill file with `Read`, and that file's absolute path depends
//! on the install being deployed into — the default managed root, a
//! `TRUSTY_MPM_ROOT`/`--root` override, or a trusty-code project. Agent assets
//! therefore spell the skills directory as [`SKILLS_ROOT_PLACEHOLDER`], and
//! every framework write resolves it here, in one place, with the root passed in
//! by the caller that already knows where skills deploy.
//! What: [`compose_agent_for_deploy`] composes an agent with the framework-owned
//! provenance stamp and substitutes the placeholder; [`resolve_skills_root`] is
//! the substitution alone; [`check_skills_root`] refuses a root that cannot be
//! written into a body (relative, non-UTF-8, multi-line).
//! Test: `agents::skill_root_tests`.

use std::path::Path;

use crate::agents::builder::{AgentBuildError, compose_agent_with_provenance};
use crate::agents::provenance::Provenance;

/// The token agent assets use for the absolute skills directory.
///
/// Why: short, because it is resident in every composed agent before
/// substitution (the rust-engineer budget is measured pre-substitution), and
/// spelled so it cannot collide with existing asset text — no asset, doc or
/// script contained `TM_SKILLS` when it was chosen.
/// What: `{{TM_SKILLS}}`. A pointer reads `{{TM_SKILLS}}/<skill>/SKILL.md`.
pub const SKILLS_ROOT_PLACEHOLDER: &str = "{{TM_SKILLS}}";

/// Validate a skills root and return the text written into agent bodies.
///
/// Why: a relative root (the `"."` no-home fallback) would resolve against
/// whatever directory the reading agent runs in, so the deploy must refuse it
/// rather than write a path that silently points nowhere.
/// What: `Ok(text)` for an absolute, UTF-8, single-line path that does not
/// itself contain the placeholder, with one trailing `/` removed; otherwise
/// [`AgentBuildError::UnresolvedSkillsRoot`] naming the reason.
/// Test: `relative_skills_root_is_refused`, `resolves_every_placeholder`.
pub fn check_skills_root(skills_root: &Path) -> Result<&str, AgentBuildError> {
    let text = skills_root.to_str().ok_or_else(|| {
        AgentBuildError::UnresolvedSkillsRoot(format!(
            "`{}` is not valid UTF-8",
            skills_root.display()
        ))
    })?;
    let refuse = |why: &str| AgentBuildError::UnresolvedSkillsRoot(format!("`{text}` {why}"));
    if !skills_root.is_absolute() {
        return Err(refuse("is not an absolute path"));
    }
    if text.contains('\n') || text.contains(SKILLS_ROOT_PLACEHOLDER) {
        return Err(refuse("cannot be written into an agent body"));
    }
    Ok(match text.strip_suffix('/') {
        Some(trimmed) if !trimmed.is_empty() => trimmed,
        _ => text,
    })
}

/// Replace every [`SKILLS_ROOT_PLACEHOLDER`] in `body` with `skills_root`.
///
/// What: validates the root with [`check_skills_root`] even when `body` holds
/// no placeholder, so an unresolvable root fails every deploy the same way.
/// Postcondition: the result contains no placeholder.
/// Test: `resolves_every_placeholder`, `relative_skills_root_is_refused`.
pub fn resolve_skills_root(body: &str, skills_root: &Path) -> Result<String, AgentBuildError> {
    let root = check_skills_root(skills_root)?;
    let resolved = body.replace(SKILLS_ROOT_PLACEHOLDER, root);
    debug_assert!(!resolved.contains(SKILLS_ROOT_PLACEHOLDER));
    Ok(resolved)
}

/// Compose `name` exactly as a framework deploy writes it.
///
/// Why: the deployer, `tm install --reset-agents` and the catalog staleness
/// hash must produce identical bytes, or a freshly deployed file reads as
/// drifted. One function is what keeps the three in step.
/// What: [`compose_agent_with_provenance`] with [`Provenance::FrameworkOwned`],
/// then [`resolve_skills_root`].
/// Test: `compose_for_deploy_resolves_the_real_base_agent`.
pub fn compose_agent_for_deploy(
    name: &str,
    source_dir: &Path,
    skills_root: &Path,
) -> Result<String, AgentBuildError> {
    let composed = compose_agent_with_provenance(name, source_dir, Provenance::FrameworkOwned)?;
    // #7684: every framework deploy starts its subagent without CLAUDE.md.
    resolve_skills_root(&stamp_omit_claude_md(&composed), skills_root)
}

/// The frontmatter line that starts a subagent without CLAUDE.md (#7684).
///
/// Why: a dispatched subagent takes its rules from its own prompt and the
/// delegation brief; reloading the project CLAUDE.md hierarchy into every
/// subagent turn is resident cost the PM already pays once.
/// What: Claude Code's `omitClaudeMd` subagent field. Claude Code older than
/// v2.1.271 ignores the key, and Claude Code ignores it for a main-session
/// `--agent`, so the PM keeps its CLAUDE.md. The project rules reach the
/// subagent through tm's `PreToolUse` hooks instead (pm-guard, or the egress
/// guard when pm-guard is off); a Claude Code session without tm's hooks has
/// no carrier, so its subagents get neither CLAUDE.md nor the rules
/// (docs/reference/agent-cost-controls.md section 4).
pub const OMIT_CLAUDE_MD_LINE: &str = "omitClaudeMd: true";

/// Add [`OMIT_CLAUDE_MD_LINE`] as the first frontmatter key of `composed`.
///
/// What: inserts after the opening `---` fence; a text with no frontmatter
/// fence, or one already carrying an `omitClaudeMd:` key, is returned as is.
/// Test: `compose_for_deploy_stamps_omit_claude_md`,
/// `stamp_omit_claude_md_is_idempotent_and_skips_unfenced_text`.
pub fn stamp_omit_claude_md(composed: &str) -> String {
    let Some(rest) = composed.strip_prefix("---\n") else {
        return composed.to_string();
    };
    let fm_end = rest.find("\n---").unwrap_or(rest.len());
    if rest[..fm_end]
        .lines()
        .any(|l| l.starts_with("omitClaudeMd:"))
    {
        return composed.to_string();
    }
    format!("---\n{OMIT_CLAUDE_MD_LINE}\n{rest}")
}
