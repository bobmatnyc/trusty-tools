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

impl std::str::FromStr for AgenticMode {
    type Err = ();

    /// Why: centralises the string↔enum mapping so callers use the same
    /// strings as `as_str()` without a hand-rolled `match`. Unknown → `Err(())`.
    /// What: inverse of `as_str()`; unrecognised strings return `Err(())`.
    /// Test: `tests::agentic_mode_from_str_round_trips`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "full_agentic" => Ok(AgenticMode::FullAgentic),
            "ide_assisted" => Ok(AgenticMode::IdeAssisted),
            "none" => Ok(AgenticMode::None),
            _ => Err(()),
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
    /// Matches Cursor AI assistant by email domain (`@cursor.sh`) or standalone
    /// tool name (`\bCursor\b`). The hyphen-suffix false-positive guard (e.g.
    /// "Alice Cursor-Williams") is applied in [`is_cursor_match`], not here.
    cursor: Regex,
    /// Matches "Generated with Claude Code" in commit body (issue #1113).
    generated_with_claude_code: Regex,
    /// Matches `X-AI-Tokens-In:` or `X-AI-Tokens-Out:` trailer (commit_cost_tracker).
    x_ai_tokens: Regex,
    /// Matches `X-AI-Model:` trailer (commit_cost_tracker).
    x_ai_model: Regex,
}

/// Why: `(?i)\bCursor\b` alone would match "Alice Cursor-Williams" (false
/// positive); Rust's `regex` crate has no lookahead, so the guard is code-level.
/// What: returns `true` when the cursor pattern fires AND `m.as_str()` contains
/// `@` (email-domain form) OR the match is NOT followed by `-` (word form,
/// rejects hyphenated surnames like "Cursor-Williams").
/// Test: `tests::detect_agentic_mode_cursor_in_human_name_is_not_ide_assisted`
/// and `tests::is_cursor_match_email_domain_form`.
fn is_cursor_match(p: &AiPatterns, trailer_value: &str) -> bool {
    if let Some(m) = p.cursor.find(trailer_value) {
        if m.as_str().contains('@') {
            return true; // email-domain form: @cursor.sh
        }
        let after = trailer_value.get(m.end()..).unwrap_or("");
        !after.starts_with('-') // word form: reject hyphenated surnames
    } else {
        false
    }
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
        // Match Cursor by either the canonical email domain OR the standalone
        // tool name. The word-boundary `\bCursor\b` alone would also match
        // human surnames like "Alice Cursor-Williams"; the hyphen guard is
        // enforced in the calling code (see `is_cursor_match`) since Rust's
        // regex crate does not support lookahead assertions.
        cursor: Regex::new(r"(?i)@cursor\.sh|\bCursor\b").expect("cursor pattern compiles"),
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
        if is_cursor_match(p, trailer_value) {
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
        if p.copilot.is_match(trailer_value) || is_cursor_match(p, trailer_value) {
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

    /// Why: Claude is the primary AI tool; must be detected.
    /// What: Claude co-author trailer → `"claude"`.
    #[test]
    fn detect_ai_tool_detects_claude() {
        let msg =
            "feat: add auth\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>";
        assert_eq!(detect_ai_tool(msg), Some("claude"));
    }

    /// Why: case-insensitive trailer key must be accepted.
    /// What: lowercase `co-authored-by:` → `"claude"`.
    #[test]
    fn detect_ai_tool_case_insensitive_key() {
        let msg = "fix: bug\n\nco-authored-by: Claude Sonnet 4 <noreply@anthropic.com>";
        assert_eq!(detect_ai_tool(msg), Some("claude"));
    }

    /// Why: Copilot must be detected by keyword.
    /// What: `"GitHub Copilot"` trailer → `"copilot"`.
    #[test]
    fn detect_ai_tool_detects_copilot() {
        let msg = "feat: autocomplete\n\nCo-Authored-By: GitHub Copilot <copilot@github.com>";
        assert_eq!(detect_ai_tool(msg), Some("copilot"));
    }

    /// Why: bare "copilot" keyword must also be detected.
    /// What: `"copilot"` trailer → `"copilot"`.
    #[test]
    fn detect_ai_tool_detects_copilot_bare() {
        let msg = "fix: npe\n\nCo-Authored-By: copilot <noreply@github.com>";
        assert_eq!(detect_ai_tool(msg), Some("copilot"));
    }

    /// Why: Cursor tool must be detected.
    /// What: `"Cursor"` trailer → `"cursor"`.
    #[test]
    fn detect_ai_tool_detects_cursor() {
        let msg = "chore: refactor\n\nCo-Authored-By: Cursor <noreply@cursor.sh>";
        assert_eq!(detect_ai_tool(msg), Some("cursor"));
    }

    /// Why: human co-authors must not be detected as AI.
    /// What: human `Co-Authored-By:` → `None`.
    #[test]
    fn detect_ai_tool_returns_none_for_human() {
        let msg = "feat: auth\n\nCo-Authored-By: Alice Smith <alice@example.com>";
        assert_eq!(detect_ai_tool(msg), None);
    }

    /// Why: no trailer → no AI tool.
    /// What: plain message with no `Co-Authored-By:` → `None`.
    #[test]
    fn detect_ai_tool_returns_none_for_no_trailer() {
        assert_eq!(detect_ai_tool("feat: add feature"), None);
        assert_eq!(detect_ai_tool(""), None);
    }

    /// Why: priority order Claude → Copilot → Cursor must be respected.
    /// What: both Claude and Copilot trailers present → `"claude"`.
    #[test]
    fn detect_ai_tool_priority_claude_before_copilot() {
        let msg = "pair session\n\n\
                   Co-Authored-By: Claude Opus <noreply@anthropic.com>\n\
                   Co-Authored-By: GitHub Copilot <copilot@github.com>";
        assert_eq!(detect_ai_tool(msg), Some("claude"));
    }

    /// Why: Copilot before Cursor in priority order.
    /// What: both Copilot and Cursor present → `"copilot"`.
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

    /// Why: stable strings needed for DB persistence and SQL filtering.
    /// What: all three variants map to their spec strings.
    #[test]
    fn agentic_mode_as_str() {
        assert_eq!(AgenticMode::FullAgentic.as_str(), "full_agentic");
        assert_eq!(AgenticMode::IdeAssisted.as_str(), "ide_assisted");
        assert_eq!(AgenticMode::None.as_str(), "none");
    }

    /// Why: `FromStr` must invert `as_str` for lossless DB round-trips.
    /// What: parses all canonical strings; unknown string → `Err`.
    #[test]
    fn agentic_mode_from_str_round_trips() {
        use std::str::FromStr;
        assert_eq!(
            AgenticMode::from_str("full_agentic"),
            Ok(AgenticMode::FullAgentic)
        );
        assert_eq!(
            AgenticMode::from_str("ide_assisted"),
            Ok(AgenticMode::IdeAssisted)
        );
        assert_eq!(AgenticMode::from_str("none"), Ok(AgenticMode::None));
        assert!(AgenticMode::from_str("unknown_value").is_err());
        assert!(AgenticMode::from_str("").is_err());
    }

    /// Why: Claude Co-Authored-By is the primary full-agentic signal.
    /// What: Claude trailer → `FullAgentic`.
    #[test]
    fn detect_agentic_mode_claude_coauthor_is_full_agentic() {
        let msg = "feat: add feature\n\n\
                   Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
    }

    /// Why: "Generated with Claude Code" is a full-agentic body signal.
    /// What: phrase anywhere in message → `FullAgentic`.
    #[test]
    fn detect_agentic_mode_generated_with_claude_code_is_full_agentic() {
        let msg = "fix: resolve timeout\n\n\
                   🤖 Generated with [Claude Code](https://claude.ai/claude-code)\n\
                   Co-Authored-By: Claude Sonnet 4 <noreply@anthropic.com>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
    }

    /// Why: body signal alone (no co-author trailer) must suffice.
    /// What: no trailer, just body phrase → `FullAgentic`.
    #[test]
    fn detect_agentic_mode_generated_body_only_is_full_agentic() {
        let msg = "chore: update deps\n\nGenerated with Claude Code";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
    }

    /// Why: X-AI-Tokens trailers from commit_cost_tracker → full_agentic.
    /// What: X-AI-Tokens-In or X-AI-Tokens-Out → `FullAgentic`.
    #[test]
    fn detect_agentic_mode_x_ai_tokens_is_full_agentic() {
        let msg = "feat: implement search\n\n\
                   X-AI-Tokens-In: 1234\n\
                   X-AI-Tokens-Out: 5678";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
    }

    /// Why: X-AI-Model trailer alone must trigger full_agentic.
    /// What: X-AI-Model present → `FullAgentic`.
    #[test]
    fn detect_agentic_mode_x_ai_model_is_full_agentic() {
        let msg = "refactor: extract helper\n\nX-AI-Model: claude-sonnet-4-6";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
    }

    /// Why: Cursor IDE trailer must be ide_assisted, not full_agentic.
    /// What: Cursor `Co-Authored-By` → `IdeAssisted`.
    #[test]
    fn detect_agentic_mode_cursor_is_ide_assisted() {
        let msg = "fix: null check\n\nCo-Authored-By: Cursor <noreply@cursor.sh>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::IdeAssisted);
    }

    /// Why: Copilot IDE trailer must be ide_assisted, not full_agentic.
    /// What: Copilot `Co-Authored-By` → `IdeAssisted`.
    #[test]
    fn detect_agentic_mode_copilot_is_ide_assisted() {
        let msg = "feat: autocomplete\n\nCo-Authored-By: GitHub Copilot <copilot@github.com>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::IdeAssisted);
    }

    /// Why: bare "copilot" keyword must also classify as ide_assisted.
    /// What: `copilot` trailer → `IdeAssisted`.
    #[test]
    fn detect_agentic_mode_copilot_bare_is_ide_assisted() {
        let msg = "fix: npe\n\nCo-Authored-By: copilot <noreply@github.com>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::IdeAssisted);
    }

    /// Why: no AI signals must yield None (not a false positive).
    /// What: plain commit → `None`.
    #[test]
    fn detect_agentic_mode_plain_commit_is_none() {
        assert_eq!(detect_agentic_mode("feat: add button"), AgenticMode::None);
        assert_eq!(detect_agentic_mode(""), AgenticMode::None);
    }

    /// Why: human co-author trailer must not trigger any AI classification.
    /// What: human `Co-Authored-By` → `None`.
    #[test]
    fn detect_agentic_mode_human_coauthor_is_none() {
        let msg = "feat: pair program\n\nCo-Authored-By: Alice Smith <alice@example.com>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::None);
    }

    /// Why: hyphen guard must reject "Cursor" in a surname like "Cursor-Williams".
    /// What: "Cursor" followed by `-` in a trailer → None, not ide_assisted.
    #[test]
    fn detect_agentic_mode_cursor_in_human_name_is_not_ide_assisted() {
        let msg = "feat: auth\n\nCo-Authored-By: Alice Cursor-Williams <alice@example.com>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::None);
        assert_eq!(detect_ai_tool(msg), None);
    }

    /// Why/What: `m.as_str().contains('@')` guard accepts `@cursor.sh` even
    /// when no word `Cursor` precedes it → `IdeAssisted` / `"cursor"`.
    #[test]
    fn is_cursor_match_email_domain_form() {
        let msg = "fix: npe\n\nCo-Authored-By: AI Bot <ai@cursor.sh>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::IdeAssisted);
        assert_eq!(detect_ai_tool(msg), Some("cursor"));
    }

    /// Why: Claude must win over Cursor when both trailers present.
    /// What: Claude + Cursor trailers → `FullAgentic`.
    #[test]
    fn detect_agentic_mode_claude_wins_over_cursor() {
        let msg = "pair: fix auth\n\n\
                   Co-Authored-By: Cursor <noreply@cursor.sh>\n\
                   Co-Authored-By: Claude Opus <noreply@anthropic.com>";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
    }
}
