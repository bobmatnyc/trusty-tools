//! Skill-summary helpers for pre-plan skill discovery (#173).
//!
//! Why: The plan-agent only needs a one-liner per candidate skill so it can
//! decide whether to load the full body. Keeping the summary assembly here
//! bounds the engine's prompt tax (~200 chars/skill * N) and isolates the
//! frontmatter-stripping logic.
//! What: `skill_summary_for` produces a single-line summary from a
//! `SkillMeta`; `truncate_summary` and `strip_skill_frontmatter` are its
//! private helpers.
//! Test: Covered indirectly via `skill_discovery_extracts_python_fastapi_tags`
//! and the plan-agent prompt assembly integration tests in `executor`.

use crate::skills::registry::SkillMeta;

/// Build a short summary string for a discovered skill (#173).
///
/// Why: The plan-agent only needs a one-liner per candidate skill so it can
/// decide whether to load the full body. Pulling the summary here keeps the
/// engine's prompt assembly bounded (~200 chars/skill * N).
/// What: Prefers the frontmatter `description` when non-empty; otherwise
/// reads the file body, strips frontmatter, and returns up to 200 chars of
/// the body trimmed and collapsed onto a single line. Returns "(no
/// description)" on read failure — never panics.
/// Test: `skill_discovery_extracts_python_fastapi_tags` covers the
/// description path; the body-fallback path is exercised when the registry
/// has skills with empty descriptions.
pub(crate) fn skill_summary_for(meta: &SkillMeta) -> String {
    let trimmed = meta.description.trim();
    if !trimmed.is_empty() {
        return truncate_summary(trimmed);
    }
    // Fallback: read the file body and synthesize a summary. Use blocking
    // read because this runs at most once per discovered skill at workflow
    // startup; the registry is small (<100 skills typical).
    match std::fs::read_to_string(&meta.source_path) {
        Ok(raw) => {
            let body = strip_skill_frontmatter(&raw);
            truncate_summary(body.trim())
        }
        Err(e) => {
            tracing::warn!(
                name = %meta.name,
                path = %meta.source_path.display(),
                error = %e,
                "skill discovery: failed to read body for summary; using placeholder"
            );
            "(no description)".to_string()
        }
    }
}

/// Collapse whitespace and clip to ~200 chars for a single-line summary.
fn truncate_summary(s: &str) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= 200 {
        collapsed
    } else {
        let mut out: String = collapsed.chars().take(200).collect();
        out.push('…');
        out
    }
}

/// Strip a leading YAML frontmatter block (`---\n...\n---\n`) from a Markdown
/// skill file. Mirrors the helper in `skills::mod` but is duplicated here to
/// avoid leaking a private module item across the crate boundary.
fn strip_skill_frontmatter(raw: &str) -> &str {
    if !raw.starts_with("---") {
        return raw;
    }
    let after_first = match raw.find("---\n") {
        Some(p) => &raw[p + 4..],
        None => return raw,
    };
    match after_first.find("\n---\n") {
        Some(p) => &after_first[p + 5..],
        None => raw,
    }
}
