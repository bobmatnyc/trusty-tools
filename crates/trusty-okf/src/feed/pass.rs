//! One feed pass (SPEC-OKF-01 §5.4).
//!
//! Why/What: see the parent module. Every arm that is not a confirmation
//! leaves the journal unchanged; a journal append that fails after a
//! confirmed put stops the pass with `JournalWrite` (F3).
//! Test: `crates/trusty-okf/tests/feed_criteria.rs`.

use serde_json::json;

use super::probe::{self, Sent};
use super::wire::{self, FILE_PUT, FILE_REMOVE, FILES_REMOVE, IndexStatus, PutVerdict};
use super::{FeedError, FeedReport, FeedStatus, IndexFeed};
use crate::OkfError;
use crate::bundle::Bundle;
use crate::bundle::paths::confined_file;
use crate::format::concept::parse_concept;
use crate::format::fields::Field;
use crate::format::hash::bytes_hash;
use crate::format::id::ConceptId;
use crate::state::IndexState;
use crate::state::backlog::{IndexTask, TaskOp, backlog_from};
use crate::state::index::{
    IndexIdentity, IndexJournal, IndexRecord, IndexStateDir, LiveIdentity, fold,
};

/// Run one feed pass of `bundle` into `index_id` (SPEC-OKF-01 §4.3 `feed`).
///
/// Why: see the module doc of `feed`. What: preflight, scope check,
/// backlog, then each task in path order; the report's status follows
/// §5.4. `state` inside the bundle is `StateDirInBundle`; a root that does
/// not contain the bundle is `RootMismatch` before any push; a leak under
/// `_sources/` or `_state/` is `IndexScope` after removing it, with nothing
/// pushed. No error and no report arm reads Searchable without a passed
/// probe under a known, matching identity.
/// Test: `c43_daemon_down_is_not_fed_and_the_journal_is_unchanged`,
/// `c44_one_failed_put_of_three_is_partial`,
/// `c45_journal_append_failure_counts_nothing_and_the_next_pass_repushes`,
/// `c128_scope_leak_is_index_scope_and_pushes_nothing`,
/// `c134_identity_change_reads_pending_and_the_next_pass_repushes`.
pub async fn feed(
    bundle: &Bundle,
    state: &IndexStateDir,
    index_id: &str,
    sink: &dyn IndexFeed,
) -> Result<FeedReport, OkfError> {
    let journal = IndexJournal::open(state, bundle.root(), index_id)?;
    let records = journal.read()?;
    let status_reply = sink
        .call(wire::STATUS, json!({ "index_id": index_id }))
        .await;
    let reply = match status_reply {
        Ok(v) => v,
        Err(e) => {
            let down = LiveIdentity::Unreadable(e.to_string());
            let owed = backlog_from(bundle, &records, &down)?.owed();
            return Ok(FeedReport::not_fed(owed, e.to_string()));
        }
    };
    let status = wire::index_status(index_id, bundle.root(), &reply)?;
    if let Err(e) = scope_check(sink, index_id, &status).await? {
        let owed = backlog_from(bundle, &records, &status.live)?.owed();
        return Ok(FeedReport::not_fed(owed, e.to_string()));
    }
    let backlog = backlog_from(bundle, &records, &status.live)?;
    let mut run = Run {
        bundle,
        journal: &journal,
        index_id,
        sink,
        status: &status,
        failed: 0,
        probe_failed: 0,
        report: FeedReport {
            status: FeedStatus::NotFed,
            acknowledged: 0,
            searchable: 0,
            removed: 0,
            pending: 0,
            errors: Vec::new(),
        },
    };
    for u in &backlog.unreadable {
        run.fail(&u.path, format!("unreadable: {}", u.error));
    }
    for task in &backlog.tasks {
        let acked = fold(&records, &task.path).acked;
        run.task(task, acked).await?;
    }
    run.finish(state)
}

/// §5.3.1 rule 3. `Ok(Err(_))`: the scope could not be read, so nothing may
/// be pushed (fail closed, NotFed). `Err(IndexScope)`: a leak, removed.
async fn scope_check(
    sink: &dyn IndexFeed,
    index_id: &str,
    status: &IndexStatus,
) -> Result<Result<(), super::SearchError>, OkfError> {
    let mut leaked = std::collections::BTreeSet::new();
    let mut prefixes = Vec::new();
    for tree in ["_sources", "_state"] {
        let prefix = status.wire(tree);
        let chunks = match probe::chunks_under(sink, index_id, &prefix).await {
            Ok(c) => c,
            Err(e) => return Ok(Err(e)),
        };
        if !chunks.is_empty() {
            prefixes.push(prefix);
        }
        for c in chunks {
            leaked.insert(
                c.wire_path(&status.root)
                    .unwrap_or_else(|| tree.to_string()),
            );
        }
    }
    if leaked.is_empty() {
        return Ok(Ok(()));
    }
    for p in prefixes {
        // #10746: best effort; the refusal stands either way and the next
        // pass checks again (§5.3.1 rule 3).
        let _ = sink
            .call(
                FILES_REMOVE,
                json!({"index_id": index_id, "body": {"path_prefix": p}}),
            )
            .await;
    }
    Err(OkfError::IndexScope {
        index_id: index_id.to_string(),
        leaked: leaked.into_iter().map(Into::into).collect(),
    })
}

/// The state of one running pass.
struct Run<'a> {
    bundle: &'a Bundle,
    journal: &'a IndexJournal,
    index_id: &'a str,
    sink: &'a dyn IndexFeed,
    status: &'a IndexStatus,
    failed: usize,
    probe_failed: usize,
    report: FeedReport,
}

impl Run<'_> {
    fn fail(&mut self, path: &str, message: String) {
        self.failed += 1;
        self.report.pending += 1;
        self.report.errors.push(FeedError {
            path: path.to_string(),
            message,
        });
    }

    fn probe_fail(&mut self, path: &str, message: String) {
        self.probe_failed += 1;
        self.report.errors.push(FeedError {
            path: path.to_string(),
            message,
        });
    }

    /// F3: a confirmed write whose record cannot be appended stops the pass.
    fn record(&self, r: IndexRecord) -> Result<(), OkfError> {
        self.journal.append(&[r]).map_err(|e| {
            let path = self.journal.path().to_path_buf();
            match e {
                OkfError::Io { source, .. } => OkfError::JournalWrite { path, source },
                other => OkfError::JournalWrite {
                    path,
                    source: std::io::Error::other(other.to_string()),
                },
            }
        })
    }

    /// The live identity as a record field.
    fn live(&self) -> Option<IndexIdentity> {
        match &self.status.live {
            LiveIdentity::Known(i) => Some(i.clone()),
            _ => None,
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
    ) -> Result<serde_json::Value, super::SearchError> {
        let wire = self.status.wire(path);
        self.sink
            .call(
                method,
                json!({"index_id": self.index_id, "body": {"path": wire}}),
            )
            .await
    }

    async fn task(
        &mut self,
        task: &IndexTask,
        acked: Option<(String, Option<IndexIdentity>)>,
    ) -> Result<(), OkfError> {
        match task.op {
            TaskOp::Push { replace } => self.push(&task.path, replace).await,
            TaskOp::Probe => {
                let identity = acked.and_then(|(_, i)| i);
                self.probe(&task.path, None, identity).await
            }
            TaskOp::Remove => self.remove(&task.path).await,
            TaskOp::Recheck => {
                let wire = self.status.wire(&task.path);
                match probe::withdrawal(self.sink, self.index_id, self.status, &wire).await {
                    Ok(true) => Ok(()),
                    // F9: the withdrawn path came back; withdraw it again.
                    Ok(false) => self.remove(&task.path).await,
                    Err(e) => {
                        self.probe_fail(&task.path, format!("recheck could not run: {e}"));
                        Ok(())
                    }
                }
            }
        }
    }

    /// Remove-then-put, reply check, record, probe (§5.4 "Per-task order").
    async fn push(&mut self, path: &str, replace: bool) -> Result<(), OkfError> {
        if replace && let Err(e) = self.call(FILE_REMOVE, path).await {
            self.fail(path, format!("remove of the old revision failed: {e}"));
            return Ok(());
        }
        let content = match read_text(self.bundle, path) {
            Ok(t) => t,
            Err(e) => {
                self.fail(path, format!("unreadable: {e}"));
                return Ok(());
            }
        };
        let hex = hex_of(&bytes_hash(content.as_bytes()));
        let reply = self
            .sink
            .call(
                FILE_PUT,
                json!({"index_id": self.index_id, "body": {"path": self.status.wire(path), "content": content}}),
            )
            .await;
        let verdict = match reply {
            Ok(v) => wire::classify_put(&v, &hex, self.status.d1()),
            Err(e) => PutVerdict::Refused(e.to_string()),
        };
        match verdict {
            PutVerdict::Refused(reason) => {
                self.fail(path, format!("put not confirmed: {reason}"));
                Ok(())
            }
            PutVerdict::Removed(identity) => {
                self.record(IndexRecord::RemoveAck {
                    path: path.to_string(),
                    identity: identity.or_else(|| self.live()),
                })?;
                self.report.removed += 1;
                Ok(())
            }
            PutVerdict::Indexed(identity) => {
                // #10746: the record names the bytes actually sent (§5.4).
                self.record(IndexRecord::PutAck {
                    path: path.to_string(),
                    bytes: bytes_hash(content.as_bytes()),
                    identity: identity.clone(),
                })?;
                self.report.acknowledged += 1;
                self.probe(path, Some(content), identity).await
            }
        }
    }

    /// Probe the acknowledged revision; record the result (§4.9).
    async fn probe(
        &mut self,
        path: &str,
        sent: Option<String>,
        identity: Option<IndexIdentity>,
    ) -> Result<(), OkfError> {
        let content = match sent.map_or_else(|| read_text(self.bundle, path), Ok) {
            Ok(t) => t,
            Err(e) => {
                self.probe_fail(path, format!("probe could not run: {e}"));
                return Ok(());
            }
        };
        let parsed = ConceptId::parse(path.trim_end_matches(".md"))
            .map_err(|e| e.to_string())
            .and_then(|id| parse_concept(id, &content).map_err(|e| e.to_string()));
        let c = match parsed {
            Ok((c, _)) => c,
            Err(e) => {
                self.probe_fail(path, format!("probe could not run: {e}"));
                return Ok(());
            }
        };
        let title = match &c.frontmatter().title {
            Field::Valid(t) => Some(t.as_str()),
            _ => None,
        };
        let bytes = bytes_hash(content.as_bytes());
        let wire = self.status.wire(path);
        let hex = hex_of(&bytes);
        let sent = Sent {
            wire: &wire,
            content: &content,
            body: &c.body,
            title,
            hex: &hex,
        };
        match probe::retrieval(self.sink, self.index_id, self.status, &sent).await {
            Ok(passed) => {
                self.record(IndexRecord::Probe {
                    path: path.to_string(),
                    bytes,
                    identity,
                    passed,
                })?;
                if passed {
                    self.report.searchable += 1;
                } else {
                    self.probe_fail(path, "probe failed: no evidence of these bytes".into());
                }
            }
            Err(e) => self.probe_fail(path, format!("probe could not run: {e}")),
        }
        Ok(())
    }

    /// Remove, record, withdrawal probe (§4.9 Withdrawn, F9).
    async fn remove(&mut self, path: &str) -> Result<(), OkfError> {
        if let Err(e) = self.call(FILE_REMOVE, path).await {
            self.fail(path, format!("remove not confirmed: {e}"));
            return Ok(());
        }
        let identity = self.live();
        self.record(IndexRecord::RemoveAck {
            path: path.to_string(),
            identity: identity.clone(),
        })?;
        self.report.removed += 1;
        let wire = self.status.wire(path);
        match probe::withdrawal(self.sink, self.index_id, self.status, &wire).await {
            Ok(passed) => {
                self.record(IndexRecord::WithdrawProbe {
                    path: path.to_string(),
                    identity,
                    passed,
                })?;
                if !passed {
                    self.probe_fail(path, "withdrawal probe found the path".into());
                }
            }
            Err(e) => self.probe_fail(path, format!("withdrawal probe could not run: {e}")),
        }
        Ok(())
    }

    /// The §5.4 status from this pass's counts and the checked states.
    fn finish(mut self, state: &IndexStateDir) -> Result<FeedReport, OkfError> {
        let r = &mut self.report;
        r.status = if self.failed > 0 {
            if r.acknowledged + r.removed == 0 {
                FeedStatus::NotFed
            } else {
                FeedStatus::Partial
            }
        } else if self.probe_failed > 0
            || !self.status.d1()
            || !all_settled(self.bundle, state, self.index_id, &self.status.live)?
        {
            FeedStatus::Unconfirmed
        } else {
            FeedStatus::Searchable
        };
        Ok(self.report)
    }
}

/// Whether every concept reads Searchable or Withdrawn under `live`.
fn all_settled(
    b: &Bundle,
    state: &IndexStateDir,
    index_id: &str,
    live: &LiveIdentity,
) -> Result<bool, OkfError> {
    let loaded = b.load_state(state, index_id)?;
    for id in b.concept_ids()? {
        match b.state_with(&id, &loaded, live) {
            Ok(r) if matches!(r.index, IndexState::Searchable | IndexState::Withdrawn) => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// The text of a concept file, confined to the bundle; not UTF-8 is an error.
fn read_text(b: &Bundle, path: &str) -> Result<String, String> {
    let file = confined_file(b.root(), path).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    String::from_utf8(bytes).map_err(|_| "concept file is not UTF-8".to_string())
}

/// The hex part of a `sha256:<hex>` hash, as the D1 wire carries it.
fn hex_of(h: &str) -> String {
    h.strip_prefix("sha256:").unwrap_or(h).to_string()
}
