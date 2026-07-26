//! Pushing OKG ingest output into the store's bound search index (#3892,
//! remediation (a)).
//!
//! Why: an OKG store's `[[stores]]` binding reads as "one store, with a tree
//! facet and an index facet", but nothing kept the two describing the same
//! content — `okg_ingest_*` wrote markdown into the KB tree while
//! `vector_search` queried an index built over an unrelated root, so an ingest
//! could report "N entities ingested" truthfully and still leave the assistant
//! finding nothing, with no error on either side. This module is the half that
//! was missing: after an entity is durably written to the tree, its content is
//! pushed into the bound index, making the binding's semantics true.
//!
//! It lives in `trusty-agents`, not `trusty-kb`, for the same reason the
//! Gmail/Drive fetchers do: the push is a NETWORK call, and `trusty-kb` stays
//! deterministic and network-free. The reconcile that decides WHAT to push is
//! pure and therefore stays in the KB crate
//! ([`KbStore::okg_index_backlog`](trusty_kb::store::KbStore::okg_index_backlog));
//! this module owns only the seam ([`IndexFeed`]) and the drive loop, so every
//! ordering property is testable against a fake with no daemon running.
//!
//! ## The contract
//!
//! 1. **The tree write is the durable record.** The item ledger is never gated
//!    on a push. A search daemon that is down, slow, or 500ing cannot lose
//!    ingestion work or make it non-convergent (DOC-55 SPEC-OKGIMPORT-06
//!    §7.2.4).
//! 2. **But success is never claimed falsely.** Every run reports `indexed`,
//!    `removed`, and `pending` — the count of entities that are in the tree and
//!    NOT yet searchable — plus one error line per failed push.
//! 3. **Ordering: push, then record.** A journal line is appended only after
//!    the daemon acknowledged the write, so a crash in between costs one
//!    redundant re-push (the push is an upsert keyed by file path), never a
//!    permanently-unindexed entity. This mirrors the ingest engine's
//!    entity-before-ledger ordering.
//! 4. **Every run reconciles the whole backlog**, not just what it ingested.
//!    An entity the ingest engine skipped as unchanged, but which never reached
//!    the index, is pushed by the next run. That is what makes a failed push
//!    self-healing rather than permanent.
//! 5. **A changed entity is withdrawn before it is re-pushed.** trusty-search
//!    chunks by content, so a shorter revision would otherwise leave the old
//!    chunks searchable alongside the new ones.
//! 6. **A bound index that cannot serve the tree is refused, not fed.**
//!    trusty-search post-filters every result whose path escapes the index
//!    root (issues #64 / #541 — verified live: the push succeeds, the chunks
//!    exist, `search` returns nothing). So the feed asks the daemon for the
//!    index's root FIRST and, when it does not contain the tree, reports that
//!    with the remediation instead of manufacturing a fresh silent failure.
//!    This is why remediation (a) does not require re-pointing anyone's
//!    existing index root: a binding whose index cannot serve its tree is a
//!    configuration error, and it is now a LOUD one.
//!
//! Test: `super::index_feed::tests` — a fake feed covers push/remove ordering,
//! idempotency, failure retention and recovery; a mock HTTP server pins the
//! real daemon route shape.

use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use trusty_kb::okg::index_journal::{IndexJournal, IndexOp, IndexTask};
use trusty_kb::okg::registry::SourceSpec;
use trusty_kb::store::KbStore;

/// Per-push timeout.
///
/// Why: generous compared to [`super::status::PROBE_TIMEOUT`] because a push is
/// real work — trusty-search chunks, embeds, and commits the file inline — but
/// still bounded, so one wedged request cannot hang an ingest indefinitely. A
/// timeout is reported as a failed push and retried on the next run, which is
/// exactly the pending-item path.
pub const PUSH_TIMEOUT: Duration = Duration::from_secs(60);

/// The seam between "decide what to index" and "talk to trusty-search".
///
/// Why: the drive loop's ordering guarantees (record only after success, retry
/// on the next run, withdraw before replace) are the part that can silently
/// regress, so they must be testable without a daemon — the same reason the
/// ingest engine splits at `SourceItem`.
/// What: two operations, both keyed by the entity file's absolute path, which
/// is the identity trusty-search stores and the one `remove_file` withdraws by.
#[async_trait]
pub trait IndexFeed: Send + Sync {
    /// Upsert one file's content into `index`.
    async fn index_file(&self, index: &str, path: &str, content: &str) -> anyhow::Result<()>;
    /// Withdraw one file from `index`.
    async fn remove_file(&self, index: &str, path: &str) -> anyhow::Result<()>;
    /// The index's configured root directory, when the daemon reports one.
    ///
    /// Why: this is a PREFLIGHT, not a nicety — see [`covers`]. trusty-search
    /// post-filters every search result whose file path falls outside the
    /// index root, so pushing an entity into a foreign-rooted index stores it
    /// and then hides it from every query. Without this check the feed would
    /// report `indexed: 2, pending: 0` while `vector_search` still returned
    /// nothing: a NEW silent failure, in the shape of the one #3892 is about.
    /// What: `Ok(None)` means the daemon reported no root and the check is
    /// skipped (fail-open on an unknown); `Err` means the index could not be
    /// interrogated at all.
    async fn index_root(&self, index: &str) -> anyhow::Result<Option<std::path::PathBuf>>;
}

/// Whether entities under `tree` would survive `index`'s out-of-root filter.
///
/// Why: trusty-search's `file_is_within_root` post-filter (issues #64 / #541)
/// drops any result whose path escapes the index root — a defence against
/// stale, mis-registered indexes leaking cross-project results. It applies to
/// pushed content exactly as it does to walked content, so an OKG store's index
/// is only usable when its root CONTAINS the tree it is bound to.
/// What: prefix check, canonicalising both sides when they exist so a
/// `/tmp` vs `/private/tmp` or symlinked spelling still matches.
/// Test: `refuses_an_index_rooted_away_from_the_tree`.
fn covers(root: &std::path::Path, tree: &std::path::Path) -> bool {
    let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canon(tree).starts_with(canon(root))
}

/// The live feed: the trusty-search daemon's per-file HTTP surface.
///
/// Why/What: `POST /indexes/{id}/index-file` takes `{path, content}` and
/// `POST /indexes/{id}/remove-file` takes `{path}` — the HTTP equivalents of
/// the `index_file` / `remove_file` MCP tools. Content is supplied in the body,
/// so an entity does NOT need to live under the index's own root: this is what
/// lets remediation (a) leave the operator's configured index root untouched.
/// Test: `http_feed_calls_the_daemon_routes`.
pub struct HttpIndexFeed {
    base: String,
    client: reqwest::Client,
}

impl HttpIndexFeed {
    /// Build a feed against a trusty-search base URL (`http://127.0.0.1:7878`).
    pub fn new(base: impl Into<String>) -> anyhow::Result<Self> {
        Ok(Self {
            base: base.into().trim_end_matches('/').to_string(),
            client: reqwest::Client::builder().timeout(PUSH_TIMEOUT).build()?,
        })
    }

    /// POST one JSON body, mapping every non-2xx answer to an error.
    async fn post(&self, url: String, body: serde_json::Value) -> anyhow::Result<()> {
        let resp = self.client.post(&url).json(&body).send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        // The daemon's error bodies are short; including one turns "500" into a
        // diagnosable line in the tool result.
        let detail = resp.text().await.unwrap_or_default();
        let detail = detail.chars().take(200).collect::<String>();
        anyhow::bail!("trusty-search returned HTTP {status} for {url}: {detail}")
    }
}

#[async_trait]
impl IndexFeed for HttpIndexFeed {
    async fn index_file(&self, index: &str, path: &str, content: &str) -> anyhow::Result<()> {
        self.post(
            format!("{}/indexes/{index}/index-file", self.base),
            serde_json::json!({ "path": path, "content": content }),
        )
        .await
    }

    async fn remove_file(&self, index: &str, path: &str) -> anyhow::Result<()> {
        self.post(
            format!("{}/indexes/{index}/remove-file", self.base),
            serde_json::json!({ "path": path }),
        )
        .await
    }

    async fn index_root(&self, index: &str) -> anyhow::Result<Option<std::path::PathBuf>> {
        let url = format!("{}/indexes/{index}/status", self.base);
        let resp = self.client.get(&url).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("search index `{index}` is not registered on the trusty-search daemon");
        }
        if !resp.status().is_success() {
            anyhow::bail!(
                "trusty-search returned HTTP {} for index `{index}`",
                resp.status()
            );
        }
        let body: serde_json::Value = resp.json().await?;
        Ok(body
            .get("root_path")
            .and_then(serde_json::Value::as_str)
            .map(std::path::PathBuf::from))
    }
}

/// What one feed pass did — the caller-facing summary.
///
/// Why: this is the honesty half of the contract. `pending` is the number the
/// #3892 report was missing entirely: entities that ARE in the tree and are NOT
/// searchable. It is non-zero whenever the daemon is unreachable, a push failed,
/// or no index is bound at all, and it must be surfaced next to the ingest
/// counts rather than folded into them.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct IndexFeedReport {
    /// The index that was fed, when one resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    /// Entities pushed into the index this pass.
    pub indexed: usize,
    /// Entities withdrawn from the index this pass.
    pub removed: usize,
    /// Entities still owed to the index after this pass — INGESTED BUT NOT YET
    /// SEARCHABLE. A later run drains them.
    pub pending: usize,
    /// Entities the index already held at this exact content.
    pub synced: usize,
    /// Why nothing was fed, when `index` is `None` (no binding, daemon
    /// undiscoverable, tree/index mismatch). Never an ingest failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Per-entity failures. A failed push leaves its entity pending.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

impl IndexFeedReport {
    /// A pass that could not run, with the pending backlog it leaves behind.
    ///
    /// Why: "no index bound" and "daemon down" must still report HOW MUCH is
    /// unsearchable, or the caller learns nothing it did not already know.
    pub fn not_fed(reason: String, pending: usize, synced: usize) -> Self {
        Self {
            index: None,
            pending,
            synced,
            reason: Some(reason),
            ..Self::default()
        }
    }
}

/// Drain one source's index backlog into `index`.
///
/// Why/What/Test: see the module doc — this is the drive loop the contract
/// describes. Per item: withdraw-if-replacing, push, then record. A failure is
/// recorded as an error and the item stays pending; it never aborts the pass,
/// because one poisoned entity must not block the other 4,999.
pub async fn feed_source(
    store: &KbStore,
    spec: &SourceSpec,
    index: &str,
    feed: &dyn IndexFeed,
    now: &str,
) -> anyhow::Result<IndexFeedReport> {
    let backlog = store.okg_index_backlog(spec)?;
    let pending = backlog.tasks.len() + backlog.notes.len();

    // Preflight: an index whose root does not contain this tree would STORE
    // every push and then hide it from every query (see `covers`). Refusing
    // loudly is the only honest outcome — pushing anyway would manufacture a
    // fresh silent failure.
    match feed.index_root(index).await {
        Ok(Some(root)) if !covers(&root, &store.root) => {
            return Ok(IndexFeedReport::not_fed(
                format!(
                    "index `{index}` is rooted at {}, which does not contain this tree ({}). \
                     trusty-search drops any search result whose path falls outside the index \
                     root, so entities pushed there would be stored and never returned. Bind a \
                     search index registered over the tree itself.",
                    root.display(),
                    store.root.display()
                ),
                pending,
                backlog.synced,
            ));
        }
        Ok(_) => {}
        Err(e) => {
            return Ok(IndexFeedReport::not_fed(
                format!("index `{index}` could not be interrogated: {e}"),
                pending,
                backlog.synced,
            ));
        }
    }

    let mut journal = IndexJournal::load(&store.root, &spec.id)?;
    let mut report = IndexFeedReport {
        index: Some(index.to_string()),
        synced: backlog.synced,
        // An unresolvable row is unsearchable too, so it counts against
        // `pending` as well as being reported — the same honesty rule.
        pending: backlog.notes.len(),
        errors: backlog.notes,
        ..IndexFeedReport::default()
    };

    for task in &backlog.tasks {
        let path = task.path.to_string_lossy().to_string();
        match run_task(store, feed, index, task, &path, &mut journal, now).await {
            Ok(IndexOp::Remove) => report.removed += 1,
            Ok(IndexOp::Index { .. }) => report.indexed += 1,
            Err(e) => {
                // Left unrecorded on purpose: the entity stays in the backlog
                // and the next run retries it. This is the fail-open half of
                // the contract — loud, counted, never lost.
                report.pending += 1;
                report.errors.push(format!("{}: {e}", task.entity));
            }
        }
    }
    Ok(report)
}

/// Perform one task and record it. Any error leaves the journal untouched.
async fn run_task(
    store: &KbStore,
    feed: &dyn IndexFeed,
    index: &str,
    task: &IndexTask,
    path: &str,
    journal: &mut IndexJournal,
    now: &str,
) -> anyhow::Result<IndexOp> {
    match task.op {
        IndexOp::Remove => feed.remove_file(index, path).await?,
        IndexOp::Index { replace } => {
            // Withdraw the stale revision first — trusty-search keys chunks by
            // content, so a shorter revision would leave orphans searchable.
            if replace {
                feed.remove_file(index, path).await?;
            }
            let content = std::fs::read_to_string(&task.path)?;
            feed.index_file(index, path, &content).await?;
        }
    }
    store.okg_record_index(journal, task, now)?;
    Ok(task.op)
}

#[cfg(test)]
#[path = "index_feed_tests.rs"]
mod tests;
