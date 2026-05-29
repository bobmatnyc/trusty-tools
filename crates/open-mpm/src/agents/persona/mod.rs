//! Coding-persona detection and skill-pack injection for sub-agent tasks.
//!
//! Why: Different tasks call for different coding styles — production-quality
//! engineering, pragmatic hacking, fast prototyping, or teaching-mode. Rather
//! than baking the style into every agent TOML, the PM detects a persona from
//! the task text (explicit tag first, then keyword heuristics) and injects a
//! short directive plus the matching language idiom skill into the task body
//! before forwarding it to the sub-agent. This keeps the same `engineer` /
//! `python-engineer` agent able to operate in multiple modes.
//! What: `detect_persona` returns one of `engineer | hacker | vibe-coder | novice`.
//! `strip_persona_tag` removes any explicit `[persona]` / `persona:foo` marker
//! from a task before forwarding. `language_for_agent` maps an agent name to a
//! language-idiomatic skill pack name. `assemble_task_with_context` reads the
//! persona and language skill files and prepends their bodies to the task as
//! `## Persona Directive` and `## Language Conventions` blocks.
//! Test: See unit tests in `tests`.

use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// Persona identifiers recognised by the dispatcher.
///
/// Why: Keeping these as `&'static str` constants (rather than an enum) keeps
/// the public surface trivial — callers compare strings, file lookups use the
/// same name verbatim, and adding a new persona only means adding a new file.
/// What: Four canonical names matching the skill files under
/// `.open-mpm/skills/personas/<name>.md`.
/// Test: `detect_persona_*` tests.
pub const PERSONA_ENGINEER: &str = "engineer";
pub const PERSONA_HACKER: &str = "hacker";
pub const PERSONA_VIBE_CODER: &str = "vibe-coder";
pub const PERSONA_NOVICE: &str = "novice";

/// Detect the persona from a task description.
///
/// Why: The PM should pick the right behavioural mode without the user needing
/// to be explicit, while still honouring explicit overrides when present.
/// What: Priority order:
///   1. Explicit tag: `[engineer]` / `persona:engineer` (and likewise for the
///      other three personas) wins immediately.
///   2. Keyword heuristics on the lowercased task text — see the match arms.
///   3. Default: `engineer` (the safest, production-leaning default).
/// Test: `detect_persona_explicit_tag_wins`,
/// `detect_persona_keyword_hacker`, `detect_persona_keyword_novice`,
/// `detect_persona_keyword_vibe`, `detect_persona_default_engineer`.
pub fn detect_persona(task: &str) -> &'static str {
    // 1. Explicit tag (highest priority).
    if task.contains("[engineer]") || task.contains("persona:engineer") {
        return PERSONA_ENGINEER;
    }
    if task.contains("[hacker]") || task.contains("persona:hacker") {
        return PERSONA_HACKER;
    }
    if task.contains("[vibe-coder]") || task.contains("persona:vibe") {
        return PERSONA_VIBE_CODER;
    }
    if task.contains("[novice]") || task.contains("persona:novice") {
        return PERSONA_NOVICE;
    }

    // 2. Keyword signals.
    //
    // #219: Hacker activation is now opt-in only via a tight set of explicit
    // exploratory keywords matched as WHOLE WORDS. Previously the keyword set
    // was so broad that substantive build tasks fired hacker by accident:
    //   - `scratch` matched "build X from scratch" (a perfectly normal phrase
    //     in production tasks)
    //   - `fast` matched "FastAPI"
    //   - `iterate` / `one-off` / `just make it work` are all colloquial
    //     enough to appear in real specs.
    // The result: research + QA phases got skipped on real builds, leaving
    // generated code unvalidated. The new contract: hacker only fires when
    // the task explicitly opts in via `[hacker]` / `persona:hacker` (handled
    // above) or contains one of the unambiguous exploratory keywords below
    // as a whole word: `hacker`, `quick`, `prototype`, `throwaway`, `spike`.
    // Default is `engineer` (full pipeline) — opt-out only via explicit tag.
    let task_lower = task.to_lowercase();
    if contains_word(&task_lower, "hacker")
        || contains_word(&task_lower, "quick")
        || contains_word(&task_lower, "prototype")
        || contains_word(&task_lower, "throwaway")
        || contains_word(&task_lower, "spike")
    {
        return PERSONA_HACKER;
    }
    if task_lower.contains("explain")
        || task_lower.contains("teach")
        || task_lower.contains("how do i")
        || task_lower.contains("step by step")
        || task_lower.contains("why does")
    {
        return PERSONA_NOVICE;
    }
    // #219: vibe-coder keywords also tightened to whole-word matches and the
    // most specific phrases. `iterate` and `poc` were previously triggering
    // on substantive specs; we keep them but require word-boundary matching.
    if contains_word(&task_lower, "poc")
        || task_lower.contains("just show")
        || task_lower.contains("no explanation")
    {
        return PERSONA_VIBE_CODER;
    }

    // 3. Default.
    PERSONA_ENGINEER
}

/// Whole-word substring check: returns true iff `needle` appears in `haystack`
/// surrounded by non-alphanumeric / non-underscore boundaries (or the string
/// edges).
///
/// Why: Plain `str::contains` makes "scratch" match "from scratch" (good) but
/// also makes "fast" match "FastAPI" and "spike" match "spiked" (bad). For the
/// persona heuristic we want activation only on standalone words. Implementing
/// this with byte-level checks avoids pulling in the `regex` crate just for
/// one helper.
/// What: Returns true if `needle` is found in `haystack` with each end either
/// at a string boundary or adjacent to a character that is NOT ASCII-alphanumeric
/// and NOT `_`. Both arguments are expected to already be lowercased by the caller.
/// Test: `contains_word_*` unit tests below.
fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    let mut i = 0usize;
    while i + n.len() <= h.len() {
        if &h[i..i + n.len()] == n {
            let left_ok = i == 0 || !is_word_byte(h[i - 1]);
            let right_idx = i + n.len();
            let right_ok = right_idx == h.len() || !is_word_byte(h[right_idx]);
            if left_ok && right_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

#[inline]
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Detect the persona AND the keyword or tag that triggered it.
///
/// Why: Callers that emit a user-visible warning (e.g. the workflow engine
/// logging which phases are skipped) need to say *why* the hacker persona fired
/// so users can understand that bare "script" in a CLI example won't do it.
/// What: Returns `(persona, matched)` where `matched` is a short human-readable
/// string — the explicit tag like `"[hacker]"`, or the first matching keyword
/// like `"quick script"`, or `"(default)"` when the engineer persona was chosen
/// by fallthrough.
/// Test: `detect_persona_matched_explicit_tag`,
/// `detect_persona_matched_keyword`, `detect_persona_matched_default`.
pub fn detect_persona_matched(task: &str) -> (&'static str, String) {
    // 1. Explicit tags.
    if task.contains("[engineer]") {
        return (PERSONA_ENGINEER, "[engineer]".to_string());
    }
    if task.contains("persona:engineer") {
        return (PERSONA_ENGINEER, "persona:engineer".to_string());
    }
    if task.contains("[hacker]") {
        return (PERSONA_HACKER, "[hacker]".to_string());
    }
    if task.contains("persona:hacker") {
        return (PERSONA_HACKER, "persona:hacker".to_string());
    }
    if task.contains("[vibe-coder]") {
        return (PERSONA_VIBE_CODER, "[vibe-coder]".to_string());
    }
    if task.contains("persona:vibe") {
        return (PERSONA_VIBE_CODER, "persona:vibe".to_string());
    }
    if task.contains("[novice]") {
        return (PERSONA_NOVICE, "[novice]".to_string());
    }
    if task.contains("persona:novice") {
        return (PERSONA_NOVICE, "persona:novice".to_string());
    }

    // 2. Keyword heuristics (same order as detect_persona). #219: tightened
    // to whole-word matching for hacker keywords so "from scratch", "FastAPI",
    // and similar phrases don't trigger spurious phase-skipping.
    let task_lower = task.to_lowercase();

    for kw in &["hacker", "quick", "prototype", "throwaway", "spike"] {
        if contains_word(&task_lower, kw) {
            return (PERSONA_HACKER, (*kw).to_string());
        }
    }
    for kw in &["explain", "teach", "how do i", "step by step", "why does"] {
        if task_lower.contains(kw) {
            return (PERSONA_NOVICE, (*kw).to_string());
        }
    }
    if contains_word(&task_lower, "poc") {
        return (PERSONA_VIBE_CODER, "poc".to_string());
    }
    for kw in &["just show", "no explanation"] {
        if task_lower.contains(kw) {
            return (PERSONA_VIBE_CODER, (*kw).to_string());
        }
    }

    (PERSONA_ENGINEER, "(default)".to_string())
}

/// Remove any `[persona-tag]` or `persona:foo` marker from the task text.
///
/// Why: The marker is a signal to the PM only; forwarding it to the sub-agent
/// is noise. Stripping it keeps the sub-agent's view of the task clean.
/// What: Removes any of the eight known explicit forms (with surrounding
/// whitespace collapsed). Non-matching tasks are returned trimmed.
/// Test: `strip_persona_tag_removes_bracketed_form`,
/// `strip_persona_tag_removes_colon_form`, `strip_persona_tag_no_op`.
pub fn strip_persona_tag(task: &str) -> String {
    let markers = [
        "[engineer]",
        "[hacker]",
        "[vibe-coder]",
        "[novice]",
        "persona:engineer",
        "persona:hacker",
        "persona:vibe",
        "persona:novice",
    ];
    let mut out = task.to_string();
    for m in &markers {
        out = out.replace(m, "");
    }
    // Collapse the resulting double-spaces / leading/trailing whitespace.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Return the language-idiomatic skill name for a given agent, if any.
///
/// Why: Sub-agents are typically named for their language (`python-engineer`,
/// `gpt-engineer` for generic, `rust-engineer` etc.). Matching the agent name
/// to a language skill pack lets the PM inject the right idiom guide
/// automatically. Generic agents (no language in the name) get no skill — we
/// don't want to inject `python-idiomatic` into a `go-engineer`.
/// What: Lowercases the agent name and matches well-known language tokens.
/// Returns the bare skill name (e.g. `"python-idiomatic"`) suitable for
/// resolving under `.open-mpm/skills/languages/`.
/// Test: `language_for_agent_python`, `language_for_agent_unknown`.
pub fn language_for_agent(agent_name: &str) -> Option<&'static str> {
    let n = agent_name.to_lowercase();
    if n.contains("python") {
        return Some("python-idiomatic");
    }
    if n.contains("typescript") || n.contains("ts-") {
        return Some("typescript-idiomatic");
    }
    if n.contains("react") {
        return Some("react-idiomatic");
    }
    if n.contains("rust") {
        return Some("rust-idiomatic");
    }
    if n.contains("java") && !n.contains("javascript") {
        return Some("java-idiomatic");
    }
    if n.contains("go-") || n == "go" || n.ends_with("-go") || n.contains("golang") {
        return Some("go-idiomatic");
    }
    None
}

/// Resolve the on-disk path of a persona skill file.
///
/// Why: Centralizes path construction so callers do not need to know the
/// directory layout. Honors the `OPEN_MPM_CONFIG_DIR` env var (set by the
/// installed binary so it can find the bundled skill files anywhere).
/// What: Returns `<config_dir>/skills/personas/<name>.md`. The `config_dir`
/// is taken from `OPEN_MPM_CONFIG_DIR` if set, otherwise the CWD-relative
/// `.open-mpm/` fallback (matching `agents/mod.rs` behaviour).
/// Test: `persona_path_uses_env_override`.
pub fn persona_skill_path(name: &str) -> PathBuf {
    config_root()
        .join("skills")
        .join("personas")
        .join(format!("{name}.md"))
}

/// Resolve the on-disk path of a language-idiomatic skill file.
pub fn language_skill_path(name: &str) -> PathBuf {
    config_root()
        .join("skills")
        .join("languages")
        .join(format!("{name}.md"))
}

/// Compute the project's config root directory.
///
/// Why: The agent loader uses `OPEN_MPM_CONFIG_DIR/<name>.toml`. Skill files
/// live alongside agent files under the same root — for an installed binary
/// that means `OPEN_MPM_CONFIG_DIR=/path/to/.open-mpm/agents`, so the parent
/// of that env var is the actual config root. We support both forms: if the
/// env var ends in `/agents` we strip it; otherwise we use it verbatim.
/// What: Returns the directory holding the `skills/` subtree.
/// Test: indirect via `persona_path_uses_env_override`.
fn config_root() -> PathBuf {
    if let Ok(s) = std::env::var("OPEN_MPM_CONFIG_DIR")
        && !s.is_empty()
    {
        let p = PathBuf::from(&s);
        // The env var canonically points at `<root>/agents`; the skills tree
        // is a sibling. Strip the trailing `agents` segment if present.
        if p.file_name().and_then(|n| n.to_str()) == Some("agents")
            && let Some(parent) = p.parent()
        {
            return parent.to_path_buf();
        }
        return p;
    }
    PathBuf::from(".open-mpm")
}

/// Strip a leading YAML frontmatter block (if any) and return the body.
///
/// Why: Persona/language skill files carry `---` frontmatter for indexing.
/// Injecting that into a task prompt is noise. Mirrors the helper in
/// `crate::skills::strip_frontmatter` but kept local to avoid a circular
/// dep on the broader skills module from this small helper.
/// What: If `content` starts with `---\n`, finds the next `\n---` and returns
/// the text after it (skipping the trailing newline). Otherwise returns
/// `content` unchanged.
/// Test: `strip_frontmatter_local_removes_block`,
/// `strip_frontmatter_local_passthrough`.
fn strip_frontmatter(content: &str) -> &str {
    let Some(rest) = content.strip_prefix("---") else {
        return content;
    };
    let Some(rest) = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
    else {
        return content;
    };
    match rest.find("\n---") {
        Some(idx) => {
            let after = &rest[idx + 4..];
            after
                .strip_prefix('\n')
                .or_else(|| after.strip_prefix("\r\n"))
                .unwrap_or(after)
        }
        None => content,
    }
}

/// Read a skill file from disk and return its body (frontmatter stripped).
///
/// Why: Sync IO is acceptable here because `assemble_task_with_context` is
/// called once per delegation, on a hot but not bursty path. Using sync `fs`
/// keeps the call-site simple and avoids an `async` infection up to
/// `DelegateToAgentTool::execute`.
/// What: Returns `Some(body)` on successful read; `None` on missing file or
/// IO error (logged at debug, since missing skill files are valid — they just
/// degrade injection to "directive only").
/// Test: indirect via `assemble_task_*` tests using temp dirs.
fn read_skill_body(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Some(strip_frontmatter(&raw).trim().to_string()),
        Err(e) => {
            tracing::debug!(
                path = %path.display(),
                error = %e,
                "persona: skill file not readable; skipping injection"
            );
            None
        }
    }
}

/// Build the final task string forwarded to a sub-agent.
///
/// Why: Encapsulates the layered injection so callers (today: `delegate_to_agent`
/// tool) hand the runner a single string. Strips any persona tag from the
/// user-visible task, then prepends a `## Language Conventions` block (when an
/// idiomatic skill matches the agent name) and a `## Persona Directive` block.
/// Persona content comes AFTER the language block so persona overrides the
/// language defaults (e.g. `hacker` says "no docstrings" — that should win
/// over the language pack's "use docstrings" directive).
/// What: Returns the concatenated string. If neither persona nor language
/// content is available the task is returned with the tag stripped only.
/// Test: `assemble_task_with_context_includes_both_blocks`,
/// `assemble_task_with_context_strips_tag`,
/// `assemble_task_with_context_no_language_for_unknown_agent`.
pub fn assemble_task_with_context(agent_name: &str, raw_task: &str) -> String {
    let persona = detect_persona(raw_task);
    let stripped = strip_persona_tag(raw_task);

    let mut sections: Vec<String> = Vec::new();

    // Language conventions first (general defaults for the language).
    if let Some(lang) = language_for_agent(agent_name) {
        let lang_path = language_skill_path(lang);
        if let Some(body) = read_skill_body(&lang_path) {
            sections.push(format!("## Language Conventions\n\n{body}"));
        }
    }

    // Persona directive second (overrides language defaults).
    let persona_path = persona_skill_path(persona);
    if let Some(body) = read_skill_body(&persona_path) {
        sections.push(format!("## Persona Directive\n\n{body}"));
    }

    if sections.is_empty() {
        return stripped;
    }

    sections.push(format!("## Task\n\n{stripped}"));
    sections.join("\n\n---\n\n")
}
