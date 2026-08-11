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
//! Also [`provenance_possibly_stripped`], the predicate behind
//! [`AgenticMode::Unknown`] (#5250): it decides whether a message with no
//! marker is one the author wrote or one git/a forge composed.
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

use std::sync::OnceLock;

use regex::Regex;

use crate::collect::ai_markers::{detect, CommitSignals};

/// Canonical agentic-mode classification for a commit (issue #1113).
///
/// Why: the binary `is_ai_assisted` flag and the tool-string `ai_tool`
/// column conflate very different working modes — a Claude Code commit
/// (autonomous CLI agent) is qualitatively different from a Cursor
/// inline-completion commit. Downstream analytics (DAAU, agentic %)
/// need to distinguish these modes without losing the existing columns.
/// What: four-valued enum, persisted as the TEXT column `agentic_mode`. The
/// column is `TEXT NOT NULL DEFAULT 'none'` with no CHECK constraint, so
/// `'unknown'` (#5250) needed no migration — the three-value restriction only
/// ever lived in `as_str`/`FromStr` here.
/// Test: `tests::detect_agentic_mode_*` below; see also
/// `core::db::migrations::v21` which adds the column.
///
/// `#[non_exhaustive]` because #5250's variant addition cost tga a major bump
/// (`cargo-semver-checks` `enum_variant_added`, and the crate is 2.x). The
/// attribute makes the next one — per-tool attribution in
/// [#5251](https://github.com/bobmatnyc/trusty-tools/issues/5251) — a minor
/// bump instead. It costs external `match` sites a `_ =>` arm; matches inside
/// this crate are unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AgenticMode {
    /// Full-agentic: autonomous CLI agent (Claude Code, Devin, OpenHands,
    /// Aider, or a house wrapper such as trusty-mpm). Which markers imply it
    /// is data, not code, since #5249 — see [`crate::collect::ai_markers`].
    FullAgentic,
    /// IDE-assisted: inline AI completions from an IDE plugin
    /// (Cursor, GitHub Copilot).
    IdeAssisted,
    /// No AI marker was found in a message the author actually wrote.
    ///
    /// This is still not the same claim as "a human wrote it" — a commit can
    /// be rewritten in ways this detector cannot see. It is the narrower
    /// claim that the message carries no rewrite fingerprint either, so the
    /// absence of a marker is the best evidence available.
    None,
    /// No AI marker was found, but the message shows a rewrite fingerprint,
    /// so a marker the author emitted would have been discarded (#5250).
    ///
    /// Why: `agentic_pct` is an acquirer-facing figure (DOC-67 §8). A
    /// git-generated merge summary and a genuinely human commit both used to
    /// persist as `'none'`, which reports "we checked, there was no AI" for a
    /// message that never had room to say so.
    /// What: reached only when no marker matched AND
    /// [`provenance_possibly_stripped`] holds for the message. It is a
    /// refinement of [`AgenticMode::None`], never of a positive
    /// classification: a marker always wins.
    /// Test: `tests::detect_agentic_mode_merge_summary_is_unknown`,
    /// `tests::unknown_is_distinct_from_none`.
    Unknown,
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
            AgenticMode::Unknown => "unknown",
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
            "unknown" => Ok(AgenticMode::Unknown),
            _ => Err(()),
        }
    }
}

/// Does this message carry a fingerprint of having been synthesized by git or
/// a forge, rather than written by the commit's author (#5250)?
///
/// Why: [`AgenticMode::None`] is a finding — "we read the author's message and
/// there was no AI marker in it". That finding is only sound when the message
/// we hold IS the author's. When git or GitHub composed it, any footer or
/// trailer the author emitted was discarded before the commit object existed,
/// and "no marker" says nothing about how the work was done.
/// What: two fingerprints, both message-only. **(a) a machine merge summary** —
/// `Merge branch 'x'`, `Merge remote-tracking branch 'x'`, `Merge tag 'x'`,
/// `Merge commit 'x'`, or `Merge pull request #N from owner/branch`. git and
/// GitHub compose these in full; none of them can contain an authored footer.
/// **(b) a forge squash-merge that replaced the body** — a subject ending in
/// `(#N)` whose every remaining non-blank line is a `Key: value` trailer. This
/// is what GitHub's "default to PR title" squash setting emits: the branch's
/// commit bodies, where footers live, are dropped and only synthesized
/// `Co-authored-by:` trailers remain.
///
/// Message-only is a deliberate constraint, not an oversight: `commits` has no
/// `committer_email` column, so `tga backfill ai-detection-commits` sees only
/// the message. A predicate reading identities would classify a freshly walked
/// row differently from a repaired one, breaking the equivalence
/// [`detect`] promises.
///
/// # What this deliberately does NOT catch
///
/// A `(#N)` squash whose body is the PR description. On this repo's HEAD
/// history the predicate claims 16 commits and leaves 128 of these unclaimed —
/// and at least some of the 128 were agentic: `a92bb941` (#5379) carries no
/// marker, while the branch commits GitHub squashed into it do.
///
/// Separating those needs a signal this function does not have (the pre-squash
/// commits, or the PR body). The subject shape alone cannot tell a squash from
/// a hand-written `fix: thing (#12)`, and on a repo that references issues in
/// subjects by convention it would claim nearly everything — trading a false
/// `none` for an `unknown` no measurement could refute.
///
/// Test: `tests::provenance_possibly_stripped_*`.
pub fn provenance_possibly_stripped(message: &str) -> bool {
    let mut lines = message.lines().map(str::trim);
    let Some(subject) = lines.next() else {
        return false;
    };
    if machine_merge_subject().is_match(subject) {
        return true;
    }
    if !squash_pr_subject().is_match(subject) {
        return false;
    }
    // An empty remainder satisfies this: a `(#N)` subject with no body at all
    // is the "PR title only" squash in its purest form.
    lines
        .filter(|l| !l.is_empty())
        .all(|l| trailer_shaped().is_match(l))
}

/// Subjects git and GitHub compose for merge commits. The quoted ref is the
/// guard: it rejects prose such as `Merge branch handling into the parser`.
fn machine_merge_subject() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^Merge (?:(?:remote-tracking )?branch|tag|commit) '|^Merge pull request #\d+ from \S",
        )
        .expect("machine_merge_subject pattern compiles")
    })
}

/// The `(#N)` suffix GitHub appends to a squash-merge subject.
fn squash_pr_subject() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\(#\d+\)$").expect("squash_pr_subject pattern compiles"))
}

/// A git trailer line — `Key: value`, the only body content GitHub synthesizes
/// for a title-only squash.
fn trailer_shaped() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^[A-Za-z][A-Za-z0-9_-]*:\s*\S").expect("trailer_shaped pattern compiles")
    })
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

/// Classify a commit into one of the four canonical agentic modes.
///
/// Why: distinguishes autonomous CLI-agent commits (Claude Code) from IDE
/// inline-completion commits (Cursor/Copilot) from plain human commits
/// (issue #1113). This finer granularity is needed for DAAU and agentic-%
/// analytics that the binary `is_ai_assisted` flag cannot express.
/// What: runs the shipped marker set (the marker set) over `message`
/// with no author or committer email. A full-agentic marker (Claude Code, the
/// trusty-mpm footer, Devin, OpenHands, Aider, the `X-AI-*` trailers) outranks
/// an IDE marker (Copilot, Cursor). With no match the result is `None` for a
/// message the author wrote and `Unknown` when the message carries a rewrite
/// fingerprint (#5250 — see [`provenance_possibly_stripped`]). Callers holding
/// a commit's identities should call [`detect`] instead so the email family is
/// not skipped.
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
        assert_eq!(AgenticMode::Unknown.as_str(), "unknown");
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
        assert_eq!(AgenticMode::from_str("unknown"), Ok(AgenticMode::Unknown));
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

    // -------------------------------------------------------------------------
    // Issue #5250 — the Unknown state
    // -------------------------------------------------------------------------

    /// Why: git composes these subjects in full, so an author's footer cannot
    /// survive into them. 117 of this repo's 453 marker-less commits are of
    /// this shape.
    /// What: each git/GitHub merge summary form is a rewrite fingerprint.
    #[test]
    fn provenance_possibly_stripped_matches_machine_merge_summaries() {
        for msg in [
            "Merge branch 'feat/x' into main",
            "Merge remote-tracking branch 'origin/main'",
            "Merge tag 'v2.15.0' into develop",
            "Merge commit 'deadbeef'",
            "Merge pull request #42 from bobmatnyc/feat-x\n\nfeat: add the thing",
        ] {
            assert!(provenance_possibly_stripped(msg), "{msg}");
        }
    }

    /// Why: GitHub's "default to PR title" squash discards the branch's commit
    /// bodies, which is where footers live, and leaves only the trailers it
    /// synthesizes.
    /// What: a `(#N)` subject whose remaining lines are all trailers — or which
    /// has no body at all — is a rewrite fingerprint.
    #[test]
    fn provenance_possibly_stripped_matches_body_replacing_squash() {
        assert!(provenance_possibly_stripped(
            "docs: align the corpus (#5397)"
        ));
        assert!(provenance_possibly_stripped(
            "ci: scope the check (#5400)\n\n\
             Co-authored-by: bobmatnyc <bobmatnyc@users.noreply.github.com>"
        ));
    }

    /// Why: the predicate is only worth having if it stays narrow. Each of
    /// these is a message the author actually wrote, so "no marker" there is a
    /// finding and must stay `None`.
    /// What: prose that merely starts with "Merge", a squash whose body is the
    /// PR description, a plain commit, and an empty message.
    #[test]
    fn provenance_possibly_stripped_rejects_authored_messages() {
        for msg in [
            "Merge branch handling into the parser",
            "refactor: merge pull request handling into one module",
            "docs: record a rejected proposal (#5379)\n\n\
             Docs-only. The linker switch measured slower on this runner.",
            "feat: add button",
            "",
        ] {
            assert!(!provenance_possibly_stripped(msg), "{msg}");
        }
    }

    /// Why: this is the #5250 acceptance — a stripped-provenance commit must
    /// stop sharing a bucket with a genuinely human one.
    /// What: a git merge summary is `Unknown`; the same author's hand-written
    /// commit is `None`; the two are not equal.
    #[test]
    fn detect_agentic_mode_merge_summary_is_unknown() {
        assert_eq!(
            detect_agentic_mode("Merge branch 'feat/x' into main"),
            AgenticMode::Unknown
        );
    }

    /// Why: `Unknown` is a refinement of `None`, and a report that treated
    /// them as one value would erase the whole point of the variant.
    #[test]
    fn unknown_is_distinct_from_none() {
        assert_ne!(AgenticMode::Unknown, AgenticMode::None);
        assert_ne!(AgenticMode::Unknown.as_str(), AgenticMode::None.as_str());
        assert_ne!(
            detect_agentic_mode("Merge branch 'feat/x' into main"),
            detect_agentic_mode("feat: add button")
        );
    }

    /// Why: `Unknown` must never displace a positive classification — a merge
    /// commit an agent authored still carries its footer.
    /// What: a merge summary plus the house footer stays `FullAgentic`.
    #[test]
    fn marker_wins_over_rewrite_fingerprint() {
        let msg = "Merge branch 'feat/x' into main\n\n\
                   🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools";
        assert_eq!(detect_agentic_mode(msg), AgenticMode::FullAgentic);
        assert_eq!(detect_ai_tool(msg), Some("trusty-mpm"));
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
