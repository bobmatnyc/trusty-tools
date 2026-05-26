//! `tga deployments collect` — ingest deployment events into the canonical
//! `fact_deployments` table (issues #207, #212).
//!
//! Supported sources (configured via `dora.deployment_source`):
//!
//! * `git_tags` — walk every tag in every configured repository, match
//!   against `dora.deployment_tag_pattern`, and emit one row per match.
//!   This is the default because it works without external credentials.
//! * `github_releases`, `github_actions` — placeholders; see the source
//!   for what's needed to enable them. The schema is the same.
//! * `manual` — no-op (operator is expected to INSERT directly).

use chrono::{DateTime, TimeZone, Utc};
use clap::Args;
use regex::Regex;
use rusqlite::params;
use tracing::{info, warn};

use tga::core::config::{Config, DoraConfig, RepositoryConfig};
use tga::core::db::Database;

/// Arguments for `tga deployments collect`.
#[derive(Args, Debug)]
pub struct DeploymentsCollectArgs {
    /// Override the deployment source from the CLI (defaults to
    /// `dora.deployment_source` or `git_tags` if no DORA config is
    /// present).
    #[arg(long, value_name = "SOURCE")]
    pub source: Option<String>,
}

/// Per-run counters surfaced on the CLI output.
#[derive(Debug, Default, Clone)]
struct CollectStats {
    inspected_tags: usize,
    matched_tags: usize,
    inserted: usize,
    skipped: usize,
}

/// Dispatch entry point for `tga deployments collect`.
///
/// # Errors
///
/// Propagates git2 / SQL errors from the underlying ingestor.
pub fn run(config: Config, db: &mut Database, args: DeploymentsCollectArgs) -> anyhow::Result<()> {
    let dora = config.dora.clone().unwrap_or_default();
    let source = args
        .source
        .clone()
        .unwrap_or_else(|| dora.deployment_source.clone());

    let stats = match source.as_str() {
        "git_tags" => ingest_git_tags(db, &config.repositories, &dora)?,
        "github_releases" => {
            warn!(
                "deployment_source = 'github_releases' is not yet implemented. \
                 Falling back to git_tags so the canonical table still populates."
            );
            ingest_git_tags(db, &config.repositories, &dora)?
        }
        "github_actions" => {
            warn!(
                "deployment_source = 'github_actions' requires GitHub Actions \
                 ingestion (not yet implemented). Falling back to git_tags."
            );
            ingest_git_tags(db, &config.repositories, &dora)?
        }
        "manual" => {
            println!(
                "deployment_source = 'manual' — no-op. INSERT into \
                 fact_deployments directly."
            );
            CollectStats::default()
        }
        other => {
            anyhow::bail!(
                "unknown deployment_source '{other}'. Expected one of: \
                 git_tags, github_releases, github_actions, manual."
            );
        }
    };

    println!(
        "Inspected {} tag(s) across {} repo(s); {} matched the deployment pattern; \
         {} inserted into fact_deployments, {} skipped (already present).",
        stats.inspected_tags,
        config.repositories.len(),
        stats.matched_tags,
        stats.inserted,
        stats.skipped,
    );
    Ok(())
}

/// Walk every tag in every configured repository, match against the
/// configured deployment-tag pattern, and INSERT OR IGNORE one row per
/// match into `fact_deployments`.
///
/// Why: git tags are the lowest-common-denominator deployment signal
/// — any project that releases via `git tag vX.Y.Z` already has the
/// data on disk; no external API or token is required.
/// What: opens each repo via git2, iterates `repo.tag_names()`, peels
/// each tag to its commit, and emits a `fact_deployments` row with
/// `source = 'git_tag'`, `git_tag`, `git_sha`, and `triggered_at` set
/// to the tagger's commit time.
/// Test: covered by `ingest_git_tags_*` integration tests.
fn ingest_git_tags(
    db: &mut Database,
    repositories: &[RepositoryConfig],
    dora: &DoraConfig,
) -> anyhow::Result<CollectStats> {
    let mut stats = CollectStats::default();
    let pattern = Regex::new(&dora.deployment_tag_pattern).map_err(|e| {
        anyhow::anyhow!(
            "dora.deployment_tag_pattern is not a valid regex: {e} \
             (pattern: {pat:?})",
            pat = dora.deployment_tag_pattern
        )
    })?;

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO fact_deployments \
             (deploy_id, repo, environment, triggered_at, completed_at, \
              status, git_sha, git_tag, triggered_by_pr, source) \
             VALUES (?1, ?2, 'production', ?3, ?3, 'success', ?4, ?5, NULL, 'git_tag')",
        )?;
        for repo_cfg in repositories {
            let repo_name = repo_cfg.name.clone().unwrap_or_else(|| {
                repo_cfg
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("(unknown)")
                    .to_string()
            });
            let repo = match git2::Repository::open(&repo_cfg.path) {
                Ok(r) => r,
                Err(e) => {
                    warn!(repo = %repo_name, error = %e, "git open failed; skipping tags");
                    continue;
                }
            };
            let tags = match repo.tag_names(None) {
                Ok(t) => t,
                Err(e) => {
                    warn!(repo = %repo_name, error = %e, "tag_names failed; skipping");
                    continue;
                }
            };
            for tag in tags.iter().flatten() {
                stats.inspected_tags += 1;
                if !pattern.is_match(tag) {
                    continue;
                }
                stats.matched_tags += 1;
                // Peel tag -> commit. Some repos use annotated tags
                // (which wrap a tag object) and some use lightweight
                // tags (which are just a ref to a commit). `peel` resolves
                // both to the final commit.
                let refname = format!("refs/tags/{tag}");
                let obj = match repo.revparse_single(&refname) {
                    Ok(o) => o,
                    Err(e) => {
                        warn!(repo = %repo_name, tag = %tag, error = %e, "revparse failed");
                        continue;
                    }
                };
                let commit = match obj.peel_to_commit() {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(repo = %repo_name, tag = %tag, error = %e, "peel failed");
                        continue;
                    }
                };
                let sha = commit.id().to_string();
                let time = commit.time();
                let triggered_at: DateTime<Utc> = Utc
                    .timestamp_opt(time.seconds(), 0)
                    .single()
                    .unwrap_or_else(Utc::now);

                // deploy_id is "<repo>@<tag>" — stable across re-ingests
                // so INSERT OR IGNORE is idempotent.
                let deploy_id = format!("{repo_name}@{tag}");
                let changed = insert.execute(params![
                    deploy_id,
                    repo_name,
                    triggered_at.to_rfc3339(),
                    sha,
                    tag,
                ])?;
                if changed > 0 {
                    stats.inserted += 1;
                } else {
                    stats.skipped += 1;
                }
            }
        }
    }
    tx.commit()?;
    info!(
        inspected = stats.inspected_tags,
        matched = stats.matched_tags,
        inserted = stats.inserted,
        skipped = stats.skipped,
        "git-tag deployment ingestion complete"
    );
    Ok(stats)
}

#[cfg(test)]
mod tests {
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
}
