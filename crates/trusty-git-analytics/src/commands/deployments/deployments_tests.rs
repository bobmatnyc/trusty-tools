use super::*;

/// Why: smoke check that a malformed `deployment_tag_pattern` is
/// rejected with a clear error rather than panicking.
/// What: pass a clearly-invalid regex through and assert the error
/// names the field.
/// Test: pure constructor exercise.
#[test]
fn bad_deployment_tag_pattern_returns_clear_error() {
    let mut db = Database::open_in_memory().expect("db");
    let dora = DoraConfig {
        deployment_tag_pattern: "[unclosed".into(),
        ..DoraConfig::default()
    };
    let err = ingest_git_tags(&mut db, &[], &dora).expect_err("bad regex");
    let msg = format!("{err}");
    assert!(
        msg.contains("dora.deployment_tag_pattern"),
        "error should name the field: {msg}"
    );
}

/// Why: idempotency is the contract for `fact_deployments.deploy_id`
/// (issue #212) — re-running `tga deployments collect` must not
/// duplicate rows.
/// What: directly INSERT OR IGNORE two rows with the same
/// `deploy_id` and assert the second is a no-op.
/// Test: pure SQL exercise; the migration runner builds the table.
#[test]
fn deploy_id_primary_key_makes_reingest_idempotent() {
    let db = Database::open_in_memory().expect("db");
    let conn = db.connection();
    for _ in 0..2 {
        conn.execute(
            "INSERT OR IGNORE INTO fact_deployments \
                 (deploy_id, repo, environment, triggered_at, status, git_sha, git_tag, source) \
                 VALUES ('repo@v1.0.0', 'repo', 'production', \
                         '2025-01-01T00:00:00Z', 'success', 'sha', 'v1.0.0', 'git_tag')",
            [],
        )
        .expect("insert");
    }
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM fact_deployments", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 1, "INSERT OR IGNORE must dedupe on deploy_id PK");
}

/// Why: the URL parser is small but critical for the github_releases
/// path — wrong owner/repo means we hit the wrong API endpoint.
/// What: probe each supported URL form and a couple of negatives.
/// Test: each call returns the expected `(owner, repo)` or `None`.
#[test]
fn extract_owner_repo_from_url_handles_common_forms() {
    assert_eq!(
        extract_owner_repo_from_url("https://github.com/acme/widget.git"),
        Some(("acme".to_string(), "widget".to_string()))
    );
    assert_eq!(
        extract_owner_repo_from_url("https://github.com/acme/widget"),
        Some(("acme".to_string(), "widget".to_string()))
    );
    assert_eq!(
        extract_owner_repo_from_url("git@github.com:acme/widget.git"),
        Some(("acme".to_string(), "widget".to_string()))
    );
    assert_eq!(
        extract_owner_repo_from_url("ssh://git@github.com/acme/widget.git"),
        Some(("acme".to_string(), "widget".to_string()))
    );
    assert_eq!(
        extract_owner_repo_from_url("https://user@github.com/acme/widget"),
        Some(("acme".to_string(), "widget".to_string()))
    );
    assert!(extract_owner_repo_from_url("https://gitlab.com/acme/widget").is_none());
    assert!(extract_owner_repo_from_url("nonsense").is_none());
}

/// Why: explicit `org:` on a repo config must short-circuit ahead of
/// the on-disk remote probe — operators set it precisely to override
/// what's on the clone.
/// What: build a `RepositoryConfig` with `org=acme` and `name=widget`.
/// Test: returns `Some(("acme", "widget"))` regardless of path.
#[test]
fn resolve_repo_to_github_slug_prefers_explicit_org() {
    let cfg = RepositoryConfig {
        path: std::path::PathBuf::from("/tmp/some-dir"),
        name: Some("widget".into()),
        org: Some("acme".into()),
        ..Default::default()
    };
    assert_eq!(
        resolve_repo_to_github_slug(&cfg),
        Some(("acme".to_string(), "widget".to_string()))
    );
}

/// Why: when `org:` is unset and the path doesn't point at a real
/// repo, slug resolution must yield `None` so the caller skips the
/// repo cleanly.
/// What: synthetic path with no `org`.
/// Test: returns `None`.
#[test]
fn resolve_repo_to_github_slug_returns_none_when_unresolvable() {
    let cfg = RepositoryConfig {
        path: std::path::PathBuf::from("/nonexistent/path-xyz-987"),
        name: None,
        org: None,
        ..Default::default()
    };
    assert_eq!(resolve_repo_to_github_slug(&cfg), None);
}

/// Why: pagination correctness hinges on `Link: rel="next"` parsing;
/// a bug here either drops pages or loops forever.
/// What: feed the canonical GitHub `Link` header value and a value
/// without `rel="next"`.
/// Test: positive case returns the URL; negative returns `None`.
#[test]
fn next_link_parses_canonical_github_header() {
    let h = "<https://api.github.com/repositories/1/releases?page=2>; rel=\"next\", \
                 <https://api.github.com/repositories/1/releases?page=5>; rel=\"last\"";
    assert_eq!(
        parse_next_link_value(h).as_deref(),
        Some("https://api.github.com/repositories/1/releases?page=2"),
    );

    let last_only = "<https://api.github.com/repositories/1/releases?page=5>; rel=\"last\"";
    assert!(parse_next_link_value(last_only).is_none());
}

/// Why: the github_releases JSON shape is small but easy to break if
/// serde drops a `#[serde(default)]`. Lock the deserializer behavior.
/// What: parse the canonical Releases API payload.
/// Test: all fields extract; missing `target_commitish` tolerated.
#[test]
fn api_release_deserializes_full_and_minimal() {
    let full = r#"{
            "id": 1,
            "tag_name": "v1.2.3",
            "target_commitish": "main",
            "published_at": "2025-01-01T00:00:00Z",
            "draft": false,
            "prerelease": false
        }"#;
    let r: ApiRelease = serde_json::from_str(full).expect("parses");
    assert_eq!(r.tag_name, "v1.2.3");
    assert_eq!(r.target_commitish.as_deref(), Some("main"));
    assert!(!r.draft && !r.prerelease);
    assert!(r.published_at.is_some());

    let minimal = r#"{"tag_name": "v0.1.0"}"#;
    let r: ApiRelease = serde_json::from_str(minimal).expect("parses");
    assert_eq!(r.tag_name, "v0.1.0");
    assert!(r.target_commitish.is_none());
    assert!(r.published_at.is_none());
    assert!(!r.draft && !r.prerelease);
}

/// Why: the github_actions JSON envelope nests runs under
/// `workflow_runs`. A schema drift here drops every run silently.
/// What: parse a minimal envelope.
/// Test: run id, head_sha, conclusion all extract.
#[test]
fn api_workflow_run_deserializes() {
    let json = r#"{
            "workflow_runs": [
                {
                    "id": 999,
                    "head_sha": "deadbeefcafebabe",
                    "head_branch": "main",
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:05:00Z",
                    "conclusion": "success",
                    "name": "deploy-production",
                    "path": ".github/workflows/deploy-production.yml"
                }
            ]
        }"#;
    let env: ApiWorkflowRunsEnvelope = serde_json::from_str(json).expect("parses");
    assert_eq!(env.workflow_runs.len(), 1);
    let r = &env.workflow_runs[0];
    assert_eq!(r.id, 999);
    assert_eq!(r.head_sha, "deadbeefcafebabe");
    assert_eq!(r.conclusion.as_deref(), Some("success"));
    assert_eq!(r.name.as_deref(), Some("deploy-production"));
}

/// Why: `is_kept_run` is the bouncer for `fact_deployments` rows —
/// wrong predicate means we either inflate deployment counts with
/// failed runs or silently drop legitimate deploys.
/// What: probe every combination of conclusion + workflow filter.
/// Test: success-no-filter keeps; success-matching-name keeps;
/// success-mismatching-name drops; non-success drops.
#[test]
fn is_kept_run_enforces_conclusion_and_workflow_filter() {
    let mut run = ApiWorkflowRun {
        id: 1,
        head_sha: "sha".into(),
        head_branch: Some("main".into()),
        created_at: None,
        updated_at: None,
        conclusion: Some("success".into()),
        name: Some("deploy-production".into()),
        path: Some(".github/workflows/deploy-production.yml".into()),
    };
    assert!(is_kept_run(&run, None));
    assert!(is_kept_run(&run, Some("deploy-production")));
    assert!(is_kept_run(&run, Some("deploy-production.yml")));
    assert!(!is_kept_run(&run, Some("ci.yml")));

    run.conclusion = Some("failure".into());
    assert!(!is_kept_run(&run, None));

    run.conclusion = None;
    assert!(!is_kept_run(&run, None));
}
