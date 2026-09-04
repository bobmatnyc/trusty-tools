//! Unit tests for the report pipeline's decisions (#6669, moved from
//! `src/cli_report.rs`).
//!
//! Why: these rules — template precedence, the code-only decision, the
//! investigation budget, the credential preflight, the inference attribution,
//! and the pre-synthesis snapshot — moved into the library with the pipeline
//! they govern, so a second front door cannot get a different answer than the
//! CLI does.
//! What: one test per rule, all pure or filesystem-only; the network-bound
//! paths stay covered by `tests/report_investigate.rs` and the live smoke run.
//! Test: this file.

use super::*;

use crate::report::manifest::parse_manifest;

/// Parse a manifest from an in-memory string.
fn manifest_from(toml: &str) -> Manifest {
    parse_manifest(toml, Path::new("m.toml")).expect("manifest parses")
}

/// A minimal one-repository manifest with `[report]` keys spliced in.
fn manifest_with(report_keys: &str) -> Manifest {
    manifest_from(&format!(
        "[report]\ntitle = \"T\"\n{report_keys}\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n"
    ))
}

// ── Template and scope precedence (#6669) ────────────────────────────────────

/// Why: an explicit request must outrank a manifest that travelled with the
/// data. The operator asking now knows more than the file does.
/// What: a request template beats the manifest key.
/// Test: this test itself.
#[test]
fn the_request_template_wins() {
    let mut req = ReportRequest::new("m.toml");
    req.template = Some("report-technical-dd".to_string());
    let manifest = manifest_with("template = \"report-technical-dd-cast\"");
    assert_eq!(
        resolve_template_name_from(&req, &manifest, None),
        "report-technical-dd"
    );
}

/// Why: `--template cast` is what a third-party runbook types; the alias has to
/// expand at whichever tier named it, not only at the CLI.
/// What: the alias expands from the request and from the manifest key.
/// Test: this test itself.
#[test]
fn the_cast_alias_expands() {
    let mut req = ReportRequest::new("m.toml");
    req.template = Some("cast".to_string());
    assert_eq!(
        resolve_template_name_from(&req, &manifest_with(""), None),
        "report-technical-dd-cast"
    );

    let from_manifest = ReportRequest::new("m.toml");
    assert_eq!(
        resolve_template_name_from(&from_manifest, &manifest_with("template = \"cast\""), None),
        "report-technical-dd-cast"
    );
}

/// Why: with nothing naming a template, the generic one is what every existing
/// run already gets — this pins that the new tiers did not disturb it.
/// What: an empty request over a bare manifest resolves to the default.
/// Test: this test itself.
#[test]
fn nothing_named_is_still_the_default_template() {
    // #6669: the environment tier is injected as `None` rather than read from
    // the process, so this asserts unconditionally instead of skipping on a
    // machine that happens to export the variable.
    assert_eq!(
        resolve_template_name_from(&ReportRequest::new("m.toml"), &manifest_with(""), None),
        DEFAULT_TEMPLATE
    );
}

/// Why: `trusty-audit` hands an engagement's template down the environment,
/// and that tier must sit BELOW the manifest — an operator editing the
/// manifest is making a later, more specific decision than the orchestrator
/// that exported the variable.
/// What: the variable alone beats the default, the manifest beats the
/// variable, and the request beats both; the `cast` alias expands from the
/// environment tier too.
/// Test: this test itself.
#[test]
fn the_environment_template_is_read_below_the_manifest() {
    let bare = ReportRequest::new("m.toml");
    let from_env = Some("report-technical-dd-cast".to_string());

    assert_eq!(
        resolve_template_name_from(&bare, &manifest_with(""), from_env.clone()),
        "report-technical-dd-cast",
        "the variable is read when nothing above it names a template"
    );
    assert_eq!(
        resolve_template_name_from(
            &bare,
            &manifest_with("template = \"report-technical-dd\""),
            from_env.clone()
        ),
        "report-technical-dd",
        "a manifest key still wins over the variable"
    );

    let mut req = ReportRequest::new("m.toml");
    req.template = Some("report-technical-dd".to_string());
    assert_eq!(
        resolve_template_name_from(&req, &manifest_with("template = \"cast\""), from_env),
        "report-technical-dd",
        "the request wins over both"
    );

    assert_eq!(
        resolve_template_name_from(&bare, &manifest_with(""), Some("cast".to_string())),
        "report-technical-dd-cast",
        "the alias expands at the environment tier too"
    );
}

/// Why: an engagement that collected only a repository declares that once, and
/// every re-render of it must keep saying so — an operator who forgets the flag
/// must not silently widen the stated scope.
/// What: the manifest key alone turns code-only on.
/// Test: this test itself.
#[test]
fn the_manifest_can_declare_code_only() {
    let req = ReportRequest::new("m.toml");
    assert!(resolve_code_only_from(
        &req,
        &manifest_with("code_only = true"),
        false
    ));
    assert!(!resolve_code_only_from(
        &req,
        &manifest_with("code_only = false"),
        false
    ));
}

/// Why: the request flag is a switch, so it can only turn the mode ON. A caller
/// omitting it over a manifest that declared it must not widen the scope.
/// What: the request flag alone turns it on; omitting it leaves a declaring
/// manifest's answer standing.
/// Test: this test itself.
#[test]
fn the_request_flag_only_widens_nothing() {
    let mut req = ReportRequest::new("m.toml");
    req.code_only = true;
    assert!(resolve_code_only_from(&req, &manifest_with(""), false));

    let bare = ReportRequest::new("m.toml");
    assert!(
        resolve_code_only_from(&bare, &manifest_with("code_only = true"), false),
        "an omitted flag must not turn off what the manifest declared"
    );
}

/// Why: `trusty-audit` declares an engagement's scope through the environment,
/// and that tier must be able to turn the mode ON on its own — an audit that
/// collected only a repository says so once, wherever the re-render happens.
/// What: the variable alone turns it on, and it cannot turn off what the
/// request or the manifest declared.
/// Test: this test itself.
#[test]
fn the_environment_can_declare_code_only() {
    let bare = ReportRequest::new("m.toml");
    assert!(
        resolve_code_only_from(&bare, &manifest_with(""), true),
        "the variable alone turns the mode on"
    );
    assert!(
        !resolve_code_only_from(&bare, &manifest_with(""), false),
        "with no tier declaring it, the render stays full scope"
    );
    assert!(
        resolve_code_only_from(&bare, &manifest_with("code_only = true"), false),
        "an unset variable must not widen what the manifest declared"
    );

    let mut req = ReportRequest::new("m.toml");
    req.code_only = true;
    assert!(
        resolve_code_only_from(&req, &manifest_with("code_only = false"), false),
        "and must not widen what the request asked for"
    );
}

/// Why: a typo in an orchestrator's environment must read as absent. Silently
/// narrowing a report's stated scope is the one failure this cannot afford.
/// What: `flag_is_truthy` — the function `env_flag` itself calls — recognises
/// the four truthy spellings after trimming and case-folding, and nothing else.
/// Test: this test itself.
#[test]
fn an_unrecognised_env_value_reads_as_absent() {
    for truthy in ["1", "true", "TRUE", " yes ", "on"] {
        assert!(flag_is_truthy(truthy), "{truthy} must read as set");
    }
    for falsy in ["", "0", "no", "off", "ture", "maybe", "1 0"] {
        assert!(!flag_is_truthy(falsy), "{falsy} must read as absent");
    }
}

/// Why: the CLI's documented defaults are the contract a runbook depends on.
/// What: a bare request writes to `./reports`, renders full scope, and asks for
/// no optional pass.
/// Test: this test itself.
#[test]
fn defaults_match_the_cli() {
    let req = ReportRequest::new("m.toml");
    assert_eq!(req.out, PathBuf::from("./reports"));
    assert!(!req.code_only);
    assert!(!req.benchmark);
    assert!(!req.corpus_add);
    assert!(!req.analyze);
    assert!(!req.no_mermaid);
    assert!(req.template.is_none());
    assert!(req.corpus.is_none());
    assert!(req.analyze_timeout_secs.is_none());
}

// ── Corpus and budget precedence ─────────────────────────────────────────────

/// Why: an explicit corpus directory must win over the manifest key.
/// What: the resolver returns the request's directory.
/// Test: this test itself.
#[test]
fn the_explicit_corpus_beats_the_manifest() {
    let mut req = ReportRequest::new("m.toml");
    req.corpus = Some(PathBuf::from("/tmp/corpus"));
    let dir = resolve_corpus_dir(&req, &manifest_with("")).expect("resolve");
    assert_eq!(dir, PathBuf::from("/tmp/corpus"));
}

/// Why (#6712): the corpus-scan budget has three tiers, and a flag that loses to
/// the manifest is worse than no flag — an operator raising it on a slow run
/// would see nothing change.
/// What: the request wins; the manifest fills an unset request; neither set
/// yields the default, and a `0` at either tier reads as unset.
/// Test: this test itself.
#[test]
fn the_request_analyze_timeout_beats_the_manifest() {
    use crate::report::analyze_endpoints::DEFAULT_CORPUS_BUDGET;
    use std::time::Duration;

    let mut req = ReportRequest::new("m.toml");
    req.analyze_timeout_secs = Some(600);
    let manifest = manifest_with("analyze_timeout_secs = 30");
    assert_eq!(
        resolve_analyze_budget(&req, &manifest),
        Duration::from_secs(600),
        "the flag wins over the manifest key"
    );

    let bare = ReportRequest::new("m.toml");
    assert_eq!(
        resolve_analyze_budget(&bare, &manifest),
        Duration::from_secs(30),
        "the manifest key fills an unset flag"
    );
    assert_eq!(
        resolve_analyze_budget(&bare, &manifest_with("")),
        DEFAULT_CORPUS_BUDGET,
        "neither tier set leaves the default, which is what a 104k-chunk corpus \
         scan needs"
    );
    assert_eq!(
        resolve_analyze_budget(&bare, &manifest_with("analyze_timeout_secs = 0")),
        DEFAULT_CORPUS_BUDGET,
        "zero means unset, never give up instantly"
    );
}

/// Why: the investigation budget resolves in the order request, manifest,
/// environment, default, and each tier fills only what the one above left
/// unset.
/// What: a request cap overrides the manifest key while the manifest's byte cap
/// still fills the unset half.
/// Test: this test itself.
#[test]
fn the_request_budget_wins_per_dimension() {
    let mut req = ReportRequest::new("m.toml");
    req.investigate_max_files = Some(7);
    let manifest = manifest_with("investigate_max_files = 3\ninvestigate_max_bytes = 1024");
    let budget = resolve_budget_from(&req, &manifest, None, None);
    assert_eq!(
        budget.max_files, 7,
        "the request wins over the manifest key"
    );
    assert_eq!(
        budget.max_bytes, 1024,
        "the manifest key fills the unset request field"
    );
}

/// #6082: the environment is the tier below the manifest, and it is what
/// carries an audit's budget across the two process boundaries in time.
///
/// Why this is the regression: `trusty-audit` records the budget in `[report]`,
/// but on the sweep path `tga audit` has already run this pipeline against that
/// manifest by the time the key is written — so the shipped manifest declared
/// `investigate_max_files = 240` while the investigation that produced the
/// report recorded `{"max_files": 40}`, this crate's bare default.
#[test]
fn the_environment_budget_is_read_below_the_manifest() {
    let req = ReportRequest::new("m.toml");
    let bare = manifest_with("");

    let from_env = resolve_budget_from(&req, &bare, Some(240), Some(2_457_600));
    assert_eq!(from_env.max_files, 240, "the audit's budget arrives");
    assert_eq!(from_env.max_bytes, 2_457_600);

    let declared = manifest_with("investigate_max_files = 3");
    let mixed = resolve_budget_from(&req, &declared, Some(240), Some(2_457_600));
    assert_eq!(mixed.max_files, 3, "an operator's manifest key still wins");
    assert_eq!(
        mixed.max_bytes, 2_457_600,
        "and the environment fills only what the manifest left unset"
    );

    let none = resolve_budget_from(&req, &bare, None, None);
    assert_eq!(
        none.max_files,
        Budget::default().max_files,
        "no manifest key and no variable is still the default"
    );
}

// ── #5454: the credential preflight ──────────────────────────────────────────

/// Why: #5454 — this is the failure that is knowable BEFORE a multi-minute
/// sweep, and letting it surface at the end (as the provider-build error it
/// used to be) wastes the whole one-shot run DOC-67 allows.
/// What: a blank OpenRouter key is rejected, and the message names the variable
/// and how to set it.
/// Test: this test itself.
#[test]
fn preflight_rejects_a_blank_openrouter_key() {
    for blank in ["", "   ", "\n"] {
        let err = credential_rule(Provider::OpenRouter, blank)
            .expect_err("a blank key must not pass the preflight");
        let msg = format!("{err}");
        assert!(
            msg.contains("OPENROUTER_API_KEY") && msg.contains("export OPENROUTER_API_KEY="),
            "the message must name the variable and how to set it: {msg}"
        );
        // #6135: the provider may have come from the manifest rather than from
        // anything on this machine, so the message names it.
        assert!(
            msg.contains("openrouter"),
            "the message must name the provider that asked for the key: {msg}"
        );
    }
}

/// Why: #6135 — the report states which models ran, and that record starts
/// here. All three roles are resolved, not only the reviewer the synthesis pass
/// builds, so the manifest's whole declared selection is attributed.
/// What: resolves against a config whose role models are the built-in
/// OpenRouter defaults, and asserts one row per role.
/// Test: this test itself.
#[test]
fn attribution_names_every_role() {
    let manifest = crate::config::RoleManifest {
        reviewer_model: Some("anthropic/claude-opus-4.8".to_string()),
        verifier_model: Some("anthropic/claude-haiku-4.5".to_string()),
        summarizer_model: Some("anthropic/claude-haiku-4.5".to_string()),
        provider: Some("openrouter".to_string()),
    };
    let config = ReviewConfig::from_env_and_manifest(None, None, Some(&manifest));
    let record = resolve_attribution(&config, "the manifest's [inference] section")
        .expect("the declared selection resolves");

    assert_eq!(record.provider, "openrouter");
    assert_eq!(
        record
            .roles
            .iter()
            .map(|r| r.role.as_str())
            .collect::<Vec<_>>(),
        vec!["reviewer", "verifier", "summarizer"]
    );
    assert!(
        record.roles.iter().all(|r| r.requested == r.ran),
        "nothing was adjusted here: {:?}",
        record.roles
    );
    assert_eq!(record.roles[0].ran, "anthropic/claude-opus-4.8");
}

/// Why: the resolver adjusting an id must never be invisible — that is the
/// whole anti-silent-wrong-model guarantee once refusal is gone.
/// What: a manifest whose reviewer id is pinned to Bedrock but spelled for
/// OpenRouter, resolved and attributed.
/// Test: this test itself.
#[test]
fn attribution_shows_a_translated_id_as_requested_then_ran() {
    let manifest = crate::config::RoleManifest {
        reviewer_model: Some("bedrock/anthropic/claude-sonnet-4.6".to_string()),
        provider: Some("bedrock".to_string()),
        ..Default::default()
    };
    let config = ReviewConfig::from_env_and_manifest(None, None, Some(&manifest));
    let record =
        resolve_attribution(&config, "the manifest's [inference] section").expect("resolves");

    let reviewer = &record.roles[0];
    assert_eq!(reviewer.requested, "bedrock/anthropic/claude-sonnet-4.6");
    assert_eq!(reviewer.ran, "us.anthropic.claude-sonnet-4-6");
    assert!(reviewer.note.is_some(), "the adjustment must be recorded");
    assert!(
        record.line().contains(" → "),
        "the page shows both halves: {}",
        record.line()
    );
}

/// Why: the preflight must not stand between an operator with a key and a run.
/// What: any non-blank key passes.
/// Test: this test itself.
#[test]
fn preflight_accepts_a_present_openrouter_key() {
    credential_rule(Provider::OpenRouter, "sk-or-v1-example")
        .expect("a present key passes the preflight");
}

/// Why: OpenRouter is the only path #5454 preflights; Bedrock resolves its
/// credentials through the AWS chain and Fireworks through its own key, so
/// neither can be judged from `openrouter_api_key`. They stay the
/// provider-build site's business — which is fatal now too.
/// What: a non-OpenRouter provider passes even with no OpenRouter key.
/// Test: this test itself.
#[test]
fn preflight_leaves_non_openrouter_providers_to_the_build_site() {
    credential_rule(Provider::Bedrock, "").expect("Bedrock is not preflighted here");
    credential_rule(Provider::Fireworks, "").expect("Fireworks is not preflighted here");
}

/// Why: an operator's terminal, their shell history, and any log scraping it
/// are all places a key must never appear.
/// What: a run with a real-looking key produces no error at all; a run with a
/// blank one produces a message containing no key material.
/// Test: this test itself.
#[test]
fn preflight_message_never_echoes_the_key() {
    let secret = "sk-or-v1-DEADBEEFdeadbeef";
    credential_rule(Provider::OpenRouter, secret).expect("present key passes");
    let msg = format!(
        "{}",
        credential_rule(Provider::OpenRouter, "").expect_err("blank key fails")
    );
    assert!(!msg.contains(secret), "no key material may appear: {msg}");
    assert!(
        !msg.contains("sk-or"),
        "no key-shaped text may appear: {msg}"
    );
}

// ── #6093: the pre-synthesis investigation snapshot ──────────────────────────

/// A one-repo investigation with a single verified finding.
fn snapshot_fixture() -> Investigation {
    use crate::report::investigate::{InvestigationStatus, RepoInvestigation, VerifiedFinding};
    Investigation {
        repos: vec![RepoInvestigation {
            verdicts: None,
            slug: "acme-core".to_string(),
            name: "Acme Core".to_string(),
            status: InvestigationStatus::Available,
            findings: vec![VerifiedFinding {
                trace_verdict: String::new(),
                cwe_id: Vec::new(),
                title: "Hardcoded credential".to_string(),
                severity: crate::report::metrics::Severity::Red,
                dimension: "security".to_string(),
                file: "src/auth.rs".to_string(),
                line: Some(12),
                evidence_quote: "let api_key = \"…\";".to_string(),
                description: "A credential is committed in source.".to_string(),
                business_impact: String::new(),
                remediation: "Move it to the secret store.".to_string(),
                cost_effort: String::new(),
            }],
            deps: Default::default(),
            traces: None,
            coverage: Default::default(),
            exposure: Vec::new(),
        }],
    }
}

/// Why: #6093 — a synthesis failure used to discard the whole investigation,
/// the expensive half of a report run. The snapshot must land before the first
/// synthesis call and must be readable afterwards.
/// What: writes a fixture investigation to a temp dir; asserts the file exists
/// at the documented name and that its JSON still carries the verified
/// finding's title and file.
/// Test: this test itself.
#[test]
fn investigation_snapshot_is_written_and_reloadable() {
    let dir = tempfile::tempdir().expect("tempdir");
    persist_investigation(dir.path(), &snapshot_fixture());

    let path = dir.path().join(INVESTIGATION_SNAPSHOT_FILENAME);
    let raw = std::fs::read_to_string(&path).expect("the snapshot must exist");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    assert_eq!(parsed["repos"][0]["slug"], "acme-core");
    assert_eq!(
        parsed["repos"][0]["findings"][0]["title"],
        "Hardcoded credential"
    );
    assert_eq!(parsed["repos"][0]["findings"][0]["file"], "src/auth.rs");
}

/// Why: #6788 — the inventory was capped at 30 rows BEFORE this snapshot was
/// written, so trusty-audit's OSV lookup read 30 packages from a repository
/// declaring 134 and reported partial coverage as complete.
/// What: builds a real inventory from a 35-dependency Cargo.toml, persists an
/// investigation carrying it, and asserts all 35 rows reach
/// `investigation.json` with the total intact.
/// Test: this test itself.
#[test]
fn investigation_snapshot_carries_every_dependency_row() {
    use crate::report::investigate::deps::build_inventory;

    let repo = tempfile::tempdir().expect("repo dir");
    let mut toml = String::from("[package]\nname = \"x\"\n[dependencies]\n");
    for i in 0..35 {
        toml.push_str(&format!("dep{i:02} = \"1.0\"\n"));
    }
    std::fs::write(repo.path().join("Cargo.toml"), toml).expect("manifest");

    let mut inv = snapshot_fixture();
    inv.repos[0].deps = build_inventory(repo.path());

    let dir = tempfile::tempdir().expect("tempdir");
    persist_investigation(dir.path(), &inv);

    let raw = std::fs::read_to_string(dir.path().join(INVESTIGATION_SNAPSHOT_FILENAME))
        .expect("the snapshot must exist");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    let rows = parsed["repos"][0]["deps"]["deps"]
        .as_array()
        .expect("deps array");
    assert_eq!(rows.len(), 35, "the snapshot must carry the full inventory");
    assert_eq!(rows[34]["name"], "dep34");
    assert_eq!(parsed["repos"][0]["deps"]["total"], 35);
}

/// Why: a recovery aid must never turn a run that would have succeeded into a
/// failure of its own.
/// What: points the snapshot at a path that cannot be a directory (an existing
/// file); asserts the call returns normally rather than panicking.
/// Test: this test itself.
#[test]
fn investigation_snapshot_failure_is_not_fatal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blocked = dir.path().join("not-a-dir");
    std::fs::write(&blocked, b"x").expect("write blocker");
    persist_investigation(&blocked, &snapshot_fixture());
    assert!(blocked.is_file(), "the blocking file is untouched");
}
