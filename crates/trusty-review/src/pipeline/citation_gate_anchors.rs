//! Anchor extraction for the line-citation gate (#8905): what a finding quotes
//! or names, used to find the code it describes.
//!
//! Why: split from `citation_gate.rs` to keep it under the 500-SLOC cap.
//! What: [`finding_anchors`] collects quoted code (backtick snippets, long
//! double-quoted spans, `[code: …]` excerpts) and identifiers (bare backtick
//! identifiers, identifier-shaped prose tokens, identifiers inside snippets).
//! Test: `citation_gate_tests.rs`.

use std::sync::LazyLock;

use regex::Regex;

use crate::models::Finding;
use crate::pipeline::citation_check::{
    BRACKET_CITATION_RE, CODE_CITATION_RE, MIN_SPAN_LEN, collect_delimited,
};

/// An identifier-shaped token: `name`, `a::b::c`, optionally followed by `(`.
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
    /// Quoted code: non-identifier backtick spans, long double-quoted spans,
    /// and `[code: …]` excerpts. Matched as whitespace-normalized substrings.
    pub(super) snippets: Vec<String>,
    /// Identifiers: bare backtick identifiers, identifier-shaped prose tokens,
    /// and identifiers inside snippets. Matched on word boundaries.
    pub(super) idents: Vec<String>,
}

impl Anchors {
    pub(super) fn is_empty(&self) -> bool {
        self.snippets.is_empty() && self.idents.is_empty()
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

    pub(super) fn add_snippet(&mut self, snippet: String) {
        for m in IDENT_RE.find_iter(&snippet) {
            self.add_ident(m.as_str());
        }
        if !self.snippets.contains(&snippet) {
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

/// Anchors from a finding's title, body and consequence — never its
/// suggestion, which is the proposed fix and need not be in the diff.
pub(super) fn finding_anchors(f: &Finding) -> Anchors {
    let mut anchors = Anchors::default();
    for text in [f.description.as_str(), f.consequence.as_str()] {
        for caps in CODE_CITATION_RE.captures_iter(text) {
            for excerpt in bracket_excerpts(caps.get(2).map_or("", |m| m.as_str())) {
                anchors.add_snippet(excerpt);
            }
        }
        let prose = BRACKET_CITATION_RE.replace_all(text, " ");
        let mut spans = Vec::new();
        collect_delimited(&prose, '`', &mut spans);
        spans.into_iter().for_each(|s| anchors.add_code_span(s));
        let mut quoted = Vec::new();
        collect_delimited(&prose, '"', &mut quoted);
        for q in quoted.into_iter().filter(|q| q.len() >= MIN_SPAN_LEN) {
            anchors.add_snippet(q);
        }
        let outside_backticks: String = prose.split('`').step_by(2).collect::<Vec<_>>().join(" ");
        add_prose_idents(&outside_backticks, &mut anchors);
    }
    add_prose_idents(&f.kind, &mut anchors);
    anchors
}

/// Add identifier-shaped prose tokens: snake_case, `a::b`, camelCase, `call(`.
fn add_prose_idents(text: &str, anchors: &mut Anchors) {
    for caps in IDENT_RE.captures_iter(text) {
        let token = caps.get(0).map_or("", |m| m.as_str()).trim_end_matches('(');
        let camel = token
            .as_bytes()
            .windows(2)
            .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
        let snake = token.contains('_') && token.chars().any(|c| c.is_ascii_alphabetic());
        if snake || camel || token.contains("::") || caps.get(1).is_some() {
            anchors.add_ident(token);
        }
    }
}

pub(super) fn bracket_excerpts(rest: &str) -> Vec<String> {
    let mut out = Vec::new();
    collect_delimited(rest, '"', &mut out);
    collect_delimited(rest, '\'', &mut out);
    out
}
