//! Tier 1: exact keyword matching via Aho-Corasick.
//!
//! All keywords from all rules are compiled into a single case-insensitive
//! Aho-Corasick automaton. On match, the pattern id is mapped back to the
//! originating rule. If multiple rules match, the one with the highest
//! [`crate::classify::rules::Rule::priority`] wins.
//!
//! A match must sit on a word boundary to count (#4331). Aho-Corasick is a
//! substring search, so before that rule the three-letter `kw-security`
//! keyword `rce` fired inside `source`, `resource`, and `commerce`, and every
//! commit carrying one of those words classified as security at priority 80.
//! See [`boundaries_for`] for which of a keyword's two edges are checked.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

use crate::classify::errors::{ClassifyError, Result};
use crate::classify::rules::Rule;

/// Which edges of a keyword must land on a word boundary to count as a match.
///
/// Why: a keyword's own spelling decides this. `cve-` ends in a hyphen, so
/// demanding a word boundary after it would reject `CVE-2024-1234`, the exact
/// text it exists to catch. `rce` ends in a letter, so not demanding one lets
/// it match inside `source`.
/// What: `left`/`right` are true when the keyword's first/last character is a
/// word character, and are the only edges [`match_is_bounded`] checks.
/// Test: `tests::boundaries_follow_the_keywords_own_edges`.
#[derive(Clone, Copy)]
struct Boundaries {
    /// The character before the match must not be a word character.
    left: bool,
    /// The character after the match must not be a word character.
    right: bool,
}

/// Word characters for boundary purposes: alphanumerics and `_`.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Derive [`Boundaries`] from a keyword's own first and last characters.
fn boundaries_for(keyword: &str) -> Boundaries {
    let mut chars = keyword.chars();
    let first = chars.next();
    let last = chars.next_back().or(first);
    Boundaries {
        left: first.is_some_and(is_word_char),
        right: last.is_some_and(is_word_char),
    }
}

/// Whether a match spanning `start..end` of `haystack` sits on the boundaries
/// its keyword requires.
///
/// Why: this is the whole of the #4331 fix — every other line in this module
/// is unchanged plumbing.
/// What: reads the character immediately before `start` and immediately after
/// `end` and rejects the match when a required edge abuts a word character.
/// Text that runs off either end of the string is a boundary. Both lookups go
/// through `str::get`, so a byte offset that is not a character boundary
/// yields `None` rather than panicking.
/// Test: `tests::a_short_keyword_does_not_match_inside_a_longer_word`,
/// `tests::a_keyword_ending_in_punctuation_still_matches_a_following_word`.
fn match_is_bounded(haystack: &str, start: usize, end: usize, edges: Boundaries) -> bool {
    if edges.left {
        let before = haystack.get(..start).and_then(|s| s.chars().next_back());
        if before.is_some_and(is_word_char) {
            return false;
        }
    }
    if edges.right {
        let after = haystack.get(end..).and_then(|s| s.chars().next());
        if after.is_some_and(is_word_char) {
            return false;
        }
    }
    true
}

/// Tier-1 exact matcher.
pub struct ExactMatcher {
    /// The compiled automaton. `None` if there were no keywords across all rules.
    automaton: Option<AhoCorasick>,
    /// For each pattern id, the index of its rule in `rules`.
    pattern_rule_idx: Vec<usize>,
    /// For each pattern id, the word boundaries that pattern requires (#4331).
    pattern_boundaries: Vec<Boundaries>,
    /// Owned copy of the input rules.
    rules: Vec<Rule>,
}

impl ExactMatcher {
    /// Build a new matcher from the given rules.
    ///
    /// # Errors
    ///
    /// Returns [`ClassifyError::RuleLoad`] if the automaton fails to build.
    pub fn new(rules: &[Rule]) -> Result<Self> {
        let mut patterns: Vec<String> = Vec::new();
        let mut pattern_rule_idx: Vec<usize> = Vec::new();
        let mut pattern_boundaries: Vec<Boundaries> = Vec::new();

        for (idx, rule) in rules.iter().enumerate() {
            for kw in &rule.keywords {
                if kw.is_empty() {
                    continue;
                }
                pattern_boundaries.push(boundaries_for(kw));
                patterns.push(kw.clone());
                pattern_rule_idx.push(idx);
            }
        }

        let automaton = if patterns.is_empty() {
            None
        } else {
            let ac = AhoCorasickBuilder::new()
                .ascii_case_insensitive(true)
                .match_kind(MatchKind::LeftmostLongest)
                .build(&patterns)
                .map_err(|e| ClassifyError::RuleLoad(format!("aho-corasick build: {e}")))?;
            Some(ac)
        };

        Ok(Self {
            automaton,
            pattern_rule_idx,
            pattern_boundaries,
            rules: rules.to_vec(),
        })
    }

    /// Classify `message` using exact keyword matching.
    ///
    /// Returns the highest-priority matching rule, or `None` if no keyword
    /// matches on a word boundary. A match that lands inside a longer word is
    /// discarded (#4331) — see [`match_is_bounded`].
    pub fn classify(&self, message: &str) -> Option<&Rule> {
        let ac = self.automaton.as_ref()?;
        let mut best: Option<&Rule> = None;
        for m in ac.find_iter(message) {
            let pattern = m.pattern().as_usize();
            let edges = self.pattern_boundaries[pattern];
            if !match_is_bounded(message, m.start(), m.end(), edges) {
                continue;
            }
            let rule_idx = self.pattern_rule_idx[pattern];
            let rule = &self.rules[rule_idx];
            best = match best {
                Some(prev) if prev.priority >= rule.priority => Some(prev),
                _ => Some(rule),
            };
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rule carrying exactly `keywords`, so a test names its own inputs.
    fn rule(id: &str, keywords: &[&str]) -> Rule {
        Rule {
            id: id.into(),
            category: id.into(),
            subcategory: None,
            keywords: keywords.iter().map(|k| (*k).to_string()).collect(),
            patterns: vec![],
            priority: 50,
            confidence: 0.9,
        }
    }

    #[test]
    fn boundaries_follow_the_keywords_own_edges() {
        let word = boundaries_for("rce");
        assert!(word.left && word.right);
        let trailing_punct = boundaries_for("cve-");
        assert!(trailing_punct.left && !trailing_punct.right);
        let leading_punct = boundaries_for("#fix");
        assert!(!leading_punct.left && leading_punct.right);
        // A one-character keyword reads the same character for both edges.
        let single = boundaries_for("v");
        assert!(single.left && single.right);
    }

    /// #4331: the defect, at the tier that produced it. Every one of these
    /// matched before the boundary check and classified as `security`.
    #[test]
    fn a_short_keyword_does_not_match_inside_a_longer_word() {
        let m = ExactMatcher::new(&[rule("security", &["rce"])]).expect("build");
        for msg in [
            "SignalStore schema for source events",
            "extract shared resource pool",
            "bump the e-commerce sdk",
            "enforce the rate limit",
        ] {
            assert!(m.classify(msg).is_none(), "{msg}");
        }
        assert!(m.classify("mitigate the RCE the pentest found").is_some());
    }

    #[test]
    fn a_keyword_ending_in_punctuation_still_matches_a_following_word() {
        let m = ExactMatcher::new(&[rule("security", &["cve-"])]).expect("build");
        assert!(m.classify("patch CVE-2024-1234").is_some());
        // The leading edge is still a letter, so it stays boundary-checked.
        assert!(m.classify("recve-2024-1234").is_none());
    }

    #[test]
    fn multi_word_keywords_are_unaffected() {
        let m = ExactMatcher::new(&[rule("bugfix", &["fix bug", "closes #"])]).expect("build");
        assert!(m.classify("fix bug in the parser").is_some());
        assert!(m.classify("closes #4331").is_some());
        assert!(m.classify("prefix bug").is_none());
    }

    /// A non-ASCII haystack must not panic the byte-offset boundary lookup.
    #[test]
    fn a_multibyte_haystack_is_scored_without_panicking() {
        let m = ExactMatcher::new(&[rule("security", &["rce"])]).expect("build");
        assert!(m.classify("refactor — drop the résumé parser").is_none());
        assert!(m.classify("— RCE —").is_some());
    }
}
