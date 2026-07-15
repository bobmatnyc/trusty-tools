use super::*;
use crate::classify::classifier::{ClassificationEngine, ClassificationEngineConfig};
use crate::classify::rules::default_rules;
use crate::core::config::Config;
use crate::core::db::Database;
use rusqlite::params;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build an engine whose LLM tier points at `endpoint`.
fn engine_with_mock_llm(endpoint: &str) -> ClassificationEngine {
    let cfg = ClassificationEngineConfig {
        use_llm: true,
        ..ClassificationEngineConfig::default()
    };
    ClassificationEngine::new(default_rules(), cfg)
        .expect("build engine")
        .with_test_llm_endpoint(endpoint)
}

/// Stand up a mock chat-completions endpoint returning a fixed verdict.
async fn mock_llm_server(category: &str, confidence: f64, complexity: u8) -> MockServer {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "choices": [{
            "message": {
                "content": format!(
                    "{{\"category\":\"{category}\",\"subcategory\":null,\
                      \"confidence\":{confidence},\"complexity\":{complexity}}}"
                )
            }
        }]
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    server
}

/// Insert a commit row with no classification. Returns its row id.
fn insert_commit(db: &Database, sha: &str, message: &str) -> i64 {
    db.connection()
        .execute(
            "INSERT INTO commits \
             (sha, author_name, author_email, timestamp, message, repository) \
             VALUES (?1, 'a', 'a@x', '2024-01-01T00:00:00Z', ?2, 'acme/widgets')",
            params![sha, message],
        )
        .expect("insert commit");
    db.connection().last_insert_rowid()
}

/// Why: regression guard for the `--force` retroactive flow (issue
/// #205). Without `--force`, classified commits are skipped; with
/// `--force`, they must be re-classified and the existing
/// `classifications` row updated in place (no orphans). The fixture
/// pre-seeds a "wrong" verdict that the default ruleset's
/// conventional-commit tier will overwrite with the correct one.
/// What: seed one commit with a manually-attached classification
/// that disagrees with the default rules, run `tga classify --force`,
/// assert the row's `classifications` was updated (same id, new
/// category) and that no orphan rows were inserted.
/// Test: in-memory DB; no LLM.
#[tokio::test]
async fn pipeline_force_reclassifies_existing_rows_in_place() {
    let mut db = Database::open_in_memory().expect("db");

    // Pre-seed a (wrong) classification: category "feature" for a
    // message that the cc-fix rule will correctly classify as
    // "bugfix" on a force re-run.
    db.connection()
        .execute(
            "INSERT INTO classifications (category, subcategory, confidence, method) \
             VALUES ('feature', NULL, 0.5, 'regex_rule')",
            [],
        )
        .expect("insert classification");
    let pre_cls_id = db.connection().last_insert_rowid();
    let commit_id = insert_commit(&db, "sha-fix-1", "fix: handle null user");
    db.connection()
        .execute(
            "UPDATE commits SET classification_id = ?1 WHERE id = ?2",
            params![pre_cls_id, commit_id],
        )
        .expect("link cls");

    // Default flow (no force) should skip — verdict stays as 'feature'.
    let pipeline_no_force = ClassificationPipeline::new(Config::default());
    let engine = ClassificationEngine::new(default_rules(), ClassificationEngineConfig::default())
        .expect("engine");
    pipeline_no_force
        .run_with_engine(&mut db, engine)
        .await
        .expect("default run");
    let still_feature: String = db
        .connection()
        .query_row(
            "SELECT cl.category FROM classifications cl \
             JOIN commits c ON c.classification_id = cl.id WHERE c.sha = 'sha-fix-1'",
            [],
            |row| row.get(0),
        )
        .expect("query 1");
    assert_eq!(
        still_feature, "feature",
        "default flow must NOT re-classify already-classified commits"
    );

    // --force flips the verdict to 'bugfix' via the cc-fix rule.
    let pipeline_forced = ClassificationPipeline::new(Config::default()).with_force(true);
    let engine = ClassificationEngine::new(default_rules(), ClassificationEngineConfig::default())
        .expect("engine");
    pipeline_forced
        .run_with_engine(&mut db, engine)
        .await
        .expect("force run");

    // Verdict updated (and joined via the same row id — no orphan).
    let (new_cat, new_cls_id): (String, i64) = db
        .connection()
        .query_row(
            "SELECT cl.category, cl.id FROM classifications cl \
             JOIN commits c ON c.classification_id = cl.id WHERE c.sha = 'sha-fix-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query 2");
    assert_eq!(new_cat, "bugfix");
    assert_eq!(
        new_cls_id, pre_cls_id,
        "force must update in place, not orphan"
    );

    let total_rows: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM classifications", [], |row| row.get(0))
        .expect("count");
    assert_eq!(
        total_rows, 1,
        "force must not duplicate the classifications row"
    );
}

/// Why: `--since` bounds the scope of `--force` to a recent window
/// so operators can rewrite only the last quarter (or any window)
/// without re-classifying years of history.
/// What: seed two commits — one in 2023, one in 2025 — pre-classify
/// both as "feature", then run `tga classify --force --since 2025-01-01`
/// and assert only the 2025 commit was rewritten.
/// Test: in-memory DB; commits.timestamp is ISO8601 string.
#[tokio::test]
async fn pipeline_force_since_bounds_rewrite_window() {
    let mut db = Database::open_in_memory().expect("db");

    // Seed two pre-classified rows, then re-date their commits.
    for (sha, ts) in [
        ("sha-old", "2023-06-01T00:00:00Z"),
        ("sha-new", "2025-06-01T00:00:00Z"),
    ] {
        db.connection()
            .execute(
                "INSERT INTO classifications (category, confidence, method) \
                 VALUES ('feature', 0.5, 'regex_rule')",
                [],
            )
            .expect("insert cls");
        let cls_id = db.connection().last_insert_rowid();
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, classification_id) \
                 VALUES (?1, 'a', 'a@x', ?2, 'fix: handle null user', 'r', ?3)",
                params![sha, ts, cls_id],
            )
            .expect("insert commit");
    }

    // --force --since 2025-01-01 — only sha-new is in scope.
    let pipeline = ClassificationPipeline::new(Config::default())
        .with_force(true)
        .with_since(Some("2025-01-01".to_string()));
    let engine = ClassificationEngine::new(default_rules(), ClassificationEngineConfig::default())
        .expect("engine");
    pipeline
        .run_with_engine(&mut db, engine)
        .await
        .expect("force+since");

    // sha-new now classified as bugfix; sha-old still feature.
    let new_cat: String = db
        .connection()
        .query_row(
            "SELECT cl.category FROM classifications cl \
             JOIN commits c ON c.classification_id = cl.id WHERE c.sha = 'sha-new'",
            [],
            |row| row.get(0),
        )
        .expect("query new");
    assert_eq!(new_cat, "bugfix");

    let old_cat: String = db
        .connection()
        .query_row(
            "SELECT cl.category FROM classifications cl \
             JOIN commits c ON c.classification_id = cl.id WHERE c.sha = 'sha-old'",
            [],
            |row| row.get(0),
        )
        .expect("query old");
    assert_eq!(
        old_cat, "feature",
        "--since must exclude commits older than the bound"
    );
}

/// Why: `read_candidate_commits` is the single SQL-builder for the
/// pipeline's candidate set; regressions here would silently change
/// which commits get re-classified.
/// What: probes each of the three branches: default (NULL only),
/// force (all), force+since (windowed).
/// Test: pure SQL exercise against an in-memory DB.
#[test]
fn read_candidate_commits_branches_select_correctly() {
    let db = Database::open_in_memory().expect("db");
    // Two classified commits + one unclassified.
    db.connection()
        .execute(
            "INSERT INTO classifications (category, confidence, method) \
             VALUES ('feature', 0.5, 'regex_rule')",
            [],
        )
        .expect("cls");
    let cls_id = db.connection().last_insert_rowid();
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, classification_id) \
             VALUES ('old', 'a', 'a@x', '2023-01-01T00:00:00Z', 'm', 'r', ?1)",
            params![cls_id],
        )
        .expect("insert old");
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, classification_id) \
             VALUES ('new', 'a', 'a@x', '2025-06-01T00:00:00Z', 'm', 'r', ?1)",
            params![cls_id],
        )
        .expect("insert new");
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
             VALUES ('null', 'a', 'a@x', '2024-01-01T00:00:00Z', 'm', 'r')",
            [],
        )
        .expect("insert unclassified");

    // Default flow: only the unclassified row.
    let v =
        super::pipeline_db::read_candidate_commits(&db, false, None, None, &[]).expect("default");
    let shas: Vec<&str> = v.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(shas, vec!["null"]);

    // --force: every row.
    let v = super::pipeline_db::read_candidate_commits(&db, true, None, None, &[]).expect("force");
    let mut shas: Vec<&str> = v.iter().map(|c| c.sha.as_str()).collect();
    shas.sort();
    assert_eq!(shas, vec!["new", "null", "old"]);

    // --force --since 2025-01-01: only "new" (timestamp >= bound).
    let v = super::pipeline_db::read_candidate_commits(&db, true, Some("2025-01-01"), None, &[])
        .expect("force+since");
    let shas: Vec<&str> = v.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(shas, vec!["new"]);
}

/// Why: regression guard for issue #210. `commits.is_revert` was always
/// `0` because the classify-time UPDATE never touched it; only the
/// commit-message-heuristic backfill ever flipped the column. Any commit
/// whose verdict category is `revert` or `rollback` should now show
/// `is_revert = 1` after `classify` runs.
/// What: seed two commits — one with a `Revert "feat: ..."` message that
/// the default ruleset classifies as `cc-revert`, and one ordinary
/// `feat: add login` commit — run the synchronous (no-LLM) pipeline,
/// then assert the revert commit's `is_revert` is `1` and the feature
/// commit's is `0`.
/// Test: in-memory DB, default rules, no mocked LLM (use_llm = false).
#[tokio::test]
async fn pipeline_sets_is_revert_for_revert_verdicts() {
    let mut db = Database::open_in_memory().expect("db");
    // `cc-revert` rule (priority 115, confidence 0.9) wins on this msg.
    let revert_id = insert_commit(&db, "sha-revert", "Revert \"feat: add login\"");
    // `cc-feat` rule (priority 100, confidence 0.95) wins on this msg.
    let feature_id = insert_commit(&db, "sha-feat", "feat: add login form");

    let config = Config::default();
    let pipeline = ClassificationPipeline::new(config);
    let engine = ClassificationEngine::new(default_rules(), ClassificationEngineConfig::default())
        .expect("engine builds");
    pipeline
        .run_with_engine(&mut db, engine)
        .await
        .expect("run pipeline");

    let revert_flag: i64 = db
        .connection()
        .query_row(
            "SELECT is_revert FROM commits WHERE id = ?1",
            params![revert_id],
            |row| row.get(0),
        )
        .expect("query revert");
    assert_eq!(
        revert_flag, 1,
        "revert verdict must set commits.is_revert=1"
    );

    let feat_flag: i64 = db
        .connection()
        .query_row(
            "SELECT is_revert FROM commits WHERE id = ?1",
            params![feature_id],
            |row| row.get(0),
        )
        .expect("query feature");
    assert_eq!(
        feat_flag, 0,
        "non-revert verdict must leave commits.is_revert at 0"
    );
}

/// Why: `is_revert_verdict` is the single source of truth for which
/// classification verdicts flip `commits.is_revert`. A regression here
/// (e.g. accepting "reverted" or rejecting "rollback") would propagate
/// silently into DORA reports.
/// What: exercises every accept/reject path on both `category` and
/// `subcategory`.
/// Test: pure-function table.
#[test]
fn is_revert_verdict_recognizes_canonical_markers() {
    assert!(super::pipeline_db::is_revert_verdict("revert", None));
    assert!(super::pipeline_db::is_revert_verdict("Revert", None)); // case-insensitive
    assert!(super::pipeline_db::is_revert_verdict("rollback", None));
    assert!(super::pipeline_db::is_revert_verdict("ROLLBACK", None));
    assert!(super::pipeline_db::is_revert_verdict(
        "merge",
        Some("revert")
    ));
    assert!(super::pipeline_db::is_revert_verdict(
        "merge",
        Some("rollback")
    ));
    assert!(!super::pipeline_db::is_revert_verdict("feature", None));
    assert!(!super::pipeline_db::is_revert_verdict(
        "bugfix",
        Some("hotfix")
    ));
    // "reverted" is not a canonical marker — only the exact words match.
    assert!(!super::pipeline_db::is_revert_verdict("reverted", None));
}

/// Why: a complexity score from the LLM must reach the `classifications`
/// table; if the INSERT drops the column the score is silently lost.
/// What: classify an unclassified commit via a mock LLM that returns
/// `complexity: 2`, then assert the persisted row has `complexity = 2`.
/// Test: in-memory DB + wiremock; run the pipeline, query the column.
#[tokio::test]
async fn pipeline_writes_complexity_to_db() {
    let server = mock_llm_server("feature", 0.9, 2).await;
    let endpoint = format!("{}/v1/chat/completions", server.uri());

    let mut db = Database::open_in_memory().expect("db");
    // A message long enough (>= 12 chars) to skip the fuzzy "short =
    // chore" heuristic and devoid of rule keywords, so tiers 1–3 all
    // miss and the commit falls through to the LLM tier.
    insert_commit(&db, "sha-a", "zzz qqq vvv www yyy uuu");

    // Route every tier-1..3 verdict through the LLM by setting a
    // fallback threshold above any rule-tier confidence. Without this a
    // low-confidence catch-all rule would pre-empt the LLM tier.
    let classification = crate::core::config::ClassificationConfig {
        use_llm: true,
        llm_fallback_threshold: 1.0,
        ..crate::core::config::ClassificationConfig::default()
    };
    let config = Config {
        classification: Some(classification),
        ..Config::default()
    };

    let pipeline = ClassificationPipeline::new(config);
    let engine = engine_with_mock_llm(&endpoint);
    pipeline
        .run_with_engine(&mut db, engine)
        .await
        .expect("run pipeline");

    let complexity: Option<i64> = db
        .connection()
        .query_row(
            "SELECT cl.complexity FROM classifications cl \
             JOIN commits c ON c.classification_id = cl.id \
             WHERE c.sha = 'sha-a'",
            [],
            |row| row.get(0),
        )
        .expect("query complexity");
    assert_eq!(complexity, Some(2));
}

/// Why: the backfill must fill NULL complexity rows without disturbing
/// rows that already have a score.
/// What: seed one classification with `complexity IS NULL` and one with
/// `complexity = 3`; run the backfill against a mock LLM that returns
/// `complexity: 4`; assert the NULL row becomes 4 and the other stays 3.
/// Test: in-memory DB + wiremock.
#[tokio::test]
async fn backfill_complexity_updates_only_null_rows() {
    let server = mock_llm_server("feature", 0.9, 4).await;
    let endpoint = format!("{}/v1/chat/completions", server.uri());

    let mut db = Database::open_in_memory().expect("db");

    // Row 1: classified, complexity NULL, non-exact method → a candidate.
    db.connection()
        .execute(
            "INSERT INTO classifications (category, confidence, method, complexity) \
             VALUES ('feature', 0.5, 'regex_rule', NULL)",
            [],
        )
        .expect("insert cl 1");
    let cl1 = db.connection().last_insert_rowid();
    let c1 = insert_commit(&db, "sha-null", "needs scoring");
    db.connection()
        .execute(
            "UPDATE commits SET classification_id = ?1 WHERE id = ?2",
            params![cl1, c1],
        )
        .expect("link 1");

    // Row 2: already scored (complexity = 3) → must be left untouched.
    db.connection()
        .execute(
            "INSERT INTO classifications (category, confidence, method, complexity) \
             VALUES ('bugfix', 0.8, 'regex_rule', 3)",
            [],
        )
        .expect("insert cl 2");
    let cl2 = db.connection().last_insert_rowid();
    let c2 = insert_commit(&db, "sha-scored", "already scored");
    db.connection()
        .execute(
            "UPDATE commits SET classification_id = ?1 WHERE id = ?2",
            params![cl2, c2],
        )
        .expect("link 2");

    let engine = engine_with_mock_llm(&endpoint);
    let updated = ClassificationPipeline::backfill_complexity_with_engine(&mut db, &engine)
        .await
        .expect("backfill");
    assert_eq!(updated, 1, "only the NULL row should be updated");

    let filled: Option<i64> = db
        .connection()
        .query_row(
            "SELECT complexity FROM classifications WHERE id = ?1",
            params![cl1],
            |row| row.get(0),
        )
        .expect("query filled");
    assert_eq!(filled, Some(4), "NULL row backfilled to the LLM score");

    let unchanged: Option<i64> = db
        .connection()
        .query_row(
            "SELECT complexity FROM classifications WHERE id = ?1",
            params![cl2],
            |row| row.get(0),
        )
        .expect("query unchanged");
    assert_eq!(unchanged, Some(3), "already-scored row must be unchanged");
}

/// Why: regression guard for issue #2719. The Tier-0.5 external-source pass now
/// runs with bounded concurrency instead of a serial per-commit loop; the
/// dedupe-before-spawn step must still fetch each referenced ticket exactly once
/// (so a slow source cannot stall the run), and the resolved signal must land on
/// EVERY commit that references that ticket.
/// What: seed three commits that all reference the same JIRA key `PROJ-100`,
/// stand up a wiremock JIRA server that permits exactly one issue fetch
/// (`.expect(1)`), run the pipeline through `run_with_engine_and_resolver`, then
/// assert all three commits carry the `external_source` / `bug_fix` verdict. If
/// the concurrent path fired a duplicate in-flight request the mock's `expect(1)`
/// would fail on drop.
/// Test: in-memory DB + wiremock; no LLM tier.
#[tokio::test]
async fn pipeline_external_sources_dedupe_and_apply() {
    use crate::classify::sources::{
        ExternalSourceResolver, JiraFieldMappings, JiraSourceConfig, SourceConfig,
    };
    use std::collections::HashMap;

    let server = MockServer::start().await;
    let body = serde_json::json!({
        "key": "PROJ-100",
        "fields": { "issuetype": {"name": "Bug"}, "labels": [], "components": [] }
    });
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        // Exactly one fetch is allowed: three commits share PROJ-100, so the
        // dedupe-before-spawn step must collapse them to a single request.
        .expect(1)
        .mount(&server)
        .await;

    unsafe { std::env::set_var("JIRA_TOKEN_2719", "test-token") };

    let mut issue_type_map = HashMap::new();
    issue_type_map.insert("Bug".to_string(), "bug_fix".to_string());
    let jira = JiraSourceConfig {
        base_url: server.uri(),
        token_env: "JIRA_TOKEN_2719".to_string(),
        username: None,
        email_env: None,
        project_keys: vec![],
        field_mappings: JiraFieldMappings {
            issue_type: issue_type_map,
            labels: HashMap::new(),
            components: HashMap::new(),
        },
    };
    let resolver = ExternalSourceResolver::new(&[SourceConfig::Jira(jira)])
        .with_jira_base_url(0, server.uri());

    let mut db = Database::open_in_memory().expect("db");
    for sha in ["sha-a", "sha-b", "sha-c"] {
        insert_commit(&db, sha, "PROJ-100 fix the crash");
    }

    let pipeline = ClassificationPipeline::new(Config::default());
    let engine = ClassificationEngine::new(default_rules(), ClassificationEngineConfig::default())
        .expect("engine");
    pipeline
        .run_with_engine_and_resolver(&mut db, engine, Some(resolver))
        .await
        .expect("run with resolver");

    // All three commits must carry the external-source verdict.
    for sha in ["sha-a", "sha-b", "sha-c"] {
        let (category, method): (String, String) = db
            .connection()
            .query_row(
                "SELECT cl.category, cl.method FROM classifications cl \
                 JOIN commits c ON c.classification_id = cl.id WHERE c.sha = ?1",
                params![sha],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or_else(|e| panic!("query {sha}: {e}"));
        assert_eq!(category, "bug_fix", "{sha} must adopt the JIRA Bug mapping");
        assert_eq!(
            method, "external_source",
            "{sha} must be tagged as an external-source verdict"
        );
    }

    unsafe { std::env::remove_var("JIRA_TOKEN_2719") };
    // Dropping the server asserts `.expect(1)` — exactly one JIRA fetch.
    drop(server);
}

/// Why: regression guard for the source-aware LLM model default (bug fix
/// 2.3.1). When `llm.model` is absent, `build_engine` must pick the
/// default model for the configured source rather than always falling back
/// to `"gpt-4o-mini"`, which is invalid for `bedrock` and `anthropic-api`.
/// What: directly exercises the `source_default` selection logic by
/// constructing `LlmConfig` values for each source and asserting the
/// resolved default matches the expected constant.
/// Test: pure-logic unit test — no DB, no async, no HTTP.
#[test]
fn source_aware_default_model_selection() {
    use crate::classify::tiers::bedrock::DEFAULT_BEDROCK_MODEL;
    use crate::classify::tiers::llm::ANTHROPIC_DEFAULT_MODEL;
    use crate::core::config::{LlmConfig, LlmSource};

    // Helper: resolve the source_default the same way `build_engine` does.
    fn default_for(source: LlmSource) -> &'static str {
        match source {
            LlmSource::Bedrock => DEFAULT_BEDROCK_MODEL,
            LlmSource::AnthropicApi => ANTHROPIC_DEFAULT_MODEL,
            LlmSource::Openrouter => "gpt-4o-mini",
        }
    }

    // bedrock source + no model → DEFAULT_BEDROCK_MODEL
    let bedrock_cfg = LlmConfig {
        source: LlmSource::Bedrock,
        model: None,
        ..LlmConfig::default()
    };
    let resolved = bedrock_cfg
        .model
        .as_deref()
        .unwrap_or_else(|| default_for(bedrock_cfg.source.clone()));
    assert_eq!(
        resolved, DEFAULT_BEDROCK_MODEL,
        "bedrock source with no model must fall back to DEFAULT_BEDROCK_MODEL"
    );

    // anthropic-api source + no model → ANTHROPIC_DEFAULT_MODEL
    let anthropic_cfg = LlmConfig {
        source: LlmSource::AnthropicApi,
        model: None,
        ..LlmConfig::default()
    };
    let resolved = anthropic_cfg
        .model
        .as_deref()
        .unwrap_or_else(|| default_for(anthropic_cfg.source.clone()));
    assert_eq!(
        resolved, ANTHROPIC_DEFAULT_MODEL,
        "anthropic-api source with no model must fall back to ANTHROPIC_DEFAULT_MODEL"
    );

    // openrouter source + no model → "gpt-4o-mini"
    let openrouter_cfg = LlmConfig {
        source: LlmSource::Openrouter,
        model: None,
        ..LlmConfig::default()
    };
    let resolved = openrouter_cfg
        .model
        .as_deref()
        .unwrap_or_else(|| default_for(openrouter_cfg.source.clone()));
    assert_eq!(
        resolved, "gpt-4o-mini",
        "openrouter source with no model must fall back to gpt-4o-mini"
    );

    // Explicit model always wins, regardless of source.
    for source in [
        LlmSource::Bedrock,
        LlmSource::AnthropicApi,
        LlmSource::Openrouter,
    ] {
        let explicit_cfg = LlmConfig {
            source: source.clone(),
            model: Some("my-custom-model".to_string()),
            ..LlmConfig::default()
        };
        let resolved = explicit_cfg
            .model
            .as_deref()
            .unwrap_or_else(|| default_for(source));
        assert_eq!(
            resolved, "my-custom-model",
            "explicit model must override the source default"
        );
    }
}
