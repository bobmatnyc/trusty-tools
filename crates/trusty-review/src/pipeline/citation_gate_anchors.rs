//! Anchor extraction for the line-citation gate (#8905): what a finding quotes
//! or names, used to find the code it describes.
//!
//! Why: split from `citation_gate.rs` to keep it under the 500-SLOC cap.
//! What: [`finding_anchors`] collects quoted code (backtick snippets and
//! `[code: …]` excerpts for the finding's own file), optional prose quotes
//! (long double-quoted spans outside backticks, #8949), and bare backtick
//! identifiers. #9188 E: identifier-shaped prose words no longer anchor
//! anything; a finding must quote the code it describes. [`ref_citations`]
//! reads the `[jira:]`/`[gh:]`/`[confluence:]` citations (#9188 D), and
//! [`is_removal_claim`] says whether a finding is about removed code (#9188 F).
//! Test: `citation_gate_tests.rs`.

use std::sync::LazyLock;

use regex::Regex;

use super::GateError;
use crate::models::Finding;
use crate::pipeline::citation_check::{
    BRACKET_CITATION_RE, CODE_CITATION_RE, MIN_SPAN_LEN, collect_delimited, normalize,
    normalize_path,
};

/// An identifier-shaped word: `name`, `a::b::c`, optionally followed by `(`.
static IDENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*(\()?")
        .expect("identifier regex is a valid literal")
});

/// Source-file extensions that mark a backtick span as a path, not code.
const PATH_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "py", "go", "java", "kt", "rb", "md", "toml", "json",
    "yaml", "yml", "sql", "sh", "swift", "c", "h", "cpp", "cs", "php", "html", "css", "svelte",
    "vue", "lock", "txt",
];

/// Keywords too common to anchor a citation on their own.
const STOP_WORDS: &[&str] = &[
    "let",
    "mut",
    "pub",
    "self",
    "Self",
    "impl",
    "return",
    "true",
    "false",
    "None",
    "Some",
    "Err",
    "const",
    "static",
    "async",
    "await",
    "use",
    "mod",
    "struct",
    "enum",
    "match",
    "else",
    "for",
    "while",
    "loop",
    "new",
    "null",
    "nil",
    "undefined",
    "this",
    "var",
    "def",
    "function",
    "the",
    "and",
    "not",
    "void",
    "int",
    "string",
    "String",
    "bool",
    "export",
    "import",
    "from",
    "class",
];

/// What the finding quotes or names, used to find the code it describes.
#[derive(Default)]
pub(super) struct Anchors {
    /// Quoted code: non-identifier backtick spans and `[code: …]` excerpts that
    /// pass [`is_specific`]. Matched as whitespace-normalized substrings. At
    /// least one must be present; a missing one marks the finding partial (#8949).
    pub(super) snippets: Vec<String>,
    /// #8949: long double-quoted spans in the prose outside backticks. They
    /// anchor a citation when present in the file and are never required,
    /// because prose quotes English as often as it quotes code.
    pub(super) prose_quotes: Vec<String>,
    /// Bare backtick identifiers (`name`, `a::b`). #9188 E: quoted, so they
    /// anchor a citation when present and nothing else is quoted; never
    /// required, and never read from unquoted prose.
    pub(super) idents: Vec<String>,
}

impl Anchors {
    pub(super) fn is_empty(&self) -> bool {
        self.snippets.is_empty() && self.prose_quotes.is_empty() && self.idents.is_empty()
    }

    fn add_ident(&mut self, ident: &str) {
        let ident = ident.trim_end_matches('(');
        if ident.len() >= 3
            && !STOP_WORDS.contains(&ident)
            && !self.idents.iter().any(|i| i == ident)
        {
            self.idents.push(ident.to_string());
        }
        if let Some((_, last)) = ident.rsplit_once("::") {
            self.add_ident(last);
        }
    }

    /// Add quoted code. #8905 row 1: its identifiers are NOT added as a
    /// fallback, so a fabricated quote cannot verify through a real name in it.
    fn add_snippet(&mut self, snippet: String) {
        if is_specific(&snippet) && !self.snippets.contains(&snippet) {
            self.snippets.push(snippet);
        }
    }

    /// Classify one backtick span as a path (ignored), an identifier, or code.
    fn add_code_span(&mut self, span: String) {
        if span.len() < 2 || is_path_like(&span) {
            return;
        }
        let bare = span.strip_suffix("()").unwrap_or(&span);
        if IDENT_RE.find(bare).is_some_and(|m| m.len() == bare.len()) {
            self.add_ident(bare);
        } else {
            self.add_snippet(span);
        }
    }
}

/// #8905 row 3: a snippet anchors a citation only when it is long enough to be
/// specific, or names an identifier of 3+ characters; `?;`, `Ok(())` and a
/// lone `e` match almost anywhere.
fn is_specific(snippet: &str) -> bool {
    snippet.len() >= MIN_SPAN_LEN
        || IDENT_RE
            .find_iter(snippet)
            .map(|m| m.as_str().trim_end_matches('('))
            .any(|word| word.len() >= 3 && !STOP_WORDS.contains(&word))
}

fn is_path_like(span: &str) -> bool {
    if span.contains(char::is_whitespace) || span.contains('(') {
        return false;
    }
    let ext = span.rsplit_once('.').map(|(_, e)| e);
    span.contains('/')
        || span
            .rsplit_once(':')
            .is_some_and(|(_, n)| n.starts_with(|c: char| c.is_ascii_digit()))
        || ext.is_some_and(|e| PATH_EXTENSIONS.contains(&e))
}

/// Whether two cited paths name the same file: equal after normalization, or
/// one is a whole-segment suffix of the other.
/// #9188 H: `src/old/foo.rs` and `src/new/foo.rs` share a basename but are
/// different files; only a suffix at a `/` boundary names the same one.
pub(super) fn same_file(a: &str, b: &str) -> bool {
    let (a, b) = (normalize_path(a), normalize_path(b));
    a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}"))
}

/// Anchors from a finding's title, body and consequence — never its
/// suggestion, which is the proposed fix and need not be in the diff.
/// #8905 row 6: a `[code: …]` excerpt anchors only its own file, so it counts
/// here only when its locator names `f.file`.
pub(super) fn finding_anchors(f: &Finding) -> Anchors {
    let mut anchors = Anchors::default();
    for text in [f.description.as_str(), f.consequence.as_str()] {
        for caps in CODE_CITATION_RE.captures_iter(text) {
            let locator = caps.get(1).map_or("", |m| m.as_str());
            let path = locator.rsplit_once(':').map_or(locator, |(p, _)| p);
            if same_file(path, &f.file) {
                let own = bracket_anchors(caps.get(2).map_or("", |m| m.as_str()));
                own.snippets
                    .into_iter()
                    .for_each(|s| anchors.add_snippet(s));
            }
        }
        let prose = BRACKET_CITATION_RE.replace_all(text, " ");
        let mut spans = Vec::new();
        collect_delimited(&prose, '`', &mut spans);
        spans.into_iter().for_each(|s| anchors.add_code_span(s));
        let outside_backticks: String = prose.split('`').step_by(2).collect::<Vec<_>>().join(" ");
        // #8949 fix 3: pair quotes outside backticks only, so a string literal
        // inside a code span never opens a prose quote.
        let mut quoted = Vec::new();
        collect_delimited(&outside_backticks, '"', &mut quoted);
        for q in quoted.into_iter().filter(|q| q.len() >= MIN_SPAN_LEN) {
            if is_specific(&q) && !anchors.prose_quotes.contains(&q) {
                anchors.prose_quotes.push(q);
            }
        }
        // #9188 E: identifier-shaped prose words are no longer anchors.
    }
    anchors
}

/// The snippets a `[code: …]` bracket quotes after its locator. #8905 row 3:
/// single quotes are read only when there is no double-quoted excerpt, so a
/// char literal such as `b'e'` inside the excerpt is not a snippet of its own.
pub(super) fn bracket_anchors(rest: &str) -> Anchors {
    let mut out = Vec::new();
    collect_delimited(rest, '"', &mut out);
    if out.is_empty() {
        collect_delimited(rest, '\'', &mut out);
    }
    let mut anchors = Anchors::default();
    out.into_iter().for_each(|e| anchors.add_snippet(e));
    anchors
}

/// Split a `[code: …]` locator into its path and optional inclusive line span.
pub(super) fn parse_locator(locator: &str) -> Result<(String, Option<(u32, u32)>), GateError> {
    let Some((path, suffix)) = locator.rsplit_once(':') else {
        return Ok((locator.trim().to_string(), None));
    };
    let suffix = suffix.trim().trim_start_matches(['L', 'l']);
    if !suffix.starts_with(|c: char| c.is_ascii_digit()) {
        return Ok((locator.trim().to_string(), None));
    }
    let bad = || GateError::BadLocator(locator.to_string());
    let (a, b) = suffix.split_once('-').unwrap_or((suffix, suffix));
    let start = a.trim().parse::<u32>().map_err(|_| bad())?;
    let end = b
        .trim()
        .trim_start_matches(['L', 'l'])
        .parse::<u32>()
        .map_err(|_| bad())?;
    Ok((path.trim().to_string(), Some((start, end.max(start)))))
}

/// A `[jira:]`/`[gh:]`/`[confluence:]` citation (#9188 D).
static REF_CITATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[(?:jira|gh|confluence):([^\]]*)\]")
        .expect("ref-citation regex is a valid literal")
});

/// The separator between a context citation's reference and its excerpt.
static REF_SEPARATOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+[—–-]+\s+").expect("ref-separator regex is a valid literal"));

/// What each context citation in a finding must resolve to (#9188 D).
///
/// What: one entry per citation, in text order: its double-quoted excerpts,
/// or, when it quotes none, its bare reference (`#123`, `TICKET-123`). Every
/// string is normalized; an empty entry names nothing checkable.
/// Test: `a_gh_citation_absent_from_the_context_is_withheld`.
pub(super) fn ref_citations(f: &Finding) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for text in [f.description.as_str(), f.consequence.as_str()] {
        for caps in REF_CITATION_RE.captures_iter(text) {
            let body = caps.get(1).map_or("", |m| m.as_str());
            let mut needles = Vec::new();
            collect_delimited(body, '"', &mut needles);
            if needles.is_empty() {
                let token = REF_SEPARATOR_RE.split(body).next().unwrap_or("");
                let token = normalize(token.trim_matches(|c: char| c == '"' || c.is_whitespace()));
                if !token.is_empty() {
                    needles.push(token);
                }
            }
            out.push(needles);
        }
    }
    out
}

/// Words that mark a finding as being about code the change removes.
const REMOVAL_MARKERS: &[&str] = &["remov", "delet", "no longer", "dropped", "dropping"];

/// #9188 F: whether a finding is about a removal, so that a quote of removed
/// (base-only) code may anchor it. Any other finding must quote head code.
/// Test: `a_base_only_quote_does_not_verify_a_finding_about_head_code`.
pub(super) fn is_removal_claim(f: &Finding) -> bool {
    let text = format!("{} {} {}", f.kind, f.description, f.consequence).to_lowercase();
    REMOVAL_MARKERS.iter().any(|m| text.contains(m))
}
