//! Exact-match floor for the hybrid search pipeline (#7675).
//!
//! Why: the default hybrid `search` fused BM25 and the vector lane by rank
//! alone, so a query naming a literal that occurs verbatim in exactly one chunk
//! could still lose the top slot to a semantically-similar guess — and, when the
//! literal tokenized into common code words, lose the top ten entirely. The
//! 9-lookup spike in `docs/research/input-token-optimization-spike-2026-09-12.md`
//! measured 3 outright misses and 2 wrong-but-plausible top hits on queries
//! `search_lexical` answered at rank 1.
//! What: a query-shape rule ([`extract_exact_literal`]), a corpus lane that
//! finds every chunk carrying that literal ([`CodeIndexer::exact_match_lane`]),
//! and a rank FLOOR ([`apply_floor`]) — every literal-carrying chunk is placed
//! above every chunk that is not, with the declaration first. A query whose
//! literal occurs nowhere in the corpus produces no lane, no promotion and no
//! floor, so conceptual ranking is byte-identical to before.
//! Test: `crates/trusty-search/src/core/indexer/tests/exact_match_floor.rs`.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;

use regex::Regex;

use super::super::{CodeChunk, CodeIndexer, RawChunk};

/// Most chunks an unquoted multi-word PHRASE may match before the floor is
/// declined.
///
/// Why: a phrase that occurs verbatim in dozens of chunks is boilerplate, not a
/// distinctive literal, and flooring it would reorder a conceptual query for no
/// gain. An identifier or an explicitly quoted string carries no such cap — the
/// caller named the literal.
/// What: checked against the lane's hit count for [`LiteralShape::Phrase`] only.
/// Test: `a_common_phrase_does_not_earn_the_floor`.
const PHRASE_HIT_CAP: usize = 16;

/// Minimum token count and byte length for an unquoted phrase to be considered.
const PHRASE_MIN_TOKENS: usize = 3;
const PHRASE_MIN_BYTES: usize = 16;

/// Keywords that introduce a declaration in the languages this crate chunks.
const DEF_KEYWORDS: &[&str] = &[
    "fn",
    "struct",
    "enum",
    "trait",
    "type",
    "const",
    "static",
    "impl",
    "class",
    "def",
    "function",
    "interface",
    "mod",
];

/// Which query shape earned the floor.
///
/// Why: the three shapes differ in how the literal is matched (whitespace is
/// flexible inside a phrase, rigid inside an identifier) and in whether a
/// commonness cap applies. Test: `extract_exact_literal` unit tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiteralShape {
    /// A single code identifier: `render_savings_segment`, `Config::new`,
    /// `SavingsTotal`, or a `fn`/`struct`/`const`-prefixed form.
    Identifier,
    /// Text the caller quoted explicitly.
    Quoted,
    /// A distinctive multi-word phrase, matched with flexible whitespace.
    Phrase,
}

/// The literal a query asked for, if it asked for one at all.
#[derive(Debug, Clone)]
pub(crate) struct ExactLiteral {
    /// The literal text, with any quoting or `fn `/`struct ` prefix stripped.
    pub(crate) text: String,
    pub(crate) shape: LiteralShape,
}

/// What the floor did on one query, for the response `meta` block.
///
/// Why: a caller that sees an unexpected top hit must be able to tell a
/// semantic ranking from a literal one without re-running the query.
/// What: `applied` is whether any chunk was floored; `literal` is the text that
/// was matched — present whenever a literal was EXTRACTED, so a caller can also
/// see that a literal was recognised and simply found nothing.
/// Test: `search_meta_reports_the_exact_match_floor`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ExactMatchReport {
    /// `true` when at least one chunk carried the literal and was floored.
    pub applied: bool,
    /// The literal the query was read as asking for, `None` when the query was
    /// conceptual.
    pub literal: Option<String>,
}

/// Decide whether a query names a literal, and what that literal is.
///
/// Why: the floor must fire for identifier- and literal-shaped queries and stay
/// completely out of the way for conceptual ones — this function is the whole
/// gate. See #7675.
/// What: quoted spans win first, then a bare identifier (optionally behind one
/// declaration keyword), then a multi-word phrase long enough to be
/// distinctive. Returns `None` for everything else, which is the conceptual
/// path and leaves ranking untouched.
/// Test: `extract_exact_literal_reads_the_query_shapes`.
pub(crate) fn extract_exact_literal(query: &str) -> Option<ExactLiteral> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    if let Some(inner) = quoted_span(q) {
        return Some(ExactLiteral {
            text: inner,
            shape: LiteralShape::Quoted,
        });
    }
    let tokens: Vec<&str> = q.split_whitespace().collect();
    match tokens.as_slice() {
        [one] if is_identifier_shaped(one) => {
            return Some(ExactLiteral {
                text: (*one).to_string(),
                shape: LiteralShape::Identifier,
            })
        }
        [kw, name]
            if DEF_KEYWORDS.contains(&kw.to_ascii_lowercase().as_str())
                && is_bare_identifier(name) =>
        {
            return Some(ExactLiteral {
                text: (*name).to_string(),
                shape: LiteralShape::Identifier,
            })
        }
        _ => {}
    }
    if tokens.len() >= PHRASE_MIN_TOKENS && q.len() >= PHRASE_MIN_BYTES {
        return Some(ExactLiteral {
            text: q.to_string(),
            shape: LiteralShape::Phrase,
        });
    }
    None
}

/// Extract the contents of the first `"…"` or `'…'` span with real content.
fn quoted_span(q: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let start = q.find(quote)?;
        if let Some(rel_end) = q[start + 1..].find(quote) {
            let inner = &q[start + 1..start + 1 + rel_end];
            if inner.trim().len() >= 2 {
                return Some(inner.trim().to_string());
            }
        }
    }
    None
}

/// A bare identifier token: `[A-Za-z_][A-Za-z0-9_:]*`, no other punctuation.
fn is_bare_identifier(tok: &str) -> bool {
    let mut chars = tok.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

/// A bare identifier that also carries a distinguishing signal — an
/// underscore, a `::` path separator, or a camelCase/PascalCase boundary.
///
/// Why: a single lowercase English word (`authenticate`, `cache`) is a
/// conceptual query, not a literal request, and flooring it would defeat the
/// semantic lane. The signal is what separates the two. See #7675.
/// What: `is_bare_identifier` plus at least one of the three signals.
/// Test: `extract_exact_literal_reads_the_query_shapes`.
fn is_identifier_shaped(tok: &str) -> bool {
    if !is_bare_identifier(tok) {
        return false;
    }
    if tok.contains('_') || tok.contains("::") {
        return true;
    }
    tok.chars()
        .zip(tok.chars().skip(1))
        .any(|(a, b)| a.is_ascii_lowercase() && b.is_ascii_uppercase())
}

/// Build the matcher for a literal.
///
/// Why: an identifier must not match inside a longer identifier (`is_trusted`
/// must not hit `is_trusted_root`), while a phrase must survive being wrapped
/// across lines in a string literal or a doc comment.
/// What: identifiers get `\b`-anchored escaped text; quoted text and phrases get
/// escaped text whose internal whitespace runs become `\s+`.
/// Test: `literal_regex_anchors_an_identifier_at_word_boundaries`.
pub(crate) fn literal_regex(lit: &ExactLiteral) -> Option<Regex> {
    let pattern = match lit.shape {
        LiteralShape::Identifier => {
            let escaped = regex::escape(&lit.text);
            let head = lit.text.chars().next()?;
            let tail = lit.text.chars().next_back()?;
            let lead = if head.is_ascii_alphanumeric() || head == '_' {
                r"\b"
            } else {
                ""
            };
            let trail = if tail.is_ascii_alphanumeric() || tail == '_' {
                r"\b"
            } else {
                ""
            };
            format!("{lead}{escaped}{trail}")
        }
        LiteralShape::Quoted | LiteralShape::Phrase => lit
            .text
            .split_whitespace()
            .map(regex::escape)
            .collect::<Vec<_>>()
            .join(r"\s+"),
    };
    Regex::new(&pattern).ok()
}

/// Per-query matcher for "does this chunk DECLARE the literal?".
///
/// Why: the predicate needs a regex built from the literal, and building it per
/// CHUNK made an identifier query 195x slower than the same query before the
/// floor existed — 0.66 ms → 129.8 ms over a 2 000-chunk corpus, because
/// `regex::Regex::new` ran once per candidate. Compiling once per query is what
/// keeps the lane's cost one corpus scan. See #7675.
/// What: holds the name matcher, built once; `matches` is the per-chunk call.
/// `None` for a non-identifier literal, where declaration-ness is meaningless.
/// Test: `l3_compound_identifier_ranks_its_declaration_first`.
struct DefinitionMatcher {
    name_re: Regex,
    bare: String,
    full: String,
}

impl DefinitionMatcher {
    fn new(lit: &ExactLiteral) -> Option<Self> {
        if !matches!(lit.shape, LiteralShape::Identifier) {
            return None;
        }
        let bare = lit
            .text
            .rsplit("::")
            .next()
            .unwrap_or(&lit.text)
            .to_string();
        let name_re = Regex::new(&format!(r"\b{}\b", regex::escape(&bare))).ok()?;
        Some(Self {
            name_re,
            bare,
            full: lit.text.clone(),
        })
    }

    fn matches(&self, content: &str, function_name: Option<&str>) -> bool {
        if let Some(name) = function_name {
            if name == self.full || name == self.bare {
                return true;
            }
        }
        static DEF_RE: OnceLock<Regex> = OnceLock::new();
        let kw = DEF_RE.get_or_init(|| {
            Regex::new(&format!(r"\b(?:{})\s+$", DEF_KEYWORDS.join("|")))
                .expect("static regex pattern must compile")
        });
        let declared = self
            .name_re
            .find_iter(content)
            .any(|m| kw.is_match(&content[..m.start()]));
        declared
    }
}

/// One chunk that carries the literal, with the two signals that order it.
#[derive(Debug, Clone)]
pub(crate) struct ExactHit {
    pub(crate) id: String,
    pub(crate) is_definition: bool,
    pub(crate) occurrences: usize,
    /// The caller's `branch_files` preference for this chunk. Always `false` in
    /// the corpus lane, which only needs the id set; filled in by the floor.
    pub(crate) on_branch: bool,
}

/// Score every literal-carrying chunk in `chunks` against `lit`.
///
/// Why: shared by the corpus lane (recall) and the floor (ranking) so the two
/// can never disagree about what counts as a hit. The ordering key deliberately
/// excludes the fused lane score: once a chunk carries the literal, letting a
/// semantic score pick WHICH occurrence wins reintroduces the exact failure
/// #7675 is about, and makes `search` and `search_lexical` disagree on top-1
/// whenever two chunks declare the same name.
/// What: matches `re` against each chunk's content, counting occurrences and
/// flagging declarations; returns hits ordered by (declaration, caller-declared
/// branch preference, occurrence count, chunk id) — every key lane-independent,
/// so the order is fully deterministic. `on_branch` is in the key because the
/// caller's `branch_files` boost is a declared preference the floor must not
/// swallow; it reaches this function as a flag rather than as the score
/// multiplier the pipeline applied.
/// Test: `hybrid_top_one_agrees_with_lexical_for_sampled_identifiers`,
/// `test_branch_boost_applied_to_matching_chunks`.
fn rank_hits<'a, I>(items: I, lit: &ExactLiteral, re: &Regex) -> Vec<ExactHit>
where
    I: Iterator<Item = (&'a str, &'a str, Option<&'a str>, bool)>,
{
    // Built once per query, never per chunk — see [`DefinitionMatcher`].
    let definition = DefinitionMatcher::new(lit);
    let mut hits: Vec<ExactHit> = items
        .filter_map(|(id, content, function_name, on_branch)| {
            let occurrences = re.find_iter(content).count();
            if occurrences == 0 {
                return None;
            }
            Some(ExactHit {
                id: id.to_string(),
                is_definition: definition
                    .as_ref()
                    .is_some_and(|d| d.matches(content, function_name)),
                occurrences,
                on_branch,
            })
        })
        .collect();
    hits.sort_by(|a, b| {
        b.is_definition
            .cmp(&a.is_definition)
            .then_with(|| b.on_branch.cmp(&a.on_branch))
            .then_with(|| b.occurrences.cmp(&a.occurrences))
            .then_with(|| a.id.cmp(&b.id))
    });
    hits
}

/// Move every id in `exact_ids` to the front of the fused candidate list.
///
/// Why: the floor is a RANKING rule, but it cannot rank a chunk that the
/// pipeline already discarded — `materialize_search_results` truncates to
/// `top_k`, and in the #7675 spike the literal-carrying chunk was outside that
/// window (L1) or absent from the fused list altogether. Promotion is what
/// buys the recall the floor then orders.
/// What: emits the exact ids first, in lane order, keeping each one's fused
/// score where it had one and `0.0` where it is newly injected; then the
/// remaining candidates in their existing order.
/// Test: `exact_hit_outside_top_k_is_promoted_into_the_results`.
pub(crate) fn promote_candidates(
    all: Vec<(String, f32)>,
    exact_ids: &[String],
) -> Vec<(String, f32)> {
    if exact_ids.is_empty() {
        return all;
    }
    let wanted: HashSet<&str> = exact_ids.iter().map(String::as_str).collect();
    let existing: std::collections::HashMap<&str, f32> =
        all.iter().map(|(id, s)| (id.as_str(), *s)).collect();
    let mut out: Vec<(String, f32)> = exact_ids
        .iter()
        .map(|id| {
            let score = existing.get(id.as_str()).copied().unwrap_or(0.0);
            (id.clone(), score)
        })
        .collect();
    out.extend(
        all.into_iter()
            .filter(|(id, _)| !wanted.contains(id.as_str())),
    );
    out
}

/// Apply the rank floor to a materialised result page.
///
/// Why: the floor is the whole fix — a chunk carrying the literal must never
/// rank below a chunk that does not, whatever the semantic lane believed. It is
/// a floor rather than a multiplier so no amount of vector similarity can lift a
/// non-matching chunk past a matching one. See #7675.
/// What: re-scores each literal-carrying chunk to `max_non_matching + 1.0 + r`,
/// where `r` falls strictly inside `(0, 1)` with the [`rank_hits`] order, leaves
/// every other chunk's score untouched, and re-sorts. Position order and score
/// order therefore agree, so a caller that re-sorts by `score` sees the same
/// ranking. Returns whether anything was floored.
/// Test: `every_literal_carrying_chunk_outranks_every_chunk_without_it`.
pub(crate) fn apply_floor(results: &mut [CodeChunk], lit: &ExactLiteral, re: &Regex) -> bool {
    let hits = rank_hits(
        results.iter().map(|c| {
            (
                c.id.as_str(),
                c.content.as_str(),
                c.function_name.as_deref(),
                c.on_branch,
            )
        }),
        lit,
        re,
    );
    if hits.is_empty() {
        return false;
    }
    let rank_of: std::collections::HashMap<&str, usize> = hits
        .iter()
        .enumerate()
        .map(|(i, h)| (h.id.as_str(), i))
        .collect();
    let floor_base = results
        .iter()
        .filter(|c| !rank_of.contains_key(c.id.as_str()))
        .map(|c| c.score)
        .fold(0.0_f32, f32::max)
        + 1.0;
    let n = hits.len() as f32;
    for chunk in results.iter_mut() {
        if let Some(&rank) = rank_of.get(chunk.id.as_str()) {
            chunk.score = floor_base + (n - rank as f32) / (n + 1.0);
        }
    }
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    true
}

impl CodeIndexer {
    /// Corpus lane: every chunk carrying `lit`, best-first, capped at `want`.
    ///
    /// Why: BM25 cannot be relied on to surface a literal — it splits compound
    /// identifiers into common words and scores a chunk that merely shares those
    /// words above the one chunk that carries the identifier whole. A direct
    /// scan is the only lane that cannot miss. See #7675.
    /// What: mirrors `grep_fallback_search`'s rehydrate-race handling, matching
    /// `re` against the in-memory chunk map and honouring the same path/repo
    /// `filter` every other lane takes. Declines a `Phrase` that matches more
    /// than [`PHRASE_HIT_CAP`] chunks — that is boilerplate, not a literal.
    ///
    /// It also applies `mode`'s own file-type and docstring rules HERE rather
    /// than leaving them to `apply_archive_downrank`, which runs after
    /// truncation: a promoted hit the mode filter then deletes costs a `top_k`
    /// slot and returns nothing in its place, which is how this lane first
    /// emptied `test_code_mode_source_outranks_changelog_pre_truncation`.
    /// Test: `conceptual_ranking_is_unchanged_by_the_floor`,
    /// `a_common_phrase_does_not_earn_the_floor`,
    /// `test_code_mode_source_outranks_changelog_pre_truncation`.
    pub(crate) async fn exact_match_lane(
        &self,
        lit: &ExactLiteral,
        re: &Regex,
        want: usize,
        mode: super::super::SearchMode,
        filter: Option<&(dyn Fn(&str) -> bool + Send + Sync)>,
    ) -> Vec<ExactHit> {
        if want == 0 {
            return Vec::new();
        }
        let mode_admits = |raw: &RawChunk| {
            if matches!(mode, super::super::SearchMode::Code)
                && matches!(raw.chunk_type, crate::core::chunker::ChunkType::Docstring)
            {
                return false;
            }
            super::docs_penalty::is_allowed_for_mode(&raw.file, mode)
        };
        for _ in 0..super::lanes::REHYDRATE_RACE_RETRIES {
            self.ensure_chunks_loaded().await;
            let chunks = self.chunks.read().await;
            if chunks.is_empty() && self.chunks_evicted.load(Ordering::Relaxed) {
                continue;
            }
            let admitted: Vec<&RawChunk> = chunks
                .values()
                .filter(|raw| filter.is_none_or(|f| f(&raw.id)) && mode_admits(raw))
                .collect();
            let mut hits = rank_hits(
                admitted.iter().map(|raw| {
                    (
                        raw.id.as_str(),
                        raw.content.as_str(),
                        raw.function_name.as_deref(),
                        false,
                    )
                }),
                lit,
                re,
            );
            if matches!(lit.shape, LiteralShape::Phrase) && hits.len() > PHRASE_HIT_CAP {
                return Vec::new();
            }
            hits.truncate(want);
            return hits;
        }
        // The chunk map never rehydrated within the bounded waits — the same
        // degrade `grep_fallback_search` reports. Answering with no exact lane
        // is the honest outcome; the lexical lane's own flag already says the
        // index is warming up. See #3683.
        Vec::new()
    }
}
