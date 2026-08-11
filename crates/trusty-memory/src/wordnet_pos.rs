//! Part-of-speech membership lookup backed by Princeton WordNet 3.1.
//!
//! Why: #5399 — the #4678 lexical filter judges a token against a closed-class
//! stopword list, so it cannot tell `hard` from `requirement` or `ancestor`
//! from `compiler`. Those are open-class words and no hand-written list can
//! separate them; what separates them is which part of speech WordNet records
//! for each. This module is the smallest thing that answers "which POS can this
//! word be" without a tagger, a model, or a runtime service.
//! What: parses the four vendored WordNet index files (`wordnet/index.{noun,
//! verb,adj,adv}`) once into a single `lemma -> POS bitmask` map. The files are
//! embedded with `include_str!`, so nothing is read from disk and the shipped
//! binary stays self-contained. Lookup is a hash probe; membership is the ONLY
//! fact exposed — no senses, glosses, or synset offsets are parsed or shipped.
//! Test: `mod tests` at the bottom of this file.
//!
//! Provenance and licence: WordNet 3.1, Princeton University, 2011. SPDX
//! `WordNet` — permissive, no copyleft, requires the copyright notice to travel
//! with every copy. The notice is preserved verbatim in `wordnet/LICENSE` and
//! also survives inside the header of each vendored index file, which this
//! parser skips at load time rather than stripping from the file.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Bit for "this lemma has a noun sense".
pub const NOUN: u8 = 1 << 0;
/// Bit for "this lemma has a verb sense".
pub const VERB: u8 = 1 << 1;
/// Bit for "this lemma has an adjective sense".
pub const ADJ: u8 = 1 << 2;
/// Bit for "this lemma has an adverb sense".
pub const ADV: u8 = 1 << 3;

/// The vendored index files, embedded at compile time.
///
/// Why: reading these from disk at runtime would make the daemon depend on an
/// install layout and break the single-self-contained-binary property that
/// `cargo install` currently gives us.
/// What: raw WordNet 3.1 index files, licence header included. Each carries one
/// record per line: `lemma pos synset_cnt ...`; only the first field is read.
/// Test: `every_index_file_parses`.
const INDEX_NOUN: &str = include_str!("../wordnet/index.noun");
const INDEX_VERB: &str = include_str!("../wordnet/index.verb");
const INDEX_ADJ: &str = include_str!("../wordnet/index.adj");
const INDEX_ADV: &str = include_str!("../wordnet/index.adv");

/// Lemma-to-POS membership table.
///
/// Why: one map keyed by lemma beats four sets — a single probe answers every
/// question the extractor asks, and a word present in three POS files is stored
/// once instead of three times.
/// What: `&'static str` keys borrowed directly out of the embedded index text,
/// so the table itself allocates no strings; only the map's own bucket array is
/// heap memory.
/// Test: `mask_reports_every_pos_for_a_four_way_lemma`.
#[derive(Debug)]
pub struct WordNetPos {
    lemmas: HashMap<&'static str, u8>,
}

impl WordNetPos {
    /// Parse all four embedded index files into the membership table.
    ///
    /// Why: called exactly once (see [`wordnet`]); the cost is paid at daemon
    /// startup rather than on the first extraction, so no user-visible request
    /// eats it.
    /// What: skips the 29-line WordNet licence header — every header line
    /// begins with a space, every data line begins with the lemma — then takes
    /// the first whitespace-delimited field of each remaining line and ORs the
    /// file's POS bit into that lemma's mask. Multi-word lemmas (WordNet joins
    /// them with `_`) are dropped: the extractor only ever looks up single
    /// whitespace-delimited tokens, so they could never match, and they are
    /// over half of `index.noun`.
    /// Test: `every_index_file_parses`, `load_drops_multiword_lemmas`.
    pub fn load() -> Self {
        // 91k single-word lemmas across the four files; pre-sizing avoids ~14
        // rehash-and-copy passes over the table during the load loop.
        let mut lemmas: HashMap<&'static str, u8> = HashMap::with_capacity(100_000);
        for (text, bit) in [
            (INDEX_NOUN, NOUN),
            (INDEX_VERB, VERB),
            (INDEX_ADJ, ADJ),
            (INDEX_ADV, ADV),
        ] {
            parse_index(text, bit, &mut lemmas);
        }
        Self { lemmas }
    }

    /// POS bitmask for `word`, or `0` when WordNet has never heard of it.
    ///
    /// Why: #5399 requires unknown words to FAIL OPEN. Returning `0` rather
    /// than an error or a default makes every caller's "unknown" branch
    /// explicit at the call site instead of hidden here.
    /// What: lower-cases into a stack-free lookup when the input is already
    /// lower-case (the extractor lower-cases its content up front), otherwise
    /// allocates once. WordNet index lemmas are all lower-case.
    /// Test: `mask_returns_zero_for_unknown_words`.
    pub fn mask(&self, word: &str) -> u8 {
        if let Some(m) = self.lemmas.get(word) {
            return *m;
        }
        if word.chars().any(char::is_uppercase) {
            let lowered = word.to_lowercase();
            return self.lemmas.get(lowered.as_str()).copied().unwrap_or(0);
        }
        0
    }

    /// Whether WordNet lists `word` under any part of speech.
    pub fn is_known(&self, word: &str) -> bool {
        self.mask(word) != 0
    }

    /// Whether `word` can be a noun.
    pub fn is_noun(&self, word: &str) -> bool {
        self.mask(word) & NOUN != 0
    }

    /// Whether `word` is an adjective and nothing else.
    ///
    /// Why: #5399 rule 1 — an object that can ONLY be an adjective names a
    /// property, not an entity, so `exhaustiveness --is-a--> hard` must not be
    /// asserted. The "and nothing else" half is what keeps the rule safe:
    /// `fast`, `safe`, and `main` are adjectives but also nouns, so they are
    /// not caught here and can still head a phrase.
    /// What: true when the ADJ bit is set and the NOUN bit is not. An unknown
    /// word has mask `0` and is therefore never adjective-only — the fail-open
    /// direction.
    /// Test: `adjective_only_catches_hard_and_spares_fast`.
    pub fn is_adjective_only(&self, word: &str) -> bool {
        let m = self.mask(word);
        m & ADJ != 0 && m & NOUN == 0
    }

    /// Number of distinct lemmas in the table.
    pub fn len(&self) -> usize {
        self.lemmas.len()
    }

    /// Whether the table is empty (only ever true if the vendored files are).
    pub fn is_empty(&self) -> bool {
        self.lemmas.is_empty()
    }
}

/// Fold one WordNet index file into the lemma table.
///
/// Why: split out of [`WordNetPos::load`] so the licence-header skip and the
/// multi-word drop can be tested against a synthetic blob rather than against
/// 6 MB of real data, where a regression would be invisible.
/// What: skips every line that starts with a space (the whole 29-line licence
/// header, and nothing else — no WordNet data line is indented) and every
/// multi-word lemma, then ORs `bit` into the first field's entry.
/// Test: `parse_index_skips_an_indented_licence_header`,
/// `parse_index_drops_multiword_lemmas`.
fn parse_index(text: &'static str, bit: u8, out: &mut HashMap<&'static str, u8>) {
    for line in text.lines() {
        if line.starts_with(' ') || line.is_empty() {
            continue;
        }
        let lemma = match line.split(' ').next() {
            Some(l) if !l.is_empty() && !l.contains('_') => l,
            _ => continue,
        };
        *out.entry(lemma).or_insert(0) |= bit;
    }
}

/// Process-wide lookup, built on first use.
///
/// Why: the table is immutable, ~2 MB of buckets, and consulted on every
/// extraction; rebuilding it per call would dominate extraction cost by four
/// orders of magnitude. A `OnceLock` is the narrowest form of the shared-state
/// exception already granted to the tracing subscriber — no interior
/// mutability, no teardown, no configuration.
/// What: returns the shared [`WordNetPos`], parsing it on the first call.
/// Test: `wordnet_is_the_same_instance_across_calls`.
pub fn wordnet() -> &'static WordNetPos {
    static POS: OnceLock<WordNetPos> = OnceLock::new();
    POS.get_or_init(WordNetPos::load)
}

/// Build the lookup now so the first extraction does not pay for it.
///
/// Why: without this the parse lands on whichever request happens to extract
/// first, turning a one-off startup cost into a visible latency spike on a
/// random user call.
/// What: forces [`wordnet`] and discards the reference.
/// Test: `preload_is_idempotent`.
pub fn preload() {
    let _ = wordnet();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_index_file_parses() {
        let wn = WordNetPos::load();
        // Sanity floor per POS: WordNet 3.1 has ~57k single-word nouns, ~21k
        // adjectives, ~8.7k verbs, ~3.8k adverbs.
        assert!(wn.len() > 80_000, "only {} lemmas parsed", wn.len());
        assert!(wn.is_noun("compiler"));
        assert!(wn.mask("run") & VERB != 0);
        assert!(wn.mask("hard") & ADJ != 0);
        assert!(wn.mask("quickly") & ADV != 0);
    }

    #[test]
    fn load_drops_multiword_lemmas() {
        let wn = WordNetPos::load();
        assert_eq!(wn.mask("hot_dog"), 0);
        assert!(wn.is_noun("dog"));
    }

    /// The real header cannot be probed through `mask` — `licensee`,
    /// `princeton` and `wordnet` are all genuine WordNet nouns, so a leak would
    /// be indistinguishable from a hit. A synthetic blob with the same shape
    /// (two leading spaces per header line) is the only way to see the skip.
    #[test]
    fn parse_index_skips_an_indented_licence_header() {
        let blob = "  This software and database is being provided to you,\n  \
                    WordNet 3.1 Copyright 2011 by Princeton University.\n\
                    compiler n 1 1 @ 1 0 06582403\n";
        let mut out = HashMap::new();
        parse_index(blob, NOUN, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out.get("compiler"), Some(&NOUN));
    }

    #[test]
    fn parse_index_drops_multiword_lemmas() {
        let blob = "hot_dog n 1 1 @ 1 0 07697100\ndog n 1 1 @ 1 0 02086723\n";
        let mut out = HashMap::new();
        parse_index(blob, NOUN, &mut out);
        assert_eq!(out.len(), 1);
        assert!(out.contains_key("dog"));
    }

    #[test]
    fn parse_index_ors_bits_across_files() {
        let mut out = HashMap::new();
        parse_index("run n 1\n", NOUN, &mut out);
        parse_index("run v 1\n", VERB, &mut out);
        assert_eq!(out.get("run"), Some(&(NOUN | VERB)));
    }

    #[test]
    fn mask_returns_zero_for_unknown_words() {
        let wn = WordNetPos::load();
        for w in ["rustc", "librs", "tantivy", "redb", "trusty-memory"] {
            assert_eq!(wn.mask(w), 0, "{w} should be unknown to WordNet");
            assert!(!wn.is_adjective_only(w), "{w} must fail open");
        }
    }

    #[test]
    fn mask_reports_every_pos_for_a_four_way_lemma() {
        let wn = WordNetPos::load();
        assert_eq!(wn.mask("fast"), NOUN | VERB | ADJ | ADV);
    }

    #[test]
    fn adjective_only_catches_hard_and_spares_fast() {
        let wn = WordNetPos::load();
        assert!(wn.is_adjective_only("hard"));
        assert!(!wn.is_adjective_only("fast"));
        assert!(!wn.is_adjective_only("parser"));
    }

    #[test]
    fn mask_is_case_insensitive() {
        let wn = WordNetPos::load();
        assert_eq!(wn.mask("Compiler"), wn.mask("compiler"));
    }

    #[test]
    fn wordnet_is_the_same_instance_across_calls() {
        assert!(std::ptr::eq(wordnet(), wordnet()));
    }

    #[test]
    fn preload_is_idempotent() {
        preload();
        preload();
        assert!(wordnet().len() > 80_000);
    }
}
