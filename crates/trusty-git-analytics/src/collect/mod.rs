//! Stage 1 of the pipeline: extract commit data from local git repositories
//! and correlate it with external systems (GitHub pull requests, JIRA
//! tickets, developer identity records). All output is persisted via
//! [`crate::core::db::Database`].
//!
//! ## Submodules
//!
//! - [`git`] — commit extraction via libgit2
//! - [`identity`] — author identity resolution (exact + fuzzy)
//! - [`github`] — GitHub REST client (PRs)
//! - [`jira`] — JIRA REST client (issues)
//! - [`linear`] — Linear GraphQL client (issues)
//! - [`azdo`] — Azure DevOps stub client (Phase 1: config + AB# detection)
//! - [`bitbucket`] — Bitbucket Cloud REST client (PRs)
//! - [`pr_provider`] — provider-agnostic PR fetch trait
//! - [`ticket`] — ticket-reference detection on commit messages
//! - [`collector`] — end-to-end pipeline orchestrator
//! - `notify` — routes the pipeline's operator-facing lines to stdout/stderr,
//!   or to the progress bus when a consumer owns the terminal (#5197)
//! - [`ai_markers`] — the agentic-marker set behind `agentic_mode` detection
//! - [`ai_marker_config`] — operator-supplied markers, read from a file (#5414)
//! - [`reclassify`] — re-runs detection over rows an older detector classified (#6748)
//! - [`errors`] — module-level error type ([`CollectError`])
//! - [`fault`] — severity-tagged non-fatal faults ([`CollectionFault`], #5655)
//! - [`work_item_pipeline`] — the provider-agnostic `work_items` pull that
//!   runs every [`PmAdapter`] over the commit corpus (#5219)

pub mod ai_attribution;
pub mod ai_marker_config;
pub mod ai_markers;
pub mod azdo;
pub mod bitbucket;
pub mod collector;
pub mod correlate;
pub mod env_expand;
pub mod errors;
pub mod fault;
pub mod git;
pub mod github;
mod github_pipeline;
pub mod identity;
pub mod jira;
pub mod linear;
mod linear_pipeline;
mod notify;
pub mod pm_adapter;
// #5734: the concurrent PR fetch drain, split out of `collector` (frozen SLOC).
mod pr_pipeline;
pub mod pr_provider;
// #6748: the detector-generation re-classification pass, run before the walk.
pub mod reclassify;
pub mod ticket;
pub mod weeks;
// #5219: the provider-agnostic `work_items` pull, and the only production
// caller of `build_adapters`.
pub mod work_item_pipeline;

pub use collector::{CollectionPipeline, CollectionStats};
pub use correlate::{correlate_commits, CorrelationOutcome};
pub use errors::{CollectError, Result};
pub use fault::{CollectionFault, FaultSeverity};
pub use pm_adapter::{
    build_adapters, AzureDevOpsAdapter, GitHubAdapter, JiraAdapter, LinearAdapter, PmAdapter,
    PmError, PmSource, PmTicket,
};
pub use pr_provider::PrProvider;
pub use reclassify::{reclassify_stale, ReclassifyStats};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{Config, RepositoryConfig};

    #[test]
    fn git_collector_rejects_missing_path() {
        let cfg = RepositoryConfig {
            path: "/definitely/does/not/exist/here".into(),
            ..Default::default()
        };
        let err = git::GitCollector::new(&cfg).expect_err("should fail");
        match err {
            CollectError::Config(msg) => assert!(msg.contains("does not exist"), "msg: {msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn git_collector_rejects_non_repo_path() {
        // /tmp exists but is not a git repo.
        let cfg = RepositoryConfig {
            path: std::env::temp_dir(),
            ..Default::default()
        };
        let err = git::GitCollector::new(&cfg).expect_err("should fail");
        assert!(matches!(err, CollectError::Git(_)));
    }

    #[test]
    fn pipeline_constructs_with_default_config() {
        let cfg = Config::default();
        let _pipeline = CollectionPipeline::new(cfg);
    }

    /// #6748: a database carried across a detector change repairs itself at
    /// the head of the collect, and the operator is told the count.
    ///
    /// Why: the repair rewrites historical rows, so a downstream figure moves
    /// with no command having asked for it. A silent rewrite is
    /// indistinguishable from a data bug. The line goes to the pipeline's
    /// warning channel — stderr on a plain CLI run, the bus under a TUI —
    /// never stdout, which carries the pipeline's own report.
    /// What: seeds one commit with a Claude trailer stored the way a
    /// pre-#6748 collector stored it, runs a pipeline over no repositories,
    /// and asserts the repaired verdict plus the notice.
    /// Test: this test itself.
    #[test]
    fn a_stale_commit_is_reclassified_before_the_walk() {
        let bus = crate::core::progress::ProgressBus::bounded(16);
        let mut db = crate::core::db::Database::open_in_memory().expect("open");
        // No `ai_detector_version` in the column list on purpose: this is the
        // INSERT a pre-#6748 collector wrote, and the migration's DEFAULT of 0
        // is what makes the row stale. The test therefore also compiles on a
        // tree without the column, where it fails.
        db.connection()
            .execute(
                "INSERT INTO commits \
                 (sha, author_name, author_email, timestamp, message, repository, \
                  is_ai_assisted, ai_tool, agentic_mode) \
                 VALUES ('stale', 'Ada', 'ada@example.com', '2026-01-01T00:00:00Z', \
                         ?1, 'testrepo', 0, NULL, 'none')",
                rusqlite::params!["feat: x\n\nCo-Authored-By: Claude <noreply@anthropic.com>"],
            )
            .expect("seed a pre-#6748 row");

        let pipeline = CollectionPipeline::new(Config::default()).with_progress(bus.clone());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(pipeline.run(&mut db)).expect("run");

        let (is_ai, mode): (i64, String) = db
            .connection()
            .query_row(
                "SELECT is_ai_assisted, agentic_mode FROM commits WHERE sha = 'stale'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("read repaired row");
        assert_eq!((is_ai, mode.as_str()), (1, "full_agentic"));

        let events = bus.drain();
        assert!(
            events.iter().any(|e| e.detail.as_deref().is_some_and(|d| d
                == "Re-classified 1 commit(s) stored by an older AI \
                                       detector; 1 verdict(s) changed.")),
            "the operator is told the count on the warning channel: {events:?}"
        );
    }

    /// #5197: the bus is opt-in, so an untouched pipeline publishes nothing.
    #[test]
    fn progress_is_disabled_by_default() {
        let observer = crate::core::progress::ProgressBus::bounded(8);
        let pipeline = CollectionPipeline::new(Config::default());
        let mut db = crate::core::db::Database::open_in_memory().expect("open");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(pipeline.run(&mut db)).expect("run");
        assert!(
            observer.drain().is_empty(),
            "a pipeline with no bus attached must publish nothing"
        );
    }

    /// #5197: every configured repository reaches a terminal progress event,
    /// even one that cannot be opened — otherwise its row spins forever.
    #[test]
    fn run_emits_a_terminal_event_per_repo() {
        let bus = crate::core::progress::ProgressBus::bounded(64);
        let cfg = Config {
            repositories: vec![RepositoryConfig {
                path: std::env::temp_dir().join("tga-5197-not-a-repo"),
                name: Some("ghost".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let pipeline = CollectionPipeline::new(cfg).with_progress(bus.clone());
        let mut db = crate::core::db::Database::open_in_memory().expect("open");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(pipeline.run(&mut db)).expect("run");

        let events = bus.drain();
        assert!(!events.is_empty());
        let terminal: Vec<_> = events
            .iter()
            .filter(|e| e.target == "ghost" && e.is_terminal())
            .collect();
        assert_eq!(terminal.len(), 1, "exactly one terminal event: {events:?}");
        assert!(
            matches!(
                terminal[0].outcome,
                Some(crate::core::progress::Outcome::Failed { .. })
            ),
            "an unopenable repo reports failed, not completed"
        );
    }

    /// Build `n` empty git repositories under a fresh temp directory.
    ///
    /// An initialised repo with no commits OPENS fine — so it gets past
    /// `GitCollector::new` — but its revwalk has no ref to seed from, so every
    /// `collect_window` call fails. That is the shape of the real failure the
    /// #5197 review found: a per-week error inside the walk, not a repo that
    /// could never be opened.
    fn empty_repos(tag: &str, n: usize) -> (std::path::PathBuf, Vec<RepositoryConfig>) {
        let root = std::env::temp_dir().join(format!("tga-5197-{tag}-{}", std::process::id()));
        let mut repos = Vec::new();
        for i in 0..n {
            let path = root.join(format!("repo{i}"));
            std::fs::create_dir_all(&path).expect("mkdir");
            git2::Repository::init(&path).expect("git init");
            repos.push(RepositoryConfig {
                path,
                name: Some(format!("repo{i}")),
                // A closed one-week window keeps the walk deterministic: one
                // week, one failure, regardless of today's date.
                since_date: Some("2026-01-05".into()),
                until_date: Some("2026-01-11".into()),
                ..Default::default()
            });
        }
        (root, repos)
    }

    fn run_pipeline(repos: Vec<RepositoryConfig>, bus: &crate::core::progress::ProgressBus) {
        let cfg = Config {
            repositories: repos,
            ..Default::default()
        };
        let pipeline = CollectionPipeline::new(cfg).with_progress(bus.clone());
        let mut db = crate::core::db::Database::open_in_memory().expect("open");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(pipeline.run(&mut db)).expect("run");
    }

    /// #5197 finding 1: a repo whose weeks failed used to report
    /// `Outcome::Completed` — the walk's errors reached `stats.errors` and the
    /// next line emitted success unconditionally.
    #[test]
    fn run_reports_failed_when_a_week_fails() {
        let bus = crate::core::progress::ProgressBus::bounded(64);
        let (root, repos) = empty_repos("week-fail", 1);
        run_pipeline(repos, &bus);
        let _ = std::fs::remove_dir_all(&root);

        let events = bus.drain();
        let terminal: Vec<_> = events
            .iter()
            .filter(|e| e.target == "repo0" && e.is_terminal())
            .collect();
        assert_eq!(terminal.len(), 1, "exactly one terminal event: {events:?}");
        match &terminal[0].outcome {
            Some(crate::core::progress::Outcome::Failed { reason }) => {
                assert!(
                    reason.contains("error(s)"),
                    "reason names the count: {reason}"
                );
            }
            other => panic!("a repo whose walk failed must report Failed, got {other:?}"),
        }
    }

    /// #5197: the pipeline's operator-facing lines must not reach the terminal
    /// while a consumer owns it.
    ///
    /// The unbounded-window branch is the cheapest one to reach that prints:
    /// with no `since_date` it warns about a full-history walk. With a bus
    /// attached that text has to arrive as a `Collect` detail event — if it
    /// still went to `eprintln!` it would land inside the TUI's drawn frame,
    /// where ratatui never repaints it.
    #[test]
    fn operator_lines_reach_the_bus_instead_of_the_terminal() {
        let bus = crate::core::progress::ProgressBus::bounded(64);
        let root = std::env::temp_dir().join(format!("tga-5197-notify-{}", std::process::id()));
        let path = root.join("repo0");
        std::fs::create_dir_all(&path).expect("mkdir");
        git2::Repository::init(&path).expect("git init");
        // No since_date / until_date: takes the full-history branch, which warns.
        run_pipeline(
            vec![RepositoryConfig {
                path,
                name: Some("repo0".into()),
                ..Default::default()
            }],
            &bus,
        );
        let _ = std::fs::remove_dir_all(&root);

        let events = bus.drain();
        assert!(
            events.iter().any(|e| e
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("collecting FULL git history"))),
            "the full-history warning must arrive on the bus, not on stderr: {events:?}"
        );
    }

    /// #5197 finding 4: the pre-walk emit tagged position-among-repositories
    /// onto the repo's own row, so the second of two repos rendered "1/2" —
    /// 50% through itself — before its walk had done anything. Nothing emits
    /// intra-repo progress, so no non-terminal event may claim a fraction.
    #[test]
    fn no_repo_row_claims_a_fraction_it_cannot_substantiate() {
        let bus = crate::core::progress::ProgressBus::bounded(64);
        let (root, repos) = empty_repos("fraction", 2);
        run_pipeline(repos, &bus);
        let _ = std::fs::remove_dir_all(&root);

        let events = bus.drain();
        let repo_rows: Vec<_> = events
            .iter()
            .filter(|e| e.target.starts_with("repo") && !e.is_terminal())
            .collect();
        assert_eq!(
            repo_rows.len(),
            2,
            "one in-flight event per repo: {events:?}"
        );
        for e in repo_rows {
            assert_eq!(e.total, None, "no total is known mid-walk: {e:?}");
            assert_eq!(e.done, 0, "no intra-repo progress is ever emitted: {e:?}");
        }
    }
}
