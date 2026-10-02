//! A redb write transaction with a deadline (#8749).
//!
//! Why: redb serialises write transactions per database — `begin_write` parks
//! until the current writer commits or aborts. #6366 bounded the palace write
//! pipeline, but a timed-out pipeline hands its commit to a detached task, and
//! that task kept redb's write lock (and the palace commit-order guard) for as
//! long as its transaction ran. One stalled writer still blocked every other
//! writer on the file.
//! What: [`DeadlinedWrite`] wraps a `WriteTransaction` and starts a clock when
//! redb hands over the lock. Callers call [`DeadlinedWrite::check`] between
//! units of work; [`DeadlinedWrite::check_before_commit`] checks once more and,
//! past the deadline, aborts (rolls back) instead of returning the transaction
//! to commit. Either way the lock is released and the caller gets
//! [`WriteTxnError::DeadlineExceeded`], never a silent success. The abort is
//! logged at warn, naming the palace and how long the lock was held.
//!
//! Residual, accepted by #8749: a stall INSIDE one redb call — a single op, or
//! `commit()`'s own fsync — cannot be interrupted in-process. The deadline is
//! checked between calls, so a transaction holds the lock for at most its
//! budget plus one call.
//! Test: `write_deadline::tests`, `kg_redb::txn_deadline_tests`.

use redb::{Database, WriteTransaction};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A write transaction refused because it outran its deadline.
///
/// Why: the caller must be able to tell "rolled back because it was too slow —
/// nothing landed, retrying is safe" apart from a data error, and an
/// `anyhow`-typed caller must be able to `downcast_ref` it. `Clone` lets the KG
/// writer actor hand the same typed error to every op queued in one batch.
/// What: names the palace, the store file, where the check fired, the budget,
/// and how long the lock was held, plus the knob that moves the budget.
/// Test: `a_transaction_past_its_deadline_is_rolled_back_not_committed`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteTxnError {
    /// The transaction was aborted; none of its writes were committed.
    #[error(
        "palace '{palace}' {store} write transaction exceeded its {budget:?} deadline \
         after holding the write lock for {held:?} ({stage}); rolled back — nothing \
         from it was committed and the lock is released (#8749). Raise \
         TRUSTY_WRITE_TXN_DEADLINE_SECS if writes on this palace are legitimately \
         this slow"
    )]
    DeadlineExceeded {
        palace: Arc<str>,
        store: &'static str,
        stage: &'static str,
        budget: Duration,
        held: Duration,
    },
}

/// The palace id a store file belongs to, for the #8749 diagnostics.
///
/// Why: every palace store lives directly in `<data_root>/<palace_id>/`, so the
/// parent directory's name is the palace id, and a store needs no extra
/// plumbing to name its palace in a warn line.
/// What: the parent directory's final component, or the whole path when there
/// is none (an in-tree relative file).
/// Test: `the_palace_label_is_the_store_files_directory`.
pub fn palace_label(store_path: &Path) -> Arc<str> {
    store_path
        .parent()
        .and_then(Path::file_name)
        .map_or_else(
            || store_path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
        .into()
}

/// A redb write transaction that refuses to commit past its deadline.
///
/// Why/What: see the module doc. Derefs to `WriteTransaction`, so
/// `open_table` and friends work unchanged at the call site.
/// Test: `a_transaction_past_its_deadline_is_rolled_back_not_committed`,
/// `a_transaction_inside_its_deadline_commits`.
pub struct DeadlinedWrite {
    txn: WriteTransaction,
    palace: Arc<str>,
    store: &'static str,
    started: Instant,
    budget: Duration,
    /// `None` when `started + budget` overflows — `Duration::MAX` means
    /// "never expires", for one-shot migrations that must run to completion.
    deadline: Option<Instant>,
}

impl DeadlinedWrite {
    /// Begin a write transaction on `db` whose clock starts once redb grants
    /// the lock.
    ///
    /// Why: time spent WAITING for the lock is the previous writer's, not this
    /// one's — counting it would abort a healthy writer for a neighbour's stall.
    /// What: `db.begin_write()`, then starts the clock against `budget`.
    /// Test: `a_transaction_inside_its_deadline_commits`.
    pub fn begin(
        db: &Database,
        palace: Arc<str>,
        store: &'static str,
        budget: Duration,
    ) -> Result<Self, redb::TransactionError> {
        let txn = db.begin_write()?;
        let started = Instant::now();
        Ok(Self {
            txn,
            palace,
            store,
            started,
            budget,
            deadline: started.checked_add(budget),
        })
    }

    /// `Err` once the deadline has passed; the caller should return it, which
    /// drops (and so rolls back) the transaction.
    ///
    /// Why: a commit abandoned by a timed-out pipeline has no caller left to
    /// read the error, so the abort is logged here, at warn, naming the palace
    /// and how long the lock was held — a silent abort is the #8729 failure.
    /// What: compares now against the deadline; past it, logs and returns
    /// [`WriteTxnError::DeadlineExceeded`].
    /// Test: `a_transaction_past_its_deadline_is_rolled_back_not_committed`,
    /// `a_stalled_commit_is_logged_with_the_palace_and_the_lock_hold`.
    pub fn check(&self, stage: &'static str) -> Result<(), WriteTxnError> {
        let now = Instant::now();
        match self.deadline {
            Some(deadline) if now >= deadline => {
                let held = now.duration_since(self.started);
                tracing::warn!(
                    palace = %self.palace,
                    store = self.store,
                    stage,
                    held_ms = held.as_millis(),
                    budget_ms = self.budget.as_millis(),
                    "#8749: write transaction held the palace write lock past its \
                     deadline; rolling it back so the next writer proceeds"
                );
                Err(WriteTxnError::DeadlineExceeded {
                    palace: Arc::clone(&self.palace),
                    store: self.store,
                    stage,
                    budget: self.budget,
                    held,
                })
            }
            _ => Ok(()),
        }
    }

    /// Hand back the transaction to commit, or abort it past the deadline.
    ///
    /// Why: the last op can be the slow one, so the check before `commit()` is
    /// what stops a stalled transaction landing late (#8749).
    /// What: [`Self::check`]; on `Err`, `abort()`s explicitly (a failed abort
    /// is logged — nothing was committed either way) and returns the error.
    /// Test: `a_transaction_past_its_deadline_is_rolled_back_not_committed`.
    pub fn check_before_commit(self) -> Result<WriteTransaction, WriteTxnError> {
        if let Err(err) = self.check("before commit") {
            if let Err(abort_err) = self.txn.abort() {
                tracing::error!(
                    palace = %self.palace,
                    store = self.store,
                    error = %abort_err,
                    "#8749: explicit abort of an over-deadline write transaction failed; \
                     it was not committed"
                );
            }
            return Err(err);
        }
        Ok(self.txn)
    }
}

impl std::ops::Deref for DeadlinedWrite {
    type Target = WriteTransaction;

    fn deref(&self) -> &WriteTransaction {
        &self.txn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redb::{ReadableDatabase, TableDefinition};

    const T: TableDefinition<u64, u64> = TableDefinition::new("t");

    fn db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::create(dir.path().join("d.redb")).expect("create redb");
        (dir, db)
    }

    fn row(db: &Database, k: u64) -> Option<u64> {
        let rtx = db.begin_read().expect("begin read");
        match rtx.open_table(T) {
            Ok(t) => t.get(k).expect("get").map(|g| g.value()),
            Err(redb::TableError::TableDoesNotExist(_)) => None,
            Err(e) => panic!("open table: {e}"),
        }
    }

    fn begin(db: &Database, budget: Duration) -> DeadlinedWrite {
        DeadlinedWrite::begin(db, "palace-x".into(), "test.redb", budget).expect("begin")
    }

    /// Why (#8749): the fix is that a transaction past its deadline does NOT
    /// commit — it rolls back, releases the lock, and says so.
    /// What: writes a row under a zero budget, asserts the typed error, that
    /// the row is absent, and that the next writer gets the lock at once.
    /// Test: itself.
    #[test]
    fn a_transaction_past_its_deadline_is_rolled_back_not_committed() {
        let (_dir, db) = db();
        let w = begin(&db, Duration::ZERO);
        {
            let mut t = w.open_table(T).expect("open table");
            t.insert(1, 1).expect("insert");
        }
        let err = w
            .check_before_commit()
            .err()
            .expect("must refuse to commit");
        assert!(
            matches!(
                &err,
                WriteTxnError::DeadlineExceeded {
                    store: "test.redb",
                    stage: "before commit",
                    ..
                }
            ),
            "{err}"
        );
        assert!(err.to_string().contains("palace 'palace-x'"), "{err}");
        assert_eq!(
            row(&db, 1),
            None,
            "#8749: an over-deadline write must not land"
        );
        // The lock is free: a second writer begins and commits immediately.
        let next = begin(&db, Duration::from_secs(60));
        {
            let mut t = next.open_table(T).expect("open table");
            t.insert(2, 2).expect("insert");
        }
        next.check_before_commit()
            .expect("in budget")
            .commit()
            .expect("commit");
        assert_eq!(row(&db, 2), Some(2));
    }

    /// Why: the deadline must not tax healthy writes, and `Duration::MAX`
    /// (the one-shot-migration opt-out) must not overflow `Instant`.
    /// What: commits under a generous and an unbounded budget.
    /// Test: itself.
    #[test]
    fn a_transaction_inside_its_deadline_commits() {
        let (_dir, db) = db();
        for (k, budget) in [(1, Duration::from_secs(60)), (2, Duration::MAX)] {
            let w = begin(&db, budget);
            {
                let mut t = w.open_table(T).expect("open table");
                t.insert(k, k).expect("insert");
            }
            w.check("between ops").expect("in budget");
            w.check_before_commit()
                .expect("in budget")
                .commit()
                .expect("commit");
            assert_eq!(row(&db, k), Some(k));
        }
    }

    /// Why: the warn line names the palace by the store's directory; a wrong
    /// component would name the data root or the file instead.
    /// What: a nested path yields its parent directory's name; a bare file
    /// name falls back to itself.
    /// Test: itself.
    #[test]
    fn the_palace_label_is_the_store_files_directory() {
        assert_eq!(
            &*palace_label(Path::new("/data/palaces/my-palace/kg.db")),
            "my-palace"
        );
        assert_eq!(&*palace_label(Path::new("kg.db")), "kg.db");
    }
}
