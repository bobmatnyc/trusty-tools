//! AI co-authorship attribution from commit message trailers.
//!
//! Why: engineering teams are increasingly using AI coding assistants
//! (Claude, GitHub Copilot, Cursor) whose contributions appear in commits
//! via `Co-Authored-By:` trailers. Detecting these at collection time lets
//! reports measure AI adoption without requiring human annotation.
//!
//! What: a single pure function [`detect_ai_tool`] that scans commit message
//! trailers (case-insensitive `Co-Authored-By:` / `Co-authored-by:`) for
//! well-known AI tool signatures and returns a stable `&'static str`
//! identifier.
//!
//! Test: unit tests in [`tests`] at the bottom of this file. The function is
//! also covered by the extractor path (`collect::git::extractor`) which calls
//! it at INSERT time for every new commit.

use std::sync::OnceLock;

use regex::Regex;

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
}
