//! Unit tests for search-driven evidence discovery (#6082).
//!
//! Every test here drives [`StubSearch`], never a daemon: the properties that
//! matter — which dimension a file is attributed to, and how many dimensions
//! the ranking reaches — are only assertable when the hits are fixed.

use std::collections::HashMap;

use super::super::priority::FunctionHotspot;
use super::*;

/// A search client answering from a table of `query substring → hits`.
#[derive(Debug, Default)]
struct StubSearch {
    answers: HashMap<String, Vec<Hit>>,
    fail: Option<String>,
}

impl StubSearch {
    /// Every query whose text contains `needle` answers with `paths`.
    fn answering(mut self, needle: &str, paths: &[(&str, f32)]) -> Self {
        self.answers.insert(
            needle.to_owned(),
            paths
                .iter()
                .map(|(path, score)| Hit {
                    path: (*path).to_owned(),
                    score: *score,
                    start_line: 1,
                    match_reason: "hybrid".to_owned(),
                })
                .collect(),
        );
        self
    }

    /// Every query fails with this reason.
    fn failing(reason: &str) -> Self {
        Self {
            answers: HashMap::new(),
            fail: Some(reason.to_owned()),
        }
    }
}

impl SearchClient for StubSearch {
    async fn hits(&self, query: &str, top_k: usize) -> Result<Vec<Hit>, String> {
        if let Some(reason) = &self.fail {
            return Err(reason.clone());
        }
        Ok(self
            .answers
            .iter()
            .find(|(needle, _)| query.contains(needle.as_str()))
            .map(|(_, hits)| hits.iter().take(top_k).cloned().collect())
            .unwrap_or_default())
    }
}

/// Every pre-#6082 test asked the floor caps; keeping that spelling short is
/// what makes the two scaling tests below read as the deviation they are.
fn floor() -> Caps {
    Caps::default()
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all() // the HTTP client's timeout needs the timer driver
        .build()
        .expect("runtime")
        .block_on(future)
}

/// The dimension names ARE the contract with trusty-review — a typo here shows
/// up as a coverage section that attributes nothing.
#[test]
fn every_dimension_matches_the_reviews_spelling() {
    // Mirrors `trusty_review::report::investigate::select::DIMENSIONS`.
    let expected = [
        "authentication & secrets",
        "dependencies",
        "state management",
        "error handling",
        "scalability",
        "test coverage",
    ];
    assert_eq!(
        DD_DIMENSIONS
            .iter()
            .map(|d| d.dimension)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(
        DD_DIMENSIONS.iter().all(|d| !d.queries.is_empty()),
        "a dimension with no query can never be covered"
    );
}

/// The point of the leg: a file arrives knowing which dimension it is evidence
/// for, and why.
#[test]
fn discovery_attributes_each_file_to_its_dimension() {
    let client = StubSearch::default()
        .answering("credential handling", &[("src/auth.rs", 0.9)])
        .answering("error swallowed", &[("src/err.rs", 0.7)]);
    let discovery = block_on(discover(&client, None, floor()));

    let auth = discovery
        .dimensions
        .iter()
        .find(|d| d.dimension == "authentication & secrets")
        .expect("the auth dimension found evidence");
    assert_eq!(auth.files[0].path, "src/auth.rs");
    assert!(
        auth.files[0].reason.contains("credential handling"),
        "the reason must name the query: {}",
        auth.files[0].reason
    );
    assert!(
        discovery
            .dimensions
            .iter()
            .any(|d| d.dimension == "error handling"),
        "error handling found evidence too"
    );
    assert!(discovery.failures.is_empty());
}

/// One query failing must not cost the dimensions that answered.
#[test]
fn a_failing_query_costs_only_its_own_evidence() {
    let client = StubSearch::failing("trusty-search did not answer (connection refused)");
    let discovery = block_on(discover(&client, None, floor()));
    assert!(discovery.dimensions.is_empty());
    assert!(
        discovery.failures.len() >= DD_DIMENSIONS.len(),
        "every query's failure is recorded: {:?}",
        discovery.failures
    );
}

/// The best-scoring query wins a file, and a file is never listed twice within
/// one dimension.
#[test]
fn a_file_matched_twice_keeps_its_best_score() {
    let client = StubSearch::default()
        .answering("credential handling", &[("src/auth.rs", 0.4)])
        .answering("authentication and authorization", &[("src/auth.rs", 0.95)]);
    let discovery = block_on(discover(&client, None, floor()));
    let auth = &discovery.dimensions[0];
    assert_eq!(auth.files.len(), 1);
    assert!(
        auth.files[0].reason.contains("authentication and"),
        "{}",
        auth.files[0].reason
    );
}

/// A brief's topics become queries under their own dimension.
#[test]
fn instruction_bullets_become_queries() {
    let brief = "# Focus areas\n\n- Payment reconciliation correctness\n* Tenant data isolation\n\
                 \n1. Vendor lock-in in the storage layer\nprose that is not a topic\n";
    let queries = instruction_queries(Some(brief));
    assert_eq!(
        queries,
        vec![
            "Focus areas".to_string(),
            "Payment reconciliation correctness".to_string(),
            "Tenant data isolation".to_string(),
            "Vendor lock-in in the storage layer".to_string(),
        ]
    );
    assert!(instruction_queries(None).is_empty());
}

/// A brief's hits land under the analyst-focus dimension, not a DD one.
#[test]
fn the_analyst_brief_gets_its_own_dimension() {
    let client = StubSearch::default().answering("Payment reconciliation", &[("src/pay.rs", 0.8)]);
    let discovery = block_on(discover(
        &client,
        Some("- Payment reconciliation correctness\n"),
        floor(),
    ));
    let focus = discovery
        .dimensions
        .iter()
        .find(|d| d.dimension == ANALYST_FOCUS)
        .expect("the brief's topic produced evidence");
    assert_eq!(focus.files[0].path, "src/pay.rs");
}

fn dimension(name: &str, paths: &[&str]) -> DimensionEvidence {
    DimensionEvidence {
        dimension: name.to_owned(),
        files: paths
            .iter()
            .map(|path| FileEvidence {
                path: (*path).to_owned(),
                reason: format!("trusty-search hit for \"{name}\""),
            })
            .collect(),
    }
}

/// Ranked files with no function-level measurement — the shape a daemon that
/// locates nothing produces, and what most of these cases care about.
fn hot(paths: &[&str]) -> Vec<RankedFile> {
    paths
        .iter()
        .map(|path| RankedFile {
            path: (*path).to_owned(),
            hotspot: None,
        })
        .collect()
}

/// The regression this issue exists for: ranking complexity first spends the
/// whole budget on one dimension. Round-robin reaches every dimension inside
/// the same number of files.
#[test]
fn blending_spreads_the_budget_across_dimensions() {
    let owned: Vec<String> = (0..10).map(|i| format!("src/hot{i}.rs")).collect();
    let hotspots = hot(&owned.iter().map(String::as_str).collect::<Vec<_>>());
    let dimensions = [
        dimension("authentication & secrets", &["src/auth.rs", "src/token.rs"]),
        dimension("error handling", &["src/err.rs"]),
        dimension("state management", &["src/store.rs"]),
        dimension("scalability", &["src/pool.rs"]),
        dimension("test coverage", &["tests/api.rs"]),
    ];

    let blended = blend(&hotspots, &dimensions, 60);
    let first_six: Vec<&str> = blended.iter().take(6).map(|p| p.path.as_str()).collect();
    let covered: Vec<&str> = blended
        .iter()
        .take(6)
        .filter_map(|p| p.dimension.as_deref())
        .collect();

    assert_eq!(first_six[0], "src/hot0.rs", "complexity still leads");
    assert_eq!(
        covered.len(),
        5,
        "the first six paths must reach all five dimensions: {first_six:?}"
    );
    // Complexity-only ranking, the pre-#6082 behaviour, reaches none of them.
    let complexity_only = blend(&hotspots, &[], 60);
    assert!(
        complexity_only
            .iter()
            .all(|p| p.dimension.is_none() && p.reason.is_some()),
        "a hotspot is attributed to no dimension, and still says why it is ranked"
    );
}

/// The regression the critic caught on PR #6124: a file that is BOTH the top
/// hotspot and a dimension's evidence used to reach the manifest with
/// `dimension: None`, because the hotspot is pushed first and a path is ranked
/// once. The report then counted that dimension as not investigated even though
/// it read a file addressing it.
///
/// The path here carries no name a path heuristic would classify, which is what
/// makes the loss observable — an `auth`-named file would have been rescued by
/// trusty-review's own heuristics and hidden the defect.
#[test]
fn a_hotspot_that_is_also_dimension_evidence_keeps_its_dimension() {
    let hotspots = hot(&["src/session_manager.rs"]);
    let dimensions = [dimension(
        "authentication & secrets",
        &["src/session_manager.rs"],
    )];
    let blended = blend(&hotspots, &dimensions, 60);

    assert_eq!(blended.len(), 1, "one entry per path: {blended:?}");
    assert_eq!(
        blended[0].dimension.as_deref(),
        Some("authentication & secrets"),
        "the dimension must survive the hotspot entry: {blended:?}"
    );
    let reason = blended[0].reason.as_deref().expect("a reason");
    assert!(reason.contains("complexity hotspot"), "{reason}");
    assert!(reason.contains("trusty-search hit for"), "{reason}");
}

/// A hotspot no query found stays unattributed — the merge must not invent a
/// dimension for a file the index never named.
#[test]
fn a_hotspot_no_query_found_carries_no_dimension() {
    let hotspots = hot(&["src/parser.rs"]);
    let dimensions = [dimension("error handling", &["src/err.rs"])];
    let blended = blend(&hotspots, &dimensions, 60);
    let parser = blended
        .iter()
        .find(|p| p.path == "src/parser.rs")
        .expect("the hotspot is ranked");
    assert!(parser.dimension.is_none(), "{blended:?}");
    assert_eq!(
        parser.reason.as_deref(),
        Some("trusty-analyze complexity hotspot (rank 1)")
    );
}

/// The cap bounds the manifest, and never truncates to nothing.
#[test]
fn the_ranking_is_capped() {
    let owned: Vec<String> = (0..80).map(|i| format!("src/hot{i}.rs")).collect();
    let hotspots = hot(&owned.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(
        blend(&hotspots, &[], MIN_PRIORITY_PATHS).len(),
        MIN_PRIORITY_PATHS
    );
    assert!(blend(&[], &[], MIN_PRIORITY_PATHS).is_empty());
}

/// #6145: the measured function survives the blend — both as the structured key
/// trusty-review reads and in the reason line a human reads. Pre-fix `blend`
/// took paths only, so there was nothing to survive.
#[test]
fn a_blended_hotspot_carries_its_measured_function() {
    let measured = FunctionHotspot {
        function: Some("settle_invoice".to_owned()),
        start_line: 40,
        end_line: 190,
        cyclomatic: 31,
    };
    let hotspots = vec![RankedFile {
        path: "src/pay.rs".to_owned(),
        hotspot: Some(measured.clone()),
    }];

    // Unattributed, and attributed to a dimension: both keep the measurement.
    for dimensions in [
        Vec::new(),
        vec![dimension("error handling", &["src/pay.rs"])],
    ] {
        let blended = blend(&hotspots, &dimensions, 60);
        assert_eq!(blended[0].hotspot.as_ref(), Some(&measured), "{blended:?}");
        let reason = blended[0].reason.as_deref().expect("a reason");
        assert!(
            reason.contains("fn settle_invoice, lines 40-190, cyclomatic 31"),
            "{reason}"
        );
    }
}

/// #6082: trusty-search expands the top hits along the symbol graph by default,
/// so relationship evidence was already entering the sample unlabelled — the
/// report could not say a file was read because it CALLS the credential handler
/// rather than because it mentions one. The daemon's own lane label now reaches
/// the reason string.
#[test]
fn a_graph_expanded_hit_says_the_graph_found_it() {
    let body = r#"{"results":[
        {"file":"","path":"src/session.rs","start_line":9,"score":0.7,"match_reason":"hybrid+kg"},
        {"file":"","path":"src/auth.rs","start_line":3,"score":0.9,"match_reason":"hybrid"}
    ]}"#;
    let envelope: SearchEnvelope = serde_json::from_str(body).expect("parses");
    let hits = envelope.into_hits("/w/repos/acme-api");
    assert!(hits[0].via_graph(), "hybrid+kg is a graph-expanded hit");
    assert!(!hits[1].via_graph(), "a plain hybrid hit is not");

    let graph = reason("credential handling", &hits[0]);
    assert!(
        graph.contains("via knowledge-graph expansion"),
        "the reason must name the graph: {graph}"
    );
    assert!(
        !reason("credential handling", &hits[1]).contains("knowledge-graph"),
        "a text match must not claim the graph found it"
    );
}

/// The graded self-audit's auth dimension, reproduced: a call-graph test file
/// at a noise score outranked the production middleware, which went unread. The
/// floor drops the noise and the demotion tier keeps the test file behind the
/// production file even when both are genuine hits.
#[test]
fn noise_and_test_files_never_lead_a_production_dimension() {
    let client = StubSearch::default().answering(
        "password hashing",
        &[
            ("crates/trusty-search/src/service/call_chain/tests.rs", 0.02),
            ("crates/x/src/auth_tests.rs", 0.88),
            ("crates/trusty-agents/src/api/server/auth.rs", 0.61),
        ],
    );
    let auth = &block_on(discover(&client, None, floor())).dimensions[0];
    assert_eq!(
        auth.files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "crates/trusty-agents/src/api/server/auth.rs",
            "crates/x/src/auth_tests.rs",
        ],
        "the 0.02 hit is dropped and the 0.88 test file sorts behind the 0.61 production file"
    );
}

/// Under "test coverage" a test file is the evidence, so the demotion must not
/// reach it — the dimension would otherwise rank its own subject last.
#[test]
fn the_test_dimension_still_leads_with_test_files() {
    let client = StubSearch::default().answering(
        "test asserting the core behaviour",
        &[
            ("crates/x/src/lib.rs", 0.55),
            ("crates/x/tests/api.rs", 0.80),
        ],
    );
    let discovery = block_on(discover(&client, None, floor()));
    let tests = discovery
        .dimensions
        .iter()
        .find(|d| d.dimension == "test coverage")
        .expect("the dimension found evidence");
    assert_eq!(
        tests.files[0].path, "crates/x/tests/api.rs",
        "score order stands under the test dimension: {:?}",
        tests.files
    );
}

/// More distinct files than any cap under test, best score first.
fn plentiful(stem: &str) -> Vec<(String, f32)> {
    (0u8..80)
        .map(|i| (format!("src/{stem}{i}.rs"), 0.9 - f32::from(i) / 1000.0))
        .collect()
}

/// [`StubSearch::answering`]'s borrowed shape.
fn borrow(paths: &[(String, f32)]) -> Vec<(&str, f32)> {
    paths.iter().map(|(p, s)| (p.as_str(), *s)).collect()
}

/// #6082: the defect this change exists for. `TRUSTY_AUDIT_INVESTIGATE_MAX_FILES`
/// raised how many files trusty-review read while the priority list stayed 60
/// and each dimension stayed 8, so everything past the 60th file reached the
/// investigation with no dimension and no reason — the knob moved half the
/// sample. Both caps now track the budget.
#[test]
fn a_raised_budget_raises_both_caps() {
    let caps = Caps::for_budget(300);
    assert_eq!(caps.priority_paths, 300, "the list tracks the budget");
    assert!(
        caps.files_per_dimension > MIN_FILES_PER_DIMENSION,
        "{caps:?}"
    );
    assert!(
        caps.files_per_dimension * (DD_DIMENSIONS.len() + 1) <= caps.priority_paths,
        "seven dimensions must fit inside the total: {caps:?}"
    );
    assert!(
        caps.top_k > MIN_TOP_K && caps.top_k <= MAX_TOP_K,
        "a raised per-dimension cap must not be starved by a top-12: {caps:?}"
    );

    // The whole per-dimension cap is reachable end to end, not just declared:
    // the stub truncates to `top_k`, so a request size that did not scale would
    // starve the dimension here rather than merely under-serve it in production.
    let auth = plentiful("auth");
    let err = plentiful("err");
    let client = StubSearch::default()
        .answering("credential handling", &borrow(&auth))
        .answering("error swallowed", &borrow(&err));
    let discovery = block_on(discover(&client, None, caps));
    assert_eq!(discovery.dimensions.len(), 2, "{discovery:?}");
    assert!(
        discovery
            .dimensions
            .iter()
            .all(|d| d.files.len() == caps.files_per_dimension),
        "each dimension fills its raised cap: {:?}",
        discovery
            .dimensions
            .iter()
            .map(|d| d.files.len())
            .collect::<Vec<_>>()
    );

    let blended = blend(&[], &discovery.dimensions, caps.priority_paths);
    assert!(
        blended.len() > MIN_PRIORITY_PATHS,
        "the ranking grows past the old fixed 60: {}",
        blended.len()
    );
    assert!(
        blended.iter().all(|p| p.dimension.is_some()),
        "every search-derived entry stays attributed"
    );
}

/// A budget at or under the old fixed list keeps the old numbers exactly, so a
/// small or unset budget behaves as it did before the caps scaled.
#[test]
fn the_default_budget_keeps_the_floor_semantics() {
    for budget in [0, 1, 40, MIN_PRIORITY_PATHS] {
        assert_eq!(
            Caps::for_budget(budget),
            Caps::default(),
            "budget {budget} must sit on the floor"
        );
    }
    let floor = Caps::default();
    assert_eq!(floor.priority_paths, MIN_PRIORITY_PATHS);
    assert_eq!(floor.files_per_dimension, MIN_FILES_PER_DIMENSION);
    assert_eq!(floor.top_k, MIN_TOP_K);

    // The audit's own default budget is above the floor and still bounded.
    let default = Caps::for_budget(crate::grounding::priority::DEFAULT_MAX_FILES);
    assert_eq!(default.priority_paths, 120);
    assert_eq!(default.files_per_dimension, 17);
    assert_eq!(default.top_k, 26);
}

/// The daemon's own response shape parses, and chunk paths come back
/// repo-relative whichever field carries them.
#[test]
fn the_daemons_search_envelope_parses_to_relative_paths() {
    let body = r#"{
        "results": [
            {"id":"a:1:9","file":"/w/repos/acme-api/src/pay.rs","path":"src/pay.rs",
             "start_line":12,"end_line":40,"content":"fn f(){}","score":0.81,
             "match_reason":"hybrid"},
            {"id":"b:1:9","file":"/w/repos/acme-api/src/auth.rs","start_line":3,
             "end_line":9,"content":"fn g(){}","score":0.62,"match_reason":"bm25"},
            {"id":"c:1:9","file":"","start_line":1,"end_line":2,"content":"",
             "score":0.1,"match_reason":"bm25"}
        ],
        "intent": "Semantic"
    }"#;
    let envelope: SearchEnvelope = serde_json::from_str(body).expect("parses");
    let hits = envelope.into_hits("/w/repos/acme-api");
    assert_eq!(
        hits.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
        vec!["src/pay.rs", "src/auth.rs"],
        "a portable path is used as-is; an absolute one is made relative; an empty one is dropped"
    );
    assert_eq!(hits[0].start_line, 12);
}

/// The endpoint names the index the repository was indexed under.
#[test]
fn the_query_url_names_the_index() {
    let client = HttpSearch::new(
        "http://127.0.0.1:7878/",
        "acme-api",
        std::path::Path::new("/w/repos/acme-api"),
    )
    .expect("a client is built");
    assert_eq!(client.url, "http://127.0.0.1:7878/indexes/acme-api/search");
}

/// A daemon that is not listening is a reason, never a panic.
#[test]
fn a_dead_daemon_is_a_reason_not_a_panic() {
    // Port 1 is never a trusty-search daemon.
    let client = HttpSearch::new(
        "http://127.0.0.1:1",
        "acme-api",
        std::path::Path::new("/w/repos/acme-api"),
    )
    .expect("a client is built");
    let err = block_on(client.hits("credential handling", MIN_TOP_K)).expect_err("must fail");
    assert!(err.contains("trusty-search did not answer"), "{err}");
}
