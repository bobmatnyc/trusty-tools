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
//!
//! The gate admits only shapes a caller could have typed into ripgrep: a quoted
//! span, an identifier carrying a distinguishing signal, a bare filename, and an
//! issue reference. An UNQUOTED multi-word phrase is a conceptual query and
//! earns nothing — flooring one let a prose sentence that happened to occur
//! verbatim in one CHANGELOG line take the top slot from every chunk the
//! semantic lane judged relevant.
//! Test: `crates/trusty-search/src/core/indexer/tests/exact_match_floor.rs`.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;

use regex::Regex;

use super::super::{CodeChunk, CodeIndexer, RawChunk};

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

/// Upper bound on how many chunks a single `Filename` match may float to the
/// floor (#7675 round 3).
///
/// Why: a basename as common as `mod.rs` (510 occurrences in this repo alone)
/// would otherwise flood the whole top-k page with an effectively arbitrary
/// subset of unrelated files — the identical flood shape the now-deleted
/// `PHRASE_HIT_CAP` existed to prevent for the old `Phrase` shape. Reusing a
/// small constant cap here, rather than relying solely on the broader `want`
/// oversample budget `exact_match_lane` already truncates every shape to, is
/// what keeps one ambiguous basename from dominating the page the way `want`
/// (≈4x `top_k`) alone does not.
/// What: applied in [`CodeIndexer::exact_match_lane`] on top of (never instead
/// of) the existing `want` truncation — `hits.truncate(want.min(FILENAME_HIT_CAP))`
/// — after [`rank_hits`] has already ordered an exact path-suffix match ahead
/// of a bare-basename-only one, so truncation keeps the most meaningful
/// matches rather than an arbitrary id-ordered subset.
///
/// #7775: "most meaningful" was only true ACROSS the two tiers. Within one tier
/// every hit tied, so the cap kept the alphabetically-first 8 and floored them
/// above the semantic page. The cap is unchanged; what it now truncates is a
/// list ordered by `HitInput::tie_score`, so the 8 it keeps are the 8 the other
/// lanes ranked best.
/// Test: `a_common_basename_is_capped_and_does_not_bury_the_relevant_file`,
/// `tied_filename_hits_are_ordered_by_lane_score_not_chunk_id`.
pub(crate) const FILENAME_HIT_CAP: usize = 8;

/// Extensions a bare token must carry to read as a filename rather than prose.
///
/// Why: `session_mcp_scope.rs` typed as a query means "show me that file", and
/// ripgrep finds it trivially — but the `.` makes it fail `is_bare_identifier`,
/// so before #7675's second round no filename query reached the floor at all.
/// Requiring a known source/doc extension keeps an ordinary sentence ending in
/// a period (`the cache is warm.`) out of the shape.
/// What: matched against the token's final dotted segment, case-sensitively.
/// Test: `extract_exact_literal_reads_the_query_shapes`.
const FILENAME_EXTENSIONS: &[&str] = &[
    "rs", "md", "toml", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "h", "cpp", "hpp",
    "yaml", "yml", "json", "sh", "sql", "svelte", "txt",
];

/// Which query shape earned the floor.
///
/// Why: the shapes differ in what they match against — content for three of
/// them, the chunk's own path for [`LiteralShape::Filename`] — and in how
/// rigid the match is (whitespace is flexible inside a quoted span, rigid
/// inside an identifier). Test: `extract_exact_literal` unit tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiteralShape {
    /// A single code identifier: `render_savings_segment`, `Config::new`,
    /// `SavingsTotal`, or a `fn`/`struct`/`const`-prefixed form.
    Identifier,
    /// Text the caller quoted explicitly.
    Quoted,
    /// A bare filename (`session_mcp_scope.rs`) or a path-shaped suffix
    /// (`indexer/search/exact.rs`). Matched against each chunk's path, never
    /// its content — a path-shaped literal requires the chunk's whole path to
    /// end with that multi-segment suffix; a bare one matches the basename.
    /// See [`filename_match_tier`] for the two-tier rule #7675 round 3 added
    /// so a precise path-suffix match outranks a same-basename-only one.
    Filename,
    /// An issue reference (`#7675`), matched verbatim in content.
    IssueRef,
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
/// semantic ranking from a literal one without re-running the query — and must
/// be able to tell a lane that found nothing from one that never ran.
/// What: `applied` is whether any chunk was floored; `literal` is the text that
/// was matched — present whenever a literal was EXTRACTED, so a caller can also
/// see that a literal was recognised and simply found nothing; `degraded` says
/// the lane could not run at all; `full_scan` says the postings prefilter was
/// unavailable and the lane fell back to scanning every chunk's content.
/// Test: `search_meta_reports_the_exact_match_floor`,
/// `exact_match_lane_degrades_observably_on_exhausted_retries`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ExactMatchReport {
    /// `true` when at least one chunk carried the literal and was floored.
    pub applied: bool,
    /// The literal the query was read as asking for, `None` when the query was
    /// conceptual.
    pub literal: Option<String>,
    /// `true` when the lane could not read the corpus and returned nothing for
    /// that reason — NOT because the literal is absent. See #7675, #3683.
    pub degraded: bool,
    /// `true` when the BM25 postings prefilter was unavailable, so the lane
    /// matched the literal against every chunk's content.
    pub full_scan: bool,
}

/// Decide whether a query names a literal, and what that literal is.
///
/// Why: the floor must fire for the shapes a caller would have reached for
/// ripgrep with, and stay completely out of the way for conceptual ones — this
/// function is the whole gate. See #7675.
/// What: quoted spans win first, then an issue reference, a bare filename, a
/// bare identifier carrying a distinguishing signal, or such an identifier
/// behind one declaration keyword. Everything else — a bare English word, a
/// single unsignalled type name, and every unquoted multi-word phrase —
/// returns `None`, which is the conceptual path and leaves ranking untouched.
/// Test: `extract_exact_literal_reads_the_query_shapes`,
/// `an_unquoted_phrase_earns_no_literal_even_when_it_occurs_verbatim`.
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
        [one] if is_issue_ref(one) => Some(ExactLiteral {
            text: (*one).to_string(),
            shape: LiteralShape::IssueRef,
        }),
        [one] if is_filename_token(one) => Some(ExactLiteral {
            text: (*one).to_string(),
            shape: LiteralShape::Filename,
        }),
        [one] if is_identifier_shaped(one) => Some(ExactLiteral {
            text: (*one).to_string(),
            shape: LiteralShape::Identifier,
        }),
        // The declaration keyword IS the disambiguation: `fn fold` asks for a
        // definition where the bare word `fold` is a conceptual query, so the
        // weaker `is_bare_identifier` is deliberate here.
        [kw, name]
            if DEF_KEYWORDS.contains(&kw.to_ascii_lowercase().as_str())
                && is_bare_identifier(name) =>
        {
            Some(ExactLiteral {
                text: (*name).to_string(),
                shape: LiteralShape::Identifier,
            })
        }
        _ => None,
    }
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

/// `#` followed by at least one digit and nothing else — `#7675`.
fn is_issue_ref(tok: &str) -> bool {
    let Some(digits) = tok.strip_prefix('#') else {
        return false;
    };
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// A bare filename (`[A-Za-z0-9_.-]+`) or a path-shaped suffix
/// (`[A-Za-z0-9_./-]+`, e.g. `indexer/search/exact.rs`), either way ending in
/// a known source/doc extension.
///
/// Why: `indexer/search/exact.rs` typed as a query means "show me that exact
/// file, unambiguously" — the same ripgrep-parity intent `session_mcp_scope.rs`
/// already earns, just spelled with enough path to disambiguate a common
/// basename. #7675 round 3.
/// Test: `extract_exact_literal_reads_the_query_shapes`,
/// `a_path_shaped_query_returns_its_file_first`.
fn is_filename_token(tok: &str) -> bool {
    let Some((stem, ext)) = tok.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || !FILENAME_EXTENSIONS.contains(&ext) {
        return false;
    }
    stem.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/')
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
/// semantic lane. The signal is what separates the two. A single Capitalized
/// name with no internal boundary (`Palace`) stays out for the same reason.
/// See #7675.
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

/// Build the content matcher for a literal.
///
/// Why: an identifier must not match inside a longer identifier (`is_trusted`
/// must not hit `is_trusted_root`), while a quoted string must survive being
/// wrapped across lines in a string literal or a doc comment.
/// What: identifiers and issue references get `\b`-anchored escaped text —
/// anchored only at an end whose character is a word character, so `#7675`
/// takes a trailing boundary and no leading one. Quoted text gets escaped text
/// whose internal whitespace runs become `\s+`. A [`LiteralShape::Filename`]
/// literal is matched against the chunk's PATH, never its content, so the regex
/// returned for it is the escaped text and is never consulted — it exists so
/// the pipeline carries one uniform `Option<Regex>`.
/// Test: `literal_regex_anchors_an_identifier_at_word_boundaries`.
pub(crate) fn literal_regex(lit: &ExactLiteral) -> Option<Regex> {
    let pattern = match lit.shape {
        LiteralShape::Identifier | LiteralShape::IssueRef | LiteralShape::Filename => {
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
        LiteralShape::Quoted => lit
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
/// keeps the lane's cost one pass over the candidates. See #7675.
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
        self.name_re
            .find_iter(content)
            .any(|m| kw.is_match(&content[..m.start()]))
    }
}

/// One chunk offered to [`rank_hits`], with every signal the order needs.
pub(crate) struct HitInput<'a> {
    pub(crate) id: &'a str,
    pub(crate) file: &'a str,
    pub(crate) content: &'a str,
    pub(crate) function_name: Option<&'a str>,
    /// The caller's `branch_files` preference for this chunk.
    pub(crate) on_branch: bool,
    /// Whether `apply_archive_downrank` stamped this chunk with a reason —
    /// archived, stale, legacy, deprecated, or docs-penalised. Always `false`
    /// in the corpus lane, which runs before that pass exists.
    pub(crate) downranked: bool,
    /// The fused lane score this chunk already carries, used ONLY to order
    /// [`LiteralShape::Filename`] hits that tie on every other key (#7775).
    /// [`rank_hits`] zeroes it for every other shape, so supplying it is
    /// always safe — see that function for why the other shapes must not
    /// consult it. `0.0` when no lane surfaced the chunk at all.
    pub(crate) tie_score: f32,
}

/// One chunk that carries the literal, with the signals that order it.
#[derive(Debug, Clone)]
pub(crate) struct ExactHit {
    pub(crate) id: String,
    pub(crate) is_definition: bool,
    pub(crate) occurrences: usize,
    pub(crate) on_branch: bool,
    pub(crate) downranked: bool,
    /// See [`HitInput::tie_score`]. Already zeroed for a non-`Filename` shape.
    pub(crate) tie_score: f32,
}

/// Borrow a corpus chunk as a [`HitInput`], carrying `tie_score` as the
/// chunk's fused lane score (`0.0` when no lane surfaced it). The two ordering
/// signals the corpus lane cannot know — the caller's branch preference and the
/// archive verdict — are `false`; both are filled in by [`apply_floor`], which
/// runs after the passes that compute them.
fn as_hit_input(raw: &RawChunk, tie_score: f32) -> HitInput<'_> {
    HitInput {
        id: raw.id.as_str(),
        file: raw.file.as_str(),
        content: raw.content.as_str(),
        function_name: raw.function_name.as_deref(),
        on_branch: false,
        downranked: false,
        tie_score,
    }
}

/// The final path segment of `path`, which is what a bare filename query
/// matches.
fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Whether `file` carries a [`LiteralShape::Filename`] literal, and at which
/// of two tiers — `Some(true)` for an exact path-suffix (or whole-path) match,
/// `Some(false)` for a same-basename-only match, `None` for no match at all.
///
/// Why: #7675 round 2 matched a `Filename` literal against the basename
/// alone, so `mod.rs` (510 files sharing that basename in this repo) floored
/// every one of them with no way to tell a precise match from an incidental
/// one. A caller who types a multi-segment query
/// (`indexer/search/exact.rs`) is disambiguating on purpose; a chunk whose
/// whole path ends with that suffix is the higher-confidence match and must
/// rank ahead of a chunk that merely shares the literal's final segment.
/// What: the higher tier requires `file` to equal `lit_text` or end with
/// `/{lit_text}`; the lower tier falls back to comparing only each side's
/// final path segment. A bare (single-segment) literal's two tiers coincide,
/// so it is unaffected — every match it has is already the higher tier.
/// Test: `a_path_shaped_query_returns_its_file_first`,
/// `a_common_basename_is_capped_and_does_not_bury_the_relevant_file`.
fn filename_match_tier(file: &str, lit_text: &str) -> Option<bool> {
    if file == lit_text || file.ends_with(&format!("/{lit_text}")) {
        Some(true)
    } else if basename(file) == basename(lit_text) {
        Some(false)
    } else {
        None
    }
}

/// How many times `lit` occurs in one chunk — content for every shape except
/// [`LiteralShape::Filename`], which matches the chunk's path via
/// [`filename_match_tier`] rather than its content.
fn occurrences_of(lit: &ExactLiteral, re: &Regex, input: &HitInput<'_>) -> usize {
    match lit.shape {
        LiteralShape::Filename => usize::from(filename_match_tier(input.file, &lit.text).is_some()),
        _ => re.find_iter(input.content).count(),
    }
}

/// Score every literal-carrying chunk in `items` against `lit`.
///
/// Why: shared by the corpus lane (recall) and the floor (ranking) so the two
/// can never disagree about what counts as a hit. The ordering key deliberately
/// excludes the fused lane score: once a chunk carries the literal, letting a
/// semantic score pick WHICH occurrence wins reintroduces the exact failure
/// #7675 is about, and makes `search` and `search_lexical` disagree on top-1
/// whenever two chunks declare the same name.
/// What: matches each chunk against `lit`, counting occurrences and flagging
/// declarations; returns hits ordered by (declaration, live-before-downranked,
/// caller-declared branch preference, occurrence count, chunk id) — every key
/// lane-independent, so the order is fully deterministic. `downranked` sits
/// directly after `is_definition` so an archived or stale duplicate of a
/// declaration can never outrank the live one; it reuses the verdict
/// `apply_archive_downrank` already computed rather than reclassifying.
/// `on_branch` is in the key because the caller's `branch_files` boost is a
/// declared preference the floor must not swallow; it reaches this function as
/// a flag rather than as the score multiplier the pipeline applied.
///
/// For [`LiteralShape::Filename`], `is_definition` is repurposed (not a second
/// field — #7675 round 3) to carry [`filename_match_tier`]'s verdict: `true`
/// for an exact path-suffix match, `false` for a same-basename-only one. This
/// is the same "precise match floats to the top of its group" mechanism the
/// `Identifier` shape already uses for a real declaration, so a path-shaped
/// query (`indexer/search/exact.rs`) outranks every chunk that only shares its
/// final segment, using the existing sort key rather than a new one.
///
/// #7775: that tier is the ONLY discriminator a `Filename` hit has — its
/// occurrence count is always 1 — so 31 files ending `src/lib.rs` tied on every
/// key and fell back to chunk id, and `FILENAME_HIT_CAP` then floored the
/// alphabetically-first 8 above everything the semantic lanes ranked. `tie_score`
/// (the fused lane score) breaks that tie BELOW the tier, so a strictly better
/// suffix match still wins and chunk id still settles a genuine draw. The score
/// is admitted for this shape ONLY: for the content-matching shapes, letting it
/// pick WHICH occurrence wins reintroduces the #7675 failure, so `rank_hits`
/// zeroes the field for them rather than trusting every caller to.
/// Test: `hybrid_top_one_agrees_with_lexical_for_every_planted_identifier`,
/// `an_archived_duplicate_ranks_below_the_live_definition`,
/// `test_branch_boost_applied_to_matching_chunks`,
/// `a_path_shaped_query_returns_its_file_first`,
/// `tied_filename_hits_are_ordered_by_lane_score_not_chunk_id`.
fn rank_hits<'a, I>(items: I, lit: &ExactLiteral, re: &Regex) -> Vec<ExactHit>
where
    I: Iterator<Item = HitInput<'a>>,
{
    // Built once per query, never per chunk — see [`DefinitionMatcher`].
    let definition = DefinitionMatcher::new(lit);
    // #7775: only the path-matching shape may consult the lane score.
    let scored_shape = matches!(lit.shape, LiteralShape::Filename);
    let mut hits: Vec<ExactHit> = items
        .filter_map(|input| {
            let occurrences = occurrences_of(lit, re, &input);
            if occurrences == 0 {
                return None;
            }
            let is_definition = match lit.shape {
                LiteralShape::Filename => {
                    filename_match_tier(input.file, &lit.text).unwrap_or(false)
                }
                _ => definition
                    .as_ref()
                    .is_some_and(|d| d.matches(input.content, input.function_name)),
            };
            Some(ExactHit {
                id: input.id.to_string(),
                is_definition,
                occurrences,
                on_branch: input.on_branch,
                downranked: input.downranked,
                tie_score: if scored_shape { input.tie_score } else { 0.0 },
            })
        })
        .collect();
    hits.sort_by(|a, b| {
        b.is_definition
            .cmp(&a.is_definition)
            .then_with(|| a.downranked.cmp(&b.downranked))
            .then_with(|| b.on_branch.cmp(&a.on_branch))
            .then_with(|| b.occurrences.cmp(&a.occurrences))
            // #7775: zero for every non-`Filename` shape, so this is a no-op
            // there and the key below stays exactly the #7675 one.
            .then_with(|| b.tie_score.total_cmp(&a.tie_score))
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
/// ranking. The archive multiplier `apply_archive_downrank` applied to a floored
/// chunk's score is overwritten here, which is why its verdict travels into the
/// ordering key as `downranked` instead. Returns whether anything was floored.
/// Test: `every_literal_carrying_chunk_outranks_every_chunk_without_it`,
/// `an_archived_duplicate_ranks_below_the_live_definition`.
pub(crate) fn apply_floor(results: &mut [CodeChunk], lit: &ExactLiteral, re: &Regex) -> bool {
    let hits = rank_hits(
        results.iter().map(|c| HitInput {
            id: c.id.as_str(),
            file: c.file.as_str(),
            content: c.content.as_str(),
            function_name: c.function_name.as_deref(),
            on_branch: c.on_branch,
            downranked: c.archive_reason.is_some(),
            // #7775: the page's own pre-floor score is this stage's copy of the
            // same fused signal the lane ranked its `Filename` hits by.
            tie_score: c.score,
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

/// What one run of the exact-match lane produced, and how it produced it.
///
/// Why: an empty `hits` alone cannot tell "the literal is absent" from "the
/// lane never ran", which is the fail-open shape the sibling lanes already
/// close with `meta.bm25_lane_degraded`. See #7675, #3683.
/// What: the hits, plus the two facts the response `meta` publishes.
/// Test: `exact_match_lane_degrades_observably_on_exhausted_retries`.
#[derive(Debug, Default)]
pub(crate) struct ExactLaneOutcome {
    pub(crate) hits: Vec<ExactHit>,
    pub(crate) degraded: bool,
    pub(crate) full_scan: bool,
}

/// Which chunks the lane has to look at.
enum Candidates {
    /// The BM25 postings intersection — a superset of the verbatim matches.
    Postings(Vec<String>),
    /// Every chunk in the map, because no usable candidate set was derivable.
    AllChunks,
}

impl CodeIndexer {
    /// Chunk ids whose BM25 postings carry every token of `lit`.
    ///
    /// Why: matching the literal against every chunk's content is `O(corpus)`
    /// in bytes and was the lane's whole cost — on a 100k-chunk index that is
    /// tens of milliseconds against a documented sub-10ms p50 target. A chunk
    /// containing a literal verbatim necessarily contains all of its tokens, so
    /// the postings intersection is a sound candidate set to verify.
    /// What: tokenizes the literal with the same tokenizer the corpus was
    /// indexed with, intersects the postings, and returns the ids beside the
    /// BM25 live-document count the caller checks completeness against.
    /// `None` whenever the prefilter cannot be trusted — a path-matched
    /// filename, a literal that tokenizes to nothing, or an empty/evicted BM25
    /// index — and the caller then scans.
    ///
    /// Read OUTSIDE the `chunks` read lock deliberately: nesting the two read
    /// guards would order this lane's locks against the ingest path's.
    /// Test: `the_postings_candidate_path_and_the_full_scan_agree`.
    async fn exact_postings_candidates(&self, lit: &ExactLiteral) -> Option<(Vec<String>, usize)> {
        if matches!(lit.shape, LiteralShape::Filename) {
            return None;
        }
        let terms = trusty_common::bm25::tokenize(&lit.text);
        if terms.is_empty() {
            return None;
        }
        self.ensure_bm25_entities_loaded().await;
        let bm25 = self.bm25.read().await;
        if bm25.is_empty() {
            return None;
        }
        let ids = bm25.docs_containing_all(&terms)?;
        let live = bm25.len();
        Some((ids.into_iter().map(str::to_string).collect(), live))
    }

    /// Corpus lane: every chunk carrying `lit`, best-first, capped at `want`.
    ///
    /// Why: BM25 cannot be relied on to RANK a literal — it splits compound
    /// identifiers into common words and scores a chunk that merely shares those
    /// words above the one chunk that carries the identifier whole. Verifying
    /// the literal directly is the only lane that cannot miss. See #7675.
    /// What: takes candidates from the BM25 postings when they are complete
    /// (`bm25.len() >= chunks.len()`, so no chunk was dropped by the corpus
    /// cap) and falls back to every chunk otherwise, reporting that fallback as
    /// `full_scan`. Mirrors `grep_fallback_search`'s rehydrate-race handling,
    /// honours the same path/repo `filter` every other lane takes, and reports
    /// an exhausted rehydrate as `degraded` rather than as an empty lane.
    ///
    /// It also applies `mode`'s own file-type and docstring rules HERE rather
    /// than leaving them to `apply_archive_downrank`, which runs after
    /// truncation: a promoted hit the mode filter then deletes costs a `top_k`
    /// slot and returns nothing in its place, which is how this lane first
    /// emptied `test_code_mode_source_outranks_changelog_pre_truncation`.
    ///
    /// `tie_scores` (#7775) looks a candidate's fused lane score up by chunk
    /// id. It orders same-tier [`LiteralShape::Filename`] hits — and nothing
    /// else, see [`rank_hits`] — so the `FILENAME_HIT_CAP` truncation below
    /// keeps the best-ranked members of a tie rather than the
    /// alphabetically-first ones. `None` (every caller that has no fused list
    /// yet) leaves every hit at `0.0`, which is the pre-#7775 chunk-id order.
    /// Test: `conceptual_ranking_is_unchanged_by_the_floor`,
    /// `exact_match_lane_degrades_observably_on_exhausted_retries`,
    /// `the_postings_candidate_path_and_the_full_scan_agree`,
    /// `test_code_mode_source_outranks_changelog_pre_truncation`,
    /// `tied_filename_hits_are_ordered_by_lane_score_not_chunk_id`.
    pub(crate) async fn exact_match_lane(
        &self,
        lit: &ExactLiteral,
        re: &Regex,
        want: usize,
        mode: super::super::SearchMode,
        filter: Option<&(dyn Fn(&str) -> bool + Send + Sync)>,
        tie_scores: Option<&(dyn Fn(&str) -> f32 + Send + Sync)>,
    ) -> ExactLaneOutcome {
        if want == 0 {
            return ExactLaneOutcome::default();
        }
        let tie_score = |id: &str| tie_scores.map_or(0.0, |f| f(id));
        let mode_admits = |raw: &RawChunk| {
            if matches!(mode, super::super::SearchMode::Code)
                && matches!(raw.chunk_type, crate::core::chunker::ChunkType::Docstring)
            {
                return false;
            }
            super::docs_penalty::is_allowed_for_mode(&raw.file, mode)
        };
        let admitted = |raw: &RawChunk| filter.is_none_or(|f| f(&raw.id)) && mode_admits(raw);
        let postings = self.exact_postings_candidates(lit).await;
        for _ in 0..super::lanes::REHYDRATE_RACE_RETRIES {
            self.ensure_chunks_loaded().await;
            let chunks = self.chunks.read().await;
            if chunks.is_empty() && self.chunks_evicted.load(Ordering::Relaxed) {
                continue;
            }
            // A corpus-capped BM25 index holds fewer documents than the chunk
            // map, so its postings can miss a literal outright. Completeness is
            // the precondition for using them at all.
            let candidates = match &postings {
                Some((ids, live)) if *live >= chunks.len() => Candidates::Postings(ids.clone()),
                _ => Candidates::AllChunks,
            };
            let mut hits = match &candidates {
                Candidates::Postings(ids) => rank_hits(
                    ids.iter()
                        .filter_map(|id| chunks.get(id.as_str()))
                        .filter(|raw| admitted(raw))
                        .map(|raw| as_hit_input(raw, tie_score(&raw.id))),
                    lit,
                    re,
                ),
                Candidates::AllChunks => rank_hits(
                    chunks
                        .values()
                        .filter(|raw| admitted(raw))
                        .map(|raw| as_hit_input(raw, tie_score(&raw.id))),
                    lit,
                    re,
                ),
            };
            // #7675 round 3: a `Filename` match gets the small, dedicated
            // `FILENAME_HIT_CAP` on top of (never instead of) `want` — see
            // that constant's doc comment for why `want` alone does not
            // bound an ambiguous basename's flood.
            let cap = if matches!(lit.shape, LiteralShape::Filename) {
                want.min(FILENAME_HIT_CAP)
            } else {
                want
            };
            hits.truncate(cap);
            return ExactLaneOutcome {
                hits,
                degraded: false,
                // A filename query never scans content at all — it compares
                // path basenames — so it is not the fallback this reports.
                full_scan: matches!(candidates, Candidates::AllChunks)
                    && !matches!(lit.shape, LiteralShape::Filename),
            };
        }
        // The chunk map never rehydrated within the bounded waits. #7675: the
        // sibling lanes (`bm25_search`, `grep_fallback_search`) flip the sticky
        // per-index flag and the shared gauge here, because an empty lane is
        // otherwise bit-for-bit indistinguishable from "the literal is absent".
        // This lane makes exactly the same claim and owes the same signal.
        self.lane_degraded.store(true, Ordering::Relaxed);
        metrics::gauge!("trusty_bm25_lane_degraded", "index" => self.index_id.clone()).set(1.0);
        metrics::counter!("trusty_exact_match_lane_degraded_total", "index" => self.index_id.clone())
            .increment(1);
        tracing::warn!(
            "index '{}': chunk rehydrate still not ready after {} bounded waits — returning an \
             empty exact-match lane for this query (degraded, not a literal that is absent; \
             issue #7675)",
            self.index_id,
            super::lanes::REHYDRATE_RACE_RETRIES
        );
        ExactLaneOutcome {
            hits: Vec::new(),
            degraded: true,
            full_scan: false,
        }
    }
}
