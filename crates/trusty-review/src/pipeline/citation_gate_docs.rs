//! The `[doc: path@sha — "excerpt"]` citation (#9193).
//!
//! Why: a finding may rest on an ADR or spec the reviewer was shown. Such a
//! citation is held to the `[code:]` rules: an exact excerpt, at least
//! `MIN_SPAN_LEN` long, from the text that reached the reviewer, or the
//! finding is withheld. The flat refs corpus cannot hold docs: doc A's text
//! would verify a citation of doc B.
//! What: [`DocCorpus`] keeps each rendered doc's kept text by path, with the
//! head SHA it was read at. [`check_doc_citations`] parses every `[doc:]`
//! bracket in a finding and returns why the first bad one fails.
//! Test: `citation_gate_docs_tests.rs`.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::models::Finding;
use crate::pipeline::citation_check::{MIN_SPAN_LEN, collect_delimited, normalize};

/// Every `[doc: …]` bracket, whatever its shape; it matches exactly what
/// `BRACKET_CITATION_RE` strips from the generic quote scan.
static DOC_CITATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)\[doc:([^\]]*)\]").expect("doc-citation regex is a valid literal")
});

/// The separator between `path@sha` and the excerpt: `—`, `–` or `-`.
static DOC_SEPARATOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+[—–-]+\s*$").expect("doc-separator regex is a valid literal"));

/// Shortest SHA prefix a citation may name (Architect ruling Q7).
pub(crate) const MIN_DOC_SHA_PREFIX: usize = 7;

/// A `[doc:]` citation that is not `path@sha — "excerpt"`.
pub(crate) const DOC_MALFORMED: &str =
    "a [doc:] citation is not of the form `path@sha — \"excerpt\"`";
/// A `[doc:]` citation of a doc, or a SHA, the reviewer was not shown.
pub(crate) const DOC_UNRESOLVED: &str =
    "a [doc:] citation names a doc that was not read at the PR head";
/// A `[doc:]` excerpt shorter than the quote floor.
pub(crate) const DOC_EXCERPT_SHORT: &str = "a [doc:] excerpt is too short to verify the citation";
/// A `[doc:]` excerpt absent from the doc text the reviewer saw.
pub(crate) const DOC_EXCERPT_ABSENT: &str =
    "a [doc:] excerpt is not in the doc text the reviewer was shown";

/// The doc text a `[doc:]` citation may quote (#9193).
///
/// Why: only text that reached the reviewer is citable; a cut tail, an
/// omitted doc and an unfetched path are not.
/// What: normalized kept text by exact repository path, and the head SHA.
/// Empty by default, so every `[doc:]` citation is withheld.
/// Test: `excerpt_from_the_cut_tail_is_withheld`,
/// `excerpt_of_doc_a_cited_as_doc_b_is_withheld`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DocCorpus {
    head_sha: String,
    docs: BTreeMap<String, String>,
}

impl DocCorpus {
    /// An empty corpus for docs read at `head_sha`.
    pub(crate) fn at(head_sha: &str) -> Self {
        Self {
            head_sha: head_sha.to_ascii_lowercase(),
            docs: BTreeMap::new(),
        }
    }

    /// Record `kept`, the text of `path` the reviewer was shown.
    pub(crate) fn insert(&mut self, path: &str, kept: &str) {
        self.docs.insert(path.to_string(), normalize(kept));
    }

    /// Whether no doc is citable.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// The kept text of `path` when `sha` names the head commit.
    fn text(&self, path: &str, sha: &str) -> Option<&str> {
        let sha = sha.to_ascii_lowercase();
        let shaped = sha.len() >= MIN_DOC_SHA_PREFIX && sha.bytes().all(|b| b.is_ascii_hexdigit());
        let at_head = shaped && !self.head_sha.is_empty() && self.head_sha.starts_with(&sha);
        at_head.then(|| self.docs.get(path).map(String::as_str))?
    }
}

/// Why the first unresolved `[doc:]` citation in `f` fails, with the
/// fragment that failed; `None` when every one resolves (#9193).
///
/// What: each bracket must read `path@sha <sep> "excerpt" …` (split at the
/// last `@`) with paired quotes; the SHA must be hex, at least [`MIN_DOC_SHA_PREFIX`] long and a
/// prefix of the head; the path must name a doc in `corpus`; there must be
/// at least one excerpt, each at least `MIN_SPAN_LEN` long and inside that
/// doc's kept text after whitespace normalization.
/// Test: `doc_citation_with_exact_excerpt_at_head_is_kept`,
/// `doc_citation_with_fabricated_excerpt_is_withheld`,
/// `doc_citation_without_excerpt_is_withheld`,
/// `doc_citation_missing_sha_or_malformed_is_withheld`,
/// `six_char_and_empty_sha_prefix_is_withheld`,
/// `doc_excerpt_containing_bracket_is_withheld`.
pub(crate) fn check_doc_citations(
    f: &Finding,
    corpus: &DocCorpus,
) -> Option<(&'static str, Option<String>)> {
    for text in [f.description.as_str(), f.consequence.as_str()] {
        for caps in DOC_CITATION_RE.captures_iter(text) {
            let body = caps.get(1).map_or("", |m| m.as_str());
            if let Some(failure) = check_one(body, corpus) {
                return Some(failure);
            }
        }
    }
    None
}

/// One bracket body, after `doc:`.
fn check_one(body: &str, corpus: &DocCorpus) -> Option<(&'static str, Option<String>)> {
    let head = body.split('"').next().unwrap_or("");
    let token = DOC_SEPARATOR_RE.replace(head, "");
    let token = token.trim();
    let mut excerpts = Vec::new();
    collect_delimited(body, '"', &mut excerpts);
    let malformed = || Some((DOC_MALFORMED, Some(token.to_string())));
    let Some((path, sha)) = token.rsplit_once('@') else {
        return malformed();
    };
    // #9193: an unpaired quote means a `]` cut the excerpt short; never check a fragment.
    let unpaired = !body.matches('"').count().is_multiple_of(2);
    if path.is_empty() || sha.is_empty() || excerpts.is_empty() || unpaired {
        return malformed();
    }
    let Some(doc) = corpus.text(path, sha) else {
        return Some((DOC_UNRESOLVED, Some(token.to_string())));
    };
    if let Some(short) = excerpts.iter().find(|e| e.len() < MIN_SPAN_LEN) {
        return Some((DOC_EXCERPT_SHORT, Some(short.clone())));
    }
    excerpts
        .into_iter()
        .find(|e| !doc.contains(normalize(e).as_str()))
        .map(|missing| (DOC_EXCERPT_ABSENT, Some(missing)))
}

#[cfg(test)]
#[path = "citation_gate_docs_tests.rs"]
mod tests;
