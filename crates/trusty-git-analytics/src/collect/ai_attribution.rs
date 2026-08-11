//! AI co-authorship attribution from commit messages and identities.
//!
//! Why: engineering teams are increasingly using AI coding assistants
//! (Claude, GitHub Copilot, Cursor, Devin, OpenHands, Aider) whose
//! contributions appear in commits via trailers, footers, or bot identities.
//! Detecting these at collection time lets reports measure AI adoption without
//! requiring human annotation.
//!
//! What: the [`AgenticMode`] classification, plus two message-only convenience
//! functions over the shipped marker set:
//! - [`detect_ai_tool`] — the stable tool identifier for the `ai_tool` column.
//! - [`detect_agentic_mode`] — the canonical [`AgenticMode`] (issue #1113).
//!
//! Since #5249 the patterns themselves live in
//! [`crate::collect::ai_markers`], which is configurable per run and also
//! matches author/committer emails. Callers that have a commit's identities —
//! `collect::git::extractor` and the backfill path — go through
//! [`detect`](crate::collect::ai_markers::detect)
//! directly; these two functions cover message-only callers and keep the
//! pre-#5249 signatures working.
//!
//! Test: unit tests in [`tests`] at the bottom of this file, and
//! `collect::ai_markers::tests` for the marker engine itself.

use crate::collect::ai_markers::{detect, CommitSignals};

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
    /// Full-agentic: autonomous CLI agent (Claude Code, Devin, OpenHands,
    /// Aider, or a house wrapper such as trusty-mpm). Which markers imply it
    /// is data, not code, since #5249 — see [`crate::collect::ai_markers`].
    FullAgentic,
    /// IDE-assisted: inline AI completions from an IDE plugin
    /// (Cursor, GitHub Copilot).
    IdeAssisted,
    /// No AI marker was found.
    ///
    /// This is NOT the same claim as "a human wrote it": a commit whose
    /// trailers were stripped, squashed, or rewritten is indistinguishable
    /// from one that never had them. #5250 proposes a distinct `unknown`
    /// state for that case.
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

/// Detect the AI tool that produced a commit, from its message alone.
///
/// Why: `commits.ai_tool` and `commits.is_ai_assisted` must be populated at
/// collection time (issue #445).
/// What: runs the shipped marker set (the marker set) over `message`
/// with no author or committer email, and returns the winning marker's label.
/// Since #5249 the label can come from a body footer as well as a
/// `Co-Authored-By:` trailer, and the recognised set is no longer limited to
/// Claude/Copilot/Cursor. Callers holding a commit's identities should call
/// [`detect`] instead so the email family is not skipped.
/// Test: `tests::detect_ai_tool_*` below.
///
/// # Stable identifiers
///
/// The shipped labels are `claude`, `trusty-mpm`, `devin`, `openhands`,
/// `aider`, `copilot`, and `cursor`; a configured marker contributes its own
/// `tool:` string. See [`crate::collect::ai_markers`].
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
    detect(&CommitSignals::from_message(message)).tool
}

/// Classify a commit into one of the three canonical agentic modes.
///
/// Why: distinguishes autonomous CLI-agent commits (Claude Code) from IDE
/// inline-completion commits (Cursor/Copilot) from plain human commits
/// (issue #1113). This finer granularity is needed for DAAU and agentic-%
/// analytics that the binary `is_ai_assisted` flag cannot express.
/// What: runs the shipped marker set (the marker set) over `message`
/// with no author or committer email. A full-agentic marker (Claude Code, the
/// trusty-mpm footer, Devin, OpenHands, Aider, the `X-AI-*` trailers) outranks
/// an IDE marker (Copilot, Cursor); no match is `None`. Callers holding a
/// commit's identities should call [`detect`] instead so the email
/// family is not skipped.
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
    detect(&CommitSignals::from_message(message)).mode
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: 1058 of this repo's 2434 commits carry the house footer and no
    /// `Co-Authored-By:` trailer. Before #5249 every one of them was recorded
    /// as a plain human commit.
    /// What: the footer alone classifies as full-agentic and labels the tool.
    #[test]
    fn detect_trusty_mpm_footer_is_full_agentic() {
        let msg = "docs: add website link to README (#5330)\n\n\
                   🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
        assert_eq!(detect_ai_tool(msg), Some("trusty-mpm"));
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
