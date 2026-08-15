//! The provider-agnostic `work_items` pull, driven by [`PmAdapter`].
//!
//! Why: #5219 — only Azure DevOps wrote `work_items` in production, through a
//! hand-rolled pipeline inside [`crate::collect::collector`], while
//! [`PmAdapter`] and [`build_adapters`] sat fully built and configured with no
//! caller outside their own tests. Adding a second and third per-provider
//! pipeline for JIRA and GitHub beside it would have made that dead abstraction
//! permanent, so the pipeline moved onto the trait instead: one fetch shape,
//! one mapping, one fault convention, for every provider that implements it.
//! What: [`fetch_and_persist_work_items`] asks each enabled adapter which
//! references its own syntax finds in the commit corpus, batch-fetches them
//! through the trait, and writes the results to `work_items` /
//! `commit_work_items` via [`persist_tickets`].
//! Test: this module's own `tests`.
//!
//! Linear is deliberately NOT routed through here — see
//! [`fetch_on_reference`]. It keeps [`crate::collect::linear_pipeline`],
//! which writes `linear_issues` in the same pass and applies
//! `linear.team_keys` before fetching; both are outside what [`PmAdapter`]
//! expresses, and routing `work_items` through this module while
//! `linear_issues` stayed behind would fetch every Linear issue twice per run.

use std::collections::{HashMap, HashSet};

use tracing::info;

use crate::collect::collector::CollectionStats;
use crate::collect::pm_adapter::{build_adapters, PmAdapter, PmSource, PmTicket};
use crate::core::config::Config;
use crate::core::db::{Database, WorkItemRow};

/// Whether reference-driven fetching is enabled for `source`.
///
/// Why: a `tga collect` run must not start calling a provider's issue API
/// because that provider happens to be configured for something else. GitHub is
/// configured in nearly every config for pull requests alone, so the opt-in is
/// what keeps this pull off by default. Linear returns `false` unconditionally:
/// [`crate::collect::linear_pipeline`] owns its rows and running both would
/// write them twice.
/// What: reads the per-provider `fetch_on_reference` flag; `false` when the
/// provider is unconfigured.
/// Test: `linear_is_never_fetched_here`, `providers_are_opt_in`.
fn fetch_on_reference(config: &Config, source: PmSource) -> bool {
    match source {
        PmSource::Jira => config.jira.as_ref().is_some_and(|c| c.fetch_on_reference),
        PmSource::GitHub => config.github.as_ref().is_some_and(|c| c.fetch_on_reference),
        PmSource::AzureDevOps => config
            .azure_devops_config()
            .is_some_and(|c| c.fetch_on_reference),
        PmSource::Linear => false,
    }
}

/// Fetch and persist the work items every configured PM system's references
/// point at.
///
/// Why: this is the single production caller of [`build_adapters`] (#5219).
/// Before it existed, `github.ticket_regex` and `jira.ticket_regex` were
/// validated at config load and then read by nothing, so setting either had no
/// effect on any `tga collect` output.
/// What: for each adapter whose provider opted in via
/// [`fetch_on_reference`], runs the adapter's own
/// [`PmAdapter::detect_ticket_refs`] over every commit message, batch-fetches
/// the unique references through [`PmAdapter::fetch_tickets`], and writes the
/// results with [`persist_tickets`]. Nothing here aborts the run; every failure
/// is recorded on `stats` and the next provider still runs.
///
/// Fault severity follows #5727: a provider that fetched nothing while
/// reporting at least one error has lost its whole corpus and is recorded with
/// [`CollectionStats::fail_stage`], which makes `tga collect` exit non-zero.
/// A provider that returned some tickets and errored on others dropped
/// individual records, so those are [`CollectionStats::skip_item`]. A reference
/// the provider authoritatively does not have comes back `Ok(None)` and is not
/// a fault at all.
/// Test: `a_total_fetch_failure_is_a_stage_failure`,
/// `a_partial_fetch_failure_skips_items`, `no_references_is_a_noop`.
pub(super) async fn fetch_and_persist_work_items(
    db: &mut Database,
    config: &Config,
    stats: &mut CollectionStats,
) {
    let adapters: Vec<Box<dyn PmAdapter>> = build_adapters(config)
        .into_iter()
        .filter(|a| fetch_on_reference(config, a.source()))
        .collect();
    if adapters.is_empty() {
        return;
    }

    let commits = match read_commits(db) {
        Ok(c) => c,
        Err(e) => {
            stats.fail_stage(format!("work_items: read commits failed: {e}"));
            return;
        }
    };

    for adapter in &adapters {
        run_one_adapter(db, adapter.as_ref(), &commits, stats).await;
    }
}

/// Every `(sha, message)` pair in the `commits` table.
///
/// The SHA is what `commit_work_items` needs; reading messages alone would make
/// a per-commit link impossible even once the tickets were known.
fn read_commits(db: &Database) -> crate::core::Result<Vec<(String, String)>> {
    let conn = db.connection();
    let mut stmt = conn.prepare("SELECT sha, message FROM commits")?;
    let mapped = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}

/// Detect, fetch, and persist one adapter's share of the corpus.
///
/// Split from [`fetch_and_persist_work_items`] so the per-provider fault
/// accounting reads as one unit.
async fn run_one_adapter(
    db: &mut Database,
    adapter: &dyn PmAdapter,
    commits: &[(String, String)],
    stats: &mut CollectionStats,
) {
    let name = adapter.name().to_string();

    // 1. Ask the adapter which references its own syntax finds, per commit.
    let mut commit_refs: HashMap<String, Vec<String>> = HashMap::new();
    let mut unique: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (sha, message) in commits {
        let refs = adapter.detect_ticket_refs(message);
        if refs.is_empty() {
            continue;
        }
        for r in &refs {
            if seen.insert(r.clone()) {
                unique.push(r.clone());
            }
        }
        commit_refs.insert(sha.clone(), refs);
    }
    if unique.is_empty() {
        info!(provider = %name, "no work-item references in commit messages");
        return;
    }

    // 2. Batch-fetch. `fetch_tickets` is positional, so zipping the request
    //    list with the results is what maps a detected reference — whatever
    //    shape the provider's regex yielded — onto the ticket's canonical id.
    let id_refs: Vec<&str> = unique.iter().map(String::as_str).collect();
    let results = adapter.fetch_tickets(&id_refs).await;

    let mut tickets: Vec<PmTicket> = Vec::new();
    let mut canonical: HashMap<String, String> = HashMap::new();
    let mut first_error: Option<String> = None;
    let mut error_count = 0usize;
    for (requested, result) in unique.iter().zip(results) {
        match result {
            Ok(Some(ticket)) => {
                canonical.insert(requested.clone(), ticket.id.clone());
                tickets.push(ticket);
            }
            Ok(None) => {}
            Err(e) => {
                error_count += 1;
                if first_error.is_none() {
                    first_error = Some(e.to_string());
                }
            }
        }
    }

    // 3. Record what the fetch cost, at the severity its blast radius earns.
    if let Some(err) = first_error {
        let message = format!(
            "{name}: fetch work items failed for {error_count}/{} references: {err}",
            unique.len()
        );
        if tickets.is_empty() {
            stats.fail_stage(message);
        } else {
            stats.skip_item(message);
        }
    }
    if tickets.is_empty() {
        return;
    }

    // 4. Persist.
    let source = adapter.source().work_item_source();
    match persist_tickets(db, source, &tickets, &commit_refs, &canonical) {
        Ok(n) => info!(stored = n, provider = %name, source, "persisted work_items rows"),
        Err(e) => stats.fail_stage(format!("{name}: store work_items failed: {e}")),
    }
}

/// Project fetched tickets into `work_items` and link the commits that
/// reference them.
///
/// Why: #5219's decision is that four providers share one projection, so the
/// [`PmTicket`] → [`WorkItemRow`] mapping exists exactly once. Every consumer of
/// the source-agnostic corpus — [`crate::collect::correlate_commits`] and
/// DOC-70's board axis — reads whatever this function writes, for every
/// provider.
///
/// What: upserts one `work_items` row per ticket under `source`, keyed on
/// [`PmTicket::id`] — the adapter's canonical form, which is also what
/// [`crate::collect::ticket::extract_ticket_id`] produces, so correlation can
/// match a stored row against a commit's ticket key. Labels are joined into the
/// `tags` CSV the column expects, and an empty label set writes `NULL` rather
/// than an empty string. Then one `commit_work_items` row per
/// `(sha, reference)` pair whose reference actually resolved: `canonical` maps
/// the detected reference to the ticket id, so a provider whose regex yields a
/// bare `42` still links to the `AB#42` row it fetched. A reference that
/// resolved to nothing is skipped, because the join table's foreign key would
/// reject it and abort the transaction.
///
/// Both halves run in one transaction, so a failure leaves neither table
/// half-written and no dangling join row.
///
/// # Errors
///
/// Returns [`crate::core::TgaError::DbError`] if opening the transaction, any
/// upsert, any link insert, or the commit fails. The error is returned, never
/// downgraded to a warning or a zero count — the caller records it as a stage
/// failure.
///
/// Test: `persist_tickets_writes_rows_and_links`,
/// `persist_tickets_is_idempotent`, `persist_tickets_skips_unresolved_refs`,
/// `persist_tickets_maps_a_bare_ref_to_its_canonical_id`,
/// `persist_tickets_reports_write_failure`,
/// `persist_tickets_rolls_back_a_partial_write`, `persist_tickets_no_op`.
pub fn persist_tickets(
    db: &mut Database,
    source: &str,
    tickets: &[PmTicket],
    commit_refs: &HashMap<String, Vec<String>>,
    canonical: &HashMap<String, String>,
) -> crate::core::Result<usize> {
    use crate::core::db::work_items::{link_commit_work_item, upsert_work_item};

    if tickets.is_empty() {
        return Ok(0);
    }

    let tx = db.connection_mut().transaction()?;
    for ticket in tickets {
        upsert_work_item(&tx, &ticket_to_row(source, ticket))?;
    }

    let stored: HashSet<&str> = tickets.iter().map(|t| t.id.as_str()).collect();
    for (sha, refs) in commit_refs {
        for r in refs {
            if let Some(id) = canonical.get(r) {
                if stored.contains(id.as_str()) {
                    link_commit_work_item(&tx, sha, id, source)?;
                }
            }
        }
    }
    tx.commit()?;
    Ok(tickets.len())
}

/// The [`PmTicket`] → [`WorkItemRow`] mapping, shared by all four providers.
///
/// `raw_json` carries the adapter's preserved upstream payload, which is what
/// lets a consumer reach a field the normalized shape has no column for.
fn ticket_to_row(source: &str, ticket: &PmTicket) -> WorkItemRow {
    WorkItemRow {
        id: ticket.id.clone(),
        source: source.to_string(),
        title: ticket.title.clone(),
        status: ticket.status.clone(),
        item_type: ticket.ticket_type.clone(),
        tags: if ticket.labels.is_empty() {
            None
        } else {
            Some(ticket.labels.join(","))
        },
        project: ticket.project.clone(),
        url: ticket.url.clone(),
        raw_json: serde_json::to_string(&ticket.raw).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{
        AzureDevOpsConfig, GithubConfig, JiraConfig, LinearConfig, PmConfig,
    };

    fn ticket(id: &str) -> PmTicket {
        PmTicket {
            id: id.to_string(),
            title: format!("Title for {id}"),
            status: "Active".to_string(),
            ticket_type: "Bug".to_string(),
            labels: vec!["a".to_string(), "b".to_string()],
            url: Some(format!("https://example.test/{id}")),
            project: Some("P".to_string()),
            source: PmSource::AzureDevOps,
            raw: serde_json::json!({ "id": id }),
        }
    }

    fn row_count(db: &Database, source: &str) -> i64 {
        db.connection()
            .query_row(
                "SELECT COUNT(*) FROM work_items WHERE source = ?1",
                [source],
                |r| r.get(0),
            )
            .expect("count work_items")
    }

    fn insert_commit(db: &Database, sha: &str, message: &str) {
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                                      repository) \
                 VALUES (?1, 'A', 'a@x', '2026-01-01T00:00:00Z', ?2, 'repo')",
                rusqlite::params![sha, message],
            )
            .expect("insert commit");
    }

    /// An ADO config whose empty PAT makes every fetch fail in
    /// `validate_credentials`, before any network call.
    fn unauthenticated_azdo() -> AzureDevOpsConfig {
        AzureDevOpsConfig {
            organization_url: "https://dev.azure.com/myorg".into(),
            pat: String::new(),
            project: Some("P".into()),
            projects: vec![],
            ticket_regex: r"(?i)\bAB#(\d+)\b".into(),
            team_keys: vec![],
            fetch_on_reference: true,
            fetch_prs: false,
        }
    }

    #[test]
    fn persist_tickets_writes_rows_and_links() {
        let mut db = Database::open_in_memory().expect("db");
        let refs = HashMap::from([("sha1".to_string(), vec!["AB#1".to_string()])]);
        let canonical = HashMap::from([("AB#1".to_string(), "AB#1".to_string())]);

        let n = persist_tickets(&mut db, "azdo", &[ticket("AB#1")], &refs, &canonical)
            .expect("persist");
        assert_eq!(n, 1);
        assert_eq!(row_count(&db, "azdo"), 1);

        let (title, status, item_type, tags, project, url): (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = db
            .connection()
            .query_row(
                "SELECT title, status, item_type, tags, project, url FROM work_items \
                 WHERE id = ?1 AND source = 'azdo'",
                ["AB#1"],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .expect("query row");
        assert_eq!(title, "Title for AB#1");
        assert_eq!(status, "Active");
        assert_eq!(item_type, "Bug");
        assert_eq!(tags.as_deref(), Some("a,b"), "labels become the tags CSV");
        assert_eq!(project.as_deref(), Some("P"));
        assert_eq!(url.as_deref(), Some("https://example.test/AB#1"));

        let linked: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM commit_work_items \
                 WHERE work_item_id = 'AB#1' AND work_item_source = 'azdo'",
                [],
                |r| r.get(0),
            )
            .expect("count links");
        assert_eq!(linked, 1);
    }

    /// The reference a provider's regex yields need not be the ticket's
    /// canonical id: `pm.azure_devops.ticket_regex` captures a bare `42`, while
    /// the row is keyed `AB#42`. The positional zip is what joins the two, and
    /// without it no ADO commit would ever be linked.
    #[test]
    fn persist_tickets_maps_a_bare_ref_to_its_canonical_id() {
        let mut db = Database::open_in_memory().expect("db");
        let refs = HashMap::from([("sha1".to_string(), vec!["42".to_string()])]);
        let canonical = HashMap::from([("42".to_string(), "AB#42".to_string())]);

        persist_tickets(&mut db, "azdo", &[ticket("AB#42")], &refs, &canonical).expect("persist");

        let linked: String = db
            .connection()
            .query_row(
                "SELECT work_item_id FROM commit_work_items WHERE commit_sha = 'sha1'",
                [],
                |r| r.get(0),
            )
            .expect("query link");
        assert_eq!(linked, "AB#42");
    }

    /// A reference that resolved to nothing must not reach the join table — its
    /// foreign key would reject it and abort the whole transaction.
    #[test]
    fn persist_tickets_skips_unresolved_refs() {
        let mut db = Database::open_in_memory().expect("db");
        let refs = HashMap::from([(
            "sha1".to_string(),
            vec!["AB#1".to_string(), "AB#9".to_string()],
        )]);
        let canonical = HashMap::from([("AB#1".to_string(), "AB#1".to_string())]);

        persist_tickets(&mut db, "azdo", &[ticket("AB#1")], &refs, &canonical).expect("persist");

        let links: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM commit_work_items", [], |r| r.get(0))
            .expect("count links");
        assert_eq!(links, 1, "only the resolved reference is linked");
    }

    #[test]
    fn persist_tickets_is_idempotent() {
        let mut db = Database::open_in_memory().expect("db");
        let refs = HashMap::from([("sha1".to_string(), vec!["AB#1".to_string()])]);
        let canonical = HashMap::from([("AB#1".to_string(), "AB#1".to_string())]);

        persist_tickets(&mut db, "azdo", &[ticket("AB#1")], &refs, &canonical).expect("first");
        let mut updated = ticket("AB#1");
        updated.status = "Closed".to_string();
        persist_tickets(&mut db, "azdo", &[updated], &refs, &canonical).expect("second");

        assert_eq!(row_count(&db, "azdo"), 1);
        let status: String = db
            .connection()
            .query_row(
                "SELECT status FROM work_items WHERE id = 'AB#1' AND source = 'azdo'",
                [],
                |r| r.get(0),
            )
            .expect("query status");
        assert_eq!(
            status, "Closed",
            "re-running refreshes rather than duplicates"
        );

        let links: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM commit_work_items", [], |r| r.get(0))
            .expect("count links");
        assert_eq!(links, 1);
    }

    /// The fail-open arm: a write failure must surface as `Err`, never as a
    /// silent `Ok(0)` that lets the run report success with an empty corpus.
    #[test]
    fn persist_tickets_reports_write_failure() {
        let mut db = Database::open_in_memory().expect("db");
        db.connection()
            .execute("DROP TABLE work_items", [])
            .expect("drop table");

        let err = persist_tickets(
            &mut db,
            "azdo",
            &[ticket("AB#1")],
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect_err("write against a missing table must fail");
        assert!(
            err.to_string().contains("work_items"),
            "error names the failing table: {err}"
        );
    }

    /// The interleaving the transaction exists for: the upsert succeeds, then
    /// the link insert fails. Dropping only `commit_work_items` reaches the link
    /// phase with a `work_items` row already written inside the open
    /// transaction, so the assertion is about rollback.
    #[test]
    fn persist_tickets_rolls_back_a_partial_write() {
        let mut db = Database::open_in_memory().expect("db");
        db.connection()
            .execute("DROP TABLE commit_work_items", [])
            .expect("drop join table");
        let refs = HashMap::from([("sha1".to_string(), vec!["AB#1".to_string()])]);
        let canonical = HashMap::from([("AB#1".to_string(), "AB#1".to_string())]);

        let err = persist_tickets(&mut db, "azdo", &[ticket("AB#1")], &refs, &canonical)
            .expect_err("link against a missing table must fail");
        assert!(
            err.to_string().contains("commit_work_items"),
            "error names the failing table: {err}"
        );
        assert_eq!(
            row_count(&db, "azdo"),
            0,
            "the upsert that already succeeded must roll back with the transaction"
        );
    }

    #[test]
    fn persist_tickets_no_op() {
        let mut db = Database::open_in_memory().expect("db");
        assert_eq!(
            persist_tickets(&mut db, "azdo", &[], &HashMap::new(), &HashMap::new())
                .expect("persist"),
            0
        );
        assert_eq!(row_count(&db, "azdo"), 0);
    }

    /// Linear keeps `linear_pipeline`; routing it here as well would write
    /// every row twice.
    #[test]
    fn linear_is_never_fetched_here() {
        let config = Config {
            linear: Some(LinearConfig {
                api_key: Some("k".into()),
                fetch_on_reference: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(!fetch_on_reference(&config, PmSource::Linear));
    }

    /// Configuring a provider is not consent to call its issue API — GitHub is
    /// configured for pull requests in nearly every config.
    #[test]
    fn providers_are_opt_in() {
        let off = Config {
            github: Some(GithubConfig::default()),
            jira: Some(JiraConfig::default()),
            pm: Some(PmConfig {
                azure_devops: Some(AzureDevOpsConfig {
                    fetch_on_reference: false,
                    ..unauthenticated_azdo()
                }),
            }),
            ..Default::default()
        };
        assert!(!fetch_on_reference(&off, PmSource::GitHub));
        assert!(!fetch_on_reference(&off, PmSource::Jira));
        assert!(!fetch_on_reference(&off, PmSource::AzureDevOps));

        let on = Config {
            github: Some(GithubConfig {
                fetch_on_reference: true,
                ..Default::default()
            }),
            jira: Some(JiraConfig {
                fetch_on_reference: true,
                ..Default::default()
            }),
            pm: Some(PmConfig {
                azure_devops: Some(unauthenticated_azdo()),
            }),
            ..Default::default()
        };
        assert!(fetch_on_reference(&on, PmSource::GitHub));
        assert!(fetch_on_reference(&on, PmSource::Jira));
        assert!(fetch_on_reference(&on, PmSource::AzureDevOps));
    }

    /// #5219: the ADO fetch failure that used to be swallowed reaches the exit
    /// code through the shared path too. Every reference failed, so the
    /// provider's whole corpus is absent.
    #[tokio::test]
    async fn a_total_fetch_failure_is_a_stage_failure() {
        let mut db = Database::open_in_memory().expect("db");
        insert_commit(&db, "sha1", "fix: AB#42 broken thing");
        let config = Config {
            pm: Some(PmConfig {
                azure_devops: Some(unauthenticated_azdo()),
            }),
            ..Default::default()
        };
        let mut stats = CollectionStats::default();

        fetch_and_persist_work_items(&mut db, &config, &mut stats).await;

        assert_eq!(
            stats.stage_failures().len(),
            1,
            "a provider that fetched nothing must reach the exit code: {:?}",
            stats.errors
        );
        assert_eq!(row_count(&db, "azdo"), 0);
    }

    /// No adapter is enabled, so nothing is read and nothing is recorded.
    #[tokio::test]
    async fn no_enabled_provider_is_a_noop() {
        let mut db = Database::open_in_memory().expect("db");
        insert_commit(&db, "sha1", "fix: AB#42 broken thing");
        let mut stats = CollectionStats::default();

        fetch_and_persist_work_items(&mut db, &Config::default(), &mut stats).await;

        assert!(stats.errors.is_empty(), "faults: {:?}", stats.errors);
    }

    /// #5219's first closure condition: GitHub Issues writes real rows into
    /// `work_items` with `source = 'github'`. Detection, fetch and persistence
    /// all run for real; only the HTTP endpoint is a mock.
    #[tokio::test]
    async fn github_issues_reach_work_items() {
        use crate::collect::pm_adapter::GitHubAdapter;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "title": "Crash on startup",
                "state": "open",
                "html_url": "https://github.com/o/r/issues/42",
                "labels": [{ "name": "bug" }],
                "body": "steps to reproduce",
            })))
            .mount(&server)
            .await;

        let gh_cfg = GithubConfig {
            repo: Some("o/r".into()),
            fetch_on_reference: true,
            ..Default::default()
        };
        let client = crate::collect::github::GitHubClient::new(&gh_cfg)
            .expect("client")
            .with_api_base(server.uri());
        let adapter = GitHubAdapter::with_ticket_regex(client, gh_cfg.ticket_regex.as_deref());

        let mut db = Database::open_in_memory().expect("db");
        let commits = vec![("sha1".to_string(), "fix: crash, closes #42".to_string())];
        let mut stats = CollectionStats::default();
        run_one_adapter(&mut db, &adapter, &commits, &mut stats).await;

        assert!(stats.errors.is_empty(), "faults: {:?}", stats.errors);
        assert_eq!(row_count(&db, "github"), 1);
        let (id, title, status, tags): (String, String, String, Option<String>) = db
            .connection()
            .query_row(
                "SELECT id, title, status, tags FROM work_items WHERE source = 'github'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("query row");
        assert_eq!(id, "#42", "the form `extract_ticket_id` produces");
        assert_eq!(title, "Crash on startup");
        assert_eq!(status, "open");
        assert_eq!(tags.as_deref(), Some("bug"));

        let linked: String = db
            .connection()
            .query_row(
                "SELECT commit_sha FROM commit_work_items WHERE work_item_source = 'github'",
                [],
                |r| r.get(0),
            )
            .expect("query link");
        assert_eq!(linked, "sha1");
    }

    /// #5219's second closure condition: JIRA writes real rows into
    /// `work_items` with `source = 'jira'`.
    #[tokio::test]
    async fn jira_issues_reach_work_items() {
        use crate::collect::pm_adapter::JiraAdapter;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/field"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "key": "PROJ-7",
                "fields": {
                    "summary": "Rework the importer",
                    "status": { "name": "In Progress" },
                    "issuetype": { "name": "Story" },
                },
            })))
            .mount(&server)
            .await;

        let jira_cfg = JiraConfig {
            url: Some(server.uri()),
            username: Some("u".into()),
            token: Some("t".into()),
            fetch_on_reference: true,
            ..Default::default()
        };
        let client = crate::collect::jira::JiraClient::new(&jira_cfg).expect("client");
        let adapter = JiraAdapter::with_ticket_regex(client, jira_cfg.ticket_regex.as_deref());

        let mut db = Database::open_in_memory().expect("db");
        let commits = vec![("sha1".to_string(), "PROJ-7: rework importer".to_string())];
        let mut stats = CollectionStats::default();
        run_one_adapter(&mut db, &adapter, &commits, &mut stats).await;

        assert!(stats.errors.is_empty(), "faults: {:?}", stats.errors);
        assert_eq!(row_count(&db, "jira"), 1);
        let (id, title, status, item_type): (String, String, String, String) = db
            .connection()
            .query_row(
                "SELECT id, title, status, item_type FROM work_items WHERE source = 'jira'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("query row");
        assert_eq!(id, "PROJ-7");
        assert_eq!(title, "Rework the importer");
        assert_eq!(status, "In Progress");
        assert_eq!(item_type, "Story");
    }

    /// The other half of the severity split: one reference failed and one
    /// resolved, so the corpus is present but incomplete. That is an item skip,
    /// which #5655 keeps out of the exit code, and the row that did resolve is
    /// still written.
    #[tokio::test]
    async fn a_partial_fetch_failure_skips_items() {
        use crate::collect::pm_adapter::GitHubAdapter;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 1,
                "title": "Fine",
                "state": "open",
                "html_url": "https://github.com/o/r/issues/1",
                "labels": [],
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/issues/2"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let gh_cfg = GithubConfig {
            repo: Some("o/r".into()),
            fetch_on_reference: true,
            ..Default::default()
        };
        let client = crate::collect::github::GitHubClient::new(&gh_cfg)
            .expect("client")
            .with_api_base(server.uri());
        let adapter = GitHubAdapter::new(client);

        let mut db = Database::open_in_memory().expect("db");
        let commits = vec![("sha1".to_string(), "touches #1 and #2".to_string())];
        let mut stats = CollectionStats::default();
        run_one_adapter(&mut db, &adapter, &commits, &mut stats).await;

        assert_eq!(stats.errors.len(), 1, "faults: {:?}", stats.errors);
        assert!(
            stats.stage_failures().is_empty(),
            "a partial fetch keeps the exit code at zero: {:?}",
            stats.errors
        );
        assert_eq!(
            row_count(&db, "github"),
            1,
            "the issue that resolved is written"
        );
    }

    /// A commit corpus with no reference in this provider's syntax never
    /// reaches the network, so a broken credential is not reported.
    #[tokio::test]
    async fn no_references_is_a_noop() {
        let mut db = Database::open_in_memory().expect("db");
        insert_commit(&db, "sha1", "chore: nothing to see");
        let config = Config {
            pm: Some(PmConfig {
                azure_devops: Some(unauthenticated_azdo()),
            }),
            ..Default::default()
        };
        let mut stats = CollectionStats::default();

        fetch_and_persist_work_items(&mut db, &config, &mut stats).await;

        assert!(stats.errors.is_empty(), "faults: {:?}", stats.errors);
    }
}
