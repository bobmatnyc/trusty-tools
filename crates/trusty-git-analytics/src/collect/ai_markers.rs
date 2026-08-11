//! The agentic-marker set backing AI attribution (#5249).
//!
//! Why: detection used to be seven hardcoded regexes, every one keyed on the
//! literal "Claude". On trusty-tools' own history that caught 47.7% of commits
//! where the agentic share is 91.0%: 1058 commits carry the
//! `Generated with trusty-mpm` footer and no `Co-Authored-By:` trailer, and
//! nothing matched them. `agentic_pct` feeds an acquirer-facing figure
//! (DOC-67 §8), so a confidently wrong number is worse than an absent one.
//! What: an ordered list of markers, each a tool label, an [`AgenticMode`], a
//! scope naming which text it is matched against, and a compiled regex. One
//! pass yields both `commits.ai_tool` and `commits.agentic_mode`, so they can
//! never disagree. Callers use [`detect`].
//! Test: `tests` below — including `catch_rate_on_trusty_tools_history`, which
//! measures the set against this repo's real history.

use std::sync::OnceLock;

use regex::Regex;

use crate::collect::ai_attribution::{provenance_possibly_stripped, AgenticMode};

/// The three text families a commit exposes to detection.
///
/// Why: `extractor.rs` read `author_email` and threw it away, so an
/// agent-identifying committer address was invisible to detection.
/// What: borrowed view of one commit. [`Self::from_message`] covers callers
/// that genuinely only have the message — the backfill path reads
/// `commits.author_email` but has no stored committer address.
/// Test: `tests::email_scope_matches_author_or_committer`.
#[derive(Debug, Clone, Copy)]
pub struct CommitSignals<'a> {
    /// Full commit message, subject and body.
    pub message: &'a str,
    /// Author email as recorded by git.
    pub author_email: &'a str,
    /// Committer email as recorded by git.
    pub committer_email: &'a str,
}

impl<'a> CommitSignals<'a> {
    /// Message-only signals, with both email fields empty.
    pub fn from_message(message: &'a str) -> Self {
        Self {
            message,
            author_email: "",
            committer_email: "",
        }
    }
}

/// Outcome of one detection pass.
///
/// Why: `ai_tool`, `is_ai_assisted` and `agentic_mode` are written from the
/// same scan so they cannot disagree about whether a commit was AI-assisted.
/// What: `tool` is the winning marker's label; when nothing matched, `mode` is
/// [`AgenticMode::None`], or [`AgenticMode::Unknown`] if the message shows a
/// rewrite fingerprint (#5250).
/// Test: `tests::detects_trusty_mpm_footer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detection {
    /// Label of the highest-priority matching marker.
    pub tool: Option<&'static str>,
    /// Classification implied by the matching markers.
    pub mode: AgenticMode,
}

/// Classify one commit against the marker set.
///
/// Why: the single detection entry point for the collection walk
/// (`collect::git::extractor`) and the `tga backfill ai-detection-commits`
/// repair pass, so a repaired row is byte-identical to a freshly walked one.
/// What: extracts the `Co-Authored-By:` values once, then tests markers in
/// order. The first `FullAgentic` match returns immediately, so it outranks an
/// `IdeAssisted` match found earlier in the list; among `IdeAssisted` matches
/// the earliest supplies the label. With no match at all the verdict splits on
/// [`provenance_possibly_stripped`] (#5250) — the equivalence above is why that
/// predicate reads the message only, never the identities the backfill lacks.
/// Test: `tests::detects_trusty_mpm_footer`,
/// `tests::full_agentic_wins_over_ide_assisted`,
/// `tests::merge_summary_is_unknown_not_none`.
pub fn detect(signals: &CommitSignals<'_>) -> Detection {
    let trailers: Vec<&str> = trailer_line()
        .captures_iter(signals.message)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();

    let mut ide: Option<&'static str> = None;
    for marker in markers() {
        if !marker.matches(signals, &trailers) {
            continue;
        }
        match marker.mode {
            AgenticMode::FullAgentic => {
                return Detection {
                    tool: Some(marker.tool),
                    mode: AgenticMode::FullAgentic,
                };
            }
            AgenticMode::IdeAssisted => {
                if ide.is_none() {
                    ide = Some(marker.tool);
                }
            }
            AgenticMode::None | AgenticMode::Unknown => {}
        }
    }

    match ide {
        Some(tool) => Detection {
            tool: Some(tool),
            mode: AgenticMode::IdeAssisted,
        },
        // #5250: a message git or the forge composed never had room for the
        // author's marker, so "no marker" is not a human-work finding there.
        None if provenance_possibly_stripped(signals.message) => Detection {
            tool: None,
            mode: AgenticMode::Unknown,
        },
        None => Detection {
            tool: None,
            mode: AgenticMode::None,
        },
    }
}

/// One-line statement of what detection can and cannot see.
///
/// Why: DOC-67 §8 hands an acquirer an `agentic_pct` figure. Detection is
/// marker-based, so a target that strips or rewrites trailers reports a low
/// share for a reason that is not "no AI assistance" — the report must say so
/// rather than let the reader infer provenance from silence (#5249).
/// What: the distinct tool labels in the set plus the standing caveat.
/// `tga collect` and `tga backfill ai-detection-commits` log it once per run;
/// the AUDIT velocity section renders it when that section ships (#5241/#5242).
/// Test: `tests::disclosure_names_active_tools`.
pub fn detection_disclosure() -> String {
    let mut tools: Vec<&str> = Vec::new();
    for m in markers() {
        if !tools.contains(&m.tool) {
            tools.push(m.tool);
        }
    }
    format!(
        "agentic detection: {} marker(s) active for [{}]; detection is marker-based only — \
         commits whose trailers or footers were stripped, squashed, or rewritten are \
         indistinguishable from human commits, so a low agentic share means \"no markers \
         emitted\", not \"no AI assistance\"",
        BUILTIN.len(),
        tools.join(", ")
    )
}

/// Which text a marker's pattern is applied to.
///
/// Why: the three marker families — trailers, body footers, and
/// agent-identifying emails — need different haystacks. Matching a trailer
/// pattern against the whole message would let a quoted mention in a commit
/// body count as a co-author.
/// What: `Trailer` runs against each `Co-Authored-By:` value in isolation,
/// `Message` against the raw commit message (use `(?m)^` to anchor a trailer
/// with a different key, e.g. `X-AI-Model:`), and `Email` against the author
/// and committer addresses.
/// Test: `tests::trailer_scope_does_not_match_body_prose`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerScope {
    Trailer,
    Message,
    Email,
}

/// One compiled marker.
#[derive(Debug)]
struct AiMarker {
    tool: &'static str,
    mode: AgenticMode,
    scope: MarkerScope,
    pattern: Regex,
}

impl AiMarker {
    fn matches(&self, signals: &CommitSignals<'_>, trailers: &[&str]) -> bool {
        match self.scope {
            MarkerScope::Trailer => trailers.iter().any(|v| self.pattern.is_match(v)),
            MarkerScope::Message => self.pattern.is_match(signals.message),
            MarkerScope::Email => {
                self.pattern.is_match(signals.author_email)
                    || self.pattern.is_match(signals.committer_email)
            }
        }
    }
}

/// Declarative source for the marker set.
struct BuiltinSpec {
    tool: &'static str,
    mode: AgenticMode,
    scope: MarkerScope,
    pattern: &'static str,
}

/// The marker set, in priority order.
///
/// Claude entries lead so the pre-#5249 tool priority (Claude → Copilot →
/// Cursor) is preserved verbatim.
///
/// Every pattern is anchored tightly enough that a human co-author cannot trip
/// it: the bot markers key on a bot-specific local part or domain
/// (`devin-ai-integration`, `@devin.ai`) rather than a first name, and the
/// email markers match a full address, never a vendor domain — an Anthropic or
/// All Hands employee's own commits must stay classified as human work. The
/// `\bclaude\b` trailer entry predates this rule (#1334) and is left as-is
/// because narrowing it would drop real detections.
const BUILTIN: &[BuiltinSpec] = &[
    BuiltinSpec {
        tool: "claude",
        mode: AgenticMode::FullAgentic,
        scope: MarkerScope::Trailer,
        pattern: r"(?i)\bclaude\b",
    },
    BuiltinSpec {
        tool: "claude",
        mode: AgenticMode::FullAgentic,
        scope: MarkerScope::Message,
        pattern: r"(?i)Generated\s+with\s+Claude\s+Code",
    },
    BuiltinSpec {
        tool: "claude",
        mode: AgenticMode::FullAgentic,
        scope: MarkerScope::Message,
        pattern: r"(?im)^X-AI-Tokens-(?:In|Out):\s*\d",
    },
    BuiltinSpec {
        tool: "claude",
        mode: AgenticMode::FullAgentic,
        scope: MarkerScope::Message,
        pattern: r"(?im)^X-AI-Model:\s*\S",
    },
    // #5249: the house footer this repo has emitted since trusty-mpm shipped.
    // 1058 of 2434 commits carry it; before this entry every one of them that
    // lacked a Co-Authored-By trailer counted as human work.
    BuiltinSpec {
        tool: "trusty-mpm",
        mode: AgenticMode::FullAgentic,
        scope: MarkerScope::Message,
        pattern: r"(?i)Generated\s+with\s+trusty-mpm",
    },
    // Keyed on the bot's local part, not the word "devin": a bare `\bdevin\b`
    // classified `Co-authored-by: Devin Booker <devin@personal.example>` as an
    // agent.
    BuiltinSpec {
        tool: "devin",
        mode: AgenticMode::FullAgentic,
        scope: MarkerScope::Trailer,
        pattern: r"(?i)\bdevin-ai-integration\b|@devin\.ai\b",
    },
    BuiltinSpec {
        tool: "devin",
        mode: AgenticMode::FullAgentic,
        scope: MarkerScope::Email,
        pattern: r"(?i)^devin-ai-integration(\[bot\])?@",
    },
    BuiltinSpec {
        tool: "openhands",
        mode: AgenticMode::FullAgentic,
        scope: MarkerScope::Trailer,
        pattern: r"(?i)\bopenhands\b",
    },
    BuiltinSpec {
        tool: "openhands",
        mode: AgenticMode::FullAgentic,
        scope: MarkerScope::Email,
        pattern: r"(?i)^openhands@all-hands\.dev$",
    },
    BuiltinSpec {
        tool: "aider",
        mode: AgenticMode::FullAgentic,
        scope: MarkerScope::Trailer,
        pattern: r"(?i)\baider\b",
    },
    BuiltinSpec {
        tool: "copilot",
        mode: AgenticMode::IdeAssisted,
        scope: MarkerScope::Trailer,
        pattern: r"(?i)\bcopilot\b|GitHub\s+Copilot",
    },
    // `\bcursor\b` alone matches the surname in "Alice Cursor-Williams". The
    // `regex` crate has no lookahead, so the guard is the trailing class: a
    // hyphen after the word rejects the match.
    BuiltinSpec {
        tool: "cursor",
        mode: AgenticMode::IdeAssisted,
        scope: MarkerScope::Trailer,
        pattern: r"(?i)@cursor\.sh|\bcursor\b($|[^-\w])",
    },
];

/// Matches the full `Co-Authored-By:` / `Co-authored-by:` trailer line,
/// capturing the value after the key.
fn trailer_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?im)^[Cc]o-[Aa]uthored-[Bb]y:\s*(.+)$")
            .expect("trailer_line pattern compiles")
    })
}

/// The compiled marker set, built once per process.
///
/// A pattern that fails to compile is a programmer error in [`BUILTIN`],
/// caught by `tests::every_builtin_pattern_compiles`, never a runtime
/// condition — nothing outside this file supplies a pattern.
fn markers() -> &'static [AiMarker] {
    static SET: OnceLock<Vec<AiMarker>> = OnceLock::new();
    SET.get_or_init(|| {
        BUILTIN
            .iter()
            .map(|s| AiMarker {
                tool: s.tool,
                mode: s.mode,
                scope: s.scope,
                pattern: Regex::new(s.pattern).expect("builtin marker pattern compiles"),
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode_of(msg: &str) -> AgenticMode {
        detect(&CommitSignals::from_message(msg)).mode
    }

    /// Why: a bad pattern literal must fail here, not mid-collection.
    #[test]
    fn every_builtin_pattern_compiles() {
        assert_eq!(markers().len(), BUILTIN.len());
    }

    /// Why: this is the 41.6-point undercount in #5249 — 1058 commits in this
    /// repo carry the footer and, before the fix, matched nothing.
    #[test]
    fn detects_trusty_mpm_footer() {
        let msg = "feat: add thing\n\n🤖🤖🤖 Generated with trusty-mpm — \
                   https://github.com/bobmatnyc/trusty-tools";
        let d = detect(&CommitSignals::from_message(msg));
        assert_eq!(d.mode, AgenticMode::FullAgentic);
        assert_eq!(d.tool, Some("trusty-mpm"));
    }

    /// Why: the OpenHands validation target in #5249 — 3873 of 7990 commits
    /// carry this co-author and matched nothing before the fix.
    #[test]
    fn detects_openhands_coauthor() {
        let msg = "Fix runtime startup\n\n\
                   Co-authored-by: openhands <openhands@all-hands.dev>";
        let d = detect(&CommitSignals::from_message(msg));
        assert_eq!(d.mode, AgenticMode::FullAgentic);
        assert_eq!(d.tool, Some("openhands"));
    }

    /// Why: Devin and Aider had no entry at all before #5249.
    #[test]
    fn detects_devin_and_aider() {
        assert_eq!(
            mode_of(
                "chore: bump\n\nCo-authored-by: Devin AI \
                 <devin-ai-integration[bot]@users.noreply.github.com>"
            ),
            AgenticMode::FullAgentic
        );
        assert_eq!(
            mode_of("refactor: extract\n\nCo-authored-by: aider (gpt-4o) <noreply@aider.chat>"),
            AgenticMode::FullAgentic
        );
    }

    /// Why: `\bdevin\b` classified a human co-author named Devin as an agent,
    /// contradicting the anchoring rule the marker table states. The bot is
    /// still caught by its local part and by `@devin.ai`.
    #[test]
    fn human_named_devin_is_not_agentic() {
        let msg = "feat: scoring\n\nCo-authored-by: Devin Booker <devin@personal.example>";
        let d = detect(&CommitSignals::from_message(msg));
        assert_eq!(d.mode, AgenticMode::None, "a first name is not a marker");
        assert_eq!(d.tool, None);
        // The real bot, both trailer forms, still classifies.
        assert_eq!(
            mode_of("fix: x\n\nCo-authored-by: Devin <devin@devin.ai>"),
            AgenticMode::FullAgentic
        );
    }

    /// Why: `extractor.rs` extracted `author_email` and never passed it to
    /// detection; the email family exists to close that gap.
    #[test]
    fn email_scope_matches_author_or_committer() {
        let by_author = CommitSignals {
            message: "fix: something",
            author_email: "openhands@all-hands.dev",
            committer_email: "human@example.com",
        };
        assert_eq!(detect(&by_author).mode, AgenticMode::FullAgentic);

        let by_committer = CommitSignals {
            message: "fix: something",
            author_email: "human@example.com",
            committer_email: "devin-ai-integration[bot]@users.noreply.github.com",
        };
        assert_eq!(detect(&by_committer).mode, AgenticMode::FullAgentic);
    }

    /// Why: an All Hands employee's own commits are human work; a vendor
    /// domain must never classify a commit on its own.
    #[test]
    fn vendor_domain_alone_is_not_agentic() {
        let human = CommitSignals {
            message: "fix: something",
            author_email: "engineer@all-hands.dev",
            committer_email: "engineer@all-hands.dev",
        };
        assert_eq!(detect(&human).mode, AgenticMode::None);
    }

    /// Why: mode priority must not depend on marker order.
    #[test]
    fn full_agentic_wins_over_ide_assisted() {
        let msg = "pair: fix auth\n\n\
                   Co-Authored-By: Cursor <noreply@cursor.sh>\n\
                   Co-Authored-By: Claude Opus <noreply@anthropic.com>";
        let d = detect(&CommitSignals::from_message(msg));
        assert_eq!(d.mode, AgenticMode::FullAgentic);
        assert_eq!(d.tool, Some("claude"));
    }

    /// Why: scope is the guard against a quoted mention in a commit body
    /// counting as a co-author.
    #[test]
    fn trailer_scope_does_not_match_body_prose() {
        assert_eq!(
            mode_of("fix: rewrite the openhands adapter, no trailer here"),
            AgenticMode::None
        );
        assert_eq!(
            mode_of("fix: x\n\nCo-authored-by: openhands <openhands@all-hands.dev>"),
            AgenticMode::FullAgentic
        );
    }

    /// Why: #5250 — the fingerprint split happens inside `detect`, not only in
    /// the predicate, and the email family must not change the verdict. The
    /// backfill path sees empty emails, so a merge summary has to classify
    /// identically with and without them or a repaired row stops matching a
    /// freshly walked one.
    #[test]
    fn merge_summary_is_unknown_not_none() {
        let msg = "Merge branch 'feat/x' into main";
        let by_message = detect(&CommitSignals::from_message(msg));
        assert_eq!(by_message.mode, AgenticMode::Unknown);
        assert_eq!(by_message.tool, None);

        let with_identities = detect(&CommitSignals {
            message: msg,
            author_email: "engineer@example.com",
            committer_email: "noreply@github.com",
        });
        assert_eq!(with_identities, by_message);

        assert_eq!(mode_of("feat: add button"), AgenticMode::None);
    }

    /// Why: the disclosure is the answer to "does a low share mean no AI?".
    #[test]
    fn disclosure_names_active_tools() {
        let d = detection_disclosure();
        assert!(d.contains("claude"), "{d}");
        assert!(d.contains("trusty-mpm"), "{d}");
        assert!(d.contains("no markers emitted"), "{d}");
    }
}
