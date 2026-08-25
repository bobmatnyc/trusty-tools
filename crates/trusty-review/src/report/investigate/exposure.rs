//! Bind and exposure evidence, collected from source (#6191).
//!
//! Why: the reachability guard in [`synthesize_grounding`] decides whether a
//! finding's surface is host-local, network-reachable, or unestablished — and
//! until now it had no evidence source at all. It read the FINDING TEXT for a
//! `localhost` or `0.0.0.0` marker, so a genuinely network-facing component
//! whose findings happened not to spell one classified as UNESTABLISHED, and
//! every true reach claim about it was withheld rather than stated. A check of
//! the report model found zero bind or exposure keys among its 209.
//!
//! What: [`collect`] reads the investigation's own captured file contents — the
//! bytes already in memory, no second pass over the tree — for bind literals and
//! outbound absolute URLs, and returns one [`ExposureFact`] per file it can
//! classify. [`ExposureIndex`] is the lookup the guard consults BEFORE its text
//! markers, so a file measured from source is never re-decided by a sentence.
//!
//! Precedence within one file: a public bind beats a loopback bind, and either
//! beats an outbound call. A file that binds loopback and also calls an external
//! API is a loopback-listening surface; its outbound traffic does not make it
//! reachable.
//!
//! Bound: only files the investigation actually read carry facts. That is the
//! same byte budget the coverage section already states, and the coverage line
//! names the collected count so the bound is visible rather than implied.
//!
//! [`synthesize_grounding`]: crate::report::synthesize_grounding
//!
//! Test: `exposure_tests.rs`.

use std::collections::BTreeMap;

use serde::Serialize;

use super::select::SelectedFile;

/// Longest evidence line the record keeps, in bytes.
const MAX_EVIDENCE: usize = 200;

/// Tokens that mark a line as a socket-binding site.
///
/// Deliberately literal and framework-named. A line merely mentioning an address
/// is not a bind; the address in a comment or a doc example must not decide a
/// file's reachability.
const BIND_MARKERS: &[&str] = &[
    "tcplistener",
    "unixlistener",
    "udpsocket",
    "socketaddr",
    ".bind(",
    "bind_addr",
    "axum::serve",
    "httpserver::new",
    "listen(",
    "listener::bind",
    "app.listen",
    "server.listen",
    "uvicorn.run",
];

/// Address literals that state a host-local listening surface.
const LOOPBACK_LITERALS: &[&str] = &["127.0.0.1", "[::1]", "::1", "localhost"];

/// Address literals that state a listening surface on every interface.
const PUBLIC_LITERALS: &[&str] = &["0.0.0.0", "[::]"];

/// What the source says about one file's reachability (#6191).
///
/// Why: the guard needs the three states its tiers already model, told apart by
/// what was measured rather than by what a sentence said.
/// What: a bind on loopback, a bind on every interface, or an outbound call to a
/// non-loopback host. Ordered so `max` is the strongest claim about the file.
/// Test: `exposure_tests::a_public_bind_beats_a_loopback_bind_on_one_file`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExposureKind {
    /// The file calls out to a non-loopback host over HTTP(S).
    NetworkClient,
    /// The file binds a socket to a loopback address.
    LoopbackBind,
    /// The file binds a socket to every interface.
    PublicBind,
}

impl ExposureKind {
    /// True when this evidence places the surface beyond this host.
    ///
    /// A loopback bind does not; the other two do. This is the one question the
    /// grounding guard asks of a fact.
    pub fn is_beyond_host(self) -> bool {
        !matches!(self, ExposureKind::LoopbackBind)
    }

    /// The reader-facing name for the coverage line.
    pub fn label(self) -> &'static str {
        match self {
            ExposureKind::NetworkClient => "outbound network call",
            ExposureKind::LoopbackBind => "loopback bind",
            ExposureKind::PublicBind => "public bind",
        }
    }
}

/// One file's collected reachability evidence (#6191).
///
/// Why: the count alone would let the report say evidence was collected without
/// saying what it was; a diligence reader checking a reach claim needs the line
/// it rests on.
/// What: the repository-relative path, the strongest kind found in it, and the
/// source line that stated it, trimmed and capped at [`MAX_EVIDENCE`] bytes.
/// Test: `exposure_tests::a_loopback_bind_is_collected_with_its_line`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExposureFact {
    /// Repository-relative path of the file the evidence came from.
    pub file: String,
    /// The strongest exposure this file's source states.
    pub kind: ExposureKind,
    /// The source line the classification rests on.
    pub evidence: String,
}

/// Every collected fact, keyed for the grounding guard's lookup (#6191).
///
/// Why: the guard normalises a finding's component to a lower-cased path before
/// comparing, so the index must be keyed the same way or a fact would never
/// match the finding it is about.
/// What: built by [`ExposureIndex::from_facts`]; [`ExposureIndex::kind`] answers
/// for one normalised component, `None` when nothing was measured for it.
/// Test: `exposure_tests::the_index_is_keyed_by_lowercased_path`.
#[derive(Debug, Clone, Default)]
pub struct ExposureIndex {
    by_file: BTreeMap<String, ExposureKind>,
}

impl ExposureIndex {
    /// Index a collected fact set.
    pub fn from_facts<'a>(facts: impl IntoIterator<Item = &'a ExposureFact>) -> Self {
        let mut by_file: BTreeMap<String, ExposureKind> = BTreeMap::new();
        for f in facts {
            let key = f.file.replace('\\', "/").to_lowercase();
            let slot = by_file.entry(key).or_insert(f.kind);
            *slot = (*slot).max(f.kind);
        }
        ExposureIndex { by_file }
    }

    /// What the source measured for this normalised component, if anything.
    pub fn kind(&self, component: &str) -> Option<ExposureKind> {
        self.by_file.get(component).copied()
    }

    /// True when nothing was collected — every consumer then behaves as before.
    pub fn is_empty(&self) -> bool {
        self.by_file.is_empty()
    }
}

/// Collect one repository's bind/exposure facts from the examined files.
///
/// Why/What: see the module doc. One fact per file that states anything, the
/// strongest kind winning; a file whose source says nothing about reachability
/// produces no fact, which is what leaves the guard's text-marker path in place
/// for it.
/// Test: `exposure_tests::{a_loopback_bind_is_collected_with_its_line,
/// a_public_bind_beats_a_loopback_bind_on_one_file,
/// an_outbound_url_is_weaker_than_a_bind, a_file_stating_nothing_yields_no_fact,
/// a_commented_address_without_a_bind_marker_is_not_evidence}`.
pub fn collect(files: &[SelectedFile]) -> Vec<ExposureFact> {
    let mut out = Vec::new();
    for f in files {
        if let Some((kind, evidence)) = strongest(&f.content) {
            out.push(ExposureFact {
                file: f.path.clone(),
                kind,
                evidence,
            });
        }
    }
    out
}

/// The strongest exposure one file's text states, with the line that stated it.
fn strongest(content: &str) -> Option<(ExposureKind, String)> {
    let mut best: Option<(ExposureKind, String)> = None;
    for line in content.lines() {
        let Some(kind) = classify_line(line) else {
            continue;
        };
        if best.as_ref().is_none_or(|(seen, _)| kind > *seen) {
            best = Some((kind, cap(line)));
        }
    }
    best
}

/// What one source line states about reachability, if anything.
fn classify_line(line: &str) -> Option<ExposureKind> {
    let lower = line.to_lowercase();
    if BIND_MARKERS.iter().any(|m| lower.contains(m)) {
        if PUBLIC_LITERALS.iter().any(|a| lower.contains(a)) {
            return Some(ExposureKind::PublicBind);
        }
        if LOOPBACK_LITERALS.iter().any(|a| lower.contains(a)) {
            return Some(ExposureKind::LoopbackBind);
        }
        // A bind site with no address literal states nothing about where it
        // binds — the address arrived from config, and guessing is what #6191
        // exists to stop.
        return None;
    }
    if outbound_url(&lower) {
        return Some(ExposureKind::NetworkClient);
    }
    None
}

/// True when the line carries an absolute HTTP(S) URL to a non-loopback host.
fn outbound_url(lower: &str) -> bool {
    for scheme in ["https://", "http://"] {
        let mut rest = lower;
        while let Some(i) = rest.find(scheme) {
            let host = &rest[i + scheme.len()..];
            let host = host
                .split(['/', '"', '\'', ' ', ')', '`'])
                .next()
                .unwrap_or("");
            if !host.is_empty() && !LOOPBACK_LITERALS.iter().any(|a| host.starts_with(a)) {
                return true;
            }
            rest = &rest[i + scheme.len()..];
        }
    }
    false
}

/// Trim a source line and cap it at [`MAX_EVIDENCE`] bytes on a char boundary.
fn cap(line: &str) -> String {
    let t = line.trim();
    if t.len() <= MAX_EVIDENCE {
        return t.to_string();
    }
    let mut end = MAX_EVIDENCE;
    while end > 0 && !t.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &t[..end])
}

#[cfg(test)]
#[path = "exposure_tests.rs"]
mod tests;
