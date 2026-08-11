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

use crate::wordnet_pos;
use chrono::Utc;
use std::collections::HashSet;
use trusty_common::memory_core::store::kg::Triple;
use uuid::Uuid;

/// Default tags that cause a drawer to be skipped during auto-extraction.
///
/// Why: Drawers tagged with these labels are by definition non-factual project
/// knowledge (test fixtures, QA scaffolding, synthetic content) and should not
/// pollute the KG with noise triples.
/// What: A static slice of lowercase tag strings; matched case-insensitively
/// during extraction.
/// Test: `extract_triples_skips_denied_tags`.
pub const DEFAULT_DENY_TAGS: &[&str] = &["cross-project-qa", "test", "fixture"];

/// Configuration for a single extraction pass.
///
/// Why: Bundles per-run configuration so `extract_triples` can be called with
/// different deny-lists (e.g. the default prod list vs. an empty list in
/// integration tests) without changing the function signature.
/// What: Contains a `deny_tags` slice; the extractor skips any drawer whose
/// tags intersect this set.
/// Test: `extract_triples_skips_denied_tags`, `extract_triples_empty_deny_list`.
#[derive(Debug, Clone)]
pub struct KgExtractConfig<'a> {
    /// Tags that cause extraction to be skipped entirely. Compared
    /// case-insensitively against the drawer's tag list.
    pub deny_tags: &'a [&'a str],
}

impl Default for KgExtractConfig<'_> {
    fn default() -> Self {
        Self {
            deny_tags: DEFAULT_DENY_TAGS,
        }
    }
}

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

/// Run the deterministic heuristic extractor with default config.
///
/// Why: Convenience wrapper that uses [`KgExtractConfig::default`] (the
/// production deny-list) so call sites that do not need a custom config
/// remain unchanged.
/// What: Delegates to [`extract_triples_with_config`] with a default config.
/// Test: All existing tests call this helper and implicitly exercise the default
/// deny-list path.
pub fn extract_triples(input: &ExtractInput<'_>) -> Vec<Triple> {
    extract_triples_with_config(input, &KgExtractConfig::default())
}

/// Run the deterministic heuristic extractor.
///
/// Why: Single entry point so `memory_remember`, `memory_note`, and the
/// back-fill CLI all share the same logic. Pure function — no I/O, no async —
/// so it can be unit-tested cheaply. Accepts a [`KgExtractConfig`] so callers
/// can override the deny-list without touching the function signature.
/// What: First checks whether any of the drawer's tags appear in
/// `config.deny_tags` (case-insensitive); when a match is found the function
/// returns immediately with an empty vec and logs a debug message. Otherwise
/// walks `tags`, content tokens, and a small pattern list to emit `Triple`s;
/// deduplicates so the same `(subject, predicate, object)` never appears twice
/// in a single pass.
/// Test: `extract_triples_skips_denied_tags`, `extract_triples_emits_tag_triples`,
/// plus all other tests in this file.
pub fn extract_triples_with_config(
    input: &ExtractInput<'_>,
    config: &KgExtractConfig<'_>,
) -> Vec<Triple> {
    // Deny-list check: if any tag on this drawer is in the deny set, skip
    // extraction entirely. The check is case-insensitive to tolerate mixed-
    // case tags from different clients.
    let denied = input.tags.iter().any(|t| {
        let lower = t.trim().to_lowercase();
        config.deny_tags.contains(&lower.as_str())
    });
    if denied {
        tracing::debug!(
            drawer_id = %input.drawer_id,
            tags = ?input.tags,
            "kg_extract: skipping drawer — tag matches deny-list"
        );
        return Vec::new();
    }
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

/// Function words that can never be a KG entity.
///
/// Why: #4678 — a [`PATTERN_TABLE`] marker hit says a marker phrase appeared,
/// not that the tokens either side of it name anything. "calling them is a
/// no-op" put `them --is-a--> no-op` into the live palace. Grouped by part of
/// speech because the boundary is grammatical, not statistical: these are the
/// closed word classes English never coins new members of, so the list is
/// finite and stable rather than a frequency cut-off that needs re-tuning.
/// What: a flat lower-case slice, matched exactly against a normalised token
/// by [`is_stop_token`]. Membership is checked with a linear scan — it runs at
/// most twice per pattern hit and at most four hits exist per drawer.
/// Test: `stopwords_are_unique`, `is_stop_token_rejects_every_stopword`.
const STOPWORDS: &[&str] = &[
    // Articles and determiners.
    "a",
    "an",
    "the",
    "this",
    "that",
    "these",
    "those",
    "some",
    "any",
    "each",
    "every",
    "all",
    "both",
    "either",
    "neither",
    "another",
    "such",
    "same",
    "no",
    "none",
    // Pronouns.
    "i",
    "me",
    "my",
    "mine",
    "myself",
    "we",
    "us",
    "our",
    "ours",
    "ourselves",
    "you",
    "your",
    "yours",
    "yourself",
    "he",
    "him",
    "his",
    "himself",
    "she",
    "her",
    "hers",
    "herself",
    "it",
    "its",
    "itself",
    "they",
    "them",
    "their",
    "theirs",
    "themselves",
    "who",
    "whom",
    "whose",
    "which",
    "what",
    "one",
    "ones",
    "someone",
    "something",
    "anyone",
    "anything",
    "everyone",
    "everything",
    "nobody",
    "nothing",
    "others",
    // Prepositions.
    "of",
    "in",
    "on",
    "at",
    "to",
    "for",
    "with",
    "by",
    "from",
    "about",
    "into",
    "onto",
    "over",
    "under",
    "below",
    "above",
    "between",
    "among",
    "through",
    "during",
    "before",
    "after",
    "across",
    "against",
    "within",
    "without",
    "upon",
    "per",
    "via",
    "than",
    "toward",
    "towards",
    "off",
    "out",
    "up",
    "down",
    "near",
    "around",
    "behind",
    "beyond",
    "beside",
    "along",
    "past",
    "throughout",
    // Conjunctions and subordinators.
    "and",
    "or",
    "but",
    "nor",
    "so",
    "yet",
    "if",
    "then",
    "else",
    "because",
    "while",
    "when",
    "where",
    "whether",
    "though",
    "although",
    "since",
    "as",
    "unless",
    "until",
    "whereas",
    // Copulas, auxiliaries and modals.
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "being",
    "am",
    "do",
    "does",
    "did",
    "done",
    "doing",
    "has",
    "have",
    "had",
    "having",
    "can",
    "could",
    "will",
    "would",
    "shall",
    "should",
    "may",
    "might",
    "must",
    "ought",
    "need",
    "needs",
    "let",
    "lets",
    "get",
    "gets",
    "got",
    "gotten",
    // Degree, negation and discourse adverbs — closed-class fillers that sit
    // next to a marker often and name nothing.
    "not",
    "only",
    "just",
    "also",
    "very",
    "too",
    "still",
    "already",
    "always",
    "never",
    "often",
    "again",
    "here",
    "there",
    "now",
    "actually",
    "really",
    "simply",
    "merely",
    "quite",
    "rather",
    "even",
    "ever",
    "once",
    "yes",
    "well",
    "much",
    "many",
    "more",
    "most",
    "less",
    "least",
    "few",
    "several",
    "enough",
    "almost",
    "perhaps",
    "maybe",
    "instead",
    "otherwise",
    "hence",
    "therefore",
    "however",
    "thus",
    "moreover",
];

/// Minimum character length for a token to be accepted as an entity.
///
/// Why: one- and two-character tokens are overwhelmingly punctuation debris,
/// initials, or list markers rather than entities.
/// What: `3`. Tokens shorter than this are rejected unless they appear in
/// [`SHORT_ENTITY_ALLOWLIST`].
/// Test: `extract_triples_rejects_short_token_off_allowlist`.
pub const MIN_ENTITY_TOKEN_LEN: usize = 3;

/// Short tokens that are real entities in this workspace's subject matter.
///
/// Why: [`MIN_ENTITY_TOKEN_LEN`] would otherwise reject the languages, crate
/// aliases, and infrastructure abbreviations these palaces actually discuss —
/// a precision filter that silently drops `Go` and `C` costs recall for
/// nothing. The crate aliases (`tm`, `ts`, `tc`, `ta`) come from this repo's
/// own abbreviation table.
/// What: lower-case tokens of fewer than [`MIN_ENTITY_TOKEN_LEN`] characters
/// that bypass the length floor. The stopword check still applies first, so an
/// entry here cannot resurrect a function word.
/// Test: `extract_triples_keeps_allowlisted_go`,
/// `extract_triples_keeps_allowlisted_c`,
/// `short_entity_allowlist_entries_survive_the_length_floor`.
pub const SHORT_ENTITY_ALLOWLIST: &[&str] = &[
    // Languages and language-shaped tokens.
    "go", "c", "c#", "js", "ts", "py", "ml", // Domains and infrastructure.
    "ai", "kg", "db", "ui", "os", "io", "vm", "ip", // Process and workspace aliases.
    "pr", "ci", "qa", "pm", "tm", "tc", "ta",
];

/// Punctuation stripped from a token's edges before it is classified.
///
/// Why: `last_token` / `first_token` only strip a trailing run, so a token can
/// still arrive as `("the` or `` `redb` ``. Comparing that against
/// [`STOPWORDS`] would miss, and the filter would leak exactly the tokens it
/// exists to catch.
/// What: the edge characters [`is_stop_token`] trims before matching. Interior
/// characters are untouched, so `no-op` and `c#` survive intact.
/// Test: `is_stop_token_normalises_surrounding_punctuation`.
const TOKEN_EDGE_PUNCT: &[char] = &[
    '(', ')', '[', ']', '{', '}', '<', '>', '"', '\'', '`', ',', '.', ';', ':', '!', '?', '*', '_',
    '-', '/', '\\', '|', '—', '–', '…', '“', '”', '‘', '’',
];

/// Whether `tok` must be refused as a KG entity.
///
/// Why: #4678 — the pattern pass treated any whitespace-delimited token beside
/// a marker as an entity, which is how `them --is-a--> no-op`,
/// `exhaustiveness --is-a--> hard`, and `squash --is-a--> ancestor` reached the
/// live graph. This is the single gate both the extractor and the
/// `--purge-stale-subjects` back-fill consult, so forward extraction and
/// historical clean-up can never disagree about what counts as garbage.
/// What: normalises `tok` through [`clean_token`], lower-cases it, and rejects
/// it when the result is empty, appears in [`STOPWORDS`], or is shorter than
/// [`MIN_ENTITY_TOKEN_LEN`] without being in [`SHORT_ENTITY_ALLOWLIST`].
/// Length is counted in `char`s, not bytes, so a multi-byte token is not
/// mis-measured. Purely lexical: it judges the token alone and knows nothing of
/// the sentence, so it cannot catch a triple whose tokens are both ordinary
/// words (see #4678 for the residue).
///
/// The normalisation step is NOT redundant with `clean_token`'s use in
/// `first_token` / `last_token`. Those clean tokens on the way IN, which only
/// helps content extracted from now on. The `--purge-stale-subjects` path calls
/// this on subjects read back out of redb, and those were written by the old
/// extractor with the punctuation already welded on — normalising here is what
/// lets a stored `("the` be recognised as the stopword it is.
/// Test: `is_stop_token_rejects_every_stopword`,
/// `is_stop_token_accepts_ordinary_entities`,
/// `is_stop_token_normalises_surrounding_punctuation`,
/// `purge_selects_a_legacy_subject_with_welded_punctuation`.
pub fn is_stop_token(tok: &str) -> bool {
    let norm = clean_token(tok).to_lowercase();
    if norm.is_empty() {
        return true;
    }
    if STOPWORDS.contains(&norm.as_str()) {
        return true;
    }
    norm.chars().count() < MIN_ENTITY_TOKEN_LEN && !SHORT_ENTITY_ALLOWLIST.contains(&norm.as_str())
}

/// Apply the pattern table to a single content blob.
///
/// Why: Keeps the matching loop out of `extract_triples` so the dispatcher
/// stays readable.
/// What: For every `(predicate, markers)` row, scan every marker against the
/// lower-cased content; on the first hit emit `(left_token, predicate,
/// right_token)` and move on to the next predicate. Only the first hit per
/// predicate is taken to avoid combinatorial output on long texts. A hit whose
/// subject or object fails [`is_stop_token`] emits nothing and still consumes
/// the predicate's turn, so one rejected hit never cascades into a scan for a
/// second, lower-quality match later in the blob.
/// Test: `extract_triples_extracts_is_a_pattern`,
/// `extract_triples_rejects_pronoun_subject`,
/// `extract_triples_rejects_stopword_object`.
fn extract_patterns(content: &str) -> Vec<(String, String, String)> {
    let lower = content.to_lowercase();
    let wn = wordnet_pos::wordnet();
    let mut out: Vec<(String, String, String)> = Vec::new();
    for (predicate, markers) in PATTERN_TABLE {
        for marker in *markers {
            if let Some(idx) = lower.find(marker) {
                let left = lower[..idx].trim();
                let right_start = idx + marker.len();
                let right = lower[right_start..].trim();
                let subject_tok = last_token(left);
                // #4678: a marker hit is not evidence of two entities — reject
                // the whole triple when either side is a function word or too
                // short, never half of it.
                // #5399: and reject when POS says the token names a property
                // rather than a thing, on either side.
                let subject_ok =
                    !is_stop_token(&subject_tok) && !wn.is_adjective_only(&subject_tok);
                if subject_ok {
                    if let Some(object_tok) = select_object(right, wn) {
                        out.push((subject_tok, (*predicate).to_string(), object_tok));
                    }
                }
                break;
            }
        }
    }
    out
}

/// Prepositions that continue a noun phrase rather than closing it.
///
/// Why: #5399 rule 3 — `ancestor of origin/main` and `member of the process
/// group` are single noun phrases, so grabbing `ancestor` or `member` alone
/// truncates a relation into a bogus type. The stopping token is what reveals
/// it. The set is deliberately just `of`: it is the genitive linker and is
/// almost always NP-internal, whereas every other preposition attaches
/// ambiguously — `trusty-memory uses redb for persistence` has `for` closing
/// the object and opening a purpose adjunct on the verb, and that triple must
/// survive.
/// What: matched against the cleaned token immediately after the extracted
/// noun-phrase run; a hit rejects the whole triple.
/// Test: `rejects_ancestor_before_of`, `rejects_member_of_the_process_group`,
/// `keeps_uses_object_before_a_non_genitive_preposition`.
const NP_CONTINUING_PREPOSITIONS: &[&str] = &["of"];

/// Characters that close a noun phrase when welded to a token's trailing edge.
///
/// Why: `is a parser, and ...` must not walk the run into the next clause.
/// What: consulted on the RAW token before [`clean_token`] strips it.
/// Test: `stops_the_noun_phrase_run_at_a_comma`.
const NP_TERMINATING_PUNCT: &[char] = &['.', ',', ';', ':', '!', '?', ')'];

/// Longest noun-phrase run the object walk will consider.
///
/// Why: a bound keeps a pathological line (a long unpunctuated list of unknown
/// tokens) from letting the head drift far from the marker. Four covers
/// `[adj] [noun] [noun]` plus one, which is past the length of any real
/// technical compound in drawer prose.
/// Test: `caps_the_noun_phrase_run`.
const NP_RUN_MAX: usize = 4;

/// Pick the object entity out of the text following a pattern marker.
///
/// Why: #5399 — `first_token` grabs one token, which is wrong three ways at
/// once. It takes the modifier instead of the head (`a fast parser` -> `fast`),
/// it accepts a bare property as a type (`a hard requirement` -> `hard`), and
/// it silently truncates a relation into a type (`an ancestor of origin main`
/// -> `ancestor`). All three need to see more than one token and need to know
/// each token's part of speech.
/// What: walks forward from the marker collecting a noun-phrase run, stopping
/// at a function word, a verb-only or adverb-only token, trailing sentence
/// punctuation, an unknown token (a name is a head, not a modifier), or
/// [`NP_RUN_MAX`]. Then: rejects when the first token is adjective-only (rule
/// 1); rejects when the token that stopped the run is in
/// [`NP_CONTINUING_PREPOSITIONS`] (rule 3); otherwise returns the LAST
/// non-adjective-only token in the run, which is the head of a right-headed
/// English compound (rule 2). Every WordNet miss widens the accepted set rather
/// than narrowing it, so an unknown crate name is never rejected for being
/// unknown (rule 4).
/// Test: `selects_the_head_noun_past_an_adjective`,
/// `rejects_adjective_only_object`, `rejects_ancestor_before_of`,
/// `unknown_object_token_is_accepted`.
fn select_object(right: &str, wn: &wordnet_pos::WordNetPos) -> Option<String> {
    let raw_toks: Vec<&str> = right.split_whitespace().collect();
    let mut run: Vec<&str> = Vec::new();
    let mut idx = 0usize;
    let mut terminated = false;
    while idx < raw_toks.len() && run.len() < NP_RUN_MAX {
        let raw = raw_toks[idx];
        let tok = clean_token(raw);
        if tok.is_empty() || is_stop_token(tok) {
            break;
        }
        let mask = wn.mask(tok);
        // A word WordNet knows only as a verb or only as an adverb cannot sit
        // inside a noun phrase, so the phrase ended before it.
        if mask != 0 && mask & (wordnet_pos::NOUN | wordnet_pos::ADJ) == 0 {
            break;
        }
        run.push(tok);
        idx += 1;
        if raw.ends_with(NP_TERMINATING_PUNCT) {
            terminated = true;
            break;
        }
        // An unknown token is almost always a proper name in this corpus, and a
        // name heads its phrase rather than modifying the next word.
        if mask == 0 {
            break;
        }
    }
    let first = *run.first()?;
    if wn.is_adjective_only(first) {
        return None;
    }
    if !terminated {
        if let Some(next) = raw_toks.get(idx) {
            let boundary = clean_token(next);
            if NP_CONTINUING_PREPOSITIONS.contains(&boundary) {
                return None;
            }
        }
    }
    run.iter()
        .rev()
        .find(|t| !wn.is_adjective_only(t))
        .map(|t| (*t).to_string())
}

/// Pull the final whitespace-delimited token from a fragment.
///
/// Why: The left side of a pattern hit can contain arbitrary preamble; the
/// entity we care about is the noun immediately before the marker.
/// What: Returns the last whitespace-delimited token with [`TOKEN_EDGE_PUNCT`]
/// stripped from both edges.
/// Test: `extract_triples_extracts_is_a_pattern`,
/// `extract_triples_strips_punctuation_from_both_token_edges`.
fn last_token(s: &str) -> String {
    s.split_whitespace()
        .last()
        .map(clean_token)
        .unwrap_or("")
        .to_string()
}

// #5399: `first_token` is gone — the object side now walks a noun-phrase run
// through `select_object` instead of grabbing one token.

/// Strip surrounding punctuation from one raw token.
///
/// Why: #4678 — `first_token` trimmed a TRAILING run while its doc promised a
/// leading one, and both helpers used a set that omitted the characters drawer
/// content actually wraps names in (backticks, brackets, asterisks). So
/// `` `redb` `` reached the graph verbatim and became a second node for an
/// entity that already had one. Which SIDE of the marker a token sits on says
/// nothing about which edge carries punctuation, so both helpers clean both
/// edges through this one function rather than each guessing.
/// What: trims whitespace, then [`TOKEN_EDGE_PUNCT`] from both ends. Interior
/// characters are untouched, so `no-op`, `c#`, and `src/main.rs` survive whole.
/// Test: `extract_triples_strips_punctuation_from_both_token_edges`.
fn clean_token(raw: &str) -> &str {
    raw.trim().trim_matches(TOKEN_EDGE_PUNCT)
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

    /// Why: Drawers tagged with deny-listed labels (test fixtures, QA scaffolding)
    /// must not pollute the KG with non-factual content.
    /// What: A drawer with the `test` tag must produce zero triples even when
    /// it also has a room and content with extractable patterns.
    /// Test: This test.
    #[test]
    fn extract_triples_skips_denied_tags() {
        let id = Uuid::new_v4();
        let tags = vec!["test".to_string(), "rust".to_string()];
        let triples = extract_triples(&ExtractInput {
            drawer_id: id,
            content: "rustc is a compiler",
            tags: &tags,
            room: Some("Backend"),
        });
        assert!(
            triples.is_empty(),
            "a drawer with a deny-list tag must produce zero triples, got {triples:?}"
        );
    }

    /// Why: Deny-list matching is case-insensitive so `TEST` and `Test` are
    /// blocked the same as `test`.
    /// What: A drawer tagged `FIXTURE` (upper-case) must still produce zero
    /// triples.
    /// Test: This test.
    #[test]
    fn extract_triples_deny_list_is_case_insensitive() {
        let id = Uuid::new_v4();
        let tags = vec!["FIXTURE".to_string()];
        let triples = extract_triples(&ExtractInput {
            drawer_id: id,
            content: "some content",
            tags: &tags,
            room: None,
        });
        assert!(
            triples.is_empty(),
            "upper-cased deny tag must still be blocked"
        );
    }

    /// Collect only the triples produced by the [`PATTERN_TABLE`] pass.
    ///
    /// Why: every assertion about extraction precision is about the pattern
    /// pass; the tag / room / hashtag passes always fire and would otherwise
    /// mask a "zero triples" assertion.
    /// What: filters by predicate against [`PATTERN_TABLE`].
    /// Test: used by every precision test below.
    fn pattern_triples(triples: &[Triple]) -> Vec<(String, String, String)> {
        let predicates: Vec<&str> = PATTERN_TABLE.iter().map(|(p, _)| *p).collect();
        triples
            .iter()
            .filter(|t| predicates.contains(&t.predicate.as_str()))
            .map(|t| (t.subject.clone(), t.predicate.clone(), t.object.clone()))
            .collect()
    }

    /// Run the extractor over bare content with no tags and no room.
    ///
    /// Why: the precision fixtures care only about content; tags and rooms
    /// would add noise triples to every assertion.
    /// What: builds an `ExtractInput` with empty tags and no room.
    /// Test: used by every precision test below.
    fn patterns_for(content: &str) -> Vec<(String, String, String)> {
        let triples = extract_triples(&ExtractInput {
            drawer_id: Uuid::new_v4(),
            content,
            tags: &[],
            room: None,
        });
        pattern_triples(&triples)
    }

    /// Why: `them --is-a--> no-op` is live in the real trusty-tools palace
    /// (asserted 2026-08-04). A marker hit is not evidence that the tokens
    /// either side of it are entities; a pronoun never is one.
    /// What: "calling them is a no-op ..." must yield zero pattern triples.
    /// Test: This test.
    #[test]
    fn extract_triples_rejects_pronoun_subject() {
        let got = patterns_for("calling them is a no-op when the flag is off");
        assert!(
            got.is_empty(),
            "pronoun subject must reject the whole triple, got {got:?}"
        );
    }

    /// Why: a stopword can land on either side of a marker, so filtering only
    /// the subject would still admit `libpq --depends-on--> the`.
    /// What: "libpq depends on the license" must yield zero pattern triples —
    /// the triple is rejected whole, never truncated to a half-triple.
    /// Test: This test.
    #[test]
    fn extract_triples_rejects_stopword_object() {
        let got = patterns_for("libpq depends on the license header");
        assert!(
            got.is_empty(),
            "stopword object must reject the whole triple, got {got:?}"
        );
    }

    /// Why: a one- or two-character token is almost never an entity, and the
    /// extractor had no length floor at all.
    /// What: "x is a thing" must yield zero pattern triples.
    /// Test: This test.
    #[test]
    fn extract_triples_rejects_short_token_off_allowlist() {
        let got = patterns_for("x is a thing worth recording");
        assert!(
            got.is_empty(),
            "short token off the allowlist must be rejected, got {got:?}"
        );
    }

    /// Why: the length floor must not swallow the short names this repo
    /// genuinely discusses — without the allowlist, `Go` and `C` would be
    /// rejected as noise and the filter would cost real recall.
    /// What: "Go is a language" still extracts `go --is-a--> language`.
    /// Test: This test.
    #[test]
    fn extract_triples_keeps_allowlisted_go() {
        let got = patterns_for("Go is a language with green threads");
        assert!(
            got.contains(&("go".into(), "is-a".into(), "language".into())),
            "allowlisted `Go` must survive the length floor, got {got:?}"
        );
    }

    /// Why: `C` is the single-character case — the length floor's worst
    /// false positive if the allowlist is not consulted.
    /// What: "C is a language" still extracts `c --is-a--> language`.
    /// Test: This test.
    #[test]
    fn extract_triples_keeps_allowlisted_c() {
        let got = patterns_for("C is a language without a runtime");
        assert!(
            got.contains(&("c".into(), "is-a".into(), "language".into())),
            "allowlisted `C` must survive the length floor, got {got:?}"
        );
    }

    /// Why: the filter must not cost recall on ordinary, well-formed content.
    /// A rejection filter that also rejects the good cases is a regression,
    /// so every predicate in [`PATTERN_TABLE`] keeps a worked example.
    /// What: table-driven — each row is `(content, expected triple)` and must
    /// appear in the extracted pattern set.
    /// Test: This test.
    #[test]
    fn extract_triples_keeps_real_entities_across_all_predicates() {
        let cases: &[(&str, (&str, &str, &str))] = &[
            (
                "tokio is an executor for async rust",
                ("tokio", "is-a", "executor"),
            ),
            (
                "alice works at initech today",
                ("alice", "works-at", "initech"),
            ),
            (
                "trusty-memory uses redb for persistence",
                ("trusty-memory", "uses", "redb"),
            ),
            (
                "trusty-search depends on trusty-common for shared helpers",
                ("trusty-search", "depends-on", "trusty-common"),
            ),
            (
                "the embedder requires onnxruntime at startup",
                ("embedder", "depends-on", "onnxruntime"),
            ),
        ];
        for (content, (s, p, o)) in cases {
            let got = patterns_for(content);
            let want = ((*s).to_string(), (*p).to_string(), (*o).to_string());
            assert!(
                got.contains(&want),
                "content {content:?} must still extract {want:?}, got {got:?}"
            );
        }
    }

    /// Why: `first_token` trimmed only a TRAILING run while its doc promised it
    /// stripped the leading one, so an entity quoted the way drawer content
    /// actually quotes things — markdown backticks, brackets, an opening
    /// quote — entered the graph with the punctuation welded on: `` `redb ``
    /// rather than `redb`. Two spellings of one entity are two nodes that never
    /// join up.
    /// What: pins the emitted strings. Both helpers strip punctuation from BOTH
    /// edges, so the subject side (`last_token`) is covered too, and no emitted
    /// token may retain an edge character.
    /// Test: This test.
    #[test]
    fn extract_triples_strips_punctuation_from_both_token_edges() {
        let cases: &[(&str, (&str, &str, &str))] = &[
            // Object side, markdown backticks — the common shape in drawers.
            (
                "trusty-memory uses `redb` for persistence",
                ("trusty-memory", "uses", "redb"),
            ),
            // Object side, parenthesised.
            (
                "the daemon uses (tantivy) for search",
                ("daemon", "uses", "tantivy"),
            ),
            // Subject side, opening quote with no closing one before the marker.
            (
                "he said \"rustc is a compiler for rust",
                ("rustc", "is-a", "compiler"),
            ),
            // Object side, bold markdown.
            (
                "trusty-search depends on **trusty-common** for helpers",
                ("trusty-search", "depends-on", "trusty-common"),
            ),
        ];
        for (content, (s, p, o)) in cases {
            let got = patterns_for(content);
            let want = ((*s).to_string(), (*p).to_string(), (*o).to_string());
            assert!(
                got.contains(&want),
                "content {content:?} must emit {want:?}, got {got:?}"
            );
            for (subject, _, object) in &got {
                for tok in [subject, object] {
                    assert!(
                        !tok.starts_with(TOKEN_EDGE_PUNCT) && !tok.ends_with(TOKEN_EDGE_PUNCT),
                        "emitted token {tok:?} still carries edge punctuation"
                    );
                }
            }
        }
    }

    /// Why: a duplicated entry is dead weight and a sign the categories drifted
    /// apart during editing.
    /// What: every [`STOPWORDS`] entry appears exactly once and is already
    /// lower-case, which is what [`is_stop_token`] compares against.
    /// Test: This test.
    #[test]
    fn stopwords_are_unique() {
        let mut seen: HashSet<&str> = HashSet::new();
        for w in STOPWORDS {
            assert!(seen.insert(w), "duplicate stopword {w:?}");
            assert_eq!(*w, w.to_lowercase(), "stopword {w:?} must be lower-case");
        }
    }

    /// Why: the rejection contract is only as good as the list behind it.
    /// What: every [`STOPWORDS`] entry is rejected, in its own case and
    /// upper-cased, since content reaches the filter lower-cased but the purge
    /// path reads subjects straight out of the store.
    /// Test: This test.
    #[test]
    fn is_stop_token_rejects_every_stopword() {
        for w in STOPWORDS {
            assert!(is_stop_token(w), "{w:?} must be rejected");
            assert!(
                is_stop_token(&w.to_uppercase()),
                "{w:?} must be rejected case-insensitively"
            );
        }
    }

    /// Why: an over-broad filter costs recall, which is the failure mode that
    /// would make this change a net loss.
    /// What: ordinary multi-character entity names are accepted.
    /// Test: This test.
    #[test]
    fn is_stop_token_accepts_ordinary_entities() {
        for tok in [
            "rustc",
            "compiler",
            "trusty-memory",
            "redb",
            "no-op",
            "onnxruntime",
            "initech",
        ] {
            assert!(!is_stop_token(tok), "{tok:?} must be accepted");
        }
    }

    /// Why: `first_token` / `last_token` strip only a trailing run, so a token
    /// can still carry an opening bracket or backtick. Comparing that raw
    /// against the list would miss the stopword it is meant to catch.
    /// What: edge punctuation is stripped before matching, interior characters
    /// are not, and a token that is nothing but punctuation is rejected.
    /// Test: This test.
    #[test]
    fn is_stop_token_normalises_surrounding_punctuation() {
        assert!(
            is_stop_token("(the"),
            "leading bracket must not hide a stopword"
        );
        assert!(is_stop_token("`it`"), "backticks must not hide a stopword");
        assert!(
            is_stop_token("---"),
            "punctuation-only token must be rejected"
        );
        assert!(
            is_stop_token("   "),
            "whitespace-only token must be rejected"
        );
        assert!(
            !is_stop_token("`redb`"),
            "interior name must survive trimming"
        );
        assert!(
            !is_stop_token("no-op"),
            "interior hyphen must survive trimming"
        );
    }

    /// Why: the allowlist is the length floor's escape hatch; if an entry stops
    /// working the floor silently starts eating real entities.
    /// What: every [`SHORT_ENTITY_ALLOWLIST`] entry is accepted, and no entry
    /// collides with [`STOPWORDS`] (which is checked first and would win).
    /// Test: This test.
    #[test]
    fn short_entity_allowlist_entries_survive_the_length_floor() {
        for tok in SHORT_ENTITY_ALLOWLIST {
            assert!(
                !STOPWORDS.contains(tok),
                "{tok:?} is on both lists; the stopword check runs first and wins"
            );
            assert!(!is_stop_token(tok), "allowlisted {tok:?} must be accepted");
        }
    }

    /// Why: #4678 names three live regressions. This filter is lexical — it
    /// judges one token at a time — so it reaches the first and provably
    /// cannot reach the other two: `exhaustiveness`, `hard`, `squash` and
    /// `ancestor` are all ordinary content words, indistinguishable token-wise
    /// from the `rustc --is-a--> compiler` the extractor is supposed to keep.
    /// Rejecting them needs sentence-level context or head-noun selection,
    /// which is a different change from this one.
    ///
    /// #5399 closed that gate, so the assertion is inverted rather than
    /// deleted: the lexical half still holds — `is_stop_token` alone provably
    /// cannot reject any of these four words — but the residue it used to
    /// permit is now gone, and it is the WordNet POS pass that removes it. Both
    /// halves together are what says the fix landed where it was supposed to
    /// and not by accidentally widening the stopword list.
    /// What: asserts the four words survive the lexical filter AND that neither
    /// sentence yields a triple.
    /// Test: This test.
    #[test]
    fn lexical_filter_does_not_reach_the_two_content_word_regressions() {
        assert!(
            !is_stop_token("exhaustiveness")
                && !is_stop_token("hard")
                && !is_stop_token("squash")
                && !is_stop_token("ancestor"),
            "these are ordinary words; no lexical filter may reject them"
        );
        let exhaustiveness = patterns_for("match exhaustiveness is a hard requirement here");
        assert!(
            exhaustiveness.is_empty(),
            "#5399 should have closed the #4678 residue; got {exhaustiveness:?}"
        );
        let squash = patterns_for("confirm the squash is an ancestor of origin main");
        assert!(
            squash.is_empty(),
            "#5399 should have closed the #4678 residue; got {squash:?}"
        );
    }

    /// Why: An empty deny-list (e.g. in integration tests that want to exercise
    /// extraction regardless of tags) must not suppress any triples.
    /// What: Calling `extract_triples_with_config` with `deny_tags = &[]` on a
    /// drawer tagged `test` must produce the normal tag triple.
    /// Test: This test.
    #[test]
    fn extract_triples_empty_deny_list_passes_through() {
        let id = Uuid::new_v4();
        let tags = vec!["test".to_string()];
        let config = KgExtractConfig { deny_tags: &[] };
        let triples = extract_triples_with_config(
            &ExtractInput {
                drawer_id: id,
                content: "anything",
                tags: &tags,
                room: None,
            },
            &config,
        );
        // "test" tag should produce a tag triple when the deny-list is empty.
        assert!(
            !triples.is_empty(),
            "empty deny-list must not suppress extraction"
        );
    }
}

/// The #5399 lane-A bake-off evaluation set, plus the cases that probe where
/// the WordNet approach behaves surprisingly.
///
/// Why: the owner is picking between two lanes on identical inputs, so these
/// rows are fixed and must not be edited to suit an implementation. The
/// `surprising_*` tests below are deliberately included even where they record
/// a WRONG answer — a lane comparison built only from passing rows is useless.
/// What: each test drives `extract_triples` end to end and asserts on the
/// pattern-derived triples only (tag/room/hashtag triples are unrelated).
#[cfg(test)]
mod wordnet_eval {
    use super::*;

    /// Pattern triples only, as `(subject, predicate, object)`.
    fn pattern_triples(content: &str) -> Vec<(String, String, String)> {
        extract_triples(&ExtractInput {
            drawer_id: Uuid::new_v4(),
            content,
            tags: &[],
            room: None,
        })
        .into_iter()
        .filter(|t| PATTERN_TABLE.iter().any(|(p, _)| *p == t.predicate))
        .map(|t| (t.subject, t.predicate, t.object))
        .collect()
    }

    fn assert_none(content: &str) {
        let got = pattern_triples(content);
        assert!(
            got.is_empty(),
            "expected no triple from {content:?}, got {got:?}"
        );
    }

    fn assert_one(content: &str, s: &str, p: &str, o: &str) {
        let got = pattern_triples(content);
        assert_eq!(
            got,
            vec![(s.to_string(), p.to_string(), o.to_string())],
            "wrong extraction from {content:?}"
        );
    }

    // ---- Row 1: adjective-only object. `hard` is ADJ|ADV, never NOUN. ----
    #[test]
    fn row1_rejects_adjective_only_object() {
        assert_none("match exhaustiveness is a hard requirement here");
    }

    // ---- Row 2: genitive boundary. The NP is `ancestor of origin main`. ----
    #[test]
    fn row2_rejects_ancestor_truncated_before_of() {
        assert_none("confirm the squash is an ancestor of origin main");
    }

    // ---- Row 3: the baseline good triple must survive untouched. ----
    #[test]
    fn row3_keeps_rustc_is_a_compiler() {
        assert_one("rustc is a compiler", "rustc", "is-a", "compiler");
    }

    // ---- Row 4: head-noun re-walk past a modifier. ----
    #[test]
    fn row4_rewalks_past_the_adjective_to_the_head_noun() {
        assert_one("librs is a fast parser", "librs", "is-a", "parser");
    }

    // ---- Row 5: `for` closes the object; it is a verb adjunct, not a NP. ----
    #[test]
    fn row5_keeps_uses_object_before_a_non_genitive_preposition() {
        assert_one(
            "trusty-memory uses redb for persistence",
            "trusty-memory",
            "uses",
            "redb",
        );
    }

    // ---- Row 6: same genitive shape as row 2, different noun. ----
    #[test]
    fn row6_rejects_member_of_the_process_group() {
        assert_none("the daemon is a member of the process group");
    }

    // ---- Row 7: right-headed compound; the head is the LAST noun. ----
    #[test]
    fn row7_takes_the_head_of_a_noun_noun_compound() {
        assert_one("tantivy is a search library", "tantivy", "is-a", "library");
    }

    // ================= where this approach behaves surprisingly =============

    /// The row-1 rule is decided by a lexicographic accident, not by grammar.
    ///
    /// `hard` has no noun sense in WordNet so `is a hard requirement` is
    /// dropped whole; `fast` does have one (a period of fasting) so `is a fast
    /// parser` re-walks and succeeds. The two inputs are grammatically
    /// identical — DET ADJ NOUN — and the only thing separating them is
    /// whether Princeton happened to record a noun sense for the modifier.
    /// Every adjective-only modifier therefore destroys an otherwise good
    /// triple.
    #[test]
    fn surprising_adjective_only_modifier_destroys_a_good_triple() {
        // Grammatically identical to row 4, and the object is a real entity.
        assert_none("librs is a robust parser");
        assert_none("tantivy is an embedded library");
        assert_none("sled is a concurrent database");
        // Change one word to a modifier WordNet happens to give a noun sense
        // and the same sentence works. Nothing grammatical separates them.
        assert_one("librs is a fast parser", "librs", "is-a", "parser");
        assert_one(
            "tantivy is a portable library",
            "tantivy",
            "is-a",
            "library",
        );
    }

    /// How often the rule above fires, measured rather than asserted by feel.
    ///
    /// Over 78 adjectives drawn from ordinary technical prose, 43 have no noun
    /// sense in WordNet 3.1 and therefore destroy the whole triple instead of
    /// re-walking to the head noun. That is the dominant false-reject source in
    /// this lane, and it is a property of Princeton's lexicography, not of
    /// anything tunable here.
    #[test]
    fn measures_the_adjective_only_false_reject_rate() {
        let wn = wordnet_pos::wordnet();
        let sample: &[&str] = &[
            "fast",
            "small",
            "simple",
            "modern",
            "lightweight",
            "portable",
            "generic",
            "old",
            "good",
            "great",
            "better",
            "new",
            "robust",
            "minimal",
            "tiny",
            "huge",
            "concurrent",
            "embedded",
            "reliable",
            "lazy",
            "secure",
            "rusty",
            "async",
            "asynchronous",
            "synchronous",
            "distributed",
            "persistent",
            "immutable",
            "mutable",
            "functional",
            "relational",
            "hierarchical",
            "incremental",
            "deterministic",
            "idempotent",
            "scalable",
            "extensible",
            "pluggable",
            "configurable",
            "optional",
            "required",
            "deprecated",
            "experimental",
            "stable",
            "unstable",
            "legacy",
            "native",
            "remote",
            "local",
            "static",
            "dynamic",
            "public",
            "private",
            "internal",
            "external",
            "open",
            "closed",
            "free",
            "paid",
            "commercial",
            "fancy",
            "neat",
            "clean",
            "dirty",
            "quick",
            "slow",
            "heavy",
            "light",
            "cheap",
            "expensive",
            "strict",
            "lenient",
            "safe",
            "unsafe",
            "correct",
            "incorrect",
            "complete",
            "partial",
        ];
        let adj_only = sample.iter().filter(|w| wn.is_adjective_only(w)).count();
        assert_eq!(
            (sample.len(), adj_only),
            (78, 43),
            "adjective-only false-reject rate moved; re-measure before trusting the report"
        );
    }

    /// Only `of` is treated as NP-continuing, so the same truncation slips
    /// through with any other preposition.
    #[test]
    fn surprising_non_genitive_relational_phrase_still_truncates() {
        assert_one(
            "the daemon is a participant in the process group",
            "daemon",
            "is-a",
            "participant",
        );
    }

    /// WordNet knows ordinary English, and ordinary English words are also
    /// language and tool names. Nothing here rejects a known word, so this
    /// works — but it is worth pinning that the fail-open rule is what saves
    /// it, and any future "reject known common nouns" tightening would break
    /// exactly these.
    #[test]
    fn common_english_words_that_are_real_entity_names_survive() {
        assert_one("rust is a language", "rust", "is-a", "language");
        assert_one("go is a language", "go", "is-a", "language");
        assert_one("python is a language", "python", "is-a", "language");
    }

    /// A plural head is unknown to WordNet (index files hold base forms only),
    /// so the run stops at it and it becomes the head. Correct here, but it
    /// means every inflected form takes the fail-open path rather than the
    /// POS-checked one.
    #[test]
    fn surprising_plural_heads_are_unknown_to_wordnet() {
        let wn = wordnet_pos::wordnet();
        assert_eq!(wn.mask("parsers"), 0, "WordNet indexes base forms only");
        assert_one("librs is a fast parsers", "librs", "is-a", "parsers");
    }

    /// Two unknown tokens in a row: the run stops at the first, so the head is
    /// the first name rather than the last. Right for `uses redb for X`, wrong
    /// for a genuine unknown-unknown compound.
    #[test]
    fn surprising_run_stops_at_the_first_unknown_token() {
        assert_one(
            "trusty-memory uses redb sled",
            "trusty-memory",
            "uses",
            "redb",
        );
    }

    #[test]
    fn stops_the_noun_phrase_run_at_a_comma() {
        assert_one(
            "rustc is a compiler, and cargo is the build tool",
            "rustc",
            "is-a",
            "compiler",
        );
    }

    #[test]
    fn caps_the_noun_phrase_run() {
        // Five noun/adjective tokens; the head must come from the first four.
        assert_one(
            "librs is a fast small modular parser core",
            "librs",
            "is-a",
            "parser",
        );
    }

    #[test]
    fn unknown_subject_and_object_both_fail_open() {
        assert_one("tantivy uses fst", "tantivy", "uses", "fst");
    }

    #[test]
    fn adjective_only_subject_is_rejected() {
        assert_none("anything robust is a compiler");
    }

    /// An inflected form WordNet does not index takes the fail-open path, so
    /// the adjective-only rule silently does not apply to it.
    #[test]
    fn surprising_comparative_adjectives_slip_past_the_pos_check() {
        let wn = wordnet_pos::wordnet();
        assert_eq!(
            wn.mask("quicker"),
            wordnet_pos::ADV,
            "not indexed as an adjective"
        );
        assert_one(
            "something quicker is a compiler",
            "quicker",
            "is-a",
            "compiler",
        );
    }
}
