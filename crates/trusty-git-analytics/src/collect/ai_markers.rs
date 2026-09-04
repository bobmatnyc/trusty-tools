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
//! never disagree. Callers use [`detect`]. Since #5414 the list is
//! [`BUILTIN`] plus whatever [`crate::collect::ai_marker_config`] loads from
//! disk, so a house footer is addable without a code change or a release. The
//! two halves are scanned in sequence, not merged: [`BUILTIN`] decides first,
//! and an operator marker is consulted only for a commit [`BUILTIN`] left
//! unmarked.
//! Test: `tests` below — including `catch_rate_on_trusty_tools_history`, which
//! measures the set against this repo's real history.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use crate::collect::ai_attribution::{provenance_possibly_stripped, AgenticMode};
use crate::collect::ai_marker_config::{
    marker_file_path, MarkerConfig, MarkerConfigError, MarkerScope,
};

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

/// Generation number of the shipped detector, stamped onto every row it
/// classifies (#6748).
///
/// Why: `tga collect` classifies once, at walk time, and persists the verdict.
/// When the marker set gains an entry — as it did in #1334 and again in #5249 —
/// every row already stored keeps the verdict the old set produced, and nothing
/// re-reads it. Roughly 700 commits carrying a literal AI trailer sat in a
/// downstream warehouse flagged `is_ai_assisted = 0` for that reason, and the
/// consumer could not repair them because its schema drops `commits.message`.
/// Storing the generation beside the verdict makes "classified by an older
/// detector" a query rather than a guess about ingest dates.
/// What: a monotonically increasing integer. Bump it in the same change that
/// alters what [`detect`] returns; `commits.ai_detector_version` defaults to 0,
/// so every row written before this column existed sorts as older than any
/// shipped generation and is re-classified on the next collect.
///
/// Nothing derives this number from the marker table or checks that the two
/// moved together: it is hand-maintained, so a change to [`BUILTIN`] that
/// forgets to bump it silently skips re-classification, and the corpus keeps
/// the old verdicts exactly as it did before #6748. Bumping it when nothing
/// changed is harmless — one extra pass — so bump when in doubt.
///
/// This tracks the SHIPPED marker set only. Operator markers loaded from disk
/// by [`crate::collect::ai_marker_config`] change without a rebuild, so a row
/// they classified is not re-visited by a version bump alone — run
/// `tga backfill ai-detection-commits` after editing the marker file.
/// Test: `crate::collect::reclassify::tests::stale_rows_are_reclassified_and_current_rows_are_not`.
pub const DETECTOR_VERSION: i64 = 1;

/// Classify one commit against the marker set.
///
/// Why: the single detection entry point for the collection walk
/// (`collect::git::extractor`) and the `tga backfill ai-detection-commits`
/// repair pass, so a repaired row is byte-identical to a freshly walked one.
/// What: two scans, in strict order. The shipped [`BUILTIN`] markers are
/// scanned to completion first; if that yields any verdict at all, it is
/// returned and the operator markers are never consulted. Only a builtin
/// verdict of [`AgenticMode::None`] falls through to them. Within one scan the
/// first `FullAgentic` match returns immediately, so it outranks an
/// `IdeAssisted` match found earlier in that slice; among `IdeAssisted` matches
/// the earliest supplies the label. When neither scan matches, the verdict
/// splits on [`provenance_possibly_stripped`] (#5250) — that predicate reads the
/// message only, never the identities the backfill lacks.
/// Test: `tests::detects_trusty_mpm_footer`,
/// `tests::full_agentic_wins_over_ide_assisted`,
/// `tests::operator_full_agentic_cannot_upgrade_a_builtin_ide_match`,
/// `tests::merge_summary_is_unknown_not_none`.
pub fn detect(signals: &CommitSignals<'_>) -> Detection {
    detect_in(marker_set(), signals)
}

/// [`detect`] against an explicit marker set.
///
/// Why: the seam that lets the operator-file behaviour be tested without the
/// process-global [`marker_set`], whose `OnceLock` can only ever observe one
/// configuration per process.
/// What: #5414 — the two-phase scan that makes "an operator marker can only
/// classify a commit the shipped set left unmarked" true by construction. A
/// single flat scan could not deliver it: `scan` returns early on `FullAgentic`
/// but merely records `IdeAssisted` and keeps going, so an operator
/// `FullAgentic` entry appended after the two builtin `IdeAssisted` markers
/// overwrote a copilot verdict with its own. The #5250 `Unknown` split lands
/// here rather than in `scan` for the same structural reason — see the two
/// `provenance_stripped` tests below.
/// Test: `tests::operator_full_agentic_cannot_upgrade_a_builtin_ide_match`,
/// `tests::operator_marker_still_applies_to_a_provenance_stripped_subject`,
/// `tests::provenance_stripped_subject_is_unknown_when_no_marker_matches`.
fn detect_in(set: &MarkerSet, signals: &CommitSignals<'_>) -> Detection {
    let trailers: Vec<&str> = trailer_line()
        .captures_iter(signals.message)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();

    let builtin = scan(&set.markers[..set.builtin_len], signals, &trailers);
    if builtin.mode != AgenticMode::None {
        return builtin;
    }
    let operator = scan(&set.markers[set.builtin_len..], signals, &trailers);
    if operator.mode != AgenticMode::None {
        return operator;
    }

    // #5250: neither scan matched a marker. A message git or the forge composed
    // never had room for the author's marker, so "no marker" is not a
    // human-work finding there. This split runs once, AFTER both scans — a
    // `scan` that returned `Unknown` itself would satisfy the `!= None` test
    // above and skip the operator markers #5414 added.
    if provenance_possibly_stripped(signals.message) {
        return Detection {
            tool: None,
            mode: AgenticMode::Unknown,
        };
    }
    Detection {
        tool: None,
        mode: AgenticMode::None,
    }
}

/// One ordered pass over a marker slice.
fn scan(markers: &[AiMarker], signals: &CommitSignals<'_>, trailers: &[&str]) -> Detection {
    let mut ide: Option<&'static str> = None;
    for marker in markers {
        if !marker.matches(signals, trailers) {
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
        // "No marker in this slice" only. The #5250 Unknown split belongs to
        // `detect_in`, which alone knows both slices came up empty.
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
/// rather than let the reader infer provenance from silence (#5249). Since
/// #5414 the set is also extensible at runtime, which the reader has to know
/// too: two runs of the same tga version over the same repository can report
/// different shares, and a bad marker file degrades to builtins rather than
/// failing the run. The line states which of those happened.
/// What: the distinct tool labels in the set, where the operator markers came
/// from (loaded / absent / rejected, with the error), plus the standing
/// caveat. `tga collect` and `tga backfill ai-detection-commits` log it once
/// per run; the AUDIT velocity section renders it when that section ships
/// (#5241/#5242).
/// Test: `tests::disclosure_names_active_tools`,
/// `tests::disclosure_reports_a_rejected_marker_file`.
pub fn detection_disclosure() -> String {
    disclosure_for(marker_set())
}

fn disclosure_for(set: &MarkerSet) -> String {
    let mut tools: Vec<&str> = Vec::new();
    for m in &set.markers {
        if !tools.contains(&m.tool) {
            tools.push(m.tool);
        }
    }
    let operator = match &set.source {
        MarkerSource::Absent(path) => format!(
            "no operator marker file at {} (set {} to add markers without a code change)",
            path.display(),
            crate::collect::ai_marker_config::ENV_AI_MARKERS
        ),
        MarkerSource::Loaded { path, count } => {
            format!("{count} operator marker(s) loaded from {}", path.display())
        }
        MarkerSource::Failed { path, error } => format!(
            "operator marker file {} was REJECTED and none of it applied ({error}) — \
             builtin markers only",
            path.display()
        ),
    };
    format!(
        "agentic detection: {} builtin marker(s) + {}; active for [{}]; detection is \
         marker-based only — commits whose trailers or footers were stripped, squashed, or \
         rewritten are indistinguishable from human commits, so a low agentic share means \
         \"no markers emitted\", not \"no AI assistance\"",
        BUILTIN.len(),
        operator,
        tools.join(", ")
    )
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
        // The `\[?` is not cosmetic: both house footers are emitted in a plain
        // and a markdown-link form, and requiring the bare word missed 14 of
        // this repo's own commits (#5414).
        pattern: r"(?i)Generated\s+with\s+\[?Claude\s+Code",
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
        pattern: r"(?i)Generated\s+with\s+\[?trusty-mpm",
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
    // #5414: `\bopenhands\b` classified `Co-authored-by: Simon Rosenberg
    // <simon@openhands.dev>` — a human at the vendor — as an agent. Found by
    // running the marker against a real All-Hands-AI/OpenHands clone rather
    // than fixtures; the bot forms it must keep catching are enumerated in
    // `tests::openhands_trailers_from_a_real_clone`. One trailer form,
    // `OH <openhands@example.com>` (commit 06cc1ef2), is genuinely ambiguous
    // and stays uncaught: example.com is a reserved placeholder domain, and
    // anchoring on it is how the marker got this wrong in the first place.
    BuiltinSpec {
        tool: "openhands",
        mode: AgenticMode::FullAgentic,
        scope: MarkerScope::Trailer,
        pattern: r"(?i)\bopenhands@all-hands\.dev\b|\bopenhands[-_]?release-bot\b|\bopenhands\s+bot\b",
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

/// The compiled marker list plus where its operator half came from.
///
/// `markers[..builtin_len]` is [`BUILTIN`] and `markers[builtin_len..]` is the
/// operator file. The split is what makes [`detect_in`]'s precedence contract
/// structural rather than a consequence of where the operator entries happen
/// to sit (#5414).
struct MarkerSet {
    markers: Vec<AiMarker>,
    builtin_len: usize,
    source: MarkerSource,
}

/// What the operator marker file contributed.
enum MarkerSource {
    /// No file at the resolved path — the ordinary case.
    Absent(PathBuf),
    /// File read and applied.
    Loaded { path: PathBuf, count: usize },
    /// File present but unusable; none of it applied.
    Failed { path: PathBuf, error: String },
}

/// The compiled marker set, built once per process.
///
/// A pattern that fails to compile is a programmer error when it comes from
/// [`BUILTIN`] (caught by `tests::every_builtin_pattern_compiles`) and an
/// operator mistake when it comes from the marker file, where it is reported
/// and skipped rather than fatal.
fn marker_set() -> &'static MarkerSet {
    static SET: OnceLock<MarkerSet> = OnceLock::new();
    SET.get_or_init(|| build_marker_set(&marker_file_path()))
}

/// Compile [`BUILTIN`], then append the operator markers at `path`.
///
/// Why: #5414 requires a marker to be addable without a code change, and
/// requires a bad marker file not to break a collect run — a collection that
/// aborts because a config file has a typo is a worse outcome than one that
/// runs with the shipped set and says so.
/// What: operator markers are appended after [`BUILTIN`] and the boundary is
/// recorded in [`MarkerSet::builtin_len`], which is what [`detect_in`] scans
/// against — an operator entry is consulted only when the shipped set returns
/// no verdict at all, so it can classify a commit the shipped set left
/// unmarked and can do nothing else to one it already classifies. On any
/// failure — unreadable, unparseable, over the entry cap, or a pattern that
/// will not compile — the whole file is rejected, logged at WARN, and recorded
/// in [`detection_disclosure`]; a partially applied file would be the silent
/// half-configuration this rejects.
/// Test: `tests::operator_markers_extend_the_builtins`,
/// `tests::operator_full_agentic_cannot_upgrade_a_builtin_ide_match`,
/// `tests::a_bad_pattern_rejects_the_whole_file`.
fn build_marker_set(path: &Path) -> MarkerSet {
    let markers: Vec<AiMarker> = BUILTIN
        .iter()
        .map(|s| AiMarker {
            tool: s.tool,
            mode: s.mode,
            scope: s.scope,
            pattern: Regex::new(s.pattern).expect("builtin marker pattern compiles"),
        })
        .collect();
    let builtin_len = markers.len();
    let mut markers = markers;

    if !path.exists() {
        return MarkerSet {
            markers,
            builtin_len,
            source: MarkerSource::Absent(path.to_path_buf()),
        };
    }

    let source = match MarkerConfig::load_from(path).and_then(|cfg| compile_operator(&cfg)) {
        Ok(operator) => {
            let count = operator.len();
            markers.extend(operator);
            MarkerSource::Loaded {
                path: path.to_path_buf(),
                count,
            }
        }
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "operator agentic-marker file rejected; continuing with the builtin markers"
            );
            MarkerSource::Failed {
                path: path.to_path_buf(),
                error: error.to_string(),
            }
        }
    };

    MarkerSet {
        markers,
        builtin_len,
        source,
    }
}

/// Compile every operator spec, or reject the file.
///
/// The single compile site for operator patterns, so no expression is
/// validated once and compiled again.
fn compile_operator(cfg: &MarkerConfig) -> Result<Vec<AiMarker>, MarkerConfigError> {
    cfg.markers
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            if spec.tool.trim().is_empty() {
                return Err(MarkerConfigError::EmptyTool { index });
            }
            let pattern =
                Regex::new(&spec.pattern).map_err(|source| MarkerConfigError::Pattern {
                    index,
                    tool: spec.tool.clone(),
                    pattern: spec.pattern.clone(),
                    source,
                })?;
            Ok(AiMarker {
                // #5414: `Detection::tool` is `Option<&'static str>` and shipped
                // that way in 2.15.0, so widening it to an owned string is a
                // major break. The leak is bounded by the marker file and
                // happens once, giving the label exactly the process lifetime
                // the compiled set already has.
                tool: Box::leak(spec.tool.clone().into_boxed_str()),
                mode: spec.mode.as_agentic_mode(),
                scope: spec.scope,
                pattern,
            })
        })
        .collect()
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
        let set = build_marker_set(Path::new("/definitely/not/a/marker/file.yaml"));
        assert_eq!(set.markers.len(), BUILTIN.len());
    }

    /// Why: this is the 41.6-point undercount in #5249 — 1058 commits in this
    /// repo carry the footer and, before the fix, matched nothing. The
    /// markdown-link form is the same footer as emitted by an older template;
    /// 14 commits here carry it and #5249's pattern missed all of them.
    #[test]
    fn detects_trusty_mpm_footer() {
        let msg = "feat: add thing\n\n🤖🤖🤖 Generated with trusty-mpm — \
                   https://github.com/bobmatnyc/trusty-tools";
        let d = detect(&CommitSignals::from_message(msg));
        assert_eq!(d.mode, AgenticMode::FullAgentic);
        assert_eq!(d.tool, Some("trusty-mpm"));

        let linked = "fix: y\n\n🤖🤖🤖 Generated with \
                      [trusty-mpm](https://github.com/bobmatnyc/trusty-tools)";
        assert_eq!(mode_of(linked), AgenticMode::FullAgentic);
        let linked_claude = "fix: z\n\n🤖 Generated with \
                             [Claude Code](https://claude.com/claude-code)";
        assert_eq!(mode_of(linked_claude), AgenticMode::FullAgentic);
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

    // ---------------------------------------------------------------------
    // #5414 — operator-supplied markers
    // ---------------------------------------------------------------------

    fn write_marker_file(dir: &tempfile::TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("ai-markers.yaml");
        std::fs::write(&path, body).expect("writes fixture");
        path
    }

    /// Why: the whole point of #5414 — a house footer the shipped set has
    /// never heard of becomes detectable with no code change and no release.
    #[test]
    fn operator_markers_extend_the_builtins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_marker_file(
            &dir,
            "markers:\n\
             \x20 - tool: acme-copilot\n\
             \x20   mode: full_agentic\n\
             \x20   scope: message\n\
             \x20   pattern: '(?i)Produced\\s+by\\s+ACME\\s+Autopilot'\n",
        );
        let set = build_marker_set(&path);
        assert_eq!(set.markers.len(), BUILTIN.len() + 1);

        let signals = CommitSignals::from_message("feat: x\n\nProduced by ACME Autopilot v3");
        let d = detect_in(&set, &signals);
        assert_eq!(d.mode, AgenticMode::FullAgentic);
        assert_eq!(d.tool, Some("acme-copilot"));

        let disclosure = disclosure_for(&set);
        assert!(
            disclosure.contains("1 operator marker(s) loaded"),
            "{disclosure}"
        );
        assert!(disclosure.contains("acme-copilot"), "{disclosure}");
    }

    /// Why: an operator marker must not be able to relabel a commit the
    /// shipped set already classifies — appending is what guarantees that, and
    /// a future reordering would break it silently.
    #[test]
    fn operator_markers_never_relabel_a_builtin_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_marker_file(
            &dir,
            "markers:\n  - { tool: house, mode: full_agentic, scope: message, pattern: 'feat' }\n",
        );
        let set = build_marker_set(&path);
        let msg = "feat: add thing\n\n🤖🤖🤖 Generated with trusty-mpm";
        let d = detect_in(&set, &CommitSignals::from_message(msg));
        assert_eq!(d.tool, Some("trusty-mpm"), "builtin keeps the label");
    }

    /// Why: the case appending alone did NOT cover, and the one that proved
    /// the old "operator markers can never relabel a builtin match" claim was
    /// ordering luck rather than a contract. `detect_in` returns on the first
    /// `FullAgentic` match but only *records* an `IdeAssisted` one and keeps
    /// scanning, so an operator `FullAgentic` entry sitting after the two
    /// builtin `IdeAssisted` entries (copilot, cursor — last in `BUILTIN`)
    /// used to overwrite a copilot result with its own label and mode. Fixed
    /// by scanning `BUILTIN` to completion first and consulting the operator
    /// slice only when the builtin verdict is `None`.
    #[test]
    fn operator_full_agentic_cannot_upgrade_a_builtin_ide_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_marker_file(
            &dir,
            "markers:\n  - { tool: house-full, mode: full_agentic, scope: message, pattern: 'copilot' }\n",
        );
        let set = build_marker_set(&path);
        let msg = "feat: autocomplete\n\nCo-Authored-By: GitHub Copilot <copilot@github.com>";
        let d = detect_in(&set, &CommitSignals::from_message(msg));
        assert_eq!(d.tool, Some("copilot"), "the builtin verdict must survive");
        assert_eq!(d.mode, AgenticMode::IdeAssisted, "and must not be upgraded");
    }

    /// Why: an IDE-assisted operator marker must still lose to a builtin
    /// full-agentic match, so the config cannot weaken a classification.
    #[test]
    fn operator_ide_marker_loses_to_builtin_full_agentic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_marker_file(
            &dir,
            "markers:\n  - { tool: house-ide, mode: ide_assisted, scope: message, pattern: 'refactor' }\n",
        );
        let set = build_marker_set(&path);
        let msg = "refactor: split\n\nCo-Authored-By: Claude <noreply@anthropic.com>";
        let d = detect_in(&set, &CommitSignals::from_message(msg));
        assert_eq!(d.mode, AgenticMode::FullAgentic);
        assert_eq!(d.tool, Some("claude"));
    }

    /// Why: the seam where #5414 and #5250 meet. `provenance_possibly_stripped`
    /// is true for this subject, so deciding `Unknown` inside `scan` would let
    /// the builtin pass return a non-`None` verdict and skip the operator
    /// markers entirely — a merge-summary-shaped commit would become
    /// unclassifiable by any house marker. The split therefore runs once, after
    /// both passes.
    /// What: an operator marker matching a machine merge subject still wins.
    #[test]
    fn operator_marker_still_applies_to_a_provenance_stripped_subject() {
        let msg = "Merge branch 'feat/x' into main";
        assert!(
            provenance_possibly_stripped(msg),
            "precondition: the subject must look stripped, else this proves nothing"
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_marker_file(
            &dir,
            "markers:\n  - { tool: house-bot, mode: full_agentic, scope: message, pattern: \"Merge branch\" }\n",
        );
        let set = build_marker_set(&path);
        let d = detect_in(&set, &CommitSignals::from_message(msg));
        assert_eq!(
            d.mode,
            AgenticMode::FullAgentic,
            "the operator marker must be consulted, not pre-empted by Unknown"
        );
        assert_eq!(d.tool, Some("house-bot"));
    }

    /// Why: the other half of the pair above — with no operator marker to match,
    /// the same subject must still reach the #5250 `Unknown` verdict.
    #[test]
    fn provenance_stripped_subject_is_unknown_when_no_marker_matches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_marker_file(
            &dir,
            "markers:\n  - { tool: house-bot, mode: full_agentic, scope: message, pattern: 'nothing-matches-this' }\n",
        );
        let set = build_marker_set(&path);
        let d = detect_in(
            &set,
            &CommitSignals::from_message("Merge branch 'feat/x' into main"),
        );
        assert_eq!(d.mode, AgenticMode::Unknown);
        assert_eq!(d.tool, None);
    }

    /// Why: THE error arm. A marker file with a pattern the regex crate
    /// rejects must not abort a collect run, must not apply half of itself,
    /// and must not disappear quietly — a silently ignored bad config reports
    /// the same number as a correct one that found nothing.
    #[test]
    fn a_bad_pattern_rejects_the_whole_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_marker_file(
            &dir,
            "markers:\n\
             \x20 - { tool: good, mode: full_agentic, scope: message, pattern: 'Acme Bot' }\n\
             \x20 - { tool: broken, mode: full_agentic, scope: message, pattern: '([unclosed' }\n",
        );
        let set = build_marker_set(&path);

        assert_eq!(set.markers.len(), BUILTIN.len(), "no partial application");
        // Detection still works, on the builtin set.
        assert_eq!(
            detect_in(
                &set,
                &CommitSignals::from_message("x\n\nGenerated with trusty-mpm")
            )
            .mode,
            AgenticMode::FullAgentic
        );
        // Even the valid sibling entry is not applied.
        assert_eq!(
            detect_in(&set, &CommitSignals::from_message("chore: Acme Bot ran")).mode,
            AgenticMode::None
        );

        let disclosure = disclosure_for(&set);
        assert!(disclosure.contains("REJECTED"), "{disclosure}");
        assert!(
            disclosure.contains("marker 1 (tool `broken`)"),
            "{disclosure}"
        );
    }

    /// Why: the other two error shapes reach the same fail-open branch.
    #[test]
    fn disclosure_reports_a_rejected_marker_file() {
        let dir = tempfile::tempdir().expect("tempdir");

        let malformed = write_marker_file(&dir, "markers: [ this is not: valid yaml\n");
        let set = build_marker_set(&malformed);
        assert_eq!(set.markers.len(), BUILTIN.len());
        assert!(disclosure_for(&set).contains("REJECTED"));

        let blank_tool = dir.path().join("blank.yaml");
        std::fs::write(
            &blank_tool,
            "markers:\n  - { tool: '  ', mode: full_agentic, scope: message, pattern: 'x' }\n",
        )
        .expect("writes fixture");
        let set = build_marker_set(&blank_tool);
        assert_eq!(set.markers.len(), BUILTIN.len());
        let disclosure = disclosure_for(&set);
        assert!(disclosure.contains("empty `tool` label"), "{disclosure}");
    }

    /// Why: the ordinary case — no file — must read as "nothing configured",
    /// not as a failure, and must point the reader at how to configure one.
    #[test]
    fn an_absent_marker_file_is_not_a_failure() {
        let set = build_marker_set(Path::new("/definitely/not/here/ai-markers.yaml"));
        let disclosure = disclosure_for(&set);
        assert!(
            disclosure.contains("no operator marker file at"),
            "{disclosure}"
        );
        assert!(disclosure.contains("TGA_AI_MARKERS"), "{disclosure}");
        assert!(!disclosure.contains("REJECTED"), "{disclosure}");
    }

    /// Why: bullet 4 of #5249 — the OpenHands markers were proven only by
    /// invented fixtures. These six trailer lines are verbatim extracts from a
    /// real `All-Hands-AI/OpenHands` clone (8010 commits, cloned 2026-08-11),
    /// each cited by the commit it was taken from. The last two are the reason
    /// the marker changed in #5414: `\bopenhands\b` classified both as agents.
    #[test]
    fn openhands_trailers_from_a_real_clone() {
        // Builtins only: this asserts what the SHIPPED set does, so it must
        // not depend on whether the machine running it has a marker file.
        let set = build_marker_set(Path::new("/definitely/not/here/ai-markers.yaml"));

        // 21f0967c — the dominant form, 3117 occurrences.
        // d6d34956 — the CVE bot, 64 occurrences.
        // 4d0fe498 — the release bot, 35 occurrences.
        // b4e87121 — the contact-address bot, 29 occurrences.
        for trailer in [
            "Co-authored-by: openhands <openhands@all-hands.dev>",
            "Co-authored-by: OpenHands CVE Fix Bot <openhands@all-hands.dev>",
            "Co-authored-by: openhands-release-bot[bot] \
             <290150379+openhands-release-bot[bot]@users.noreply.github.com>",
            "Co-authored-by: OpenHands Bot <contact@all-hands.dev>",
        ] {
            let msg = format!("fix: something\n\n{trailer}");
            let d = detect_in(&set, &CommitSignals::from_message(&msg));
            assert_eq!(d.mode, AgenticMode::FullAgentic, "{trailer}");
            assert_eq!(d.tool, Some("openhands"), "{trailer}");
        }

        // 7b8b2626 — a human maintainer, 23 occurrences.
        // b18ebefb — a human contributor's handle, 27 occurrences.
        for trailer in [
            "Co-authored-by: Simon Rosenberg <simon@openhands.dev>",
            "Co-authored-by: aivong-openhands <ai.vong@openhands.dev>",
        ] {
            let msg = format!("fix: something\n\n{trailer}");
            let d = detect_in(&set, &CommitSignals::from_message(&msg));
            assert_eq!(d.mode, AgenticMode::None, "{trailer}");
        }
    }
}
