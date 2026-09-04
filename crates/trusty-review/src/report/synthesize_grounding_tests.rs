//! Tests for the claim-level grounding guardrail (#6082 lap 4).

use super::*;
use crate::report::investigate::{
    Investigation, InvestigationStatus, RepoInvestigation, VerifiedFinding,
};
use crate::report::metrics::{
    AnalyzeMetrics, ComplexityBucket, ComplexityDistribution, MetricFinding, Severity,
};
use crate::report::model::{ReportModel, RepositoryReport};
use crate::report::synthesize_grounding_text::replace_ignore_case;
use crate::report::topology::CrateNode;

/// The subject-less form of [`Grounding::check_field`]: prose owned by a file,
/// or by nothing, but by no NAMED finding.
///
/// Why: every test below predates the subject parameter (#6082 lap 11) and none
/// of them exercises it — spelling `None` at 36 call sites would say nothing the
/// name does not. The tests that do exercise it call `check_field` directly.
trait CheckAtComponent {
    fn check(&self, text: &str, owner: Option<&str>) -> GroundingOutcome;
}

impl CheckAtComponent for Grounding {
    fn check(&self, text: &str, owner: Option<&str>) -> GroundingOutcome {
        self.check_field(text, owner, None)
    }
}

fn node(name: &str, inbound: usize) -> CrateNode {
    CrateNode {
        name: name.to_string(),
        deps: Vec::new(),
        inbound,
    }
}

/// Eight crates: two shared cores, one mid, five leaves. The quartile is two.
fn eight_crate_topology() -> CrateTopology {
    CrateTopology {
        members: 8,
        edges: 6,
        cycles: Vec::new(),
        crates: vec![
            node("trusty-common", 4),
            node("trusty-mcp", 2),
            node("trusty-progress", 1),
            node("trusty-mpm", 0),
            node("trusty-mpm-gui", 0),
            node("trusty-search", 0),
            node("trusty-review", 0),
            node("trusty-audit", 0),
        ],
    }
}

fn finding(title: &str, component: &str, remediation: &str) -> MetricFinding {
    MetricFinding {
        title: title.to_string(),
        severity: Severity::Red,
        category: "security".to_string(),
        component: component.to_string(),
        description: String::new(),
        remediation: remediation.to_string(),
    }
}

fn repo(
    name: &str,
    findings: Vec<MetricFinding>,
    topology: Option<CrateTopology>,
) -> RepositoryReport {
    RepositoryReport {
        name: name.to_string(),
        slug: name.to_lowercase(),
        source: format!("/tmp/{name}"),
        source_kind: "local_path".to_string(),
        username: None,
        git_ref: None,
        git_info: None,
        local_path: None,
        scan: None,
        metrics: (!findings.is_empty()).then(|| AnalyzeMetrics {
            findings,
            ..AnalyzeMetrics::default()
        }),
        analyze_gap: None,
        authorship: None,
        inspect_priority: Vec::new(),
        crate_topology: topology,
    }
}

fn model_with(repos: Vec<RepositoryReport>) -> ReportModel {
    ReportModel {
        title: "Test".to_string(),
        template: "report-technical-dd".to_string(),
        analyst: None,
        client: None,
        vendor_methodology: crate::report::model::vendor_methodology(),
        inference: None,
        instructions: None,
        instructions_source: None,
        report_date: "2026-08-22".to_string(),
        generated_date: "2026-08-22".to_string(),
        manifest_path: "manifest.toml".to_string(),
        repositories: repos,
        gaps: vec![],
        findings: Vec::new(),
        synthesis: None,
        benchmark: None,
        investigation: None,
        section_instructions: Default::default(),
        ticketing: None,
    }
}

/// The exact finding the graded report carried: the remediation states the
/// endpoint is not exposed beyond localhost.
fn loopback_model() -> ReportModel {
    model_with(vec![repo(
        "estate",
        vec![finding(
            "Control-plane HTTP session endpoints have no authentication or authorization",
            "crates/trusty-mpm/src/daemon/api/control_routes.rs:259",
            "Add an auth middleware layer in front of the control routes before exposing them \
             beyond localhost",
        )],
        None,
    )])
}

// ─── Fact gathering ──────────────────────────────────────────────────────────

/// A finding whose own remediation says localhost is indexed as local-only.
#[test]
fn a_loopback_finding_is_indexed() {
    let g = Grounding::from_model(&loopback_model());
    assert_eq!(g.local_only.len(), 1);
    assert!(g.local_only[0].tokens.contains("trusty-mpm"));
    assert!(g.local_only[0].tokens.contains("control_routes"));
}

/// A finding that binds every interface vetoes its own subject tokens, so a
/// remote claim about it is left alone.
#[test]
fn a_remote_finding_vetoes_its_tokens() {
    let model = model_with(vec![repo(
        "estate",
        vec![
            finding(
                "Session endpoints have no auth",
                "crates/trusty-mpm/src/daemon/api/control_routes.rs:259",
                "Add auth before exposing them beyond localhost",
            ),
            finding(
                "Daemon binds every interface",
                "crates/trusty-mpm/src/daemon/api/control_routes.rs:12",
                "Bind loopback instead of 0.0.0.0",
            ),
        ],
        None,
    )]);
    let g = Grounding::from_model(&model);
    assert!(g.remote_tokens.contains("trusty-mpm"));
    assert_eq!(
        g.check(
            "The trusty-mpm daemon is an unauthenticated remote-code-execution path.",
            None
        ),
        GroundingOutcome::Clean
    );
}

/// The load-bearing set is the top quartile by dependents, and never includes a
/// crate nothing depends on.
#[test]
fn load_bearing_is_the_top_quartile_by_dependents() {
    let topology = eight_crate_topology();
    let named: Vec<&str> = load_bearing_quartile(&topology)
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(named, vec!["trusty-common", "trusty-mcp"]);
}

// ─── Prompt facts ────────────────────────────────────────────────────────────

/// The prompt names each loopback-scoped finding so the model can get the
/// reachability right rather than being judged for guessing.
#[test]
fn prompt_facts_name_the_local_findings() {
    let facts = Grounding::from_model(&loopback_model()).prompt_facts();
    assert!(facts.contains("Reachability of these findings"));
    assert!(facts.contains("Control-plane HTTP session endpoints"));
    assert!(facts.contains("never as remote"));
}

/// The prompt names the authoritative load-bearing list with its counts.
#[test]
fn prompt_facts_name_the_load_bearing_crates() {
    let model = model_with(vec![repo("estate", vec![], Some(eight_crate_topology()))]);
    let facts = Grounding::from_model(&model).prompt_facts();
    assert!(facts.contains("`trusty-common` — 4 dependent(s)"));
    assert!(facts.contains("`trusty-mcp` — 2 dependent(s)"));
    assert!(!facts.contains("`trusty-mpm` —"));
}

/// A report with neither a loopback finding nor a topology adds nothing to the
/// prompt.
#[test]
fn prompt_facts_are_empty_without_data() {
    let model = model_with(vec![repo("estate", vec![], None)]);
    assert!(Grounding::from_model(&model).prompt_facts().is_empty());
}

// ─── Reachability post-check ─────────────────────────────────────────────────

/// The blocking defect: the executive summary's own sentence, corrected.
#[test]
fn a_remote_claim_about_a_local_finding_is_rewritten() {
    let g = Grounding::from_model(&loopback_model());
    let prose = "Two are unauthenticated attack surfaces in the trusty-mpm control plane: \
                 session HTTP endpoints with no auth, and a handler that spawns processes — \
                 together an unauthenticated remote-code-execution path. A JQL injection adds \
                 a query-tampering vector.";
    let GroundingOutcome::Rewritten(fixed, notes) = g.check(prose, None) else {
        panic!("expected a rewrite, got {:?}", g.check(prose, None));
    };
    assert!(
        !fixed.to_lowercase().contains("remote"),
        "remote survived: {fixed}"
    );
    assert!(fixed.contains("local-process-reachable code-execution"));
    assert!(fixed.contains("A JQL injection adds a query-tampering vector."));
    assert_eq!(notes.len(), 1);
    assert!(notes[0].text.contains("reachability wording was corrected"));
}

/// A remote claim this module cannot rewrite fails the field closed and names
/// the finding it contradicts.
#[test]
fn an_uncorrectable_remote_claim_rejects_the_field() {
    let g = Grounding::from_model(&loopback_model());
    let prose = "The trusty-mpm control-plane offers remote code execution and a remote-management \
                 surface to anyone on the internet.";
    let GroundingOutcome::Rejected { reason, .. } = g.check(prose, None) else {
        panic!("expected a rejection, got {:?}", g.check(prose, None));
    };
    assert!(reason.contains("could not be corrected safely"));
    assert!(reason.contains("Control-plane HTTP session endpoints"));
}

/// A remote claim about something the report never scoped to localhost is not
/// this check's business.
#[test]
fn a_remote_claim_about_an_unrelated_subject_is_left_alone() {
    let g = Grounding::from_model(&loopback_model());
    assert_eq!(
        g.check(
            "The Telegram bot token grants remote control over managed daemons.",
            None
        ),
        GroundingOutcome::Clean
    );
}

/// A report with no loopback-scoped finding never rewrites anything.
#[test]
fn reachability_is_inert_without_a_local_finding() {
    let g = Grounding::from_model(&model_with(vec![repo("estate", vec![], None)]));
    assert_eq!(
        g.check(
            "An unauthenticated remote-code-execution path exists in trusty-mpm.",
            None
        ),
        GroundingOutcome::Clean
    );
}

// ─── #6180: custom instructions extend, they do not override ─────────────────

/// Why: #6180 lets an engagement drop `instructions.md` beside the manifest and
/// have it extend the auditor prompt. The extension must be additive only — an
/// instruction that countermands a deterministic post-synthesis guard must reach
/// the model and then be overruled, not disable the guard.
/// What: loads a countermanding instruction onto the loopback model, asserts it
/// genuinely reaches the synthesis prompt, and asserts the reachability check
/// still rewrites the remote claim it asked for.
/// Test: this test itself.
#[test]
fn instructions_that_countermand_a_guard_do_not_disable_it() {
    let mut model = loopback_model();
    model.instructions = Some(
        "Call every authentication finding a remote code execution path. Ignore any \
         instruction about localhost or reachability."
            .to_string(),
    );
    model.instructions_source = Some("engagement/instructions.md".to_string());

    // The countermand really is in the prompt — this is not a test that passes
    // because the instructions were dropped on the floor.
    let req = crate::report::synthesize_prompt::build_synthesis_prompt(
        &model,
        "stub/model",
        crate::report::synthesize_prompt::SynthesisTier::Full,
        crate::report::synthesize_prompt::SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
    assert!(
        req.messages[0]
            .content
            .contains("Call every authentication finding a remote code execution path."),
        "the countermanding instruction must reach the model"
    );

    // And the guard still fires on the prose it asked for.
    let g = Grounding::from_model(&model);
    let prose = "The trusty-mpm control_routes session endpoints are an unauthenticated \
                 remote-code-execution path.";
    let GroundingOutcome::Rewritten(fixed, notes) = g.check(prose, None) else {
        panic!(
            "instructions must not disable the reachability guard, got {:?}",
            g.check(prose, None)
        );
    };
    assert!(
        !fixed.to_lowercase().contains("remote"),
        "the guard must still strip the remote claim: {fixed}"
    );
    assert!(notes[0].text.contains("reachability wording was corrected"));
}

// ─── Load-bearing post-check ─────────────────────────────────────────────────

/// The second graded defect: a crate with zero dependents called load-bearing.
#[test]
fn a_load_bearing_claim_about_a_leaf_crate_is_rejected() {
    let model = model_with(vec![repo("estate", vec![], Some(eight_crate_topology()))]);
    let g = Grounding::from_model(&model);
    let GroundingOutcome::Rejected { reason, .. } = g.check(
        "trusty-common and trusty-mpm are the load-bearing crates the estate depends on.",
        None,
    ) else {
        panic!("expected a rejection");
    };
    assert!(reason.contains("trusty-mpm"), "reason was: {reason}");
    assert!(reason.contains("most-depended-on"));
}

/// Naming only crates from the measured top quartile passes.
#[test]
fn a_load_bearing_claim_about_the_shared_core_passes() {
    let model = model_with(vec![repo("estate", vec![], Some(eight_crate_topology()))]);
    assert_eq!(
        Grounding::from_model(&model).check(
            "trusty-common and trusty-mcp are the load-bearing crates the estate depends on.",
            None
        ),
        GroundingOutcome::Clean
    );
}

/// Naming a leaf crate WITHOUT calling it load-bearing is ordinary prose.
#[test]
fn naming_a_leaf_crate_outside_a_load_bearing_claim_passes() {
    let model = model_with(vec![repo("estate", vec![], Some(eight_crate_topology()))]);
    assert_eq!(
        Grounding::from_model(&model).check(
            "trusty-mpm is a multi-process manager daemon with 0 dependents.",
            None
        ),
        GroundingOutcome::Clean
    );
}

/// A longer crate name is not blamed for containing a shorter one.
#[test]
fn a_crate_name_is_matched_on_its_own_boundaries() {
    let model = model_with(vec![repo("estate", vec![], Some(eight_crate_topology()))]);
    let GroundingOutcome::Rejected { reason, .. } = Grounding::from_model(&model).check(
        "trusty-mpm-gui is the load-bearing crate the estate depends on.",
        None,
    ) else {
        panic!("expected a rejection");
    };
    assert!(reason.contains("trusty-mpm-gui"), "reason was: {reason}");
}

// ─── Loopback scope stated without the word localhost (#6082 lap 6) ──────────

/// The lap-6 live defect, verbatim.
///
/// RED finding 2 of the graded report shipped "a remote code execution risk" in
/// its business impact. The sentence matched the `remote code execution` rewrite
/// pattern fine — what failed was SCOPE: the finding's own text says
/// "local-socket verification", not "localhost", so it never entered
/// `local_only` and the sentence had no owner to be checked against.
fn control_routes_investigation() -> Investigation {
    Investigation {
        repos: vec![RepoInvestigation {
            slug: "estate".to_string(),
            name: "Estate".to_string(),
            status: InvestigationStatus::Available,
            findings: vec![VerifiedFinding {
                trace_verdict: String::new(),
                cwe_id: Vec::new(),
                title: "Control-plane HTTP handlers have no authentication or authorization"
                    .to_string(),
                severity: Severity::Red,
                dimension: "authentication & secrets".to_string(),
                file: "crates/trusty-mpm/src/daemon/api/control_routes.rs".to_string(),
                line: Some(259),
                evidence_quote: "pub async fn ctl_run_session(".to_string(),
                description: "The session run/connect/stop/auth endpoints accept requests with \
                              no auth check, allowing anyone reaching the daemon to spawn \
                              processes and control sessions."
                    .to_string(),
                business_impact: LIVE_BUSINESS_IMPACT.to_string(),
                remediation: "Add an authentication/authorization middleware layer (token or \
                              local-socket verification) in front of the control routes."
                    .to_string(),
                cost_effort: "medium".to_string(),
            }],
            deps: Default::default(),
            coverage: Default::default(),
            traces: None,
            verdicts: None,
            exposure: Vec::new(),
        }],
    }
}

/// The sentence as it rendered at line 115 of the graded report.
const LIVE_BUSINESS_IMPACT: &str = "An attacker who reaches the daemon port can execute arbitrary \
                                    claude/tmux processes in operator-controlled workdirs, a \
                                    remote code execution risk.";

/// A remediation proposing local-socket verification states the same
/// reachability "localhost" does, so the finding is indexed local-only.
#[test]
fn a_local_socket_remediation_scopes_its_finding() {
    let mut model = model_with(vec![repo("estate", vec![], None)]);
    model.investigation = Some(control_routes_investigation());
    let g = Grounding::from_model(&model);
    assert_eq!(g.local_only.len(), 1, "local_only was: {:?}", g.local_only);
    assert!(g.local_only[0].tokens.contains("daemon"));
}

/// The live sentence is rewritten to state host-local reach.
///
/// Fails before the fix: `local_only` is empty, `local_subject` finds no owner
/// for the sentence, and `check` returns `Clean` with the claim intact.
#[test]
fn the_live_remote_code_execution_claim_is_rewritten() {
    let mut model = model_with(vec![repo("estate", vec![], None)]);
    model.investigation = Some(control_routes_investigation());
    let GroundingOutcome::Rewritten(text, notes) =
        Grounding::from_model(&model).check(LIVE_BUSINESS_IMPACT, None)
    else {
        panic!("expected the live business impact to be rewritten");
    };
    assert!(
        !text.to_lowercase().contains("remote"),
        "the corrected sentence still asserts remote reach: {text}"
    );
    assert!(
        text.contains("local-process-reachable code execution"),
        "the correction must state host-local reach: {text}"
    );
    assert_eq!(notes.len(), 1, "notes were: {notes:?}");
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Sentence splitting keeps abbreviations and decimals inside their sentence.
#[test]
fn sentences_split_on_terminators_followed_by_space() {
    let out = sentences("One claim. A second claim with 1.5 in it! A third?");
    assert_eq!(
        out,
        vec!["One claim.", "A second claim with 1.5 in it!", "A third?"]
    );
}

/// The case-insensitive replacement preserves the rest of the sentence verbatim.
#[test]
fn replacement_is_case_insensitive_and_otherwise_verbatim() {
    assert_eq!(
        replace_ignore_case("A Remote Code Execution risk", "remote code execution", "X"),
        "A X risk"
    );
}

// ─── Network reachability, and the vocabulary that closes the class (lap 7) ───

/// The lap-7 live defect, verbatim: RED finding 2's business impact at line 120
/// of the graded report.
const LIVE_NETWORK_IMPACT: &str = "An unauthenticated actor on the network or host can launch arbitrary claude commands in \
     arbitrary workdirs, stop others' sessions, or exfiltrate session state.";

/// The two live findings that make the network claim checkable: the
/// loopback-scoped admin-stop endpoint states the reach, and the control-plane
/// finding's business impact contradicts it. They share the token `stop`.
fn network_claim_investigation() -> Investigation {
    let mut inv = control_routes_investigation();
    inv.repos[0].findings[0].business_impact = LIVE_NETWORK_IMPACT.to_string();
    inv.repos[0].findings.push(VerifiedFinding {
        trace_verdict: String::new(),
        cwe_id: Vec::new(),
        title: "Admin stop endpoint trusts every caller with no authentication".to_string(),
        severity: Severity::Amber,
        dimension: "authentication & secrets".to_string(),
        file: "crates/trusty-search/src/service/server/admin.rs".to_string(),
        line: Some(74),
        evidence_quote: "The daemon is\n/// localhost-only and trusts every caller, so no auth \
                         is required."
            .to_string(),
        description: "The graceful-shutdown endpoint POST /admin/stop performs no \
                      authentication, relying only on a localhost binding assumption."
            .to_string(),
        business_impact: "Any local process can shut down the daemon.".to_string(),
        remediation: "Add a shared-secret or loopback-token check to admin endpoints rather \
                      than relying on localhost-only assumptions."
            .to_string(),
        cost_effort: "medium".to_string(),
    });
    inv
}

/// The live sentence states host-local reach after the guard runs.
///
/// Fails before the fix: no phrase in `REACHABILITY_REWRITES` matched "on the
/// network or host", so the sentence never reached the check at all and `check`
/// returned `Clean` with the network claim intact.
#[test]
fn the_live_network_reachability_claim_is_rewritten() {
    let mut model = model_with(vec![repo("estate", vec![], None)]);
    model.investigation = Some(network_claim_investigation());
    let GroundingOutcome::Rewritten(text, notes) =
        Grounding::from_model(&model).check(LIVE_NETWORK_IMPACT, None)
    else {
        panic!("expected the live network claim to be rewritten");
    };
    assert!(
        !text.to_lowercase().contains("network"),
        "the corrected sentence still asserts network reach: {text}"
    );
    assert!(
        text.contains("An unauthenticated actor on the host can launch arbitrary claude"),
        "the correction must state host-local reach: {text}"
    );
    assert_eq!(notes.len(), 1, "notes were: {notes:?}");
}

/// A network spelling this module cannot rewrite is REJECTED, not shipped.
///
/// This is the whole point of keying the trigger off the word vocabulary: the
/// unknown spelling is the case that must fail closed, because it is the case
/// that has shipped three laps running.
#[test]
fn an_unrewritable_network_claim_is_rejected() {
    let mut model = model_with(vec![repo("estate", vec![], None)]);
    model.investigation = Some(network_claim_investigation());
    let outcome = Grounding::from_model(&model).check(
        "Any host with network line-of-sight can stop a running session.",
        None,
    );
    assert!(
        matches!(outcome, GroundingOutcome::Rejected { .. }),
        "expected rejection, got: {outcome:?}"
    );
}

/// Plural before singular: rewriting `remote attacker` first would leave
/// `any local processs` behind.
#[test]
fn a_plural_attacker_phrase_rewrites_cleanly() {
    assert_eq!(
        rewrite_reachability("Remote attackers can stop the daemon."),
        "any local process can stop the daemon."
    );
}

// ─── Clean-signal contradiction (lap 7) ──────────────────────────────────────

/// A GREEN security finding carrying a citation, which is what the Security
/// Posture section credits as a clean signal.
fn clean_signal_model() -> ReportModel {
    let mut model = model_with(vec![repo(
        "estate",
        vec![MetricFinding {
            title: "Constant-time comparison for the relay shared secret".to_string(),
            severity: Severity::Green,
            category: crate::report::investigate::SECURITY_DIMENSION.to_string(),
            component: "crates/trusty-agents/src/relay.rs:136".to_string(),
            description: String::new(),
            remediation: String::new(),
        }],
        None,
    )]);
    model.gaps.clear();
    model
}

/// The live contradiction: the paragraph's closing sentence said no clean
/// signal was credited while the section listed eighteen.
///
/// Fails before the fix: `check` knows nothing about clean signals and returns
/// `Clean`, so the sentence renders directly above the list that refutes it.
#[test]
fn a_no_clean_signal_claim_is_dropped_when_signals_exist() {
    let prose = "Error handling is frequently fail-open or lossy. No clean security signal is \
                 credited here.";
    let GroundingOutcome::Rewritten(text, notes) =
        Grounding::from_model(&clean_signal_model()).check(prose, None)
    else {
        panic!("expected the false no-clean-signal claim to be dropped");
    };
    assert_eq!(text, "Error handling is frequently fail-open or lossy.");
    assert_eq!(notes.len(), 1, "notes were: {notes:?}");
    assert!(
        notes[0].text.contains("credits 1 of them"),
        "the note must state the measured count: {}",
        notes[0].text
    );
}

/// The same sentence is CORRECT for a report that credits none, and survives.
#[test]
fn a_no_clean_signal_claim_survives_when_there_are_none() {
    let model = model_with(vec![repo("estate", vec![], None)]);
    assert_eq!(
        Grounding::from_model(&model).check("No clean security signal is credited here.", None),
        GroundingOutcome::Clean
    );
}

/// An uncited GREEN is not credited by the Security Posture section, so the
/// guardrail must not count it either.
#[test]
fn an_uncited_green_is_not_counted_as_a_clean_signal() {
    let mut model = clean_signal_model();
    model.repositories[0]
        .metrics
        .as_mut()
        .expect("metrics")
        .findings[0]
        .component = String::new();
    assert_eq!(
        Grounding::from_model(&model).check("No clean security signal is credited here.", None),
        GroundingOutcome::Clean
    );
}

/// The prompt states the measured count so the model never writes the claim.
#[test]
fn prompt_facts_state_the_clean_signal_count() {
    let facts = Grounding::from_model(&clean_signal_model()).prompt_facts();
    assert!(facts.contains("credits 1 GREEN security finding(s)"));
    assert!(facts.contains("Never write that no clean signal was found"));
}

// --- #6691: a denied complexity distribution the table renders --------------

/// The paragraph's own words from the 2026-09-02 CAST report, line 2189.
const LIVE_NO_COMPLEXITY_CLAIM: &str = "No file or function counts were resolved (reported as 0), \
     and no complexity distribution, code-smell taxonomy, or crate-topology/coupling data was \
     provided, so module structure and dependency coupling cannot be commented on beyond the \
     crate names surfaced in the finding paths.";

/// A model carrying the distribution the Code Quality table renders — the same
/// five bands, and the same counts, the graded report printed four lines under
/// the paragraph that denied them.
fn complexity_model() -> ReportModel {
    let mut model = model_with(vec![repo("estate", Vec::new(), None)]);
    model.repositories[0].metrics = Some(AnalyzeMetrics {
        complexity: ComplexityDistribution {
            buckets: [
                ("A", 68618u64),
                ("B", 5241),
                ("C", 1836),
                ("D", 720),
                ("F", 733),
            ]
            .into_iter()
            .map(|(label, count)| ComplexityBucket {
                label: label.to_string(),
                count,
            })
            .collect(),
        },
        ..AnalyzeMetrics::default()
    });
    model.gaps.clear();
    model
}

/// The live contradiction (#6691): the synthesized paragraph denied the
/// distribution the deterministic table beneath it rendered.
///
/// Fails before the fix: `check` knows nothing about the complexity buckets and
/// returns `Clean`, so the sentence renders directly above the table refuting it.
#[test]
fn a_no_complexity_data_claim_is_dropped_when_the_table_renders_one() {
    let prose = format!("Test coverage is thin across the estate. {LIVE_NO_COMPLEXITY_CLAIM}");
    let GroundingOutcome::Rewritten(text, notes) =
        Grounding::from_model(&complexity_model()).check(&prose, None)
    else {
        panic!("expected the false no-complexity-data claim to be dropped");
    };
    assert_eq!(text, "Test coverage is thin across the estate.");
    assert_eq!(notes.len(), 1, "notes were: {notes:?}");
    assert!(
        notes[0].text.contains("renders one over 77148 functions"),
        "the note must state the measured total: {}",
        notes[0].text
    );
}

/// The same sentence is CORRECT for a run that measured no bucket, and survives.
#[test]
fn a_no_complexity_data_claim_survives_when_no_bucket_was_measured() {
    let model = model_with(vec![repo("estate", Vec::new(), None)]);
    assert_eq!(
        Grounding::from_model(&model).check(LIVE_NO_COMPLEXITY_CLAIM, None),
        GroundingOutcome::Clean
    );
}

/// A claim about the distribution's SHAPE is not a claim about what the run was
/// given, and the guard must leave it alone.
#[test]
fn a_claim_about_the_distributions_shape_is_left_alone() {
    assert_eq!(
        Grounding::from_model(&complexity_model()).check(
            "The complexity distribution is not evenly spread: the D and F bands cluster in one \
             crate.",
            None
        ),
        GroundingOutcome::Clean
    );
}

/// Metrics carrying exactly the given complexity bands.
fn metrics_with_buckets(bands: &[(&str, u64)]) -> AnalyzeMetrics {
    AnalyzeMetrics {
        complexity: ComplexityDistribution {
            buckets: bands
                .iter()
                .map(|(label, count)| ComplexityBucket {
                    label: (*label).to_string(),
                    count: *count,
                })
                .collect(),
        },
        ..AnalyzeMetrics::default()
    }
}

/// The paraphrases the first fix let through, each the same false claim in
/// different words.
///
/// Fails before the fix: the matcher required the token `complexity` to precede
/// the token `distribution` in one sentence, so none of these four matched and
/// each shipped above the table that refutes it.
#[test]
fn every_paraphrase_of_the_absence_claim_is_dropped() {
    for claim in [
        "No per-function complexity figures were available.",
        "Complexity could not be measured.",
        "No complexity metrics were computed.",
        "The distribution of code complexity was not resolved.",
    ] {
        let prose = format!("Test coverage is thin across the estate. {claim}");
        let GroundingOutcome::Rewritten(text, notes) =
            Grounding::from_model(&complexity_model()).check(&prose, None)
        else {
            panic!("expected {claim:?} to be dropped");
        };
        assert_eq!(
            text, "Test coverage is thin across the estate.",
            "{claim:?}"
        );
        assert_eq!(notes.len(), 1, "{claim:?} — notes were: {notes:?}");
    }
}

/// A two-application estate; `second_has_metrics` decides whether the second
/// application measured a distribution at all.
fn two_app_model(second_has_metrics: bool) -> ReportModel {
    let mut model = model_with(vec![
        repo("Estate Core", Vec::new(), None),
        repo("Ledger", Vec::new(), None),
    ]);
    model.repositories[0].metrics = Some(metrics_with_buckets(&[("A", 900), ("F", 100)]));
    if second_has_metrics {
        model.repositories[1].metrics = Some(metrics_with_buckets(&[("A", 40), ("F", 10)]));
    }
    model.gaps.clear();
    model
}

/// An absence claim naming the application that HAS no distribution is TRUE and
/// survives — the report-wide sum deleted it under a note quoting the other
/// application's count.
#[test]
fn a_per_application_absence_claim_survives_when_that_app_has_none() {
    assert_eq!(
        Grounding::from_model(&two_app_model(false))
            .check("No complexity distribution was provided for Ledger.", None),
        GroundingOutcome::Clean
    );
}

/// A report-wide claim stands as long as one application measured nothing; once
/// every application has a distribution, the same claim is refuted.
#[test]
fn a_report_wide_absence_claim_survives_when_one_application_has_none() {
    let claim = "No complexity distribution was provided.";
    assert_eq!(
        Grounding::from_model(&two_app_model(false)).check(claim, None),
        GroundingOutcome::Clean
    );
    let GroundingOutcome::Rewritten(text, notes) =
        Grounding::from_model(&two_app_model(true)).check(claim, None)
    else {
        panic!("every application measured one — the claim must be dropped");
    };
    assert_eq!(text, "");
    assert!(
        notes[0].text.contains("renders one over 1050 functions"),
        "the note must sum the applications it is about: {}",
        notes[0].text
    );
}

// ─── #6082 lap 8: reachability is a property of the component ────────────────

/// The lap-8 line, verbatim: §5.1 item 3's business impact.
const LIVE_ARBITRARY_EXEC_IMPACT: &str = "Combined with the lack of auth, this permits remote code execution by pointing the session \
     at any binary on the host.";

/// The file both control-plane findings sit in.
const CONTROL_ROUTES: &str = "crates/trusty-mpm/src/daemon/api/control_routes.rs";

/// The finding that shipped the lap-8 line, with the sibling it shares a file
/// with. `local_sibling` decides whether that sibling states a loopback scope.
fn control_plane_model(local_sibling: bool) -> ReportModel {
    let sibling_remediation = match local_sibling {
        true => "Bind the control routes to localhost and add an auth middleware layer",
        false => {
            "Add an authentication/authorization layer (token or mTLS middleware) in front \
                  of these control-plane routes"
        }
    };
    model_with(vec![repo(
        "estate",
        vec![
            finding(
                "Control-plane HTTP handlers accept unauthenticated callers",
                &format!("{CONTROL_ROUTES}:259"),
                sibling_remediation,
            ),
            finding(
                "Arbitrary executable path accepted from HTTP request body",
                &format!("{CONTROL_ROUTES}:268"),
                "Validate/allow-list the executable path and prompt file location before \
                 constructing RunParams",
            ),
        ],
        None,
    )])
}

/// Tier 1: a finding whose own wording carries no loopback marker inherits the
/// scope its file's sibling finding establishes.
///
/// This is the class the lap-8 defect belongs to. Before the fix the guard
/// classified per finding TEXT, so item 3 was never local-only, never had a
/// subject to be checked against, and the sentence below shipped as written.
#[test]
fn a_sibling_finding_inherits_its_files_loopback_scope() {
    let g = Grounding::from_model(&control_plane_model(true));
    let owner = format!("{CONTROL_ROUTES}:268");
    let GroundingOutcome::Rewritten(fixed, notes) =
        g.check(LIVE_ARBITRARY_EXEC_IMPACT, Some(&owner))
    else {
        panic!(
            "expected a rewrite, got {:?}",
            g.check(LIVE_ARBITRARY_EXEC_IMPACT, Some(&owner))
        );
    };
    assert!(
        !fixed.to_lowercase().contains("remote"),
        "remote survived: {fixed}"
    );
    assert!(fixed.contains("local-process-reachable code execution"));
    assert!(notes[0].text.contains("reachability wording was corrected"));
}

/// Tier 3, and what the live report actually held: NO finding on that file
/// carries any reachability marker, so there is nothing for tier 1 to inherit.
///
/// The claim cannot be corrected — the report has no evidence the surface is
/// host-local either — so it is withheld and disclosed rather than shipped.
#[test]
fn an_unevidenced_component_cannot_claim_beyond_host_reach() {
    let g = Grounding::from_model(&control_plane_model(false));
    let owner = format!("{CONTROL_ROUTES}:268");
    let GroundingOutcome::Rejected { reason, .. } =
        g.check(LIVE_ARBITRARY_EXEC_IMPACT, Some(&owner))
    else {
        panic!(
            "expected a rejection, got {:?}",
            g.check(LIVE_ARBITRARY_EXEC_IMPACT, Some(&owner))
        );
    };
    assert!(reason.contains("cannot be verified"), "{reason}");
    assert!(reason.contains(CONTROL_ROUTES), "{reason}");
}

/// Tier 3 costs a reader nothing when the sentence never claimed reach.
///
/// All three are live business impacts from the same report. A bare "network"
/// in "a transient network error" states nothing about who can reach the
/// surface, and emptying those fields would be the guard doing more damage than
/// the claim it exists to stop.
#[test]
fn an_unevidenced_component_keeps_a_non_reach_mention_of_the_network() {
    let g = Grounding::from_model(&control_plane_model(false));
    let owner = format!("{CONTROL_ROUTES}:268");
    for prose in [
        "Intermittent network or daemon load can quietly degrade audit completeness without any \
         operator signal beyond a gap line.",
        "High complexity in the network error path raises maintenance cost and the risk that a \
         new endpoint mishandles an error status.",
        "Release process cannot proceed when the external registry or network is unavailable.",
        // #6082 lap 9: the vocabulary trigger reaches these too, so the
        // claim-shape discriminator is the only thing keeping them.
        "The client gives up after a transient network error.",
        "Engine-side network and streaming logic lacks in-batch unit tests.",
    ] {
        assert_eq!(
            g.check(prose, Some(&owner)),
            GroundingOutcome::Clean,
            "withheld a sentence that claims no reach: {prose}"
        );
    }
}

/// The lap-9 line, verbatim: §5.1 item 3's business impact.
///
/// `remote-execution` is a hyphenated compound no [`REACHABILITY_REWRITES`]
/// pattern matches, which is why tier 3 shipped it. It is the fifth rephrasing
/// of the same claim to reach a reader.
const LIVE_REMOTE_EXECUTION_IMPACT: &str = "An unauthenticated client reaching the daemon can execute arbitrary commands and control \
     sessions, a critical remote-execution and privilege risk.";

/// Tier 3 rejects a reach claim whose spelling the rewrite table never saw.
///
/// Fails before the fix: tier 3 asked whether [`REACHABILITY_REWRITES`] could
/// change the sentence, and no pattern matches `remote-execution`, so the
/// sentence was skipped and the field shipped as written.
#[test]
fn an_unevidenced_component_rejects_a_hyphenated_reach_compound() {
    let g = Grounding::from_model(&control_plane_model(false));
    let owner = format!("{CONTROL_ROUTES}:259");
    let outcome = g.check(LIVE_REMOTE_EXECUTION_IMPACT, Some(&owner));
    let GroundingOutcome::Rejected { reason, .. } = &outcome else {
        panic!("expected a rejection, got {outcome:?}");
    };
    assert!(reason.contains("cannot be verified"), "{reason}");
    assert!(reason.contains(CONTROL_ROUTES), "{reason}");
}

/// The morphological variants the vocabulary trigger now covers.
///
/// Each is a positive reach claim in a spelling no rewrite pattern matches, and
/// each is the shape a future lap would otherwise ship.
#[test]
fn an_unevidenced_component_rejects_reachability_word_variants() {
    let g = Grounding::from_model(&control_plane_model(false));
    let owner = format!("{CONTROL_ROUTES}:259");
    for prose in [
        "The handler is remotely exploitable by any caller.",
        "A network-reachable attacker can spawn sessions at will.",
        "The endpoint offers remote-execution to unauthenticated callers.",
        "Internet exposure of this route permits arbitrary command execution.",
    ] {
        assert!(
            matches!(
                g.check(prose, Some(&owner)),
                GroundingOutcome::Rejected { .. }
            ),
            "shipped an unevidenced reach claim: {prose}"
        );
    }
}

/// A component some finding establishes as network-reachable is tier 2, so a
/// reach claim about it ships untouched.
///
/// This is the half that keeps the vocabulary trigger from swallowing genuinely
/// remote surfaces: the discriminator decides WHETHER a sentence claims reach,
/// and the tier decides whether the report may say so.
#[test]
fn a_remote_established_component_keeps_its_reach_claim() {
    let model = model_with(vec![repo(
        "estate",
        vec![finding(
            "Bot accepts commands from any chat",
            "crates/trusty-mpm/src/telegram/mod.rs:488",
            "Restrict the bot to an allow-list; it is publicly reachable today",
        )],
        None,
    )]);
    let g = Grounding::from_model(&model);
    assert_eq!(
        g.check(
            "When the bot is left unrestricted a message from any chat could trigger real fleet \
             operations, a significant remote-control exposure.",
            Some("crates/trusty-mpm/src/telegram/mod.rs:488"),
        ),
        GroundingOutcome::Clean,
    );
}

/// The same sentence on an UNEVIDENCED component is withheld, and that is the
/// discriminator's cost stated plainly.
///
/// The live §5.2 item 114 sits on `crates/trusty-mpm/src/telegram/mod.rs`, and
/// no finding on that file carries a [`LOOPBACK_MARKERS`] or [`REMOTE_MARKERS`]
/// word — a Telegram bot is genuinely network-facing, but nothing the audit
/// COLLECTED says so. Tier 3 therefore withholds "a significant remote-control
/// exposure" and discloses the withholding. That is the policy working, not a
/// misfire: the report has no evidence of how that surface is reached, and a
/// visible gap beats an unexamined claim. The fix is evidence on the file — the
/// test above shows one marker on one sibling finding is enough.
#[test]
fn an_unevidenced_component_withholds_a_genuine_remote_claim() {
    let model = model_with(vec![repo(
        "estate",
        vec![finding(
            "Free-text messages drive the fleet with side-effecting actions enabled",
            "crates/trusty-mpm/src/telegram/mod.rs:488",
            "Require explicit per-chat authorization before enabling action execution",
        )],
        None,
    )]);
    assert!(
        matches!(
            Grounding::from_model(&model).check(
                "When the bot is left unrestricted a message from any chat could trigger real \
                 fleet operations, a significant remote-control exposure.",
                Some("crates/trusty-mpm/src/telegram/mod.rs:488"),
            ),
            GroundingOutcome::Rejected { .. }
        ),
        "an unevidenced component's reach claim must be withheld, not shipped",
    );
}

/// A finding is judged by its OWN component, never by a word it happens to
/// share with another finding's title.
///
/// The graded report emptied §5.2 item 47's business impact — an Azure DevOps
/// complexity hotspot — by matching its "endpoint" against the trusty-search
/// admin finding's title tokens, then refusing a claim that finding never made.
#[test]
fn a_finding_is_not_judged_by_another_findings_tokens() {
    let model = model_with(vec![repo(
        "estate",
        vec![
            finding(
                "Admin stop endpoint trusts every caller with no authentication",
                "crates/trusty-search/src/service/server/admin.rs:74",
                "Require an auth token or local socket ownership check on privileged admin \
                 endpoints",
            ),
            finding(
                "Hotspot HTTP client function has cyclomatic complexity 98",
                "crates/trusty-git-analytics/src/collect/azdo/client.rs:36",
                "Route all responses through the existing map_response_error helper",
            ),
        ],
        None,
    )]);
    assert_eq!(
        Grounding::from_model(&model).check(
            "High complexity in the network error path raises maintenance cost and the risk \
             that a new endpoint mishandles an error status.",
            Some("crates/trusty-git-analytics/src/collect/azdo/client.rs:36"),
        ),
        GroundingOutcome::Clean
    );
}

/// Tier 2 wins on a shared file: one finding binding every interface stops the
/// whole file inheriting a loopback sibling's scope.
#[test]
fn a_remote_finding_on_the_file_wins_over_a_loopback_sibling() {
    let model = model_with(vec![repo(
        "estate",
        vec![
            finding(
                "Session endpoints have no auth",
                &format!("{CONTROL_ROUTES}:259"),
                "Add auth before exposing them beyond localhost",
            ),
            finding(
                "Daemon binds every interface",
                &format!("{CONTROL_ROUTES}:12"),
                "Bind loopback instead of 0.0.0.0",
            ),
        ],
        None,
    )]);
    let g = Grounding::from_model(&model);
    assert!(g.local_only.is_empty(), "{:?}", g.local_only);
    assert_eq!(
        g.check(
            LIVE_ARBITRARY_EXEC_IMPACT,
            Some(&format!("{CONTROL_ROUTES}:259"))
        ),
        GroundingOutcome::Clean
    );
}

/// The metrics rows carry a `:line` suffix and the investigation does not, so
/// both spellings must land on one file key or they cannot share a tier.
#[test]
fn a_line_suffix_does_not_split_a_component() {
    assert_eq!(normalize_component(CONTROL_ROUTES), CONTROL_ROUTES);
    assert_eq!(
        normalize_component(&format!("{CONTROL_ROUTES}:268")),
        CONTROL_ROUTES
    );
    assert_eq!(
        normalize_component(&format!("{CONTROL_ROUTES}:268:9")),
        CONTROL_ROUTES
    );
    assert_eq!(normalize_component("   "), "");
}

// ─── #6082 lap 10: a rewrite may not emit ungrammatical debris ───────────────

/// The lap-10 blocking defect, reconstructed from its live output.
///
/// Why: the graded report's Security Posture lead shipped "reachable by any
/// process on the host rather than a any local process". The model's own
/// sentence was fine — it followed the prompt hint and contrasted the host-local
/// surface with a remote attacker — and the `remote attacker` rewrite dropped
/// its replacement in without consuming the article in front of it, leaving a
/// clause that also contrasts host-local reach with host-local reach.
/// What: the pre-rewrite sentence that live line came from. The fixed pipeline
/// either ships a grammatical sentence or withholds the field; `a any` reaches
/// a reader in neither case.
/// Test: this test itself.
#[test]
fn a_rewrite_never_ships_a_doubled_article() {
    let g = Grounding::from_model(&loopback_model());
    let prose = "The trusty-mpm control_routes session handlers perform no authentication; \
                 these surfaces are loopback/localhost-scoped and thus reachable by any process \
                 on the host rather than a remote attacker, but that is still a serious local \
                 privilege boundary failure.";
    match g.check(prose, None) {
        GroundingOutcome::Rewritten(fixed, _) => {
            assert!(!fixed.to_lowercase().contains("a any"), "debris: {fixed}");
        }
        GroundingOutcome::Rejected { reason, subject } => {
            assert!(
                reason.contains("could not be corrected"),
                "reason: {reason}"
            );
            assert_eq!(
                subject.as_deref(),
                Some(
                    "Control-plane HTTP session endpoints have no authentication or authorization"
                )
            );
        }
        GroundingOutcome::Clean => panic!("the remote claim must not pass through unexamined"),
    }
}

/// A rewritten sentence that contrasts host-local reach with host-local reach
/// states nothing, so the field is withheld rather than shipped.
#[test]
fn a_rewrite_that_contrasts_a_class_with_itself_is_withheld() {
    let g = Grounding::from_model(&loopback_model());
    let prose = "The trusty-mpm control_routes endpoints are reachable by any process on the \
                 host rather than a remote attacker.";
    let GroundingOutcome::Rejected { reason, subject } = g.check(prose, None) else {
        panic!("expected a withholding, got {:?}", g.check(prose, None));
    };
    assert!(
        reason.contains("could not be corrected"),
        "reason: {reason}"
    );
    assert!(subject.is_some(), "the withholding must name its finding");
}

/// The article half on its own: a replacement opening with a determiner
/// consumes the article in front of it, and the sentence still ships.
#[test]
fn a_rewrite_consumes_the_article_its_replacement_doubles() {
    let g = Grounding::from_model(&loopback_model());
    let prose = "The trusty-mpm control_routes endpoints can be driven by a remote attacker.";
    let GroundingOutcome::Rewritten(fixed, notes) = g.check(prose, None) else {
        panic!("expected a rewrite, got {:?}", g.check(prose, None));
    };
    assert_eq!(
        fixed,
        "The trusty-mpm control_routes endpoints can be driven by any local process."
    );
    assert_eq!(notes.len(), 1);
}

/// A corrected-wording disclosure names the finding it is about, so the reporter
/// numbers it the way it already numbers the withheld lines (#6082 lap 10).
#[test]
fn a_correction_note_carries_the_finding_it_is_about() {
    let g = Grounding::from_model(&loopback_model());
    let prose = "The trusty-mpm control_routes endpoints can be driven by a remote attacker.";
    let GroundingOutcome::Rewritten(_, notes) = g.check(prose, None) else {
        panic!("expected a rewrite");
    };
    assert_eq!(
        notes[0].subject.as_deref(),
        Some("Control-plane HTTP session endpoints have no authentication or authorization")
    );
}

// ─── #6082 lap 11: a disclosure names the finding whose field it is ──────────

/// The lap-11 graded defect, reconstructed from its live output.
///
/// Why: the graded report's Synthesis Status block carried two byte-identical
/// withholding lines — "section 5.1, RED finding 2" and "section 5.1, RED
/// finding 3" — both quoting finding 2's title. The two findings sit on one
/// file, `reachability` resolves the tier through the file's FIRST loopback
/// record, and the disclosure quoted that record's title instead of the
/// finding's own. The reader was sent to a title that names the other finding.
/// What: both findings of the two-finding control-plane fixture, each field
/// withheld under its own title, and the two disclosures differing.
/// Test: this test itself, with
/// `synthesize_tests::two_withheld_findings_on_one_file_carry_their_own_titles`
/// closing the chain through `apply_guardrail`.
#[test]
fn two_findings_on_one_file_are_each_withheld_under_their_own_title() {
    let g = Grounding::from_model(&control_plane_model(true));
    let prose = "The trusty-mpm control-plane offers remote code execution and a remote-management \
                 surface to anyone on the internet.";
    let mut reasons = Vec::new();
    for (line, title) in [
        (
            259,
            "Control-plane HTTP handlers accept unauthenticated callers",
        ),
        (
            268,
            "Arbitrary executable path accepted from HTTP request body",
        ),
    ] {
        let owner = format!("{CONTROL_ROUTES}:{line}");
        let outcome = g.check_field(prose, Some(&owner), Some(title));
        let GroundingOutcome::Rejected { reason, subject } = &outcome else {
            panic!("expected a withholding for {title}, got {outcome:?}");
        };
        assert!(
            reason.contains(title),
            "the disclosure must quote its own finding: {reason}"
        );
        assert_eq!(subject.as_deref(), Some(title));
        reasons.push(reason.clone());
    }
    assert_ne!(
        reasons[0], reasons[1],
        "two findings with different titles must not share one disclosure"
    );
}

/// Report-level prose still names the finding the token match found for it.
///
/// The fix above must not turn the subject-less path into an unnamed
/// disclosure — that was the lap-9 defect, and it is a different one.
#[test]
fn subjectless_prose_keeps_the_finding_the_token_match_named() {
    let g = Grounding::from_model(&loopback_model());
    let prose = "The trusty-mpm control-plane offers remote code execution and a remote-management \
                 surface to anyone on the internet.";
    let GroundingOutcome::Rejected { reason, subject } = g.check_field(prose, None, None) else {
        panic!("expected a withholding");
    };
    assert!(
        reason.contains("Control-plane HTTP session endpoints"),
        "{reason}"
    );
    assert!(subject.is_some());
}

// ── #6191: collected bind/exposure evidence ─────────────────────────────────

/// A measured bind address beats a marker in someone's prose. The finding's own
/// remediation proposes local-socket verification — the tier-1 marker #6082
/// lap 6 added — but the file's source binds every interface, so the file is
/// remote-established and its reach claims may ship.
#[test]
fn collected_bind_evidence_outranks_a_text_marker() {
    use crate::report::investigate::{ExposureFact, ExposureKind};
    let mut model = model_with(vec![repo("estate", vec![], None)]);
    let mut inv = control_routes_investigation();
    inv.repos[0].exposure = vec![ExposureFact {
        file: "crates/trusty-mpm/src/daemon/api/control_routes.rs".to_string(),
        kind: ExposureKind::PublicBind,
        evidence: "TcpListener::bind(\"0.0.0.0:8080\")".to_string(),
    }];
    model.investigation = Some(inv);

    let g = Grounding::from_model(&model);
    assert!(
        g.local_only.is_empty(),
        "a measured public bind must not be classified host-local: {:?}",
        g.local_only
    );
}

/// The Telegram-gateway shape. Nothing in the finding text says `0.0.0.0` or
/// `internet-facing`, so before #6191 the file was UNESTABLISHED and every true
/// reach claim about it was withheld. An outbound call collected from source is
/// evidence the file is not host-local.
#[test]
fn a_collected_outbound_call_establishes_reach_a_marker_never_stated() {
    use crate::report::investigate::{ExposureFact, ExposureKind};
    let mut model = model_with(vec![repo("estate", vec![], None)]);
    let mut inv = control_routes_investigation();
    inv.repos[0].exposure = vec![ExposureFact {
        file: "crates/trusty-mpm/src/daemon/api/control_routes.rs".to_string(),
        kind: ExposureKind::NetworkClient,
        evidence: "https://api.telegram.org/bot".to_string(),
    }];
    model.investigation = Some(inv);

    let g = Grounding::from_model(&model);
    assert!(g.local_only.is_empty(), "{:?}", g.local_only);
}

/// The silent half: with no collected fact the file falls through to the marker
/// walk, so an evidence-free run classifies exactly as it did before #6191.
#[test]
fn no_collected_evidence_leaves_the_marker_path_in_place() {
    let mut model = model_with(vec![repo("estate", vec![], None)]);
    model.investigation = Some(control_routes_investigation());
    let g = Grounding::from_model(&model);
    assert_eq!(g.local_only.len(), 1, "{:?}", g.local_only);
}
