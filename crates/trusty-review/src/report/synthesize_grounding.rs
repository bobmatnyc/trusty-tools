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
//! #6082 lap 7 added a third claim and changed how the first is triggered. The
//! third: a sentence saying this report credits no clean security signal, in a
//! report whose Security Posture section credits eighteen. The change: what
//! makes a sentence the reachability check's business is now the
//! [`REACHABILITY_WORDS`] vocabulary rather than the [`REACHABILITY_REWRITES`]
//! phrase table, so an unrecognised spelling is rejected instead of skipped.
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
///
/// #6082 lap 6: the socket spellings state the same fact without the word
/// localhost. The graded report's RED finding 2 shipped "a remote code execution
/// risk" because its remediation proposed "token or local-socket verification"
/// and none of the four original markers matched — the finding never entered
/// [`Grounding::local_only`], so the sentence had no owner and the check never
/// ran on it. A remediation that proposes verifying the peer over a local socket
/// is stating that the peer is on this host, which is a reachability statement
/// as literal as "localhost".
const LOOPBACK_MARKERS: &[&str] = &[
    "localhost",
    "loopback",
    "127.0.0.1",
    "::1",
    "local-socket",
    "local socket",
    "unix socket",
    "unix-socket",
    "unix domain socket",
    "unix-domain socket",
];

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

/// Phrases that assert reachability beyond the host, with the host-local form
/// each is rewritten to.
///
/// Longest first: `remote-code-execution` must be matched before a bare
/// `remote`, and every plural before its singular, or the rewrite would leave a
/// broken fragment behind (`remote attackers` → `any local processs`).
///
/// This table is a CONVENIENCE, not the gate. [`REACHABILITY_WORDS`] decides
/// what gets checked and what may ship; a claim this table cannot rewrite is
/// rejected rather than passed through.
const REACHABILITY_REWRITES: &[(&str, &str)] = &[
    // #6082 lap 5: the hedged form. RED finding 3's business impact read
    // "enabling remote/local code execution" — no pattern below matches it,
    // because the `/local` sits between the two words every other pattern
    // expects to be adjacent, so the sentence never even triggered the check.
    (
        "remote/local code execution",
        "local-process-reachable code execution",
    ),
    (
        "remote/local code-execution",
        "local-process-reachable code-execution",
    ),
    ("remote/local", "local-process-reachable"),
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
    ("remote attackers", "any local process"),
    ("remote attacker", "any local process"),
    ("remote control over", "control over"),
    ("remote unauthenticated", "unauthenticated local-process"),
    // #6082 lap 7: the network family. RED finding 2's business impact shipped
    // "An unauthenticated actor on the network or host can launch arbitrary
    // claude commands" — a reachability claim carrying no `remote` token, so
    // the pattern table above never matched it and the check never ran.
    ("on the network or on the host", "on the host"),
    ("on the network or host", "on the host"),
    ("over the network", "on the host"),
    ("across the network", "on the host"),
    ("from the network", "from any process on the host"),
    ("network-reachable", "reachable by any process on the host"),
    ("network reachable", "reachable by any process on the host"),
    ("network attackers", "any local process"),
    ("network attacker", "any local process"),
    ("unauthenticated network", "unauthenticated local-process"),
    ("network-facing", "host-local"),
    ("on the network", "on the host"),
];

/// Every word that asserts reachability beyond this host.
///
/// This list is the guardrail, and [`REACHABILITY_REWRITES`] only softens it.
/// A sentence about a local-only finding is CHECKED when it carries one of these
/// words, and may SHIP only when none survives the rewrite pass — so a spelling
/// this module has never seen is rejected and disclosed rather than passed
/// through unexamined.
///
/// #6082 lap 7 replaced a phrase blocklist with this vocabulary. Three laps in a
/// row shipped the same defect in a new spelling (`RCE`, then `remote/local`,
/// then `on the network`), each time because the trigger was a list of known
/// phrases: an unrecognised phrase was not corrected AND not checked. Keying
/// both halves off one word list inverts that — an unrecognised phrase is now
/// the case that fails closed.
///
/// The cost is real and deliberate: a sentence about a local-only finding that
/// mentions a network for an unrelated reason ("network I/O", "network calls to
/// the embedder") is rejected too, and its field falls back to the honesty
/// marker with a note in Synthesis Status. A due-diligence report claiming
/// network reach for a loopback-bound daemon is the worse of the two errors.
const REACHABILITY_WORDS: &[&str] = &["remote", "network", "internet"];

/// The two words every "this dimension has no clean signal" sentence is built
/// around (#6082 lap 7).
///
/// Two words rather than one phrase, because the model puts the dimension
/// between them — the graded report wrote "No clean security signal is credited
/// here", and a literal `clean signal` match misses that. Paired with
/// [`CLEAN_SIGNAL_NEGATIONS`], which is what turns the noun phrase into a claim
/// of absence.
const CLEAN_SIGNAL_WORDS: (&str, &str) = ("clean", "signal");

/// The negations that turn [`CLEAN_SIGNAL_WORDS`] into a claim of absence.
const CLEAN_SIGNAL_NEGATIONS: &[&str] = &[
    "no ", "not ", "none", "nothing", "never", "n't", "zero ", "absent", "without",
];

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
    /// GREEN security findings the Security Posture section credits by name.
    clean_signals: usize,
}

impl Grounding {
    /// Gather the grounding facts from the deterministic model.
    ///
    /// Why: every input is already in the report — the findings' own prose and
    /// the measured cargo topology — so nothing here is a second source of
    /// truth that could drift from what the report renders.
    /// What: [`Self::from_model_with`] over the investigation the model already
    /// carries.
    /// Test: `synthesize_grounding_tests.rs::{a_loopback_finding_is_indexed,
    /// a_remote_finding_vetoes_its_tokens}`.
    pub(crate) fn from_model(model: &ReportModel) -> Self {
        Self::from_model_with(model, model.investigation.as_ref())
    }

    /// Gather the grounding facts, reading the investigation from `inv` rather
    /// than from the model's own copy.
    ///
    /// Why: the investigation prose is guarded BEFORE `apply_investigation`
    /// records it on the model (#6082 lap 6), so at that point
    /// `model.investigation` is still `None` and the two loopback-scoped
    /// findings this report has would be invisible to the guard checking them.
    /// What: walks every repository's metric findings and `inv`'s verified
    /// findings for loopback/remote markers, and folds every declared
    /// [`CrateTopology`] into the load-bearing set.
    /// Test: `synthesize_grounding_tests.rs::a_loopback_finding_is_indexed`,
    /// `synthesize_tests.rs::investigation_business_impact_states_host_local_reach`.
    pub(crate) fn from_model_with(
        model: &ReportModel,
        inv: Option<&super::investigate::Investigation>,
    ) -> Self {
        let mut out = Grounding::default();
        let mut from_metrics = 0usize;
        let mut from_inv = 0usize;
        for repo in &model.repositories {
            if let Some(metrics) = &repo.metrics {
                for f in &metrics.findings {
                    out.ingest(
                        &f.title,
                        &f.component,
                        &[&f.description, &f.remediation, &f.component],
                    );
                    if is_clean_security_signal(f.category.as_str(), f.severity, &f.component) {
                        from_metrics += 1;
                    }
                }
            }
        }
        if let Some(inv) = inv {
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
                    if is_clean_security_signal(f.dimension.as_str(), f.severity, &f.file) {
                        from_inv += 1;
                    }
                }
            }
        }
        // The investigation's findings are COPIED into `metrics.findings` by
        // `apply_investigation`, so after that pass both loops see the same
        // signals. The larger count is the real one either way.
        out.clean_signals = from_metrics.max(from_inv);
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
                 never as remote, remotely exploitable, reachable by a remote attacker, or \
                 reachable over/from/on the network:\n",
            );
            for f in &self.local_only {
                out.push_str(&format!("- {}\n", f.title));
            }
        }
        if self.clean_signals > 0 {
            out.push_str(&format!(
                "\n## Clean security signals (measured — listed in the report you are writing \
                 for)\n\nThe Security Posture section credits {} GREEN security finding(s), each \
                 naming the file it was read from. Never write that no clean signal was found, \
                 credited, or recorded.\n",
                self.clean_signals
            ));
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
    /// Why: the single entry point `apply_guardrail` calls, so the three checks
    /// can never be applied to one field and not another.
    /// What: runs the load-bearing check first (it can only reject, so there is
    /// nothing to rewrite afterwards), then drops any false no-clean-signal
    /// claim, then runs the reachability check over what is left. Returns
    /// `Clean` when none fired.
    /// Test: `synthesize_grounding_tests.rs`.
    pub(crate) fn check(&self, text: &str) -> GroundingOutcome {
        if let Some(reason) = self.load_bearing_violation(text) {
            return GroundingOutcome::Rejected(reason);
        }
        let (text, mut notes) = self.drop_false_no_clean_signal(text);
        match self.reachability(&text) {
            GroundingOutcome::Clean if notes.is_empty() => GroundingOutcome::Clean,
            GroundingOutcome::Clean => GroundingOutcome::Rewritten(text, notes),
            GroundingOutcome::Rewritten(fixed, more) => {
                notes.extend(more);
                GroundingOutcome::Rewritten(fixed, notes)
            }
            rejected @ GroundingOutcome::Rejected(_) => rejected,
        }
    }

    /// Remove any sentence claiming this dimension credits no clean signal,
    /// when the deterministic list credits some (#6082 lap 7).
    ///
    /// Why: the graded report's Security Posture paragraph ended "No clean
    /// security signal is credited here." and the deterministic line directly
    /// under it listed eighteen, each with a `file:line`. The model writes that
    /// paragraph from the RED/AMBER findings alone — the GREEN clean signals are
    /// never in its prompt — so it has no way to know the list exists and states
    /// its absence in good faith.
    /// What: `(text, notes)` unchanged when the report credits no clean signal,
    /// which is the case the sentence is right about. Otherwise every sentence
    /// carrying [`CLEAN_SIGNAL_WORDS`] under a [`CLEAN_SIGNAL_NEGATIONS`] word
    /// is cut out and one note records the cut. The rest of the paragraph
    /// survives: one contradicted sentence is not grounds to drop a paragraph of
    /// otherwise-grounded prose.
    /// Test: `synthesize_grounding_tests.rs::{a_no_clean_signal_claim_is_dropped_when_signals_exist,
    /// a_no_clean_signal_claim_survives_when_there_are_none}`.
    fn drop_false_no_clean_signal(&self, text: &str) -> (String, Vec<String>) {
        if self.clean_signals == 0 {
            return (text.to_string(), Vec::new());
        }
        let mut out = text.to_string();
        let mut notes = Vec::new();
        for sentence in sentences(text) {
            let lower = sentence.to_lowercase();
            let Some(at) = lower.find(CLEAN_SIGNAL_WORDS.0) else {
                continue;
            };
            if !lower[at..].contains(CLEAN_SIGNAL_WORDS.1) {
                continue;
            }
            if !CLEAN_SIGNAL_NEGATIONS.iter().any(|n| lower.contains(n)) {
                continue;
            }
            out = remove_sentence(&out, sentence);
            notes.push(format!(
                "synthesis: a sentence claiming no clean signal was credited was dropped — the \
                 Security Posture section credits {} of them, each with the file it was read from",
                self.clean_signals
            ));
        }
        (out.trim().to_string(), notes)
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

    /// Correct — or refuse — a beyond-the-host reachability claim about a
    /// local-only finding.
    ///
    /// Why/What: see the module doc. A sentence is about a local-only finding
    /// when it shares one of that finding's subject tokens and no
    /// network-reachable finding claims the same token. The trigger and the
    /// ship/reject decision read the SAME [`REACHABILITY_WORDS`] vocabulary, so
    /// a spelling [`REACHABILITY_REWRITES`] cannot correct is rejected instead
    /// of skipped.
    fn reachability(&self, text: &str) -> GroundingOutcome {
        if self.local_only.is_empty() {
            return GroundingOutcome::Clean;
        }
        let mut out = text.to_string();
        let mut notes = Vec::new();
        for sentence in sentences(text) {
            let lower = sentence.to_lowercase();
            if !asserts_reachability(&lower) {
                continue;
            }
            let Some(finding) = self.local_subject(&lower) else {
                continue;
            };
            let corrected = rewrite_reachability(sentence);
            if asserts_reachability(&corrected) {
                return GroundingOutcome::Rejected(format!(
                    "a beyond-the-host reachability claim about '{}' could not be corrected \
                     safely — that finding's own text scopes it to localhost",
                    finding.title
                ));
            }
            out = out.replace(sentence, &corrected);
            notes.push(format!(
                "synthesis: reachability corrected — '{}' is loopback-scoped by its own text, so \
                 the beyond-the-host reachability wording was rewritten",
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

/// Is this finding a clean security signal the Security Posture section credits?
///
/// Mirrors `reporter_codesec::push_security_violation_rows` exactly, including
/// its requirement that a clean signal name the file it was read from — a GREEN
/// with no citation is not credited, so the guardrail must not count it either.
fn is_clean_security_signal(
    dimension: &str,
    severity: super::metrics::Severity,
    cite: &str,
) -> bool {
    severity == super::metrics::Severity::Green
        && dimension == super::investigate::SECURITY_DIMENSION
        && !cite.trim().is_empty()
}

/// Cut `sentence` out of `text`, taking the space that separated it from its
/// neighbour so the remaining prose keeps single spacing.
fn remove_sentence(text: &str, sentence: &str) -> String {
    let spaced = format!("{sentence} ");
    if text.contains(&spaced) {
        return text.replace(&spaced, "");
    }
    text.replace(sentence, "")
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

/// True when a sentence asserts reachability beyond this host.
///
/// A plain substring test, not a word-boundary one: every rewrite in
/// [`REACHABILITY_REWRITES`] removes its own reachability word, so any
/// occurrence left belongs to a phrase this module does not know how to
/// correct — including a hyphenated one like `remote-management` or
/// `network-attached`, which a boundary test would miss.
fn asserts_reachability(sentence: &str) -> bool {
    let lower = sentence.to_lowercase();
    REACHABILITY_WORDS.iter().any(|w| lower.contains(w))
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
