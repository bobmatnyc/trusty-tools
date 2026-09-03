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
//! told the answer rather than left to guess it. And it runs [`Grounding::check_field`]
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
//! component now falls in one of three tiers, and [`Grounding::check_field`] takes the
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
//! #6082 lap 10 made the REWRITE itself fail closed. Until then a rewrite always
//! shipped, and the graded report shipped "reachable by any process on the host
//! rather than a any local process": the `remote attacker` rule spliced its
//! replacement in without consuming the article in front of it, and the
//! corrected clause then contrasted host-local reach with host-local reach.
//! Two changes. A replacement opening with a determiner now takes the preceding
//! article with the phrase it replaces, so no doubled article can be emitted;
//! and every rewritten sentence is read by
//! [`synthesize_grounding_text::grammar_debris`] before it may ship, which
//! sends a doubled determiner or a self-contrast down the reject-and-disclose
//! path instead. A visible withholding is the better of the two failures.
//!
//! #6691 added a fourth claim, in the lap-7 shape: a Code Quality paragraph
//! saying no complexity distribution was provided, four lines above the
//! deterministic table that renders one. The model wrote it in good faith — the
//! digest never carried the distribution. Both halves are fixed:
//! `synthesize_prompt::repo_profile_lines` now states the same string the
//! `cq_complexity` cell renders, and [`Grounding::check_field`] drops the claim
//! afterwards whatever the model was given.
//!
//! #6082 lap 11 made a disclosure name the finding whose field it is. The tier
//! is read from the FILE, so [`Grounding::reachability`] resolves it through
//! that file's first loopback record — and it quoted that record's title. The
//! graded report's RED findings 2 and 3 both sit in `control_routes.rs`, so
//! their two withholding lines shipped byte-identical, both naming finding 2.
//! [`Grounding::check_field`] now takes the field's own subject, and
//! [`named_subject`] prefers it over the shared record.
//!
//! [`synthesize_grounding_text::CLAIM_WORDS`]: super::synthesize_grounding_text::CLAIM_WORDS
//! [`synthesize_grounding_text::grammar_debris`]: super::synthesize_grounding_text::grammar_debris
//!
//! Test: `synthesize_grounding_tests.rs`.

use std::collections::BTreeSet;

use super::investigate::ExposureIndex;
use super::model::ReportModel;
use super::synthesize_grounding_text::{
    asserts_reachability, asserts_reachability_claim, contains_name, grammar_debris,
    remove_sentence, rewrite_reachability, sentences, subject_tokens,
};
use super::synthesize_grounding_vocab::{
    ABSENCE_NEGATIONS, CLEAN_SIGNAL_WORDS, COMPLEXITY_WORDS, DATA_SUPPLY_WORDS,
    LOAD_BEARING_PHRASES, LOOPBACK_MARKERS, REMOTE_MARKERS,
};
use super::topology::CrateTopology;

// The four tables `synthesize_grounding_text` reads keep their existing path
// through this module, so the split that moved them (#6191, SLOC cap) changed no
// import anywhere else.
pub(super) use super::synthesize_grounding_vocab::{
    GENERIC_TOKENS, MIN_TOKEN_LEN, REACHABILITY_REWRITES, REACHABILITY_WORDS,
};

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
/// corrected text plus one [`Correction`] per correction; `Rejected` carries the
/// reason the field cannot ship.
/// Test: `synthesize_grounding_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GroundingOutcome {
    /// Nothing contradicted the model's data.
    Clean,
    /// The text was corrected; the corrections name what changed.
    Rewritten(String, Vec<Correction>),
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

/// One disclosure that the guardrail changed the model's prose, and the finding
/// it changed it about.
///
/// Why: a corrected-wording line and a withheld line are the same kind of
/// disclosure to a reader, so both must point at the same numbered §5.1/§5.2
/// row. Only the withheld path carried its subject, and the graded lap-10 report
/// showed the split in one block: two lines reading "section 5.1, RED finding 2"
/// beside a corrected-wording line that quoted a title and left the reader to
/// find it (#6082 lap 10).
/// What: `text` is the complete reader sentence; `subject` is the finding title
/// the reporter resolves to a number, absent when the correction was about no
/// single finding.
/// Test: `synthesize_grounding_tests.rs::a_correction_note_carries_the_finding_it_is_about`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Correction {
    /// The disclosure, as the reader sees it.
    pub(crate) text: String,
    /// The finding this correction was about, when it was about one.
    pub(crate) subject: Option<String>,
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
    /// Functions the Code Quality table's complexity distribution accounts for.
    ///
    /// #6691: the same buckets `reporter_codesec::bucket_summary` renders into
    /// the `cq_complexity` cell. Non-zero means the report states a distribution,
    /// and a paragraph above that table saying none was provided contradicts it.
    complexity_functions: u64,
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
        // #6691: read from the same buckets the Code Quality table renders.
        out.complexity_functions = model
            .repositories
            .iter()
            .filter_map(|r| r.metrics.as_ref())
            .flat_map(|m| m.complexity.buckets.iter())
            .map(|b| b.count)
            .sum();
        // #6191: the investigation's collected bind/exposure facts decide a
        // file's tier before any text marker gets a vote.
        let evidence = ExposureIndex::from_facts(
            inv.into_iter()
                .flat_map(|i| i.repos.iter())
                .flat_map(|r| r.exposure.iter()),
        );
        out.classify(staged, &evidence);
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
    ///
    /// #6191: `evidence` is the collection-side answer, read from the files'
    /// own source, and it settles a file's tier OUTRIGHT — a measured bind
    /// address is a fact about the surface, and a marker in some finding's prose
    /// is a guess about it. Only a file the collector said nothing about falls
    /// through to the marker walk below, which is what leaves an
    /// evidence-free run byte-identical to the pre-#6191 one.
    /// Test: `synthesize_grounding_tests.rs::{a_sibling_finding_inherits_its_files_loopback_scope,
    /// a_remote_finding_on_the_file_wins_over_a_loopback_sibling,
    /// collected_bind_evidence_outranks_a_text_marker,
    /// a_collected_outbound_call_establishes_reach_a_marker_never_stated}`.
    fn classify(&mut self, staged: Vec<Staged>, evidence: &ExposureIndex) {
        let mut local_files = BTreeSet::new();
        for s in &staged {
            if s.component.is_empty() {
                continue;
            }
            if let Some(kind) = evidence.kind(&s.component) {
                if kind.is_beyond_host() {
                    self.remote_components.insert(s.component.clone());
                } else {
                    local_files.insert(s.component.clone());
                }
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
    /// claim and any false no-complexity-data claim (#6691), then runs the
    /// reachability check over what is left. Returns `Clean` when none fired.
    ///
    /// `owner` is the component of the finding this field belongs to, and `None`
    /// for report-level prose that belongs to no single finding — the executive,
    /// code-quality, security and authorship summaries, and the top-risk rows.
    /// A named owner decides the reachability tier on its own; ownerless prose
    /// falls back to matching sentences against every loopback-scoped finding's
    /// subject tokens, which is the only thing available when the prose is about
    /// the whole estate (#6082 lap 8).
    ///
    /// `subject` is that finding's own title. The tier is read from the FILE, so
    /// two findings on one file share the record it is read from — but each
    /// disclosure must name the finding whose field it is, not that shared
    /// record's (#6082 lap 11).
    /// Test: `synthesize_grounding_tests.rs`.
    pub(crate) fn check_field(
        &self,
        text: &str,
        owner: Option<&str>,
        subject: Option<&str>,
    ) -> GroundingOutcome {
        if let Some(reason) = self.load_bearing_violation(text) {
            // A load-bearing violation names a CRATE, not a finding, so there is
            // no §5.x row to send the reader to.
            return GroundingOutcome::Rejected {
                reason,
                subject: None,
            };
        }
        let (text, mut notes) = self.drop_false_no_clean_signal(text);
        // #6691: the same shape, for a paragraph denying the complexity
        // distribution the table under it renders.
        let (text, complexity_notes) = self.drop_false_no_complexity_data(&text);
        notes.extend(complexity_notes);
        match self.reachability(&text, owner, subject) {
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
    /// carrying [`CLEAN_SIGNAL_WORDS`] under an [`ABSENCE_NEGATIONS`] word
    /// is cut out and one note records the cut. The rest of the paragraph
    /// survives: one contradicted sentence is not grounds to drop a paragraph of
    /// otherwise-grounded prose.
    /// Test: `synthesize_grounding_tests.rs::{a_no_clean_signal_claim_is_dropped_when_signals_exist,
    /// a_no_clean_signal_claim_survives_when_there_are_none}`.
    fn drop_false_no_clean_signal(&self, text: &str) -> (String, Vec<Correction>) {
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
            if !ABSENCE_NEGATIONS.iter().any(|n| lower.contains(n)) {
                continue;
            }
            out = remove_sentence(&out, sentence);
            // A clean-signal claim is about the report's own GREEN list, not
            // about any one §5.x finding, so there is no subject to number.
            notes.push(Correction {
                text: format!(
                    "a sentence claiming no clean signal was credited was dropped — the Security \
                     Posture section credits {} of them, each with the file it was read from",
                    self.clean_signals
                ),
                subject: None,
            });
        }
        (out.trim().to_string(), notes)
    }

    /// Remove any sentence claiming the run was given no complexity data, when
    /// the Code Quality table renders a distribution (#6691).
    ///
    /// Why: the graded report's Code Quality paragraph said "no complexity
    /// distribution, code-smell taxonomy, or crate-topology/coupling data was
    /// provided", and four lines under it the deterministic table rendered
    /// A 68618 / B 5241 / C 1836 / D 720 / F 733. Feeding the distribution into
    /// the digest (`synthesize_prompt::repo_profile_lines`) is the derivation
    /// half; this is the mechanical half, so the contradiction cannot ship even
    /// when the model writes past its input.
    /// What: `(text, notes)` unchanged when no repository contributed a bucket —
    /// the case the sentence is right about. Otherwise every sentence carrying
    /// [`COMPLEXITY_WORDS`] under an [`ABSENCE_NEGATIONS`] word AND a
    /// [`DATA_SUPPLY_WORDS`] word is cut out and one note records the cut. The
    /// three conditions together are what leave a sentence about the
    /// distribution's SHAPE alone; only a claim about what the run was given is
    /// this check's business.
    /// Test: `synthesize_grounding_tests.rs::{a_no_complexity_data_claim_is_dropped_when_the_table_renders_one,
    /// a_no_complexity_data_claim_survives_when_no_bucket_was_measured,
    /// a_claim_about_the_distributions_shape_is_left_alone}`.
    fn drop_false_no_complexity_data(&self, text: &str) -> (String, Vec<Correction>) {
        if self.complexity_functions == 0 {
            return (text.to_string(), Vec::new());
        }
        let mut out = text.to_string();
        let mut notes = Vec::new();
        for sentence in sentences(text) {
            let lower = sentence.to_lowercase();
            let Some(at) = lower.find(COMPLEXITY_WORDS.0) else {
                continue;
            };
            if !lower[at..].contains(COMPLEXITY_WORDS.1) {
                continue;
            }
            if !ABSENCE_NEGATIONS.iter().any(|n| lower.contains(n)) {
                continue;
            }
            if !DATA_SUPPLY_WORDS.iter().any(|w| lower.contains(w)) {
                continue;
            }
            out = remove_sentence(&out, sentence);
            // The distribution is the report's own measurement, not any one
            // §5.x finding, so there is no subject to number.
            notes.push(Correction {
                text: format!(
                    "a sentence claiming no complexity distribution was provided was dropped — \
                     the Code Quality table renders one over {} functions",
                    self.complexity_functions
                ),
                subject: None,
            });
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
    ///
    /// `subject` names the finding the field belongs to. The record `owned`
    /// finds is keyed by FILE, so every finding on one file resolves to the
    /// first of them — correct for the tier, wrong for the disclosure, which
    /// quotes a title (#6082 lap 11).
    /// Test: `synthesize_grounding_tests.rs::{a_sibling_finding_inherits_its_files_loopback_scope,
    /// an_unevidenced_component_cannot_claim_beyond_host_reach,
    /// an_unevidenced_component_keeps_a_non_reach_mention_of_the_network,
    /// two_findings_on_one_file_are_each_withheld_under_their_own_title}`.
    fn reachability(
        &self,
        text: &str,
        owner: Option<&str>,
        subject: Option<&str>,
    ) -> GroundingOutcome {
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
            // #6082 lap 11: the disclosure names the finding whose field this is
            // — `finding` is the file's first loopback record, which is the same
            // record for every finding on that file.
            let named = named_subject(subject, finding);
            let corrected = rewrite_reachability(sentence);
            // #6082 lap 10: a rewrite that leaves the sentence ungrammatical, or
            // contrasting one reachability class with itself, takes the same
            // withhold path as a claim no rewrite reaches.
            let debris = grammar_debris(&corrected);
            if asserts_reachability(&corrected) || debris.is_some() {
                let detail = debris.unwrap_or_else(|| {
                    "the report's own evidence scopes that surface to localhost".to_string()
                });
                return GroundingOutcome::Rejected {
                    reason: format!(
                        "a beyond-the-host reachability claim about '{named}' could not be \
                         corrected safely — {detail}"
                    ),
                    subject: Some(named.to_string()),
                };
            }
            out = out.replace(sentence, &corrected);
            notes.push(Correction {
                text: format!(
                    "the beyond-the-host reachability wording was corrected — the report's own \
                     evidence scopes '{named}' to a loopback-only surface"
                ),
                subject: Some(named.to_string()),
            });
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

/// The finding title a disclosure quotes: the field's own, or the record the
/// tier was read from when the field belongs to no named finding.
///
/// Why: [`Grounding::reachability`] resolves the tier through a record keyed by
/// FILE. Two findings on one file resolve to the same record, so quoting that
/// record's title gave RED findings 2 and 3 of the graded report byte-identical
/// withholding disclosures, both naming finding 2 (#6082 lap 11).
/// What: the caller's `subject` when it is non-empty, else `fallback`'s title —
/// which is what report-level prose, matched by subject token, still needs.
/// Test: `synthesize_grounding_tests.rs::two_findings_on_one_file_are_each_withheld_under_their_own_title`.
fn named_subject<'a>(subject: Option<&'a str>, fallback: &'a LocalOnly) -> &'a str {
    subject
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback.title.as_str())
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
