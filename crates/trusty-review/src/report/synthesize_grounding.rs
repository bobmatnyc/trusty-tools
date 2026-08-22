//! Grounding guardrail for synthesized narrative prose (#6082 lap 4).
//!
//! Why: the numeric guardrail in [`super::synthesize_guard`] checks that every
//! FIGURE traces back to the model. It says nothing about the claims those
//! figures sit inside, and two of those claims went out contradicting the
//! report's own data in the same document that carried them.
//!
//! The executive summary, Top Risks row 1 and the Security Posture lead all
//! called a loopback-bound, origin-guarded daemon endpoint an "unauthenticated
//! remote-code-execution" path. The finding is real and RED; the reachability
//! word is wrong, and the finding's OWN remediation says so — "before exposing
//! them beyond localhost". Separately, the executive summary named trusty-mpm a
//! load-bearing crate while the topology table two sections down showed it with
//! zero dependents.
//!
//! What: [`Grounding`] is built from the deterministic model and does two
//! things. It renders [`prompt_facts`] — the reachability hints and the
//! authoritative load-bearing list — into the synthesis prompt, so the model is
//! told the answer rather than left to guess it. And it runs [`Grounding::check`]
//! over each narrative field afterwards, because a prompt instruction is a hope
//! and this is the mechanical half: a reachability contradiction is REWRITTEN
//! when the phrase is one this module knows how to rewrite, and the field is
//! REJECTED when it is not; a load-bearing claim naming a crate outside the top
//! quartile by dependents is always rejected. A rejected field falls back to the
//! deterministic composition (#5374), exactly as a numeric rejection does.
//!
//! Test: `synthesize_grounding_tests.rs`.

use std::collections::BTreeSet;

use super::model::ReportModel;
use super::topology::CrateTopology;

/// Text markers that scope a finding to a loopback-only surface.
///
/// Deliberately short and literal. A finding whose own remediation or evidence
/// says "localhost" is stating its reachability; inferring one from softer
/// words would make the check fire on findings it was never about.
const LOOPBACK_MARKERS: &[&str] = &["localhost", "loopback", "127.0.0.1", "::1"];

/// Text markers that scope a finding to a network-reachable surface.
///
/// A finding carrying one of these vetoes the contradiction check for every
/// subject token it shares, so a genuinely remote endpoint in the same
/// repository can still be described as remote.
const REMOTE_MARKERS: &[&str] = &[
    "0.0.0.0",
    "internet-facing",
    "publicly reachable",
    "publicly accessible",
    "all interfaces",
];

/// Phrases that assert remote reachability, with the local-reachability form
/// each is rewritten to.
///
/// Longest first: `remote-code-execution` must be matched before a bare
/// `remote`, or the rewrite would leave a broken fragment behind.
const REACHABILITY_REWRITES: &[(&str, &str)] = &[
    (
        "unauthenticated remote-code-execution",
        "unauthenticated local-process-reachable code-execution",
    ),
    (
        "unauthenticated remote code execution",
        "unauthenticated local-process-reachable code execution",
    ),
    (
        "remote-code-execution",
        "local-process-reachable code-execution",
    ),
    (
        "remote code execution",
        "local-process-reachable code execution",
    ),
    (
        "remote code-execution",
        "local-process-reachable code-execution",
    ),
    ("remotely exploitable", "exploitable by any local process"),
    ("remote attacker", "any local process"),
    ("remote attackers", "any local process"),
    ("remote control over", "control over"),
    ("remote unauthenticated", "unauthenticated local-process"),
];

/// Words that still assert remote reachability after every known rewrite ran.
///
/// Their presence in a contradicting sentence is what makes the field
/// unrewritable, and therefore rejected rather than silently shipped.
const RESIDUAL_REMOTE_WORDS: &[&str] = &["remote"];

/// Phrases that claim a crate is what the rest of the estate is built on.
const LOAD_BEARING_PHRASES: &[&str] = &[
    "load-bearing",
    "load bearing",
    "most depended",
    "most-depended",
    "depended on by the rest",
];

/// Path and title words too generic to identify a finding's subject.
const GENERIC_TOKENS: &[&str] = &[
    "crates", "src", "main", "lib", "mod", "test", "tests", "with", "this", "that", "from", "into",
    "have", "http", "https", "file", "line", "code", "data", "self", "type", "used", "when",
    "over", "path", "call", "rust",
];

/// Shortest path/title word admitted as a subject token.
const MIN_TOKEN_LEN: usize = 4;

/// One finding the report's own text scopes to a loopback-only surface.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalOnly {
    /// The finding's title, for the note the guardrail records.
    title: String,
    /// The words a sentence must share to be about this finding.
    tokens: BTreeSet<String>,
}

/// What the grounding guardrail decided about one narrative field.
///
/// Why: rewriting and rejecting are different outcomes for a reader — the first
/// keeps the model's paragraph with one claim corrected, the second drops it
/// entirely — and the caller records a different note for each.
/// What: `Clean` passes the text through untouched; `Rewritten` carries the
/// corrected text plus one note per correction; `Rejected` carries the reason
/// the field cannot ship.
/// Test: `synthesize_grounding_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GroundingOutcome {
    /// Nothing contradicted the model's data.
    Clean,
    /// The text was corrected; the notes name each correction.
    Rewritten(String, Vec<String>),
    /// The field cannot be corrected safely and must be dropped.
    Rejected(String),
}

/// The grounding facts one report's synthesis is checked against.
///
/// Why: both halves — the prompt hint and the post-check — must read the same
/// facts, or the model would be told one thing and judged by another.
/// What: the loopback-scoped findings with their subject tokens, the crates that
/// may be called load-bearing, and every crate name the topology declares (the
/// vocabulary the post-check matches sentences against).
/// Test: `synthesize_grounding_tests.rs::{a_loopback_finding_is_indexed,
/// load_bearing_is_the_top_quartile_by_dependents}`.
#[derive(Debug, Default, Clone)]
pub(crate) struct Grounding {
    local_only: Vec<LocalOnly>,
    /// Subject tokens belonging to at least one network-reachable finding.
    remote_tokens: BTreeSet<String>,
    /// Crates admitted as load-bearing, most-depended first, with their counts.
    load_bearing: Vec<(String, usize)>,
    /// Every declared crate name — the match vocabulary for the post-check.
    known_crates: Vec<String>,
}

impl Grounding {
    /// Gather the grounding facts from the deterministic model.
    ///
    /// Why: every input is already in the report — the findings' own prose and
    /// the measured cargo topology — so nothing here is a second source of
    /// truth that could drift from what the report renders.
    /// What: walks every repository's metric findings and its verified
    /// investigation findings for loopback/remote markers, and folds every
    /// declared [`CrateTopology`] into the load-bearing set.
    /// Test: `synthesize_grounding_tests.rs::{a_loopback_finding_is_indexed,
    /// a_remote_finding_vetoes_its_tokens}`.
    pub(crate) fn from_model(model: &ReportModel) -> Self {
        let mut out = Grounding::default();
        for repo in &model.repositories {
            if let Some(metrics) = &repo.metrics {
                for f in &metrics.findings {
                    out.ingest(
                        &f.title,
                        &f.component,
                        &[&f.description, &f.remediation, &f.component],
                    );
                }
            }
        }
        if let Some(inv) = &model.investigation {
            for repo in &inv.repos {
                for f in &repo.findings {
                    out.ingest(
                        &f.title,
                        &f.file,
                        &[
                            &f.description,
                            &f.remediation,
                            &f.business_impact,
                            &f.evidence_quote,
                            &f.file,
                        ],
                    );
                }
            }
        }
        for topology in model
            .repositories
            .iter()
            .filter_map(|r| r.crate_topology.as_ref())
        {
            out.ingest_topology(topology);
        }
        out
    }

    /// Record one finding's reachability, if its own text states one.
    fn ingest(&mut self, title: &str, component: &str, texts: &[&String]) {
        let joined = texts
            .iter()
            .map(|t| t.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        let remote = REMOTE_MARKERS.iter().any(|m| joined.contains(m));
        let local = LOOPBACK_MARKERS.iter().any(|m| joined.contains(m));
        if !remote && !local {
            return;
        }
        let tokens = subject_tokens(title, component);
        if tokens.is_empty() {
            return;
        }
        if remote {
            self.remote_tokens.extend(tokens);
            return;
        }
        self.local_only.push(LocalOnly {
            title: title.trim().to_string(),
            tokens,
        });
    }

    /// Fold one workspace's topology into the load-bearing set and vocabulary.
    fn ingest_topology(&mut self, topology: &CrateTopology) {
        for node in &topology.crates {
            self.known_crates.push(node.name.clone());
        }
        for node in load_bearing_quartile(topology) {
            self.load_bearing.push((node.name.clone(), node.inbound));
        }
    }

    /// The prompt block naming the facts the model must not contradict.
    ///
    /// Why: the post-check below is fail-closed, and a fail-closed check the
    /// model was never told about drops good paragraphs for a claim it had no
    /// way to get right. This is the half that lets it get it right.
    /// What: an empty string when nothing was derivable; otherwise a
    /// reachability block naming each loopback-scoped finding and a
    /// load-bearing block naming the only crates that may be called load-bearing.
    /// Test: `synthesize_grounding_tests.rs::{prompt_facts_name_the_local_findings,
    /// prompt_facts_name_the_load_bearing_crates, prompt_facts_are_empty_without_data}`.
    pub(crate) fn prompt_facts(&self) -> String {
        let mut out = String::new();
        if !self.local_only.is_empty() {
            out.push_str(
                "\n## Reachability of these findings (derived from their own remediation and \
                 evidence text)\n\nEach finding below is scoped to a loopback/localhost-only \
                 surface by its own text. Describe it as reachable by any process on the host — \
                 never as remote, remotely exploitable, or reachable by a remote attacker:\n",
            );
            for f in &self.local_only {
                out.push_str(&format!("- {}\n", f.title));
            }
        }
        if !self.load_bearing.is_empty() {
            out.push_str(
                "\n## Load-bearing crates (measured from cargo metadata — authoritative)\n\nThese \
                 are the ONLY crates you may call load-bearing, most depended on, or the \
                 foundation the estate is built on, with their dependent counts:\n",
            );
            for (name, inbound) in &self.load_bearing {
                out.push_str(&format!("- `{name}` — {inbound} dependent(s)\n"));
            }
            out.push_str(
                "Never apply that description to any other crate. A crate no other member depends \
                 on is a leaf, whatever else it does.\n",
            );
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }

    /// Check one narrative field against the grounding facts.
    ///
    /// Why: the single entry point `apply_guardrail` calls, so the two checks
    /// can never be applied to one field and not another.
    /// What: runs the load-bearing check first (it can only reject, so there is
    /// nothing to rewrite afterwards), then the reachability check. Returns
    /// `Clean` when neither fired.
    /// Test: `synthesize_grounding_tests.rs`.
    pub(crate) fn check(&self, text: &str) -> GroundingOutcome {
        if let Some(reason) = self.load_bearing_violation(text) {
            return GroundingOutcome::Rejected(reason);
        }
        self.reachability(text)
    }

    /// The crate a sentence wrongly calls load-bearing, if any.
    ///
    /// Why/What: see the module doc. A sentence that carries a load-bearing
    /// phrase may only name crates in [`Self::load_bearing`]; naming any other
    /// declared crate is the contradiction, because the topology table in the
    /// same report states that crate's dependent count.
    fn load_bearing_violation(&self, text: &str) -> Option<String> {
        if self.load_bearing.is_empty() {
            return None;
        }
        for sentence in sentences(text) {
            let lower = sentence.to_lowercase();
            if !LOAD_BEARING_PHRASES.iter().any(|p| lower.contains(p)) {
                continue;
            }
            for name in &self.known_crates {
                if !contains_name(&lower, &name.to_lowercase()) {
                    continue;
                }
                if self.load_bearing.iter().any(|(n, _)| n == name) {
                    continue;
                }
                return Some(format!(
                    "'{name}' is called load-bearing but is not among the {} most-depended-on \
                     crate(s) the topology measured",
                    self.load_bearing.len()
                ));
            }
        }
        None
    }

    /// Correct — or refuse — a remote-reachability claim about a local-only
    /// finding.
    ///
    /// Why/What: see the module doc. A sentence is about a local-only finding
    /// when it shares one of that finding's subject tokens and no
    /// network-reachable finding claims the same token.
    fn reachability(&self, text: &str) -> GroundingOutcome {
        if self.local_only.is_empty() {
            return GroundingOutcome::Clean;
        }
        let mut out = text.to_string();
        let mut notes = Vec::new();
        for sentence in sentences(text) {
            let lower = sentence.to_lowercase();
            if !REACHABILITY_REWRITES.iter().any(|(p, _)| lower.contains(p)) {
                continue;
            }
            let Some(finding) = self.local_subject(&lower) else {
                continue;
            };
            let corrected = rewrite_reachability(sentence);
            if has_residual_remote(&corrected) {
                return GroundingOutcome::Rejected(format!(
                    "a remote-reachability claim about '{}' could not be corrected safely — that \
                     finding's own text scopes it to localhost",
                    finding.title
                ));
            }
            out = out.replace(sentence, &corrected);
            notes.push(format!(
                "synthesis: reachability corrected — '{}' is loopback-scoped by its own text, so \
                 the remote-reachability wording was rewritten",
                finding.title
            ));
        }
        match notes.is_empty() {
            true => GroundingOutcome::Clean,
            false => GroundingOutcome::Rewritten(out, notes),
        }
    }

    /// The local-only finding a lower-cased sentence is about, if exactly that.
    fn local_subject(&self, lower_sentence: &str) -> Option<&LocalOnly> {
        self.local_only.iter().find(|f| {
            f.tokens
                .iter()
                .any(|t| contains_name(lower_sentence, t) && !self.remote_tokens.contains(t))
        })
    }
}

/// The crates admitted as load-bearing: the top quartile by dependent count.
///
/// Why: "load-bearing" is a claim about the top of a distribution, and a
/// quartile is the coarsest cut that still excludes a crate nothing depends on.
/// A crate with zero dependents is never admitted, whatever the member count.
/// What: `ceil(members / 4)` crates, at least one, taken from
/// [`CrateTopology`]'s own inbound-descending order over the crates with a
/// non-zero dependent count.
/// Test: `synthesize_grounding_tests.rs::load_bearing_is_the_top_quartile_by_dependents`.
fn load_bearing_quartile(topology: &CrateTopology) -> Vec<&super::topology::CrateNode> {
    let mut ranked: Vec<&super::topology::CrateNode> = topology
        .crates
        .iter()
        .filter(|c| c.inbound > 0)
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.inbound.cmp(&a.inbound).then_with(|| a.name.cmp(&b.name)));
    let members = topology.members.max(topology.crates.len());
    let quartile = members.div_ceil(4).max(1);
    ranked.truncate(quartile);
    ranked
}

/// The words that identify one finding's subject: its component path segments
/// and its title words, minus the generic ones.
fn subject_tokens(title: &str, component: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for raw in component
        .split(['/', '\\', '.', ':'])
        .chain(title.split_whitespace())
    {
        let word: String = raw
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .to_lowercase();
        if word.len() < MIN_TOKEN_LEN || GENERIC_TOKENS.contains(&word.as_str()) {
            continue;
        }
        if word.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        out.insert(word);
    }
    out
}

/// True when `haystack` contains `needle` bounded by non-identifier characters.
///
/// Why: a bare `contains` would match `trusty-mpm` inside `trusty-mpm-gui` and
/// blame the wrong crate.
fn contains_name(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    haystack.match_indices(needle).any(|(i, _)| {
        let before_ok = i == 0 || !ident(bytes[i - 1]);
        let after = i + needle.len();
        let after_ok = after >= bytes.len() || !ident(bytes[after]);
        before_ok && after_ok
    })
}

/// Apply every known reachability rewrite to one sentence, case-insensitively.
fn rewrite_reachability(sentence: &str) -> String {
    let mut out = sentence.to_string();
    for (phrase, replacement) in REACHABILITY_REWRITES {
        out = replace_ignore_case(&out, phrase, replacement);
    }
    out
}

/// Case-insensitive literal replacement preserving everything else verbatim.
fn replace_ignore_case(haystack: &str, needle: &str, replacement: &str) -> String {
    let lower = haystack.to_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut cursor = 0usize;
    while let Some(rel) = lower[cursor..].find(needle) {
        let at = cursor + rel;
        out.push_str(&haystack[cursor..at]);
        out.push_str(replacement);
        cursor = at + needle.len();
    }
    out.push_str(&haystack[cursor..]);
    out
}

/// True when a corrected sentence still asserts remote reachability.
///
/// A plain substring test, not a word-boundary one: every rewrite in
/// [`REACHABILITY_REWRITES`] removes its own `remote`, so any occurrence left
/// belongs to a phrase this module does not know how to correct — including a
/// hyphenated one like `remote-management`, which a boundary test would miss.
fn has_residual_remote(sentence: &str) -> bool {
    let lower = sentence.to_lowercase();
    RESIDUAL_REMOTE_WORDS.iter().any(|w| lower.contains(w))
}

/// Split prose into sentences on `. `, `? ` and `! ` boundaries, keeping each
/// sentence's own text so the caller can splice a correction back in.
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if !matches!(b, b'.' | b'?' | b'!' | b'\n') {
            continue;
        }
        let next_is_break = bytes.get(i + 1).is_none_or(|n| n.is_ascii_whitespace());
        if !next_is_break {
            continue;
        }
        let piece = text[start..=i].trim();
        if !piece.is_empty() {
            out.push(piece);
        }
        start = i + 1;
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

#[cfg(test)]
#[path = "synthesize_grounding_tests.rs"]
mod tests;
