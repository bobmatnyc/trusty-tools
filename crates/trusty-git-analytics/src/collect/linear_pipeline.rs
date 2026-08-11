//! Linear issue enrichment for the Stage 1 collection pipeline.
//!
//! Why: this was inline in [`crate::collect::collector::CollectionPipeline::run`],
//! which sits over the 500-SLOC production cap on a frozen ratchet budget.
//! Lifting it out mirrors the existing `github_pipeline` split and keeps
//! `collector.rs` shrinking rather than growing (#5197).
//! What: [`fetch_and_store_linear_issues`] — reads every commit message, asks
//! the Linear client which referenced issues exist, and persists them. Behavior
//! is unchanged from the inline version: every failure is non-fatal and lands
//! in `stats.errors`.
//! Test: covered by `linear`'s own client tests; the orchestration here is
//! exercised end-to-end by the gated integration tests.

use tracing::info;

use crate::collect::collector::CollectionStats;
use crate::collect::linear::LinearClient;
use crate::core::config::Config;
use crate::core::db::Database;

/// Fetch and persist the Linear issues referenced by collected commits.
///
/// Why: Linear identifiers appear in commit messages, and the issue's state and
/// team are what turn a bare `ENG-123` into something a report can group by.
/// What: no-ops unless `linear.fetch_on_reference` is set. Otherwise collects
/// every commit message, calls `fetch_referenced_issues`, and stores the result
/// into `linear_issues`. Every failure — client init, query, store — is pushed
/// into `stats.errors` and returns without aborting the surrounding run.
/// Test: exercised by the gated integration tests; the client half is unit-tested
/// in `crate::collect::linear`.
pub(super) async fn fetch_and_store_linear_issues(
    db: &mut Database,
    config: &Config,
    stats: &mut CollectionStats,
) {
    // Optional: Linear issue enrichment.
    if let Some(linear_cfg) = &config.linear {
        if linear_cfg.fetch_on_reference {
            match LinearClient::new(linear_cfg) {
                Ok(client) => {
                    // Collect commit messages from DB.
                    let messages: Vec<String> = {
                        let conn = db.connection();
                        let mut stmt = match conn.prepare("SELECT message FROM commits") {
                            Ok(s) => s,
                            Err(e) => {
                                stats
                                    .errors
                                    .push(format!("Linear: query commits failed: {e}"));
                                return;
                            }
                        };
                        let rows = match stmt.query_map([], |row| row.get::<_, String>(0)) {
                            Ok(r) => r,
                            Err(e) => {
                                stats
                                    .errors
                                    .push(format!("Linear: read commits failed: {e}"));
                                return;
                            }
                        };
                        let mut out = Vec::new();
                        for r in rows.flatten() {
                            out.push(r);
                        }
                        out
                    };

                    let msg_refs: Vec<&str> = messages.iter().map(String::as_str).collect();
                    let issues = client
                        .fetch_referenced_issues(&msg_refs, &linear_cfg.team_keys)
                        .await;
                    for issue in &issues {
                        info!(
                            id = %issue.identifier,
                            state = %issue.state,
                            team = %issue.team,
                            "Linear issue fetched"
                        );
                    }
                    match client.store_issues(db, &issues) {
                        Ok(n) => {
                            info!(stored = n, "persisted linear_issues rows");
                            stats.linear_issues_fetched += n;
                        }
                        Err(e) => {
                            stats
                                .errors
                                .push(format!("Linear: store issues failed: {e}"));
                        }
                    }
                }
                Err(e) => {
                    stats.errors.push(format!("Linear client init failed: {e}"));
                }
            }
        }
    }
}
