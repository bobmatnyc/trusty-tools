//! Part-of-speech membership lookup backed by Princeton WordNet 3.1.
//!
//! Why: #5399 — the #4678 lexical filter judges a token against a closed-class
//! stopword list, so it cannot tell `hard` from `requirement` or `ancestor`
//! from `compiler`. Those are open-class words and no hand-written list can
//! separate them; what separates them is which part of speech WordNet records
//! for each. This module is the smallest thing that answers "which POS can this
//! word be" without a tagger, a model, or a runtime service.
//! What: binary-searches a byte-sorted `<lemma>\t<mask>` table embedded with
//! `include_str!`. Membership is the ONLY fact exposed — no senses, glosses, or
//! synset offsets are shipped. There is no load step, no cache, and no shared
//! mutable state: [`WordNetPos`] is a 16-byte `Copy` handle over `&'static str`,
//! so constructing one is free and every caller can own its own.
//! Test: `mod tests` at the bottom of this file.
//!
//! Provenance and licence: WordNet 3.1, Princeton University, 2011. SPDX
//! `WordNet` — permissive, no copyleft, requires the copyright notice to travel
//! with every copy. The notice is preserved verbatim in `wordnet/LICENSE` and
//! carried again in the `#` header of `wordnet/lemma-pos.txt`, which this
//! module skips at lookup time rather than stripping from the file.
//! Regeneration: `wordnet/README.md`.

use std::cmp::Ordering;

/// Bit for "this lemma has a noun sense".
pub const NOUN: u8 = 1 << 0;
/// Bit for "this lemma has a verb sense".
pub const VERB: u8 = 1 << 1;
/// Bit for "this lemma has an adjective sense".
pub const ADJ: u8 = 1 << 2;
/// Bit for "this lemma has an adverb sense".
pub const ADV: u8 = 1 << 3;

/// The vendored lemma/POS projection, embedded at compile time.
///
/// Why: reading this from disk at runtime would make the daemon depend on an
/// install layout and break the self-contained-binary property `cargo install`
/// gives us. It is the PROJECTION rather than WordNet's own `index.*` files
/// because the extractor reads one field per line and the upstream files carry
/// six more — 6,305,332 bytes of source collapses to 979,462 here (#5399).
/// What: `#`-prefixed licence/provenance header, then one `<lemma>\t<mask>`
/// record per line, byte-sorted by lemma. The sort is a correctness property:
/// [`WordNetPos::lookup`] binary-searches it in place.
/// Test: `the_shipped_table_is_sorted_and_parseable`.
const TABLE: &str = include_str!("../wordnet/lemma-pos.txt");

/// Byte offset of the first data line in [`TABLE`], resolved at compile time.
const TABLE_DATA_START: usize = data_start(TABLE.as_bytes());

/// Find the first line that is not part of the `#` header.
///
/// Why: the licence header must travel with the data (see the module docs), but
/// a binary search that could land inside it would compare a prose line as if
/// it were a lemma. Resolving the boundary as a `const` means the search starts
/// past the header at zero runtime cost.
/// What: skips whole lines while they begin with `#`; returns the offset of the
/// first line that does not, or the length when every line is header.
/// Test: `data_start_skips_the_whole_header`, `data_start_handles_no_header`.
const fn data_start(bytes: &[u8]) -> usize {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'#' {
            return i;
        }
        while i < bytes.len() && bytes[i] != b'\n' {
            i += 1;
        }
        i += 1;
    }
    bytes.len()
}

/// Lemma-to-POS membership lookup.
///
/// Why: #5399 rejected a process-wide `OnceLock<HashMap>` — CLAUDE.md permits
/// global state only for the tracing subscriber. Binary-searching the sorted
/// table directly removes the reason the global existed: there is nothing to
/// build, so there is nothing to share. The type is `Copy` and 16 bytes, so
/// threading it through [`crate::kg_extract::KgExtractConfig`] costs a pointer
/// pair rather than an `Arc`.
/// What: holds the table text and the offset its data starts at. Every lookup
/// is an O(log n) probe over `&'static str`; no allocation, no interior
/// mutability, no teardown.
/// Test: `shipped_table_answers_the_four_pos_classes`, `mask_is_case_insensitive`.
#[derive(Debug, Clone, Copy)]
pub struct WordNetPos {
    table: &'static str,
    data_start: usize,
}

impl Default for WordNetPos {
    fn default() -> Self {
        Self::shipped()
    }
}

impl WordNetPos {
    /// The vendored WordNet 3.1 table.
    ///
    /// Why: `const` so a caller that wants the shipped data pays nothing —
    /// this is what lets `KgExtractConfig::default()` stay free.
    /// What: pairs [`TABLE`] with its precomputed [`TABLE_DATA_START`].
    /// Test: `shipped_table_answers_the_four_pos_classes`.
    pub const fn shipped() -> Self {
        Self {
            table: TABLE,
            data_start: TABLE_DATA_START,
        }
    }

    /// Build a lookup over a caller-supplied table in the shipped format.
    ///
    /// Why: the binary search's edge cases (first record, last record, absent
    /// key either side of the range) are invisible against 83k real lemmas but
    /// obvious against six synthetic ones.
    /// What: same contract as [`Self::shipped`]; the caller owes byte-sorted
    /// `<lemma>\t<mask>` lines and an optional `#` header.
    /// Test: `lookup_finds_the_first_and_last_records`.
    pub fn from_table(table: &'static str) -> Self {
        Self {
            table,
            data_start: data_start(table.as_bytes()),
        }
    }

    /// POS bitmask for `word`, or `0` when WordNet has never heard of it.
    ///
    /// Why: #5399 requires unknown words to FAIL OPEN. Returning `0` rather
    /// than an error or a default makes every caller's "unknown" branch
    /// explicit at the call site instead of hidden here.
    /// What: probes as given first — the extractor lower-cases its content up
    /// front, so that path allocates nothing — and retries lower-cased only
    /// when the input actually contains an upper-case character. WordNet index
    /// lemmas are all lower-case.
    /// Test: `mask_returns_zero_for_unknown_words`, `mask_is_case_insensitive`.
    pub fn mask(&self, word: &str) -> u8 {
        if let Some(m) = self.lookup(word.as_bytes()) {
            return m;
        }
        if word.chars().any(char::is_uppercase) {
            let lowered = word.to_lowercase();
            return self.lookup(lowered.as_bytes()).unwrap_or(0);
        }
        0
    }

    /// Binary-search the table for one lemma.
    ///
    /// Why: split out so the two `mask` probes share one implementation and so
    /// a malformed table degrades to "unknown" (fail open) rather than
    /// panicking inside the daemon's write path.
    /// What: standard bisection, except the midpoint is walked back to its
    /// line start before comparing — `lo` and `hi` are therefore always
    /// line-aligned, which is what makes the forward scan for the line end
    /// safe. Both branches strictly narrow the range, so it always terminates.
    /// Test: `lookup_finds_the_first_and_last_records`,
    /// `lookup_misses_outside_the_table_range`, `lookup_tolerates_a_bad_line`.
    fn lookup(&self, needle: &[u8]) -> Option<u8> {
        let bytes = self.table.as_bytes();
        let mut lo = self.data_start;
        let mut hi = bytes.len();
        while lo < hi {
            let mut start = lo + (hi - lo) / 2;
            while start > lo && bytes[start - 1] != b'\n' {
                start -= 1;
            }
            let mut end = start;
            while end < hi && bytes[end] != b'\n' {
                end += 1;
            }
            let line = &bytes[start..end];
            let tab = line.iter().position(|b| *b == b'\t')?;
            match line[..tab].cmp(needle) {
                Ordering::Less => lo = end + 1,
                Ordering::Greater => hi = start,
                Ordering::Equal => {
                    return std::str::from_utf8(&line[tab + 1..])
                        .ok()?
                        .trim()
                        .parse::<u8>()
                        .ok();
                }
            }
        }
        None
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
    /// Why: this is the head-eligibility test the noun-phrase walk uses. A word
    /// that can ONLY be an adjective names a property, so it cannot be the head
    /// of the phrase — `hard` in `a hard requirement` modifies, it does not
    /// name. #5399 uses that to SKIP such a token when picking the head, not to
    /// reject the triple: the re-walk lands on `requirement`, which is what the
    /// sentence actually asserts.
    /// What: true when the ADJ bit is set and the NOUN bit is not. An unknown
    /// word has mask `0` and is therefore never adjective-only — the fail-open
    /// direction, which is what keeps unknown crate names eligible as heads.
    /// Test: `adjective_only_catches_hard_and_spares_fast`.
    pub fn is_adjective_only(&self, word: &str) -> bool {
        let m = self.mask(word);
        m & ADJ != 0 && m & NOUN == 0
    }

    /// Number of records in the table.
    ///
    /// Why: the measurement harness and the table's own sanity floor need it.
    /// What: counts data lines — an O(n) scan, so it is not a hot-path call.
    /// Test: `shipped_table_answers_the_four_pos_classes`.
    pub fn lemma_count(&self) -> usize {
        self.table[self.data_start..]
            .lines()
            .filter(|l| !l.is_empty())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Six records with a header, exercising both range ends.
    const TINY: &str = "# notice line\n# another\nalpha\t1\nbeta\t4\ndelta\t2\nomega\t15\n";

    #[test]
    fn data_start_skips_the_whole_header() {
        assert_eq!(
            &TINY[data_start(TINY.as_bytes())..],
            "alpha\t1\nbeta\t4\ndelta\t2\nomega\t15\n"
        );
    }

    #[test]
    fn data_start_handles_no_header() {
        assert_eq!(data_start(b"alpha\t1\n"), 0);
        assert_eq!(data_start(b"# only header\n"), 14);
    }

    #[test]
    fn lookup_finds_the_first_and_last_records() {
        let wn = WordNetPos::from_table(TINY);
        assert_eq!(wn.mask("alpha"), 1);
        assert_eq!(wn.mask("beta"), 4);
        assert_eq!(wn.mask("delta"), 2);
        assert_eq!(wn.mask("omega"), 15);
    }

    #[test]
    fn lookup_misses_outside_the_table_range() {
        let wn = WordNetPos::from_table(TINY);
        // Before the first record, after the last, and in the gaps between.
        for w in ["aardvark", "zulu", "carrot", "epsilon", "alph", "alphas"] {
            assert_eq!(wn.mask(w), 0, "{w} should not be found");
        }
    }

    #[test]
    fn lookup_tolerates_a_bad_line() {
        // A line with no tab is malformed; the probe must fail open, not panic.
        let wn = WordNetPos::from_table("alpha\t1\nbroken-line\nomega\t15\n");
        assert_eq!(wn.mask("nonsense"), 0);
    }

    #[test]
    fn shipped_table_answers_the_four_pos_classes() {
        let wn = WordNetPos::shipped();
        assert_eq!(wn.lemma_count(), 83_253);
        assert!(wn.is_noun("compiler"));
        assert!(wn.mask("run") & VERB != 0);
        assert!(wn.mask("hard") & ADJ != 0);
        assert!(wn.mask("quickly") & ADV != 0);
    }

    /// The projection's invariants, checked against the committed file rather
    /// than trusted from the generator's last run.
    ///
    /// Why: the table is regenerated by hand (`wordnet/README.md`), so nothing
    /// mechanical guarantees a re-run stayed sorted or kept the `\t<mask>`
    /// shape. An unsorted table does not fail loudly — it silently returns 0
    /// for arbitrary words, which reads as "WordNet does not know this" and
    /// would quietly disable the whole filter.
    /// What: walks every data line once, asserting byte-ascending lemma order,
    /// a parseable non-zero mask, and no multi-word lemma.
    #[test]
    fn the_shipped_table_is_sorted_and_parseable() {
        let wn = WordNetPos::shipped();
        let mut prev: &str = "";
        let mut n = 0usize;
        for line in wn.table[wn.data_start..].lines() {
            if line.is_empty() {
                continue;
            }
            let (lemma, mask) = line.split_once('\t').expect("every data line has a tab");
            assert!(
                lemma.as_bytes() > prev.as_bytes(),
                "table out of order at {lemma:?} (after {prev:?}) — binary search is invalid"
            );
            assert!(
                !lemma.contains('_'),
                "multi-word lemma {lemma:?} is dead weight"
            );
            let m: u8 = mask.parse().expect("mask parses");
            assert!(
                m > 0 && m <= (NOUN | VERB | ADJ | ADV),
                "bad mask {m} for {lemma:?}"
            );
            prev = lemma;
            n += 1;
        }
        assert_eq!(n, wn.lemma_count());
    }

    #[test]
    fn multiword_lemmas_are_absent() {
        let wn = WordNetPos::shipped();
        assert_eq!(wn.mask("hot_dog"), 0);
        assert!(wn.is_noun("dog"));
    }

    #[test]
    fn mask_returns_zero_for_unknown_words() {
        let wn = WordNetPos::shipped();
        for w in ["rustc", "librs", "tantivy", "redb", "trusty-memory"] {
            assert_eq!(wn.mask(w), 0, "{w} should be unknown to WordNet");
            assert!(!wn.is_adjective_only(w), "{w} must fail open");
        }
    }

    #[test]
    fn mask_reports_every_pos_for_a_four_way_lemma() {
        let wn = WordNetPos::shipped();
        assert_eq!(wn.mask("fast"), NOUN | VERB | ADJ | ADV);
    }

    #[test]
    fn adjective_only_catches_hard_and_spares_fast() {
        let wn = WordNetPos::shipped();
        assert!(wn.is_adjective_only("hard"));
        assert!(!wn.is_adjective_only("fast"));
        assert!(!wn.is_adjective_only("parser"));
    }

    #[test]
    fn mask_is_case_insensitive() {
        let wn = WordNetPos::shipped();
        assert_eq!(wn.mask("Compiler"), wn.mask("compiler"));
        assert_eq!(wn.mask("HARD"), wn.mask("hard"));
    }

    /// Two handles must agree without sharing anything — this is the property
    /// that made the `OnceLock` unnecessary.
    #[test]
    fn independent_handles_agree() {
        let a = WordNetPos::shipped();
        let b = WordNetPos::default();
        for w in ["compiler", "hard", "fast", "unknownium"] {
            assert_eq!(a.mask(w), b.mask(w));
        }
    }
}
