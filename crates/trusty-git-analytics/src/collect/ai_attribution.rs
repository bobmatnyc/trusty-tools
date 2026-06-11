//! AI co-authorship attribution from commit message trailers.
//!
//! Why: engineering teams are increasingly using AI coding assistants
//! (Claude, GitHub Copilot, Cursor) whose contributions appear in commits
//! via `Co-Authored-By:` trailers. Detecting these at collection time lets
//! reports measure AI adoption without requiring human annotation.
//!
//! What: two pure functions:
//! - [`detect_ai_tool`] — returns the stable tool identifier string used by
//!   the existing `ai_tool` column (unchanged for backward compatibility).
//! - [`detect_agentic_mode`] — returns a canonical [`AgenticMode`] that
//!   distinguishes full-agentic CLI tools (Claude Code) from IDE-assisted
//!   tools (Cursor, Copilot inline) from plain human commits (issue #1113).
//!
//! Test: unit tests in [`tests`] at the bottom of this file. Both functions
//! are also covered by the extractor path (`collect::git::extractor`) which
//! calls them at INSERT time for every new commit.

use std::sync::OnceLock;

use regex::Regex;

/// Canonical agentic-mode classification for a commit (issue #1113).
///
/// Why: the binary `is_ai_assisted` flag and the tool-string `ai_tool`
/// column conflate very different working modes — a Claude Code commit
/// (autonomous CLI agent) is qualitatively different from a Cursor
/// inline-completion commit. Downstream analytics (DAAU, agentic %)
/// need to distinguish these modes without losing the existing columns.
/// What: three-valued enum, persisted as the TEXT column `agentic_mode`.
/// Test: `tests::detect_agentic_mode_*` below; see also
/// `core::db::migrations::v21` which adds the column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgenticMode {
    /// Full-agentic: autonomous CLI tool (e.g. Claude Code). Signals:
    /// `Co-Authored-By: Claude…`, `Generated with Claude Code` in message
    /// body, `X-AI-Tokens-In/Out` / `X-AI-Model` trailers (commit_cost_tracker),
    /// or `ai_tool == "claude"` (the existing detection path maps these already).
    FullAgentic,
    /// IDE-assisted: inline AI completions from an IDE plugin
    /// (Cursor, GitHub Copilot). Signals: `ai_tool` in {"cursor", "copilot"}.
    IdeAssisted,
    /// Plain human commit with no detectable AI involvement.
    None,
}

impl AgenticMode {
    /// Stable DB string used in the `agentic_mode` TEXT column.
    ///
    /// Why: the column stores a TEXT value so SQL queries can filter on it
    /// without JOIN'ing an enum table.
    /// What: maps each variant to its canonical string per the issue spec.
    /// Test: `tests::agentic_mode_as_str` checks the round-trip.
    pub fn as_str(self) -> &'static str {
        match self {
            AgenticMode::FullAgentic => "full_agentic",
            AgenticMode::IdeAssisted => "ide_assisted",
            AgenticMode::None => "none",
        }
    }
}

/// Compiled AI-tool detection patterns.
struct AiPatterns {
    /// Matches the full `Co-Authored-By:` or `Co-authored-by:` trailer line.
    trailer_line: Regex,
    /// Matches "claude" (Anthropic Claude assistant).
    claude: Regex,
    /// Matches "github copilot" (GitHub Copilot assistant).
    copilot: Regex,
    /// Matches "cursor" (Cursor AI assistant).
    cursor: Regex,
    /// Matches "Generated with Claude Code" in commit body (issue #1113).
    generated_with_claude_code: Regex,
    /// Matches `X-AI-Tokens-In:` or `X-AI-Tokens-Out:` trailer (commit_cost_tracker).
    x_ai_tokens: Regex,
    /// Matches `X-AI-Model:` trailer (commit_cost_tracker).
    x_ai_model: Regex,
}

/// Global, lazily-initialized pattern set.
///
/// Why: `OnceLock` gives thread-safe one-time initialisation without a
/// global mutex on every call.
/// What: compiles the regexes once and reuses them for the lifetime of the
/// process.
/// Test: `tests::ai_patterns_compile` forces initialisation.
fn ai_patterns() -> &'static AiPatterns {
    static PATTERNS: OnceLock<AiPatterns> = OnceLock::new();
    PATTERNS.get_or_init(|| AiPatterns {
        // Capture the content after the trailer key (case-insensitive key).
        trailer_line: Regex::new(r"(?im)^[Cc]o-[Aa]uthored-[Bb]y:\s*(.+)$")
            .expect("trailer_line pattern compiles"),
        claude: Regex::new(r"(?i)\bclaude\b").expect("claude pattern compiles"),
        copilot: Regex::new(r"(?i)\bcopilot\b|GitHub\s+Copilot").expect("copilot pattern compiles"),
        cursor: Regex::new(r"(?i)\bcursor\b").expect("cursor pattern compiles"),
        // "Generated with Claude Code" may appear anywhere in the message body
        // (e.g. inside a Markdown link that Claude Code appends to PR descriptions
        // or commit messages via its --message template). Case-insensitive.
        generated_with_claude_code: Regex::new(r"(?i)Generated\s+with\s+Claude\s+Code")
            .expect("generated_with_claude_code pattern compiles"),
        // commit_cost_tracker writes X-AI-Tokens-In and X-AI-Tokens-Out trailers.
        x_ai_tokens: Regex::new(r"(?im)^X-AI-Tokens-(?:In|Out):\s*\d")
            .expect("x_ai_tokens pattern compiles"),
        // commit_cost_tracker also writes an X-AI-Model trailer.
        x_ai_model: Regex::new(r"(?im)^X-AI-Model:\s*\S").expect("x_ai_model pattern compiles"),
    })
}

/// Detect the AI tool that co-authored a commit from its message.
///
/// Why: `commits.ai_tool` and `commits.is_ai_assisted` must be populated at
/// collection time (issue #445). This function provides the detection logic
/// shared between the initial `tga collect` INSERT and the retroactive
/// `tga backfill ai-detection-commits` path.
/// What: scans all `Co-Authored-By:` / `Co-authored-by:` trailer lines in
/// `message` for the signatures of known AI tools. Returns the first match
/// as a stable `&'static str` identifier, or `None` if no known AI trailer
/// is present. Priority order: Claude → Copilot → Cursor.
/// Test: `tests::detect_ai_tool_*` below.
///
/// # Stable identifiers
///
/// | Detected tool     | Returned string |
/// |-------------------|-----------------|
/// | Anthropic Claude  | `"claude"`      |
/// | GitHub Copilot    | `"copilot"`     |
/// | Cursor            | `"cursor"`      |
///
/// # Examples
///
/// ```
/// use tga::collect::ai_attribution::detect_ai_tool;
///
/// let msg = "feat: add auth\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>";
/// assert_eq!(detect_ai_tool(msg), Some("claude"));
///
/// let human = "feat: add auth\n\nCo-Authored-By: Alice <alice@example.com>";
/// assert_eq!(detect_ai_tool(human), None);
/// ```
pub fn detect_ai_tool(message: &str) -> Option<&'static str> {
    let p = ai_patterns();

    for caps in p.trailer_line.captures_iter(message) {
        let trailer_value = caps.get(1).map(|m| m.as_str()).unwrap_or("");

        if p.claude.is_match(trailer_value) {
            return Some("claude");
        }
        if p.copilot.is_match(trailer_value) {
            return Some("copilot");
        }
        if p.cursor.is_match(trailer_value) {
            return Some("cursor");
        }
    }

    None
}

/// Classify a commit into one of the three canonical agentic modes.
///
/// Why: distinguishes autonomous CLI-agent commits (Claude Code) from IDE
/// inline-completion commits (Cursor/Copilot) from plain human commits
/// (issue #1113). This finer granularity is needed for DAAU and agentic-%
/// analytics that the binary `is_ai_assisted` flag cannot express.
/// What: applies a deterministic, trailer-based classification. Signals
/// checked in priority order:
///
/// 1. `Co-Authored-By: Claude…` — full_agentic (Claude Code CLI pattern)
/// 2. `Generated with Claude Code` anywhere in the message — full_agentic
/// 3. `X-AI-Tokens-In/Out:` or `X-AI-Model:` trailers — full_agentic
///    (written by commit_cost_tracker when Claude Code is used)
/// 4. `Co-Authored-By: copilot/cursor…` — ide_assisted
/// 5. No recognised AI signal — none
///
/// Test: `tests::detect_agentic_mode_*` below.
///
/// # Examples
///
/// ```
/// use tga::collect::ai_attribution::{detect_agentic_mode, AgenticMode};
///
/// let msg = "feat: add auth\n\nCo-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>";
/// assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
///
/// let ide = "fix: npe\n\nCo-Authored-By: Cursor <noreply@cursor.sh>";
/// assert_eq!(detect_agentic_mode(ide), AgenticMode::IdeAssisted);
///
/// let human = "chore: bump dep";
/// assert_eq!(detect_agentic_mode(human), AgenticMode::None);
/// ```
pub fn detect_agentic_mode(message: &str) -> AgenticMode {
    let p = ai_patterns();

    // Signal 1 & 4: Co-Authored-By trailers.
    // Check all trailer lines; Claude wins over Copilot/Cursor if both present.
    let mut has_ide = false;
    for caps in p.trailer_line.captures_iter(message) {
        let trailer_value = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        if p.claude.is_match(trailer_value) {
            return AgenticMode::FullAgentic;
        }
        if p.copilot.is_match(trailer_value) || p.cursor.is_match(trailer_value) {
            has_ide = true;
        }
    }

    // Signal 2: "Generated with Claude Code" anywhere in the message body.
    if p.generated_with_claude_code.is_match(message) {
        return AgenticMode::FullAgentic;
    }

    // Signal 3: X-AI-* trailers written by commit_cost_tracker.
    if p.x_ai_tokens.is_match(message) || p.x_ai_model.is_match(message) {
        return AgenticMode::FullAgentic;
    }

    // Signal 4 conclusion: only IDE-assisted signals found.
    if has_ide {
        return AgenticMode::IdeAssisted;
    }

    AgenticMode::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_patterns_compile() {
        // Force lazy init; any bad pattern literal panics here, not at runtime.
        let _ = ai_patterns();
    }

    /// Why: Claude is the primary AI tool in this codebase; must be detected.
    /// What: message with a Claude co-author trailer returns `"claude"`.
    /// Test: this test itself.
    #[test]
    fn detect_ai_tool_detects_claude() {
        let msg =
            "feat: add auth\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>";
        assert_eq!(detect_ai_tool(msg), Some("claude"));
    }

    /// Why: case-insensitive trailer key must be accepted.
    /// What: lowercase `co-authored-by:` is recognised.
    /// Test: this test itself.
    #[test]
    fn detect_ai_tool_case_insensitive_key() {
        let msg = "fix: bug\n\nco-authored-by: Claude Sonnet 4 <noreply@anthropic.com>";
        assert_eq!(detect_ai_tool(msg), Some("claude"));
    }

    /// Why: Copilot must be detected by keyword.
    /// What: `"GitHub Copilot"` in trailer value returns `"copilot"`.
    /// Test: this test itself.
    #[test]
    fn detect_ai_tool_detects_copilot() {
        let msg = "feat: autocomplete\n\nCo-Authored-By: GitHub Copilot <copilot@github.com>";
        assert_eq!(detect_ai_tool(msg), Some("copilot"));
    }

    /// Why: Copilot detection must also match just "copilot" (bare keyword).
    /// What: `"copilot"` anywhere in the trailer value returns `"copilot"`.
    /// Test: this test itself.
    #[test]
    fn detect_ai_tool_detects_copilot_bare() {
        let msg = "fix: npe\n\nCo-Authored-By: copilot <noreply@github.com>";
        assert_eq!(detect_ai_tool(msg), Some("copilot"));
    }

    /// Why: Cursor must be detected by keyword.
    /// What: `"Cursor"` in trailer value returns `"cursor"`.
    /// Test: this test itself.
    #[test]
    fn detect_ai_tool_detects_cursor() {
        let msg = "chore: refactor\n\nCo-Authored-By: Cursor <noreply@cursor.sh>";
        assert_eq!(detect_ai_tool(msg), Some("cursor"));
    }

    /// Why: human co-authors must not be detected as AI.
    /// What: ordinary `Co-Authored-By:` with a human name returns `None`.
    /// Test: this test itself.
    #[test]
    fn detect_ai_tool_returns_none_for_human() {
        let msg = "feat: auth\n\nCo-Authored-By: Alice Smith <alice@example.com>";
        assert_eq!(detect_ai_tool(msg), None);
    }

    /// Why: commits without any trailer must return `None`.
    /// What: plain commit message with no `Co-Authored-By:` returns `None`.
    /// Test: this test itself.
    #[test]
    fn detect_ai_tool_returns_none_for_no_trailer() {
        assert_eq!(detect_ai_tool("feat: add feature"), None);
        assert_eq!(detect_ai_tool(""), None);
    }

    /// Why: multiple trailers — Claude takes priority over Copilot in the
    /// priority order (Claude → Copilot → Cursor).
    /// What: message with both Claude and Copilot trailers returns `"claude"`.
    /// Test: this test itself.
    #[test]
    fn detect_ai_tool_priority_claude_before_copilot() {
        let msg = "pair session\n\n\
                   Co-Authored-By: Claude Opus <noreply@anthropic.com>\n\
                   Co-Authored-By: GitHub Copilot <copilot@github.com>";
        assert_eq!(detect_ai_tool(msg), Some("claude"));
    }

    /// Why: priority order — Copilot before Cursor when both present.
    /// What: Copilot trailer appears before Cursor; returns `"copilot"`.
    /// Test: this test itself.
    #[test]
    fn detect_ai_tool_priority_copilot_before_cursor() {
        let msg = "pair session\n\n\
                   Co-Authored-By: GitHub Copilot <copilot@github.com>\n\
                   Co-Authored-By: Cursor <noreply@cursor.sh>";
        assert_eq!(detect_ai_tool(msg), Some("copilot"));
    }

    // -------------------------------------------------------------------------
    // Issue #1113 — AgenticMode tests
    // -------------------------------------------------------------------------

    /// Why: `as_str` must return stable string constants for DB persistence.
    /// What: checks all three variants against the spec values.
    /// Test: this test itself.
    #[test]
    fn agentic_mode_as_str() {
        assert_eq!(AgenticMode::FullAgentic.as_str(), "full_agentic");
        assert_eq!(AgenticMode::IdeAssisted.as_str(), "ide_assisted");
        assert_eq!(AgenticMode::None.as_str(), "none");
    }

    /// Why: Claude Co-Authored-By trailer → full_agentic (primary signal).
    /// What: a standard Claude Code commit message classifies as FullAgentic.
    /// Test: this test itself.
    #[test]
    fn detect_agentic_mode_claude_coauthor_is_full_agentic() {
        let msg = "feat: add feature\n\n\
                   Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
    }

    /// Why: "Generated with Claude Code" body signal → full_agentic.
    /// What: the phrase anywhere in the message marks this as full-agentic
    ///   even without a Co-Authored-By trailer.
    /// Test: this test itself.
    #[test]
    fn detect_agentic_mode_generated_with_claude_code_is_full_agentic() {
        let msg = "fix: resolve timeout\n\n\
                   🤖 Generated with [Claude Code](https://claude.ai/claude-code)\n\
                   Co-Authored-By: Claude Sonnet 4 <noreply@anthropic.com>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
    }

    /// Why: "Generated with Claude Code" alone (no co-author) → full_agentic.
    /// What: body signal without any trailer still classifies correctly.
    /// Test: this test itself.
    #[test]
    fn detect_agentic_mode_generated_body_only_is_full_agentic() {
        let msg = "chore: update deps\n\nGenerated with Claude Code";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
    }

    /// Why: X-AI-Tokens trailers written by commit_cost_tracker → full_agentic.
    /// What: presence of X-AI-Tokens-In or X-AI-Tokens-Out classifies the
    ///   commit as full-agentic regardless of other signals.
    /// Test: this test itself.
    #[test]
    fn detect_agentic_mode_x_ai_tokens_is_full_agentic() {
        let msg = "feat: implement search\n\n\
                   X-AI-Tokens-In: 1234\n\
                   X-AI-Tokens-Out: 5678";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
    }

    /// Why: X-AI-Model trailer → full_agentic.
    /// What: the model trailer alone marks the commit as full-agentic.
    /// Test: this test itself.
    #[test]
    fn detect_agentic_mode_x_ai_model_is_full_agentic() {
        let msg = "refactor: extract helper\n\nX-AI-Model: claude-sonnet-4-6";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
    }

    /// Why: Cursor Co-Authored-By → ide_assisted.
    /// What: Cursor inline-completion commits classify as IdeAssisted.
    /// Test: this test itself.
    #[test]
    fn detect_agentic_mode_cursor_is_ide_assisted() {
        let msg = "fix: null check\n\nCo-Authored-By: Cursor <noreply@cursor.sh>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::IdeAssisted);
    }

    /// Why: GitHub Copilot Co-Authored-By → ide_assisted.
    /// What: Copilot inline-completion commits classify as IdeAssisted.
    /// Test: this test itself.
    #[test]
    fn detect_agentic_mode_copilot_is_ide_assisted() {
        let msg = "feat: autocomplete\n\nCo-Authored-By: GitHub Copilot <copilot@github.com>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::IdeAssisted);
    }

    /// Why: bare copilot keyword → ide_assisted.
    /// What: `copilot` anywhere in the Co-Authored-By value classifies as IDE.
    /// Test: this test itself.
    #[test]
    fn detect_agentic_mode_copilot_bare_is_ide_assisted() {
        let msg = "fix: npe\n\nCo-Authored-By: copilot <noreply@github.com>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::IdeAssisted);
    }

    /// Why: plain human commit → none.
    /// What: a commit with no AI signals classifies as None.
    /// Test: this test itself.
    #[test]
    fn detect_agentic_mode_plain_commit_is_none() {
        assert_eq!(detect_agentic_mode("feat: add button"), AgenticMode::None);
        assert_eq!(detect_agentic_mode(""), AgenticMode::None);
    }

    /// Why: human co-author trailer must NOT classify as AI.
    /// What: Co-Authored-By with a human name is None, not ide_assisted.
    /// Test: this test itself.
    #[test]
    fn detect_agentic_mode_human_coauthor_is_none() {
        let msg = "feat: pair program\n\nCo-Authored-By: Alice Smith <alice@example.com>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::None);
    }

    /// Why: when both Claude and Cursor trailers exist, Claude (full_agentic)
    ///   wins because it is checked first in the priority order.
    /// What: mixed trailer scenario returns FullAgentic.
    /// Test: this test itself.
    #[test]
    fn detect_agentic_mode_claude_wins_over_cursor() {
        let msg = "pair: fix auth\n\n\
                   Co-Authored-By: Cursor <noreply@cursor.sh>\n\
                   Co-Authored-By: Claude Opus <noreply@anthropic.com>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
    }
}
