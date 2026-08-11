//! The spaCy-informed precision gate, layered on PR #5402's pattern pass.
//!
//! Why: #5402's `extract_patterns` takes the whitespace token either side of a
//! marker and filters it lexically (`is_stop_token`). That is blind to grammar,
//! so `hard` survives as an is-a object and `ancestor` survives with its `of`
//! complement chopped off. This module keeps #5402's marker table and stopword
//! floor and adds the three things only a parse can supply: the part of speech
//! of the candidate, the noun-phrase boundary around it, and the phrase's head.
//! What: for each marker hit, resolve subject and object to spaCy noun chunks,
//! reject an adjective-headed object, re-walk a modifier-stacked object to its
//! head noun, and reject an object NP immediately followed by `of`. Every step
//! FAILS OPEN — when spaCy offers no chunk, the #5402 lexical answer stands.
//! Test: `cargo run -- eval` reproduces the #5399 evaluation table.

use crate::sidecar::Doc;

/// #5402's marker table, verbatim. Kept identical so the bake-off measures the
/// gate, not a different set of trigger phrases.
pub const PATTERN_TABLE: &[(&str, &[&str])] = &[
    ("is-a", &[" is a ", " is an "]),
    ("works-at", &[" works at "]),
    ("uses", &[" uses ", " using "]),
    ("depends-on", &[" depends on ", " requires "]),
];

/// Why a candidate triple was dropped, so the evaluation table can show the
/// mechanism rather than just an absence.
#[derive(Debug, Clone, PartialEq)]
pub enum Reject {
    AdjectiveHead(String),
    TruncatedBeforeOf(String),
    StopToken(String),
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub triples: Vec<(String, String, String)>,
    pub rejects: Vec<Reject>,
}

/// Minimal slice of #5402's `STOPWORDS` — the closed classes that reach this
/// eval set. The real integration would call `kg_extract::is_stop_token`.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "this", "that", "these", "those", "it", "its", "them", "they", "he", "she",
    "we", "us", "you", "i", "here", "there",
];

fn is_stop_token(tok: &str) -> bool {
    let norm = tok
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    norm.is_empty() || STOPWORDS.contains(&norm.as_str()) || norm.chars().count() < 2
}

/// The chunk whose right edge is the marker's left edge (the subject NP).
fn chunk_ending_at(doc: &Doc, end: usize) -> Option<&crate::sidecar::NounChunk> {
    doc.noun_chunks.iter().find(|c| c.end == end)
}

/// The chunk containing the first character after the marker (the object NP).
///
/// The marker swallows the determiner (`" is a "`), so the object character
/// offset lands INSIDE the chunk rather than at its start — containment, not
/// equality, is the correct test.
fn chunk_containing(doc: &Doc, pos: usize) -> Option<&crate::sidecar::NounChunk> {
    doc.noun_chunks
        .iter()
        .find(|c| c.start <= pos && pos < c.end)
}

/// Strip a leading determiner from an NP's surface form.
///
/// Only `DET` is stripped, never a leading `ADJ`: spaCy tags the `trusty` of
/// `trusty-memory` as `ADJ`, so an adjective strip would amputate a real crate
/// name. See the report's false-reject discussion.
fn np_surface(doc: &Doc, chunk: &crate::sidecar::NounChunk) -> String {
    let first = doc
        .tokens
        .iter()
        .find(|t| t.start >= chunk.start && t.end <= chunk.end);
    match first {
        Some(t) if t.pos == "DET" => chunk.text[(t.end - chunk.start)..].trim().to_string(),
        _ => chunk.text.trim().to_string(),
    }
}

/// Whether the token immediately following `chunk` is the preposition `of`.
///
/// Why: `an ancestor of origin main` and `a member of the process group` are
/// relational nouns — the noun alone does not name a class, so a triple built
/// from it asserts something the sentence never said. spaCy's chunker ends the
/// NP at `ancestor`, which gives the boundary but NOT the verdict; this is the
/// rule that turns the boundary into one.
fn followed_by_of(doc: &Doc, chunk: &crate::sidecar::NounChunk) -> bool {
    doc.tokens
        .iter()
        .find(|t| t.start >= chunk.end)
        .is_some_and(|t| t.pos == "ADP" && t.text.eq_ignore_ascii_case("of"))
}

fn last_token(s: &str) -> String {
    s.split_whitespace()
        .last()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '#')
        .to_string()
}

fn first_token(s: &str) -> String {
    s.split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '#')
        .to_string()
}

/// Run the gate over one already-parsed sentence.
pub fn extract(text: &str, doc: &Doc) -> Outcome {
    let lower = text.to_lowercase();
    let mut triples = Vec::new();
    let mut rejects = Vec::new();

    for (predicate, markers) in PATTERN_TABLE {
        for marker in *markers {
            let Some(idx) = lower.find(marker) else {
                continue;
            };
            let left_end = lower[..idx].trim_end().len();
            let right_start = idx + marker.len();

            // ---- subject: whole NP, determiner stripped; lexical fallback ----
            let subject = match chunk_ending_at(doc, left_end) {
                Some(c) => np_surface(doc, c),
                // FAIL OPEN. `rustc` is tagged ADJ and gets no chunk at all; a
                // chunk-required subject rule would silently delete it.
                None => last_token(&text[..idx]),
            };

            // ---- object: head-noun re-walk, adjective + `of` guards ----
            let object = match chunk_containing(doc, right_start) {
                Some(c) => {
                    if followed_by_of(doc, c) {
                        rejects.push(Reject::TruncatedBeforeOf(np_surface(doc, c)));
                        break;
                    }
                    if c.root_pos == "ADJ" {
                        rejects.push(Reject::AdjectiveHead(c.text.clone()));
                        break;
                    }
                    // The re-walk itself: the chunk's syntactic head, which
                    // skips any stacked determiners and adjectives.
                    doc.tokens
                        .get(c.root)
                        .map(|t| t.text.clone())
                        .unwrap_or_else(|| first_token(&text[right_start..]))
                }
                None => first_token(&text[right_start..]),
            };

            if is_stop_token(&subject) || is_stop_token(&object) {
                rejects.push(Reject::StopToken(format!("{subject} / {object}")));
                break;
            }
            triples.push((subject, (*predicate).to_string(), object));
            break;
        }
    }
    Outcome { triples, rejects }
}
