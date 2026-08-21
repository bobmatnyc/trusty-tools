//! Tests for [`super::pm_adapter`] — project-management adapter implementations.
//!
//! Loaded via `#[cfg(test)] #[path = "pm_adapter_tests.rs"] mod tests;` in `pm_adapter.rs`.
//! All items from `super::*` are accessible via the parent-module relationship.

use crate::core::config::Config;

use super::*;

#[test]
fn pm_source_as_str_is_stable() {
    assert_eq!(PmSource::Jira.as_str(), "jira");
    assert_eq!(PmSource::GitHub.as_str(), "github");
    assert_eq!(PmSource::Linear.as_str(), "linear");
    assert_eq!(PmSource::AzureDevOps.as_str(), "azure_devops");
}

#[test]
fn jira_ref_re_extracts_keys() {
    let out = extract_unique(jira_ref_re(), "PROJ-123 and ENG-456 and PROJ-123 again");
    assert_eq!(out, vec!["PROJ-123".to_string(), "ENG-456".to_string()]);
}

#[test]
fn github_ref_re_extracts_numbers() {
    let out = extract_unique(github_ref_re(), "fixes #42 see also #99 and #42 again");
    assert_eq!(out, vec!["#42".to_string(), "#99".to_string()]);
}

#[test]
fn github_ref_re_ignores_hex_colors() {
    let out = extract_unique(github_ref_re(), "color #abc123 not a ticket");
    assert!(out.is_empty());
}

#[test]
fn azdo_ref_re_extracts_ab_refs() {
    let out = extract_unique(azdo_ref_re(), "AB#1234 and AB#7 and AB#1234 again");
    assert_eq!(out, vec!["AB#1234".to_string(), "AB#7".to_string()]);
}

#[test]
fn build_adapters_returns_empty_for_default_config() {
    let cfg = Config::default();
    let adapters = build_adapters(&cfg);
    assert!(adapters.is_empty());
}

/// 🔴 #6130: a declared-absent GitHub leg builds no adapter, so nothing
/// queries an identity that does not exist. Paired with
/// `a_configured_slug_still_builds_the_adapter` below, this is the whole
/// distinction: the declaration turns the leg off, and nothing else does.
#[test]
fn a_declared_absent_github_leg_builds_no_adapter() {
    use crate::core::config::GithubConfig;
    let cfg = Config {
        github: Some(GithubConfig {
            repo: None,
            work_items_unavailable: Some("the checkout at /srv/apex names no GitHub remote".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(build_adapters(&cfg).is_empty());
}

/// The other side of that fork: a real slug still builds the adapter, so its
/// fetch still runs and still fails closed when GitHub refuses it.
#[test]
fn a_configured_slug_still_builds_the_adapter() {
    use crate::core::config::GithubConfig;
    let cfg = Config {
        github: Some(GithubConfig {
            repo: Some("bobmatnyc/trusty-tools".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let adapters = build_adapters(&cfg);
    assert_eq!(adapters.len(), 1);
    assert_eq!(adapters[0].source(), PmSource::GitHub);
}

/// A blank reason declares nothing — an empty string names no gap, so treating
/// it as a declaration would let a formatting slip silence the leg with a
/// Gaps line that says why nothing.
#[test]
fn a_blank_reason_does_not_declare_the_leg_absent() {
    use crate::core::config::GithubConfig;
    let cfg = GithubConfig {
        work_items_unavailable: Some("   ".into()),
        ..Default::default()
    };
    assert_eq!(cfg.work_items_declared_absent(), None);
    assert_eq!(GithubConfig::default().work_items_declared_absent(), None);
}

#[test]
fn build_adapters_includes_ado_when_configured() {
    use crate::core::config::{AzureDevOpsConfig, PmConfig};
    let cfg = Config {
        pm: Some(PmConfig {
            azure_devops: Some(AzureDevOpsConfig {
                organization_url: "https://dev.azure.com/myorg".into(),
                pat: "x".into(),
                project: Some("MyProject".into()),
                projects: vec![],
                ticket_regex: r"AB#(\d+)".into(),
                team_keys: vec![],
                fetch_on_reference: true,
                fetch_prs: false,
            }),
        }),
        ..Default::default()
    };
    let adapters = build_adapters(&cfg);
    assert_eq!(adapters.len(), 1);
    assert_eq!(adapters[0].name(), "azure_devops");
    assert_eq!(adapters[0].source(), PmSource::AzureDevOps);
}

/// Verify the `GitHubIssue` → `PmTicket` mapping used inside
/// `GitHubAdapter::fetch_ticket`. Constructed directly so the test
/// does not depend on network access.
///
/// Why: protects the contract that `id` is re-prefixed with `#`,
/// labels are flattened to strings, and `ticket_type` is `"issue"`.
/// What: builds a `GitHubIssue`, runs the same mapping, asserts fields.
/// Test: pass when `id == "#42"`, labels are `["bug", "p1"]`, type is
/// `"issue"`, status mirrors `state`.
#[test]
fn github_issue_maps_to_pm_ticket() {
    use crate::collect::github::{GhLabel, GitHubIssue};

    let issue = GitHubIssue {
        number: 42,
        title: "Crash".into(),
        state: "open".into(),
        html_url: "https://github.com/o/r/issues/42".into(),
        labels: vec![
            GhLabel { name: "bug".into() },
            GhLabel { name: "p1".into() },
        ],
        body: Some("repro".into()),
    };

    // Replicate the mapping inside fetch_ticket.
    let labels: Vec<String> = issue.labels.iter().map(|l| l.name.clone()).collect();
    let url = issue.html_url.clone();
    let raw = serde_json::to_value(&issue).expect("raw serializes");
    let ticket = PmTicket {
        id: format!("#{}", issue.number),
        title: issue.title.clone(),
        status: issue.state.clone(),
        ticket_type: "issue".into(),
        labels,
        url: Some(url),
        project: None,
        source: PmSource::GitHub,
        raw,
    };

    assert_eq!(ticket.id, "#42");
    assert_eq!(ticket.title, "Crash");
    assert_eq!(ticket.status, "open");
    assert_eq!(ticket.ticket_type, "issue");
    assert_eq!(ticket.labels, vec!["bug".to_string(), "p1".to_string()]);
    assert_eq!(
        ticket.url.as_deref(),
        Some("https://github.com/o/r/issues/42")
    );
    assert_eq!(ticket.source, PmSource::GitHub);
    assert!(ticket.raw.get("body").is_some());
}

/// Verify that a user-supplied regex with no capture group is rejected
/// at adapter-construction time (returns `None`, falls back to default).
///
/// Why: protects the documented contract that capture group 1 is the
/// ticket ID; a zero-group regex would silently break detection.
/// What: calls `compile_user_regex` with a pattern that has no groups
/// and asserts the result is `None`.
/// Test: `compile_user_regex("x", Some("\\d+"))` returns `None`.
#[test]
fn compile_user_regex_rejects_zero_capture_groups() {
    assert!(compile_user_regex("jira", Some(r"\d+")).is_none());
    assert!(compile_user_regex("jira", Some(r"(\d+)")).is_some());
    assert!(compile_user_regex("jira", None).is_none());
}

/// Verify that an invalid regex string yields `None` (caller falls back
/// to the default pattern).
#[test]
fn compile_user_regex_handles_invalid_pattern() {
    assert!(compile_user_regex("github", Some("[")).is_none());
}

/// Verify `extract_user_regex` extracts and deduplicates group-1 matches.
#[test]
fn extract_user_regex_dedupes_group_one() {
    let re = Regex::new(r"(?i)([a-z]+-\d+)").expect("compiles");
    let out = extract_user_regex(&re, "fix proj-123 and PROJ-123 and other-9");
    assert_eq!(
        out,
        vec![
            "proj-123".to_string(),
            "PROJ-123".to_string(),
            "other-9".to_string()
        ]
    );
}

/// JIRA adapter with a custom regex picks up lowercase keys.
#[test]
fn jira_adapter_uses_user_regex_for_lowercase_keys() {
    // Default JIRA pattern rejects lowercase — verify override fixes it.
    let cfg = crate::core::config::JiraConfig {
        url: Some("https://x.atlassian.net".into()),
        username: Some("u".into()),
        token: Some("t".into()),
        ..Default::default()
    };
    let client = crate::collect::jira::JiraClient::new(&cfg).expect("client");
    let adapter = JiraAdapter::with_ticket_regex(client, Some(r"(?i)\b([A-Z][A-Z0-9]*-\d+)\b"));
    let refs = adapter.detect_ticket_refs("see proj-123 and ENG-456");
    assert!(refs.contains(&"proj-123".to_string()));
    assert!(refs.contains(&"ENG-456".to_string()));
}

/// GitHub adapter with a custom regex picks up `Fix:#123` (no leading space).
#[test]
fn github_adapter_uses_user_regex_for_tight_refs() {
    let cfg = crate::core::config::GithubConfig {
        token: Some("t".into()),
        repo: Some("owner/name".into()),
        ..Default::default()
    };
    let client = crate::collect::github::GitHubClient::new(&cfg).expect("client");
    let adapter = GitHubAdapter::with_ticket_regex(client, Some(r"(#\d+)"));
    let refs = adapter.detect_ticket_refs("Fix:#123 and (#456) and closes#42");
    assert_eq!(
        refs,
        vec!["#123".to_string(), "#456".to_string(), "#42".to_string()]
    );
}

/// Linear adapter falls back to the default pattern when no override.
#[test]
fn linear_adapter_defaults_when_no_override() {
    let cfg = crate::core::config::LinearConfig {
        api_key: Some("k".into()),
        ..Default::default()
    };
    let client = crate::collect::linear::LinearClient::new(&cfg).expect("client");
    let adapter = LinearAdapter::with_ticket_regex(client, None);
    let refs = adapter.detect_ticket_refs("ENG-1 and FE-2");
    assert_eq!(refs, vec!["ENG-1".to_string(), "FE-2".to_string()]);
}

/// Smoke-test: an adapter built behind `dyn PmAdapter` can still call
/// `detect_ticket_refs` (verifies object-safety).
#[test]
fn adapters_are_object_safe_for_detect() {
    use crate::core::config::{AzureDevOpsConfig, PmConfig};
    let cfg = Config {
        pm: Some(PmConfig {
            azure_devops: Some(AzureDevOpsConfig {
                organization_url: "https://dev.azure.com/myorg".into(),
                pat: "x".into(),
                project: Some("P".into()),
                projects: vec![],
                ticket_regex: r"AB#(\d+)".into(),
                team_keys: vec![],
                fetch_on_reference: true,
                fetch_prs: false,
            }),
        }),
        ..Default::default()
    };
    let adapters = build_adapters(&cfg);
    let refs = adapters[0].detect_ticket_refs("see AB#7 and AB#8");
    // #5219: `build_adapters` now forwards `pm.azure_devops.ticket_regex`, and
    // capture group 1 of `AB#(\d+)` is the bare number. The pipeline maps a
    // detected reference onto the ticket's canonical `AB#N` id positionally, so
    // the two shapes never have to agree — see
    // `work_item_pipeline::tests::persist_tickets_maps_a_bare_ref_to_its_canonical_id`.
    assert_eq!(refs, vec!["7".to_string(), "8".to_string()]);
}

/// #5219: `work_items.source` for Azure DevOps is `azdo`, not the
/// `azure_devops` display label. Writing the label would orphan every ADO row
/// already in a user's database and break DOC-70's provider tag.
#[test]
fn work_item_source_keeps_the_azdo_database_tag() {
    assert_eq!(PmSource::AzureDevOps.work_item_source(), "azdo");
    assert_eq!(PmSource::AzureDevOps.as_str(), "azure_devops");
    assert_eq!(PmSource::Jira.work_item_source(), "jira");
    assert_eq!(PmSource::GitHub.work_item_source(), "github");
    assert_eq!(PmSource::Linear.work_item_source(), "linear");
}

/// #5219: `fetch_tickets` is positional — the caller pairs request `i` with
/// result `i` to learn a reference's canonical id. ADO's `workitemsbatch`
/// returns one flat list and drops unresolvable IDs (`errorPolicy=omit`), so
/// the override has to restore the order itself. Here `AB#7` resolves, `AB#8`
/// is omitted by the server, and `nonsense` never parses.
#[tokio::test]
async fn azdo_batch_fetch_preserves_request_order() {
    use crate::core::config::AzureDevOpsConfig;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/_apis/wit/workitemsbatch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1,
            "value": [{
                "id": 7,
                "fields": {
                    "System.Title": "Broken thing",
                    "System.State": "Active",
                    "System.WorkItemType": "Bug",
                    "System.Tags": "alpha; beta",
                    "System.TeamProject": "Widgets",
                },
            }],
        })))
        .mount(&server)
        .await;

    let cfg = AzureDevOpsConfig {
        organization_url: server.uri(),
        pat: "x".into(),
        project: Some("Widgets".into()),
        projects: vec![],
        ticket_regex: r"(?i)\bAB#(\d+)\b".into(),
        team_keys: vec![],
        fetch_on_reference: true,
        fetch_prs: false,
    };
    let adapter = AzureDevOpsAdapter::new(crate::collect::azdo::AzureDevOpsClient::new(cfg));

    let results = adapter.fetch_tickets(&["AB#8", "AB#7", "nonsense"]).await;
    assert_eq!(results.len(), 3);
    assert!(
        matches!(&results[0], Ok(None)),
        "an omitted id is an authoritative not-found"
    );
    let ticket = results[1]
        .as_ref()
        .expect("no transport error")
        .as_ref()
        .expect("AB#7 resolved");
    assert_eq!(
        ticket.id, "AB#7",
        "canonicalized to the correlate-able form"
    );
    assert_eq!(ticket.title, "Broken thing");
    assert_eq!(
        ticket.project.as_deref(),
        Some("Widgets"),
        "the ADO team project reaches work_items.project"
    );
    assert!(matches!(&results[2], Ok(None)), "unparseable id");
}

/// The complement: a failed batch must not read as three not-founds. Every
/// requested position gets the error, which is what lets the pipeline tell an
/// absent corpus from an empty one (#5219).
#[tokio::test]
async fn azdo_batch_fetch_reports_the_error_at_every_position() {
    use crate::core::config::AzureDevOpsConfig;

    let cfg = AzureDevOpsConfig {
        organization_url: "https://dev.azure.com/myorg".into(),
        // An empty PAT fails in `validate_credentials`, before any network call.
        pat: String::new(),
        project: Some("P".into()),
        projects: vec![],
        ticket_regex: r"(?i)\bAB#(\d+)\b".into(),
        team_keys: vec![],
        fetch_on_reference: true,
        fetch_prs: false,
    };
    let adapter = AzureDevOpsAdapter::new(crate::collect::azdo::AzureDevOpsClient::new(cfg));

    let results = adapter.fetch_tickets(&["AB#7", "AB#8"]).await;
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.is_err()), "results: {results:?}");
}

// ---------------------------------------------------------------------
// Issue #76 — fill test coverage gaps for the ticket_regex fix
// ---------------------------------------------------------------------

/// End-to-end: in-memory SQLite + an ADO adapter with detected ticket
/// references is persisted via the existing `work_items` /
/// `commit_work_items` writers.
///
/// Why: previously no test exercised the full path from detection through
/// to SQLite persistence — gap #1 in #76. This test verifies that
/// detection output can be persisted with the existing DB API so any
/// future change to detection that breaks the schema mapping surfaces
/// here.
/// What: opens an in-memory `Database`, runs `detect_ticket_refs` on a
/// list of commit messages, upserts the resulting `WorkItemRow`s, links
/// them to fake commit SHAs, then reads them back via
/// `get_work_items_for_commit` and asserts the round-trip is faithful.
/// Test: passes when 2 unique work items are stored and both are linked
/// to commit "sha1".
#[test]
fn collector_persists_detected_ado_refs_to_sqlite() {
    use crate::core::config::{AzureDevOpsConfig, PmConfig};
    use crate::core::db::{Database, WorkItemRow};

    // 1. Build an in-memory DB (runs migrations + WAL pragma).
    let mut db = Database::open_in_memory().expect("open in-memory db");

    // 2. Build an ADO adapter via the public factory.
    let cfg = Config {
        pm: Some(PmConfig {
            azure_devops: Some(AzureDevOpsConfig {
                organization_url: "https://dev.azure.com/myorg".into(),
                pat: "x".into(),
                project: Some("P".into()),
                projects: vec![],
                ticket_regex: r"AB#(\d+)".into(),
                team_keys: vec![],
                fetch_on_reference: true,
                fetch_prs: false,
            }),
        }),
        ..Default::default()
    };
    let adapters = build_adapters(&cfg);
    let adapter = adapters
        .iter()
        .find(|a| a.source() == PmSource::AzureDevOps)
        .expect("ADO adapter built");

    // 3. Run detection against a small commit corpus.
    let messages = [
        ("sha1", "Fixes AB#42 and AB#100"),
        ("sha1", "another commit referencing AB#42 again"),
        ("sha2", "no ticket here"),
    ];
    let mut detected: Vec<(String, String)> = Vec::new();
    for (sha, msg) in &messages {
        for id in adapter.detect_ticket_refs(msg) {
            detected.push(((*sha).to_string(), id));
        }
    }
    assert!(!detected.is_empty(), "detection produced refs");

    // 4. Persist via the existing DB writers in a single transaction.
    let conn = db.connection_mut();
    let tx = conn.transaction().expect("begin tx");
    let mut seen = std::collections::HashSet::new();
    for (sha, id) in &detected {
        let row = WorkItemRow {
            id: id.trim_start_matches("AB#").to_string(),
            source: "azdo".into(),
            title: format!("ticket {id}"),
            status: "Active".into(),
            item_type: "Bug".into(),
            tags: None,
            project: Some("P".into()),
            url: None,
            raw_json: None,
        };
        if seen.insert(row.id.clone()) {
            crate::core::db::work_items::upsert_work_item(&tx, &row).expect("upsert work item");
        }
        crate::core::db::work_items::link_commit_work_item(&tx, sha, &row.id, "azdo")
            .expect("link commit");
    }
    tx.commit().expect("commit tx");

    // 5. Read back and assert the round-trip.
    let conn = db.connection();
    let sha1_items = crate::core::db::work_items::get_work_items_for_commit(conn, "sha1")
        .expect("query sha1 items");
    let mut sha1_ids: Vec<String> = sha1_items.iter().map(|w| w.id.clone()).collect();
    sha1_ids.sort();
    assert_eq!(sha1_ids, vec!["100".to_string(), "42".to_string()]);

    let all =
        crate::core::db::work_items::list_work_items(conn, "azdo").expect("list azdo work items");
    assert_eq!(all.len(), 2, "two unique work items stored");
}

/// YAML → `build_adapters` → `detect_ticket_refs` round-trip with a
/// non-default `ticket_regex` actually changing detection output.
///
/// Why: gap #2 in #76. The config struct's YAML tests prove the field
/// parses; the adapter's unit tests prove a constructed regex changes
/// behavior — but nothing previously connected the two. A regression
/// where `build_adapters` forgets to forward `ticket_regex` would have
/// gone undetected.
/// What: parses a YAML config string with a lowercase-friendly
/// `jira.ticket_regex`, calls `build_adapters`, and asserts the JIRA
/// adapter's `detect_ticket_refs` honors the override (matches lowercase
/// keys the default pattern would reject).
/// Test: passes when `proj-123` is detected with the override and would
/// be rejected by the default JIRA pattern.
#[test]
fn pm_yaml_custom_ticket_regex_flows_to_adapter_detection() {
    // Override: case-insensitive JIRA-shape keys (default is upper only).
    let yaml = r#"
jira:
  url: "https://example.atlassian.net"
  username: "u"
  token: "t"
  ticket_regex: "(?i)\\b([A-Z][A-Z0-9]*-\\d+)\\b"
"#;
    let cfg: Config = serde_yaml::from_str(yaml).expect("yaml parses");
    let adapters = build_adapters(&cfg);
    let jira = adapters
        .iter()
        .find(|a| a.source() == PmSource::Jira)
        .expect("jira adapter built from yaml");

    let refs = jira.detect_ticket_refs("see proj-123 and ENG-456");
    // The override permits lowercase; default JIRA pattern would not.
    assert!(refs.iter().any(|s| s == "proj-123"));
    assert!(refs.iter().any(|s| s == "ENG-456"));

    // Sanity: without the override, lowercase is rejected.
    let default_adapter = JiraAdapter::with_ticket_regex(
        crate::collect::jira::JiraClient::new(cfg.jira.as_ref().unwrap()).expect("client"),
        None,
    );
    let default_refs = default_adapter.detect_ticket_refs("see proj-123 and ENG-456");
    assert!(!default_refs.iter().any(|s| s == "proj-123"));
    assert!(default_refs.iter().any(|s| s == "ENG-456"));
}

/// Capture-group *content* behavior: a regex whose group 1 matches but
/// captures non-ticket-shaped strings is accepted at compile time and
/// returns whatever group 1 captured.
///
/// Why: gap #3 in #76. `compile_user_regex` validates capture-group
/// *count* (must be >= 1) but cannot statically know whether group 1
/// will capture useful content. This test documents the current
/// contract: garbage-in, garbage-out — detection returns whatever the
/// regex says, never silently fabricates results, never panics.
/// What: feeds a regex with an *optional* capture group (which can match
/// while capturing nothing) and a regex that captures non-ticket text;
/// asserts behavior is well-defined and non-silent.
/// Test: optional group matches but group 1 may be empty/absent — output
/// contains only the captures the regex actually produced; no panic.
#[test]
fn user_regex_with_useless_capture_group_returns_well_defined_output() {
    // Optional capture group: matches every space-separated word but
    // group 1 may capture nothing. `extract_user_regex` only pushes
    // when `cap.get(1)` is Some, so a never-captured group yields [].
    let re = Regex::new(r"foo(\d+)?bar").expect("compiles");
    // "foobar" — group 1 is None for this match → no output pushed.
    let out = extract_user_regex(&re, "foobar and foobar");
    assert!(
        out.is_empty(),
        "optional group with no capture yields empty"
    );

    // Group 1 captures non-numeric text — extraction still returns it
    // verbatim. Downstream code that needs a u32 must handle parse
    // errors; this layer is intentionally type-agnostic so per-system
    // adapters can apply their own validation.
    let re = Regex::new(r"BUG-([A-Z]+)").expect("compiles");
    let out = extract_user_regex(&re, "see BUG-ABC and BUG-XYZ");
    assert_eq!(out, vec!["ABC".to_string(), "XYZ".to_string()]);

    // Zero-width assertion only: `(?:^)` — no capture group, fails
    // `compile_user_regex`'s capture-count gate → falls back to default.
    assert!(compile_user_regex("jira", Some(r"^")).is_none());
}

/// Verifies that supplying a regex with no capture group emits a
/// `tracing::warn!` carrying the offending pattern.
///
/// Why: gap #4 in #76. The fallback to the default pattern is now
/// defense-in-depth — if the warn ever stops firing, the silent-failure
/// path is back. This test pins the observable side-effect, not just
/// the return value.
/// What: invokes `compile_user_regex` with a zero-capture pattern under
/// the `tracing_test::traced_test` macro and asserts a log line with
/// the pattern text was recorded at WARN level.
/// Test: `logs_contain` returns true for a substring of the warn body.
#[test]
#[tracing_test::traced_test]
fn compile_user_regex_emits_warn_when_no_capture_groups() {
    let result = compile_user_regex("jira", Some(r"\d+"));
    assert!(result.is_none());
    // The warn message includes the literal text "no capture group" and
    // the offending pattern as a field — tracing_test captures both.
    assert!(logs_contain("no capture group"));
    assert!(logs_contain("\\d+"));
}

/// Same as above but for the invalid-pattern branch (the `Err(e)`
/// match arm of `compile_user_regex`).
#[test]
#[tracing_test::traced_test]
fn compile_user_regex_emits_warn_when_pattern_is_invalid() {
    let result = compile_user_regex("github", Some("["));
    assert!(result.is_none());
    assert!(logs_contain("failed to compile"));
}

/// Empty-corpus path: detection on `""` and on a non-matching string
/// returns an empty Vec without panicking, for every adapter.
///
/// Why: gap #5 in #76. Empty / no-match inputs are easy to overlook
/// and are real (e.g. binary-only commits, merge commits with no body).
/// What: walks every adapter built from a fully-populated `Config` and
/// asserts `detect_ticket_refs("")` and a no-ticket sentence are both
/// empty.
/// Test: every adapter returns `vec![]` for both inputs.
#[test]
fn detect_ticket_refs_handles_empty_corpus() {
    use crate::core::config::{
        AzureDevOpsConfig, GithubConfig, JiraConfig, LinearConfig, PmConfig,
    };
    let cfg = Config {
        jira: Some(JiraConfig {
            url: Some("https://x.atlassian.net".into()),
            username: Some("u".into()),
            token: Some("t".into()),
            ..Default::default()
        }),
        github: Some(GithubConfig {
            token: Some("t".into()),
            repo: Some("o/n".into()),
            ..Default::default()
        }),
        linear: Some(LinearConfig {
            api_key: Some("k".into()),
            ..Default::default()
        }),
        pm: Some(PmConfig {
            azure_devops: Some(AzureDevOpsConfig {
                organization_url: "https://dev.azure.com/myorg".into(),
                pat: "x".into(),
                project: Some("P".into()),
                projects: vec![],
                ticket_regex: r"AB#(\d+)".into(),
                team_keys: vec![],
                fetch_on_reference: true,
                fetch_prs: false,
            }),
        }),
        ..Default::default()
    };

    let adapters = build_adapters(&cfg);
    assert!(!adapters.is_empty(), "expected at least one adapter");

    for adapter in &adapters {
        let empty = adapter.detect_ticket_refs("");
        assert!(
            empty.is_empty(),
            "{} adapter must return empty for empty input",
            adapter.name()
        );

        let no_match = adapter.detect_ticket_refs("a commit message with no ticket refs");
        assert!(
            no_match.is_empty(),
            "{} adapter must return empty for no-match input, got {:?}",
            adapter.name(),
            no_match
        );
    }
}
