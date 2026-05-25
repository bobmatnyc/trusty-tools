//! Deterministic KG triple extraction from drawer content.
//!
//! Why: Issue #97 — `memory_remember` should populate the knowledge graph
//! automatically so palaces with drawers always have a non-empty KG. Calling an
//! LLM on every write would blow up latency and require network access; a
//! deterministic heuristic stays fast and offline while still producing useful
//! triples for tag membership, key-phrase mentions, and obvious is-a / has-a /
//! works-at patterns. The visual graph view (the other half of #97) renders
//! whatever shows up here, so this pass is the data source for "every palace
//! has a graph".
//! What: A pure function `extract_triples` that takes drawer content + tags +
//! drawer id and returns a `Vec<Triple>` with `provenance = "auto:remember"`.
//! The current heuristics are tag→drawer, room→drawer, hashtag→drawer, and a
//! short pattern table (`X is a Y`, `X works at Y`, `X uses Y`, `X depends on
//! Y`). Drawer ids are encoded as `drawer:<uuid>` so the subject keeps a
//! stable, palace-unique identity that the graph view can dereference back
//! to the source drawer.
//! Test: `extract_triples_emits_tag_triples`,
//! `extract_triples_emits_hashtag_mentions`,
//! `extract_triples_extracts_is_a_pattern`,
//! `extract_triples_never_panics_on_empty_input`.

use chrono::Utc;
use std::collections::HashSet;
use trusty_common::memory_core::store::kg::Triple;
use uuid::Uuid;

/// Provenance tag stamped on every auto-extracted triple.
///
/// Why: Operators need a stable string to filter / retract the auto-extracted
/// subset without scanning content. Centralising the constant keeps every
/// emitter and the back-fill CLI in sync.
/// What: A `&'static str` containing the literal `auto:remember`.
/// Test: `extract_triples_stamps_provenance`.
pub const AUTO_PROVENANCE: &str = "auto:remember";

/// Confidence applied to auto-extracted triples.
///
/// Why: Heuristic extraction is not authoritative; downstream rankers can use
/// the confidence to prefer explicit `kg_assert` triples over auto-extracted
/// noise.
/// What: `0.6` — high enough to surface in queries, low enough to be
/// over-ridden by a manual `kg_assert` of the same `(subject, predicate)`.
/// Test: `extract_triples_uses_reduced_confidence`.
pub const AUTO_CONFIDENCE: f32 = 0.6;

/// Subject prefix used for drawer-identity triples.
///
/// Why: A stable, palace-unique identifier lets the graph view dereference a
/// node back to the source drawer (and the back-fill CLI dedupe by drawer).
/// What: `drawer:` — concatenated with the drawer UUID hyphenated form.
/// Test: every test in this module asserts the prefix.
pub const DRAWER_SUBJECT_PREFIX: &str = "drawer:";

/// Subject prefix used for tag entities.
///
/// Why: The KG enforces at most one active triple per `(subject, predicate)`,
/// so we can't emit `drawer:X has-tag t1; drawer:X has-tag t2` — the second
/// assert would close the first. By promoting each tag to its own subject
/// (`tag:t1`, `tag:t2`) we keep multiple tags as distinct edges and the graph
/// view gets natural tag-clusters around each drawer.
/// What: `tag:` — concatenated with the lower-cased tag string.
/// Test: `extract_triples_emits_tag_triples`.
pub const TAG_SUBJECT_PREFIX: &str = "tag:";

/// Subject prefix used for free-text mention entities.
///
/// Why: Same temporal-invariant reasoning as `TAG_SUBJECT_PREFIX`. Hashtag
/// mentions and other discovered topical terms become their own subjects so
/// multiple mentions per drawer survive the assert pipeline.
/// What: `topic:` — concatenated with the lower-cased term.
/// Test: `extract_triples_emits_hashtag_mentions`.
pub const TOPIC_SUBJECT_PREFIX: &str = "topic:";

/// Subject prefix used for room entities.
///
/// Why: A drawer can only sit in one room, but encoding the room as its own
/// subject keeps the graph topology consistent (all "discovered metadata"
/// entities live under prefixed namespaces) and lets multiple drawers from
/// the same room cluster around a shared room node.
/// What: `room:` — concatenated with the room label.
/// Test: `extract_triples_emits_tag_triples`.
pub const ROOM_SUBJECT_PREFIX: &str = "room:";

/// Build the drawer subject string used as the (s) for every per-drawer
/// triple emitted by this module.
///
/// Why: Centralises the `drawer:<uuid>` encoding so call sites cannot drift.
/// What: Returns `format!("{DRAWER_SUBJECT_PREFIX}{id}")`.
/// Test: covered by every extractor test.
pub fn drawer_subject(id: Uuid) -> String {
    format!("{DRAWER_SUBJECT_PREFIX}{id}")
}

/// Inputs to a single extraction pass.
///
/// Why: Bundling the inputs keeps `extract_triples` signature small and lets
/// us add new fields (e.g. drawer_type) without breaking call sites.
/// What: Plain data struct; all fields are borrowed so the caller keeps
/// ownership.
/// Test: indirectly via every test that constructs one.
#[derive(Debug, Clone)]
pub struct ExtractInput<'a> {
    pub drawer_id: Uuid,
    pub content: &'a str,
    pub tags: &'a [String],
    pub room: Option<&'a str>,
}

/// Run the deterministic heuristic extractor.
///
/// Why: Single entry point so `memory_remember`, `memory_note`, and the
/// back-fill CLI all share the same logic. Pure function — no I/O, no async —
/// so it can be unit-tested cheaply.
/// What: Walks `tags`, content tokens, and a small pattern list to emit
/// `Triple`s; deduplicates so the same `(subject, predicate, object)` never
/// appears twice in a single pass.
/// Test: see the four tests at the bottom of this file.
pub fn extract_triples(input: &ExtractInput<'_>) -> Vec<Triple> {
    let now = Utc::now();
    let subject = drawer_subject(input.drawer_id);
    let mut out: Vec<Triple> = Vec::new();
    let mut seen: HashSet<(String, String, String)> = HashSet::new();

    let push = |out: &mut Vec<Triple>,
                seen: &mut HashSet<(String, String, String)>,
                s: String,
                p: String,
                o: String| {
        let key = (s.clone(), p.clone(), o.clone());
        if seen.insert(key) {
            out.push(Triple {
                subject: s,
                predicate: p,
                object: o,
                valid_from: now,
                valid_to: None,
                confidence: AUTO_CONFIDENCE,
                provenance: Some(AUTO_PROVENANCE.to_string()),
            });
        }
    };

    // Tag membership — each tag becomes its own subject so multiple tags on
    // the same drawer don't collide under the "one active triple per
    // (s, p)" invariant. Edge direction is `tag:<t> tags drawer:<id>` so the
    // graph clusters drawers under their shared tag nodes.
    for tag in input.tags {
        let clean = tag.trim();
        if clean.is_empty() {
            continue;
        }
        push(
            &mut out,
            &mut seen,
            format!("{TAG_SUBJECT_PREFIX}{}", clean.to_lowercase()),
            "tags".to_string(),
            subject.clone(),
        );
    }

    // Room membership — `room:<r> contains drawer:<id>` for the same reason
    // (multiple drawers per room must coexist).
    if let Some(room) = input.room {
        let clean = room.trim();
        if !clean.is_empty() {
            push(
                &mut out,
                &mut seen,
                format!("{ROOM_SUBJECT_PREFIX}{clean}"),
                "contains".to_string(),
                subject.clone(),
            );
        }
    }

    // Hashtag-style mentions — `topic:<term> mentioned-in drawer:<id>` so
    // multiple terms per drawer can coexist as distinct active edges.
    for term in extract_hashtags(input.content) {
        push(
            &mut out,
            &mut seen,
            format!("{TOPIC_SUBJECT_PREFIX}{term}"),
            "mentioned-in".to_string(),
            subject.clone(),
        );
    }

    // Simple natural-language patterns. Each yields a free-form
    // `<subject> <predicate> <object>` triple anchored to entities found in
    // the content (not the drawer subject), so the graph develops topical
    // edges over time.
    for (s, p, o) in extract_patterns(input.content) {
        push(&mut out, &mut seen, s, p, o);
    }

    out
}

/// Pull `#hashtag`-style tokens out of free-form content.
///
/// Why: Hashtags are a cheap, intentional signal — when a user writes `#rust`
/// or `#design-doc` we should record the mention so the graph picks it up.
/// What: Walks the string, captures runs of `[a-zA-Z0-9_-]` following a `#`,
/// lower-cases and deduplicates. Skips empty captures (a lone `#`).
/// Test: `extract_triples_emits_hashtag_mentions`.
fn extract_hashtags(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut iter = content.char_indices().peekable();
    while let Some((_, c)) = iter.next() {
        if c != '#' {
            continue;
        }
        let mut term = String::new();
        while let Some(&(_, nc)) = iter.peek() {
            if nc.is_ascii_alphanumeric() || nc == '_' || nc == '-' {
                term.push(nc.to_ascii_lowercase());
                iter.next();
            } else {
                break;
            }
        }
        if term.is_empty() {
            continue;
        }
        if seen.insert(term.clone()) {
            out.push(term);
        }
    }
    out
}

/// Pattern dictionary used by `extract_patterns`.
///
/// Why: A small, predictable set of (predicate, marker phrases) keeps the
/// extractor explicable and deterministic. Each entry maps a predicate to one
/// or more space-padded marker phrases; when the marker appears in the lower-
/// cased content we split on it and read the entity tokens immediately to
/// each side.
/// What: A static slice of `(predicate, &[marker, ...])`. Markers must be
/// lower-case and surrounded by whatever whitespace the input has — we add
/// the padding ourselves.
/// Test: `extract_triples_extracts_is_a_pattern`.
const PATTERN_TABLE: &[(&str, &[&str])] = &[
    ("is-a", &[" is a ", " is an "]),
    ("works-at", &[" works at "]),
    ("uses", &[" uses ", " using "]),
    ("depends-on", &[" depends on ", " requires "]),
];

/// Apply the pattern table to a single content blob.
///
/// Why: Keeps the matching loop out of `extract_triples` so the dispatcher
/// stays readable.
/// What: For every `(predicate, markers)` row, scan every marker against the
/// lower-cased content; on the first hit emit `(left_token, predicate,
/// right_token)` and move on to the next predicate. Only the first hit per
/// predicate is taken to avoid combinatorial output on long texts.
/// Test: `extract_triples_extracts_is_a_pattern`.
fn extract_patterns(content: &str) -> Vec<(String, String, String)> {
    let lower = content.to_lowercase();
    let mut out: Vec<(String, String, String)> = Vec::new();
    for (predicate, markers) in PATTERN_TABLE {
        for marker in *markers {
            if let Some(idx) = lower.find(marker) {
                let left = lower[..idx].trim();
                let right_start = idx + marker.len();
                let right = lower[right_start..].trim();
                let subject_tok = last_token(left);
                let object_tok = first_token(right);
                if !subject_tok.is_empty() && !object_tok.is_empty() {
                    out.push((subject_tok, (*predicate).to_string(), object_tok));
                }
                break;
            }
        }
    }
    out
}

/// Pull the final whitespace-delimited token from a fragment.
///
/// Why: The left side of a pattern hit can contain arbitrary preamble; the
/// entity we care about is the noun immediately before the marker.
/// What: Trims trailing punctuation off the last whitespace-delimited token.
/// Test: indirectly via `extract_triples_extracts_is_a_pattern`.
fn last_token(s: &str) -> String {
    s.split_whitespace()
        .last()
        .map(|t| t.trim_end_matches([',', '.', ';', ':', '!', '?', '"', '\'']))
        .unwrap_or("")
        .to_string()
}

/// Pull the first whitespace-delimited token from a fragment.
///
/// Why: Mirror of `last_token` for the right side of a pattern hit.
/// What: Trims leading punctuation off the first whitespace-delimited token.
/// Test: indirectly via `extract_triples_extracts_is_a_pattern`.
fn first_token(s: &str) -> String {
    s.split_whitespace()
        .next()
        .map(|t| t.trim_end_matches([',', '.', ';', ':', '!', '?', '"', '\'']))
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input_for(content: &str, tags: &[&str], room: Option<&str>) -> (Uuid, Vec<String>) {
        let id = Uuid::new_v4();
        let owned_tags: Vec<String> = tags.iter().map(|s| s.to_string()).collect();
        let _ = content; // silence unused warning if test ignores content
        let _ = room;
        (id, owned_tags)
    }

    /// Why: Tag-derived triples are the lowest-hanging extraction and the
    /// graph view's first signal when no patterns fire. The KG's temporal
    /// model only allows one active triple per `(subject, predicate)`, so
    /// each tag becomes its own subject (`tag:<name>`) with a `tags`
    /// predicate pointing at the drawer.
    /// What: One `tag:<t> tags drawer:<id>` per non-empty tag, plus
    /// `room:<r> contains drawer:<id>` when a room is supplied.
    /// Test: This test.
    #[test]
    fn extract_triples_emits_tag_triples() {
        let (id, tags) = input_for("hello world", &["rust", "design"], Some("Backend"));
        let triples = extract_triples(&ExtractInput {
            drawer_id: id,
            content: "hello world",
            tags: &tags,
            room: Some("Backend"),
        });
        let object = drawer_subject(id);
        assert!(triples
            .iter()
            .any(|t| t.subject == "tag:rust" && t.predicate == "tags" && t.object == object));
        assert!(triples
            .iter()
            .any(|t| t.subject == "tag:design" && t.predicate == "tags" && t.object == object));
        assert!(triples.iter().any(|t| t.subject == "room:Backend"
            && t.predicate == "contains"
            && t.object == object));
    }

    /// Why: Hashtag tokens are a cheap user signal; the extractor must catch
    /// them so the graph picks up topical entities.
    /// What: `#rust` and `#design-doc` both become `topic:<term>
    /// mentioned-in drawer:<id>` triples, lower-cased and deduplicated.
    /// Test: This test.
    #[test]
    fn extract_triples_emits_hashtag_mentions() {
        let (id, tags) = input_for("see #Rust and #design-doc and #rust again", &[], None);
        let triples = extract_triples(&ExtractInput {
            drawer_id: id,
            content: "see #Rust and #design-doc and #rust again",
            tags: &tags,
            room: None,
        });
        let mention_subjects: Vec<&str> = triples
            .iter()
            .filter(|t| t.predicate == "mentioned-in")
            .map(|t| t.subject.as_str())
            .collect();
        assert!(mention_subjects.contains(&"topic:rust"));
        assert!(mention_subjects.contains(&"topic:design-doc"));
        // Dedupe — `#rust` and `#Rust` collapse.
        assert_eq!(
            mention_subjects
                .iter()
                .filter(|s| **s == "topic:rust")
                .count(),
            1
        );
    }

    /// Why: `is a` is the simplest NL pattern and the most common idiom in
    /// quick notes ("rustc is a compiler").
    /// What: Pattern fires once per content blob; subject and object are the
    /// nouns either side of the marker.
    /// Test: This test.
    #[test]
    fn extract_triples_extracts_is_a_pattern() {
        let (id, _) = input_for("rustc is a compiler for rust", &[], None);
        let triples = extract_triples(&ExtractInput {
            drawer_id: id,
            content: "rustc is a compiler for rust",
            tags: &[],
            room: None,
        });
        assert!(triples
            .iter()
            .any(|t| t.subject == "rustc" && t.predicate == "is-a" && t.object == "compiler"));
    }

    /// Why: Confidence and provenance are guard-rails — extracted triples
    /// must be recognisable and over-ridable.
    /// What: Every triple carries `provenance = Some("auto:remember")` and
    /// `confidence == AUTO_CONFIDENCE`.
    /// Test: This test.
    #[test]
    fn extract_triples_stamps_provenance() {
        let (id, tags) = input_for("anything", &["x"], None);
        let triples = extract_triples(&ExtractInput {
            drawer_id: id,
            content: "anything",
            tags: &tags,
            room: None,
        });
        assert!(!triples.is_empty());
        for t in &triples {
            assert_eq!(t.provenance.as_deref(), Some(AUTO_PROVENANCE));
            assert!((t.confidence - AUTO_CONFIDENCE).abs() < f32::EPSILON);
        }
    }

    /// Why: Reduced confidence is the contract a manual `kg_assert` of the
    /// same `(subject, predicate)` needs in order to "win" against the
    /// auto-extracted edge.
    /// What: Every triple carries `confidence == AUTO_CONFIDENCE` (currently
    /// 0.6); the constant is asserted to stay strictly below 1.0 so manual
    /// asserts always rank higher.
    /// Test: This test.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn extract_triples_uses_reduced_confidence() {
        // Why: both bounds are static facts about the AUTO_CONFIDENCE
        // constant; the assertion is documentation for future tweakers.
        assert!(AUTO_CONFIDENCE < 1.0);
        assert!(AUTO_CONFIDENCE > 0.0);
    }

    /// Why: Empty / whitespace-only content must not panic or emit garbage.
    /// What: No tags, no room, no content → empty vec.
    /// Test: This test.
    #[test]
    fn extract_triples_never_panics_on_empty_input() {
        let id = Uuid::new_v4();
        let triples = extract_triples(&ExtractInput {
            drawer_id: id,
            content: "",
            tags: &[],
            room: None,
        });
        assert!(triples.is_empty());
    }

    /// Why: Edge-case test — content with no patterns but tags should still
    /// produce the tag triples (the graph view's primary signal).
    /// What: Single tag, no room, prose with no pattern hits → exactly one
    /// triple shaped as `tag:meeting tags drawer:<id>`.
    /// Test: This test.
    #[test]
    fn extract_triples_tags_only_path() {
        let id = Uuid::new_v4();
        let tags = vec!["meeting".to_string()];
        let triples = extract_triples(&ExtractInput {
            drawer_id: id,
            content: "Discussed roadmap.",
            tags: &tags,
            room: None,
        });
        assert_eq!(triples.len(), 1);
        assert_eq!(triples[0].subject, "tag:meeting");
        assert_eq!(triples[0].predicate, "tags");
        assert_eq!(triples[0].object, drawer_subject(id));
    }
}
