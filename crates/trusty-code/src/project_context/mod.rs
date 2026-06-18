//! Project-context ingestion — load `CLAUDE.md` for prompt injection (#1033).
//!
//! Why: A Claude-Code-compatible harness must give the PM and every sub-agent
//! the same project-specific rules a human Claude Code session would see — the
//! `CLAUDE.md` at the project root. The parity prompt assembler (#1032) already
//! accepts a verbatim project-context section; this module is the missing piece
//! that *reads* that section off disk, bounds its size so a runaway file cannot
//! blow the model's context budget, and degrades gracefully when no file exists.
//! What: [`load_project_context`] walks the project root for `CLAUDE.md` (and,
//! when present, the nested `.claude/CLAUDE.md` Claude-Code convention), reads
//! the first one found, caps it at [`MAX_CONTEXT_BYTES`], and — on truncation —
//! appends a provenance note so the model knows the context was clipped. A
//! missing file yields `None`, never an error.
//! Test: `project_context::tests` cover present/oversized/missing/positioning.

use std::path::{Path, PathBuf};

/// Maximum number of bytes of `CLAUDE.md` content injected into a prompt.
///
/// Why: `CLAUDE.md` can grow without bound (this very repo's is tens of KB).
/// Injecting it whole would dominate the model's context window and inflate
/// per-call cost. 16 KiB is a generous budget for project rules while keeping
/// the injected surface bounded and comparable across runs.
/// What: 16 * 1024 bytes. Content longer than this is truncated on a UTF-8
/// boundary and annotated (see [`truncate_with_note`]).
/// Test: `oversized_context_is_truncated_with_note`.
pub const MAX_CONTEXT_BYTES: usize = 16 * 1024;

/// Canonical filename for Claude-Code project context.
///
/// Why: Centralises the magic string so the root and nested lookups agree.
/// What: The literal `"CLAUDE.md"`.
/// Test: Exercised by every loader test.
const CLAUDE_MD: &str = "CLAUDE.md";

/// Load the project's `CLAUDE.md` context, size-capped and provenance-annotated.
///
/// Why: The PM and sub-agents must see the project's own rules; this is the
/// single entry point the `run-task` handler and the in-process runner use to
/// obtain the verbatim context section for `assemble_system_prompt`.
/// What: Resolves the first existing of `<root>/CLAUDE.md` then
/// `<root>/.claude/CLAUDE.md`; reads it; returns `Some(content)` capped at
/// [`MAX_CONTEXT_BYTES`] (with a truncation note appended when clipped). Returns
/// `None` when no file exists or the file cannot be read (absence is not an
/// error — a project simply may not define project context).
/// Test: `loads_root_claude_md`, `loads_nested_claude_md`,
/// `missing_file_returns_none`, `oversized_context_is_truncated_with_note`.
pub fn load_project_context(project_root: &Path) -> Option<String> {
    let path = locate_claude_md(project_root)?;
    // A read failure (permissions, races) is treated as "no context" rather than
    // a hard error: the harness must still run with just the BASE + agent prompt.
    let raw = std::fs::read_to_string(&path).ok()?;
    Some(truncate_with_note(&raw, MAX_CONTEXT_BYTES))
}

/// Locate the project `CLAUDE.md`, preferring the root then the `.claude/` nest.
///
/// Why: Claude Code reads a root `CLAUDE.md`; some projects nest it under
/// `.claude/`. Checking both (root first) matches what a human session sees
/// without forcing a particular layout.
/// What: Returns the first existing path of `<root>/CLAUDE.md` or
/// `<root>/.claude/CLAUDE.md`; `None` when neither exists.
/// Test: `loads_root_claude_md`, `loads_nested_claude_md`,
/// `missing_file_returns_none`.
fn locate_claude_md(project_root: &Path) -> Option<PathBuf> {
    let root = project_root.join(CLAUDE_MD);
    if root.is_file() {
        return Some(root);
    }
    let nested = project_root.join(".claude").join(CLAUDE_MD);
    if nested.is_file() {
        return Some(nested);
    }
    None
}

/// Truncate `content` to at most `max_bytes`, appending a provenance note when
/// clipping occurs.
///
/// Why: A bounded context must remain valid UTF-8 (the model receives a string,
/// and slicing mid-codepoint would panic) and must tell the model it is reading
/// a partial file so it does not assume the rules are complete.
/// What: If `content` is within budget, returns it unchanged. Otherwise truncates
/// at the largest UTF-8 char boundary `<= max_bytes`, then appends
/// `"\n\n[CLAUDE.md truncated to N bytes of M total]"` where `N` is the kept byte
/// count and `M` the original length.
/// Test: `oversized_context_is_truncated_with_note`,
/// `within_budget_is_returned_verbatim`, `truncation_respects_utf8_boundary`.
fn truncate_with_note(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }

    // Find the largest char boundary at or below max_bytes so the slice is valid
    // UTF-8 even when a multi-byte codepoint straddles the cap.
    let mut end = max_bytes;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }

    let kept = &content[..end];
    format!(
        "{kept}\n\n[CLAUDE.md truncated to {end} bytes of {total} total]",
        total = content.len()
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A root `CLAUDE.md` is loaded and its content returned verbatim.
    ///
    /// Why: The common case — the PM must see the project's rules from the root
    /// file exactly as written.
    /// What: Write `<root>/CLAUDE.md`; assert `load_project_context` returns its
    /// exact content.
    /// Test: this test.
    #[test]
    fn loads_root_claude_md() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("CLAUDE.md"), "PROJECT RULES: be concise.")
            .expect("write CLAUDE.md");

        let ctx = load_project_context(tmp.path()).expect("context loaded");
        assert_eq!(ctx, "PROJECT RULES: be concise.");
    }

    /// A nested `.claude/CLAUDE.md` is loaded when no root file exists.
    ///
    /// Why: Some projects nest their context; the loader must find it too.
    /// What: Write only `<root>/.claude/CLAUDE.md`; assert it is returned.
    /// Test: this test.
    #[test]
    fn loads_nested_claude_md() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nested = tmp.path().join(".claude");
        std::fs::create_dir_all(&nested).expect("mkdir .claude");
        std::fs::write(nested.join("CLAUDE.md"), "NESTED RULES").expect("write nested");

        let ctx = load_project_context(tmp.path()).expect("nested context loaded");
        assert_eq!(ctx, "NESTED RULES");
    }

    /// The root file wins over a nested one when both exist.
    ///
    /// Why: Precedence must be deterministic; a human session reads the root.
    /// What: Write both; assert the root content is returned.
    /// Test: this test.
    #[test]
    fn root_takes_precedence_over_nested() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("CLAUDE.md"), "ROOT").expect("write root");
        let nested = tmp.path().join(".claude");
        std::fs::create_dir_all(&nested).expect("mkdir .claude");
        std::fs::write(nested.join("CLAUDE.md"), "NESTED").expect("write nested");

        let ctx = load_project_context(tmp.path()).expect("loaded");
        assert_eq!(ctx, "ROOT", "root CLAUDE.md must take precedence");
    }

    /// A project with no `CLAUDE.md` yields `None` (no error).
    ///
    /// Why: Absence must be handled gracefully — the harness still runs with just
    /// the BASE + agent prompt.
    /// What: Empty tempdir; assert `None`.
    /// Test: this test.
    #[test]
    fn missing_file_returns_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(
            load_project_context(tmp.path()).is_none(),
            "missing CLAUDE.md must return None, not error"
        );
    }

    /// Content within budget is returned verbatim (no note).
    ///
    /// Why: The truncation note must only appear when clipping actually happens.
    /// What: A short file is returned byte-for-byte with no annotation.
    /// Test: this test.
    #[test]
    fn within_budget_is_returned_verbatim() {
        let small = "a".repeat(100);
        let out = truncate_with_note(&small, MAX_CONTEXT_BYTES);
        assert_eq!(out, small);
        assert!(!out.contains("truncated"), "no note for in-budget content");
    }

    /// Oversized content is truncated and annotated with a provenance note.
    ///
    /// Why: A runaway `CLAUDE.md` must not blow the context budget, and the model
    /// must know it received a partial file.
    /// What: Build content larger than a tiny cap; assert the result is within the
    /// cap-plus-note, that it carries the provenance note, and that the kept
    /// prefix matches the original.
    /// Test: this test.
    #[test]
    fn oversized_context_is_truncated_with_note() {
        let content = "x".repeat(50);
        let out = truncate_with_note(&content, 10);
        assert!(
            out.starts_with(&"x".repeat(10)),
            "kept prefix must be the first 10 bytes"
        );
        assert!(
            out.contains("[CLAUDE.md truncated to 10 bytes of 50 total]"),
            "must carry the provenance note, got: {out}"
        );
    }

    /// Truncation never splits a multi-byte UTF-8 codepoint.
    ///
    /// Why: Slicing a `&str` mid-codepoint panics; the cap must back off to a
    /// char boundary so any content (incl. emoji / accented text) is safe.
    /// What: Content of multi-byte chars truncated at a cap that lands mid-char;
    /// assert the result is valid UTF-8 (it is, by construction) and the kept
    /// length is `<= cap`.
    /// Test: this test.
    #[test]
    fn truncation_respects_utf8_boundary() {
        // "é" is 2 bytes in UTF-8; a cap of 5 lands mid-char on the 3rd "é".
        let content = "é".repeat(10); // 20 bytes
        let out = truncate_with_note(&content, 5);
        // The kept portion before the note must be 4 bytes (two full "é"), not 5.
        let note_start = out.find("\n\n[CLAUDE.md").expect("note present");
        assert_eq!(&out[..note_start], "éé", "must back off to a char boundary");
    }

    /// Loaded context lands in the assembled prompt AFTER the agent prompt.
    ///
    /// Why: Parity-spec §2 fixes section order as BASE → agent prompt → project
    /// context. This test ties `load_project_context` to `assemble_system_prompt`
    /// to prove the injected context is positioned correctly (criterion d),
    /// not merely present.
    /// What: Build an `AgentConfig` with a distinctive agent-prompt marker; load a
    /// project `CLAUDE.md` with its own marker; assemble; assert both markers are
    /// present and the project marker appears strictly after the agent marker.
    /// Test: this test.
    #[test]
    fn loaded_context_is_positioned_after_agent_prompt() {
        use crate::agents::AgentConfig;
        use crate::agents::config::{AgentInfo, SystemPrompt};
        use crate::prompt::assemble_system_prompt;

        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("CLAUDE.md"), "PROJECT-CONTEXT-MARKER").expect("write");
        let ctx = load_project_context(tmp.path()).expect("loaded");

        let config = AgentConfig {
            agent: AgentInfo {
                name: "pm".into(),
                ..Default::default()
            },
            system_prompt: SystemPrompt {
                content: "AGENT-PROMPT-MARKER".into(),
                ..Default::default()
            },
            ..Default::default()
        };

        let prompt = assemble_system_prompt(&config, Some(&ctx), None);
        let agent_at = prompt
            .find("AGENT-PROMPT-MARKER")
            .expect("agent marker present");
        let ctx_at = prompt
            .find("PROJECT-CONTEXT-MARKER")
            .expect("project context marker present");
        assert!(
            ctx_at > agent_at,
            "project context must be positioned after the agent prompt"
        );
    }

    /// `load_project_context` applies the byte cap end-to-end through the loader.
    ///
    /// Why: Guards the integration of file read + truncation, not just the helper.
    /// What: Write a file larger than `MAX_CONTEXT_BYTES`; assert the loaded
    /// context carries the truncation note and the kept body is within budget.
    /// Test: this test.
    #[test]
    fn loader_enforces_byte_cap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let big = "z".repeat(MAX_CONTEXT_BYTES + 4096);
        std::fs::write(tmp.path().join("CLAUDE.md"), &big).expect("write big");

        let ctx = load_project_context(tmp.path()).expect("loaded");
        assert!(
            ctx.contains("[CLAUDE.md truncated to"),
            "oversized file must be truncated with a note"
        );
        let note_start = ctx.find("\n\n[CLAUDE.md").expect("note present");
        assert!(
            note_start <= MAX_CONTEXT_BYTES,
            "kept body must be within the byte cap"
        );
    }
}
