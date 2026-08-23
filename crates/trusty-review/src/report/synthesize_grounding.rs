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
//! #6082 lap 8 moved the reachability question off the finding's own wording and
//! onto its COMPONENT, because a finding whose text happened to carry no
//! loopback marker was never classified and so was never checked at all. A
//! component now falls in one of three tiers, and [`Grounding::check`] takes the
//! checked field's owning component so the tier is decided by the file the
//! finding sits in rather than by which words the sentence happens to share with
//! some other finding's title:
//!
//! 1. LOOPBACK-ESTABLISHED — some finding on that file carries a
//!    [`LOOPBACK_MARKERS`] word. Every finding on the file inherits it, and every
//!    [`REACHABILITY_WORDS`] occurrence in that finding's own narrative is
//!    rewritten where [`REACHABILITY_REWRITES`] can and rejected where it cannot.
//! 2. REMOTE-ESTABLISHED — some finding on that file carries a
//!    [`REMOTE_MARKERS`] word. Left alone; remote wins over local on one file.
//! 3. UNESTABLISHED — the collected data says nothing either way. A sentence
//!    making a positive beyond-the-host reach claim is REJECTED, never
//!    rewritten: the report has no evidence the surface is host-local either, so
//!    substituting a host-local phrase would trade one unsupported claim for
//!    another.
//!
//! Tier 3 is what stops the lap-8 line. No finding on
//! `crates/trusty-mpm/src/daemon/api/control_routes.rs` carried any reachability
//! marker, so tier 1 could not reach it however far inheritance spread, and its
//! business impact shipped "permits remote code execution" unexamined.
//!
//! #6082 lap 9 changed what makes a tier-3 sentence a reach claim. It used to be
//! the [`REACHABILITY_REWRITES`] table, and the same file shipped again — this
//! time "a critical remote-execution and privilege risk", a hyphenated compound
//! no pattern matches, so the sentence was skipped rather than examined. The
//! trigger is now the [`REACHABILITY_WORDS`] vocabulary read as tokens, which
//! sees inside a compound and across morphological variants, narrowed to CLAIM
//! shape by [`synthesize_grounding_text::CLAIM_WORDS`]: the word must be
//! attributing access, attack, execution or exposure to the surface. A bare
//! "network" in "a transient network error" still costs a reader nothing.
//!
//! [`synthesize_grounding_text::CLAIM_WORDS`]: super::synthesize_grounding_text::CLAIM_WORDS
//!
//! Test: `synthesize_grounding_tests.rs`.

use std::collections::BTreeSet;

use super::model::ReportModel;
use super::synthesize_grounding_text::{
    asserts_reachability, asserts_reachability_claim, contains_name, remove_sentence,
    rewrite_reachability, sentences, subject_tokens,
};
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
pub(super) const REACHABILITY_REWRITES: &[(&str, &str)] = &[
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
pub(super) const REACHABILITY_WORDS: &[&str] = &["remote", "network", "internet"];

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
pub(super) const GENERIC_TOKENS: &[&str] = &[
    "crates", "src", "main", "lib", "mod", "test", "tests", "with", "this", "that", "from", "into",
    "have", "http", "https", "file", "line", "code", "data", "self", "type", "used", "when",
    "over", "path", "call", "rust",
];

/// Shortest path/title word admitted as a subject token.
pub(super) const MIN_TOKEN_LEN: usize = 4;

/// One finding the report's own data scopes to a loopback-only surface.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalOnly {
    /// The finding's title, for the note the guardrail records.
    title: String,
    /// The file this finding sits in, normalized by [`normalize_component`].
    ///
    /// This is what makes the classification shared: every finding on a
    /// loopback-established file gets an entry here, whatever its own text says.
    component: String,
    /// The words a sentence must share to be about this finding.
    tokens: BTreeSet<String>,
}

/// One finding as read, before its component's tier is known.
///
/// Why: a file's tier depends on EVERY finding on it, so no single finding can
/// be classified while the walk is still running (#6082 lap 8).
struct Staged {
    title: String,
    component: String,
    tokens: BTreeSet<String>,
    local: bool,
    remote: bool,
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
    ///
    /// `subject` is the §5.1/§5.2 finding the refused claim was about, when the
    /// check identified one. Report-level prose — the executive, code-quality,
    /// security and authorship summaries, and the top-risk rows — belongs to no
    /// finding, so its disclosure had no subject to number and the reader was
    /// left to search the document for the title it quoted (#6082 lap 9).
    Rejected {
        /// Why the field cannot ship, as the reader sees it.
        reason: String,
        /// The finding title the reporter resolves to a rendered number.
        subject: Option<String>,
    },
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
    /// Files some finding establishes as network-reachable (#6082 lap 8).
    remote_components: BTreeSet<String>,
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
        let mut staged: Vec<Staged> = Vec::new();
        let mut from_metrics = 0usize;
        let mut from_inv = 0usize;
        for repo in &model.repositories {
            if let Some(metrics) = &repo.metrics {
                for f in &metrics.findings {
                    staged.push(stage(
                        &f.title,
                        &f.component,
                        &[&f.description, &f.remediation, &f.component],
                    ));
                    if is_clean_security_signal(f.category.as_str(), f.severity, &f.component) {
                        from_metrics += 1;
                    }
                }
            }
        }
        if let Some(inv) = inv {
            for repo in &inv.repos {
                for f in &repo.findings {
                    staged.push(stage(
                        &f.title,
                        &f.file,
                        &[
                            &f.description,
                            &f.remediation,
                            &f.business_impact,
                            &f.evidence_quote,
                            &f.file,
                        ],
                    ));
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
        out.classify(staged);
        for topology in model
            .repositories
            .iter()
            .filter_map(|r| r.crate_topology.as_ref())
        {
            out.ingest_topology(topology);
        }
        out
    }

    /// Settle every staged finding's tier, then spread it across its file.
    ///
    /// Why: reachability is a property of the surface, not of the sentence that
    /// happens to describe it. One finding on a file stating "localhost" states
    /// it for every finding on that file (#6082 lap 8).
    /// What: collects the loopback- and remote-established files, lets remote win
    /// where both fired, then records every finding on a loopback-established
    /// file as [`LocalOnly`] — including the ones whose own text said nothing.
    /// A finding with no component is classified by its own text alone, as
    /// before: there is no file to share the classification with.
    /// Test: `synthesize_grounding_tests.rs::{a_sibling_finding_inherits_its_files_loopback_scope,
    /// a_remote_finding_on_the_file_wins_over_a_loopback_sibling}`.
    fn classify(&mut self, staged: Vec<Staged>) {
        let mut local_files = BTreeSet::new();
        for s in &staged {
            if s.component.is_empty() {
                continue;
            }
            if s.remote {
                self.remote_components.insert(s.component.clone());
            } else if s.local {
                local_files.insert(s.component.clone());
            }
        }
        for f in &self.remote_components {
            local_files.remove(f);
        }
        let mut seen = BTreeSet::new();
        for s in staged {
            let has_file = !s.component.is_empty();
            let remote = if has_file {
                self.remote_components.contains(&s.component)
            } else {
                s.remote
            };
            if remote {
                self.remote_tokens.extend(s.tokens);
                continue;
            }
            let local = if has_file {
                local_files.contains(&s.component)
            } else {
                s.local
            };
            if !local || !seen.insert((s.title.clone(), s.component.clone())) {
                continue;
            }
            self.local_only.push(LocalOnly {
                title: s.title,
                component: s.component,
                tokens: s.tokens,
            });
        }
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
    ///
    /// `owner` is the component of the finding this field belongs to, and `None`
    /// for report-level prose that belongs to no single finding — the executive,
    /// code-quality, security and authorship summaries, and the top-risk rows.
    /// A named owner decides the reachability tier on its own; ownerless prose
    /// falls back to matching sentences against every loopback-scoped finding's
    /// subject tokens, which is the only thing available when the prose is about
    /// the whole estate (#6082 lap 8).
    /// Test: `synthesize_grounding_tests.rs`.
    pub(crate) fn check(&self, text: &str, owner: Option<&str>) -> GroundingOutcome {
        if let Some(reason) = self.load_bearing_violation(text) {
            // A load-bearing violation names a CRATE, not a finding, so there is
            // no §5.x row to send the reader to.
            return GroundingOutcome::Rejected {
                reason,
                subject: None,
            };
        }
        let (text, mut notes) = self.drop_false_no_clean_signal(text);
        match self.reachability(&text, owner) {
            GroundingOutcome::Clean if notes.is_empty() => GroundingOutcome::Clean,
            GroundingOutcome::Clean => GroundingOutcome::Rewritten(text, notes),
            GroundingOutcome::Rewritten(fixed, more) => {
                notes.extend(more);
                GroundingOutcome::Rewritten(fixed, notes)
            }
            rejected @ GroundingOutcome::Rejected { .. } => rejected,
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
                "a sentence claiming no clean signal was credited was dropped — the Security \
                 Posture section credits {} of them, each with the file it was read from",
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

    /// Correct — or refuse — a beyond-the-host reachability claim.
    ///
    /// Why/What: the three tiers in the module doc, in one pass. A field whose
    /// owning component is loopback-established is checked against that
    /// component, with no token matching at all: the field belongs to the
    /// finding, so guessing which finding a sentence is about can only get it
    /// wrong. Ownerless prose keeps the token match. A field whose owning
    /// component has no reachability evidence is tier 3 — a positive reach claim
    /// there is rejected rather than rewritten, because "host-local" would be
    /// just as unsupported as "remote".
    /// Test: `synthesize_grounding_tests.rs::{a_sibling_finding_inherits_its_files_loopback_scope,
    /// an_unevidenced_component_cannot_claim_beyond_host_reach,
    /// an_unevidenced_component_keeps_a_non_reach_mention_of_the_network}`.
    fn reachability(&self, text: &str, owner: Option<&str>) -> GroundingOutcome {
        let key = owner.map(normalize_component).filter(|k| !k.is_empty());
        let owned = key
            .as_deref()
            .and_then(|k| self.local_only.iter().find(|f| f.component == k));
        // Tier 3: an owner the collected data places neither on loopback nor on
        // the network. Tier 2 (remote-established) and an owner with no usable
        // path both fall through to `Clean` below.
        if let Some(k) = key.as_deref()
            && owned.is_none()
        {
            return match self.remote_components.contains(k) {
                true => GroundingOutcome::Clean,
                false => self.refuse_unevidenced_reach(text, owner.unwrap_or_default()),
            };
        }
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
            let finding = match owned {
                Some(f) => f,
                None => match self.local_subject(&lower) {
                    Some(f) => f,
                    None => continue,
                },
            };
            let corrected = rewrite_reachability(sentence);
            if asserts_reachability(&corrected) {
                return GroundingOutcome::Rejected {
                    reason: format!(
                        "a beyond-the-host reachability claim about '{}' could not be corrected \
                         safely — the report's own evidence scopes that surface to localhost",
                        finding.title
                    ),
                    subject: Some(finding.title.clone()),
                };
            }
            out = out.replace(sentence, &corrected);
            notes.push(format!(
                "the beyond-the-host reachability wording was corrected — the report's own \
                 evidence scopes '{}' to a loopback-only surface",
                finding.title
            ));
        }
        match notes.is_empty() {
            true => GroundingOutcome::Clean,
            false => GroundingOutcome::Rewritten(out, notes),
        }
    }

    /// Tier 3: refuse a positive beyond-the-host reach claim about a component
    /// the collected data says nothing about.
    ///
    /// Why: rewriting needs evidence the surface is host-local, and there is
    /// none — so the only honest outcomes are ship-unexamined or withhold. A
    /// due-diligence report withholds.
    /// What: `Rejected` for a sentence that CLAIMS reach, `Clean` otherwise. A
    /// sentence claims reach when [`asserts_reachability_claim`] reads a
    /// [`REACHABILITY_WORDS`] token — inside a hyphenated compound or a
    /// morphological variant, not only as a bare word — attributing access,
    /// attack, execution or exposure to the surface, or when
    /// [`REACHABILITY_REWRITES`] recognises the phrase outright.
    ///
    /// The trigger used to be the rewrite table alone, which is how
    /// `remote-execution` shipped: it matches no pattern, so the sentence was
    /// skipped (#6082 lap 9). The vocabulary alone is the other error — it would
    /// take "a transient network error" with it, and withholding a field over a
    /// word that asserts nothing about reach costs a reader more than it saves.
    /// The claim shape is what separates the two.
    fn refuse_unevidenced_reach(&self, text: &str, component: &str) -> GroundingOutcome {
        for sentence in sentences(text) {
            if !asserts_reachability(&sentence.to_lowercase()) {
                continue;
            }
            if !asserts_reachability_claim(sentence) && rewrite_reachability(sentence) == sentence {
                continue;
            }
            // The component has no finding this check can name — that is what
            // "unevidenced" means here — so the disclosure cites the file.
            return GroundingOutcome::Rejected {
                reason: format!(
                    "a beyond-the-host reachability claim about `{}` cannot be verified — the \
                     collected data records no evidence of how that surface is reached",
                    component.trim()
                ),
                subject: None,
            };
        }
        GroundingOutcome::Clean
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

/// Read one finding's reachability markers, deferring the verdict to
/// [`Grounding::classify`].
fn stage(title: &str, component: &str, texts: &[&String]) -> Staged {
    let joined = texts
        .iter()
        .map(|t| t.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    Staged {
        title: title.trim().to_string(),
        component: normalize_component(component),
        tokens: subject_tokens(title, component),
        local: LOOPBACK_MARKERS.iter().any(|m| joined.contains(m)),
        remote: REMOTE_MARKERS.iter().any(|m| joined.contains(m)),
    }
}

/// Reduce a component reference to the bare file path it names.
///
/// Why: the same file arrives as `crates/…/control_routes.rs` from the
/// investigation and `crates/…/control_routes.rs:268` from the metrics rows, and
/// two findings on one file must land on one key or they cannot share a tier.
/// What: separators normalized to `/`, any trailing `:<line>[:<column>]` dropped,
/// lower-cased. An empty or non-path component yields an empty string, which
/// [`Grounding::classify`] reads as "no file to share with".
/// Test: `synthesize_grounding_tests.rs::a_line_suffix_does_not_split_a_component`.
fn normalize_component(raw: &str) -> String {
    let normalized = raw.trim().replace('\\', "/");
    let mut path = normalized.as_str();
    while let Some((head, tail)) = path.rsplit_once(':') {
        if tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        path = head;
    }
    path.trim().to_lowercase()
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

#[cfg(test)]
#[path = "synthesize_grounding_tests.rs"]
mod tests;
