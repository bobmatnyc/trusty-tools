//! The dashboard document store: reads, and all-or-nothing batch writes.
//!
//! Why: the Architect's dashboard store exists only as a Python prototype
//! (`dashboard_store.py`); #10063 ports it into this crate so the daemon can
//! host it (owner ruling Dbc). Every write takes one exclusive lock, checks
//! `if_version` and bumps `version` (DOC-82 LDG-2.8.1), in either of the two
//! existing formats, and a store in the other format is refused, never read
//! as empty (LDG-3.2).
//! What: [`Store`] over a root and a [`Format`]. Reads ([`Store::get`],
//! [`Store::list`], [`Store::collections`], [`Store::snapshot`]) take no
//! lock: every replace is atomic. [`Store::batch`] takes `<root>/.lock`
//! with a bounded wait, stages every [`Op`], runs the optional schema check
//! ([`SchemaSet`], [`SchemaMode`]), hands each removed document to the
//! [`DeleteArchive`] (in production a [`PruneLog`]) before anything is
//! deleted, commits, then appends the change log (LDG-2.8.3). A delete with
//! no archive and any batch with no secret scan are refused: a delete must
//! never lose its record (LDG-2.8.4) and a secret must never be written
//! (LDG-2.3.5). Names that differ from an existing one only in letter case
//! are refused in both formats (#10063 df-G01). [`Store::fsck`] and
//! [`Store::validate`] check a whole store.
//! Test: `tests/dashboard_golden.rs` (prototype bytes and outcomes),
//! `tests/dashboard_store.rs` (conflict, rollback, refusals, lock, corrupt files),
//! `tests/dashboard_case.rs` (case variants), `tests/dashboard_log.rs`
//! (change log, prune log, fsck, validate, secrets).
//!
//! # Spec References
//! - [`SPEC-LEDGER-02~draft`](docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md#SPEC-LEDGER-02~draft)
//! - [`SPEC-LEDGER-03~draft`](docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md#SPEC-LEDGER-03~draft)

mod case;
mod changes;
pub mod dir;
mod fsck;
pub mod jsonl;
mod lock;
pub mod pruned;
mod stage;
mod validate;

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value};

use super::canonical::python_dumps;
use super::error::DashError;
use super::ids::check_name;
use super::schema_check::{SchemaMode, SchemaSet};
use crate::clock::{Clock, SystemClock};
use crate::secret::{SecretScan, scan_strings};
pub use pruned::PruneLog;
use stage::Staged;
pub use stage::{Op, OpResult, normalize_timestamps};
pub use validate::Validation;

/// How long a write waits for the store lock by default.
pub const LOCK_WAIT: Duration = Duration::from_secs(10);

/// The on-disk layout of a store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `<root>/<coll>/<id>.json`, one file per document.
    Dir,
    /// `<root>/<coll>.jsonl`, one canonical file per collection.
    Jsonl,
}

impl FromStr for Format {
    type Err = DashError;

    fn from_str(s: &str) -> Result<Self, DashError> {
        match s {
            "dir" => Ok(Self::Dir),
            "jsonl" => Ok(Self::Jsonl),
            other => Err(DashError::Invalid(format!(
                "unknown store format {other:?}: use dir or jsonl"
            ))),
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Dir => "dir",
            Self::Jsonl => "jsonl",
        })
    }
}

/// One document as read: `{id, version, data}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Doc {
    /// Doc id.
    pub id: String,
    /// Store-managed version, at least 1.
    pub version: u64,
    /// The body, without `version`.
    pub data: Map<String, Value>,
}

/// A stored document's `(version, body)`.
pub type Versioned = (u64, Map<String, Value>);

/// `{collection: documents}`, as the id and tally rules read the store.
pub type Snapshot = BTreeMap<String, Vec<Doc>>;

/// Filters for [`Store::list`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListQuery {
    /// Keep documents whose `status` equals this.
    pub status: Option<String>,
    /// Order by this body key (documents without it last, in id order).
    pub order_by: Option<String>,
    /// Descending order.
    pub desc: bool,
    /// At most this many documents.
    pub limit: Option<usize>,
}

/// A document a batch deletes, as handed to the [`DeleteArchive`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Removed<'a> {
    /// Collection name.
    pub collection: &'a str,
    /// Doc id.
    pub id: &'a str,
    /// Its last version.
    pub version: u64,
    /// Its full body.
    pub data: &'a Map<String, Value>,
    /// When the batch ran, `YYYY-MM-DDTHH:MM:SSZ` (#10063 PR 2).
    pub deleted_at: &'a str,
    /// Who ran it: the store's actor.
    pub by: &'a str,
}

/// Where deleted documents are recorded before they are removed (LDG-2.8.4).
/// An error refuses the whole batch; nothing is written.
pub trait DeleteArchive: Send + Sync {
    /// Durably records `removed` (all of one batch).
    ///
    /// # Errors
    /// Any error; the batch is refused with it.
    fn record(&self, removed: &[Removed<'_>]) -> Result<(), DashError>;
}

/// An owned secret-shape scanner (see [`crate::ledger::writer::SecretScan`]).
type OwnedScan = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// A dashboard store at `root` in one [`Format`].
pub struct Store {
    root: PathBuf,
    format: Format,
    lock_wait: Duration,
    schemas: Option<(Box<dyn SchemaSet>, SchemaMode)>,
    archive: Option<Box<dyn DeleteArchive>>,
    scan: Option<OwnedScan>,
    clock: Arc<dyn Clock + Send + Sync>,
    actor: String,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("root", &self.root)
            .field("format", &self.format)
            .field("lock_wait", &self.lock_wait)
            .field("schema_mode", &self.schemas.as_ref().map(|s| s.1))
            .field("archive", &self.archive.is_some())
            .field("secret_scan", &self.scan.is_some())
            .field("actor", &self.actor)
            .finish()
    }
}

impl Store {
    /// A store at `root` (created on the first write) in `format`; no schema
    /// check, no delete archive, no secret scan (so it refuses writes), the
    /// system clock, actor `unknown` (the prototype's default), a
    /// [`LOCK_WAIT`] lock wait.
    pub fn new(root: impl Into<PathBuf>, format: Format) -> Self {
        Self {
            root: root.into(),
            format,
            lock_wait: LOCK_WAIT,
            schemas: None,
            archive: None,
            scan: None,
            clock: Arc::new(SystemClock),
            actor: "unknown".into(),
        }
    }

    /// Refuses any op holding a string (key or value, collection or id) that
    /// `scan` flags; `scan` returns a shape name, never the value (LDG-2.3.5).
    #[must_use]
    pub fn with_secret_scan(
        mut self,
        scan: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.scan = Some(Box::new(scan));
        self
    }

    /// The clock change-log and prune-log times come from.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    /// The `by` of change-log and prune-log lines.
    #[must_use]
    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = actor.into();
        self
    }

    /// Bounds the wait for the store lock.
    #[must_use]
    pub fn with_lock_wait(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    /// Checks every written document against `schemas` in `mode`.
    #[must_use]
    pub fn with_schemas(mut self, schemas: Box<dyn SchemaSet>, mode: SchemaMode) -> Self {
        self.schemas = Some((schemas, mode));
        self
    }

    /// Records deleted documents in `archive` before removing them.
    #[must_use]
    pub fn with_archive(mut self, archive: Box<dyn DeleteArchive>) -> Self {
        self.archive = Some(archive);
        self
    }

    /// The store root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The store format.
    pub fn format(&self) -> Format {
        self.format
    }

    /// Refuses `coll` when it is reserved, differs from an existing
    /// collection only in letter case, or the store holds it in the other
    /// format.
    fn guard(&self, coll: &str) -> Result<(), DashError> {
        check_name("collection", coll)?;
        // #10063 PR 2: the change log is in both formats, so its name is
        // reserved in both.
        if jsonl::is_reserved(coll) {
            return Err(DashError::Invalid(format!(
                "collection name {} is reserved for the change log",
                crate::pystr::py_repr(coll)
            )));
        }
        let names = list_names(&self.root)?;
        let stems = names
            .iter()
            .filter(|n| !n.starts_with('.'))
            .map(|n| n.strip_suffix(".jsonl").unwrap_or(n));
        if let Some(other) = case::variant(coll, stems) {
            return Err(case::refusal("collection", coll, other));
        }
        let has_jsonl = jsonl::coll_path(&self.root, coll).exists();
        match self.format {
            Format::Dir if has_jsonl => Err(DashError::WrongFormat(format!(
                "{} holds {coll}.jsonl: open it as a jsonl store",
                self.root.display()
            ))),
            Format::Jsonl if !has_jsonl && dir::has_docs(&self.root, coll)? => {
                Err(DashError::WrongFormat(format!(
                    "{} holds per-document files and no {coll}.jsonl: migrate it first",
                    self.root.join(coll).display()
                )))
            }
            _ => Ok(()),
        }
    }

    /// The document `coll/id`.
    ///
    /// # Errors
    /// [`DashError::NotFound`] when absent; a bad name, the other format, or
    /// a corrupt file.
    pub fn get(&self, coll: &str, id: &str) -> Result<Doc, DashError> {
        check_name("collection", coll)?;
        check_name("doc id", id)?;
        self.guard(coll)?;
        let found = match self.format {
            Format::Dir => dir::load_exact(&self.root, coll, id)?,
            Format::Jsonl => jsonl::read_coll(&self.root, coll)?.remove(id),
        };
        let (version, data) =
            found.ok_or_else(|| DashError::NotFound(format!("{coll}/{id} not found")))?;
        Ok(Doc {
            id: id.to_owned(),
            version,
            data,
        })
    }

    /// The documents of `coll` (empty when it is absent), in id order unless
    /// `query.order_by` says otherwise.
    ///
    /// # Errors
    /// A bad name, the other format, or a corrupt file.
    pub fn list(&self, coll: &str, query: &ListQuery) -> Result<Vec<Doc>, DashError> {
        self.guard(coll)?;
        let mut docs = match self.format {
            Format::Dir => dir::list(&self.root, coll)?,
            Format::Jsonl => jsonl::read_coll(&self.root, coll)?
                .into_iter()
                .map(|(id, (version, data))| Doc { id, version, data })
                .collect(),
        };
        if let Some(status) = &query.status {
            docs.retain(|d| d.data.get("status").and_then(Value::as_str) == Some(status));
        }
        if let Some(key) = &query.order_by {
            let (mut present, absent): (Vec<Doc>, Vec<Doc>) =
                docs.into_iter().partition(|d| d.data.contains_key(key));
            present.sort_by(|a, b| {
                let ord = sort_key(&a.data[key]).cmp(&sort_key(&b.data[key]));
                if query.desc { ord.reverse() } else { ord }
            });
            docs = present.into_iter().chain(absent).collect();
        }
        if let Some(limit) = query.limit {
            docs.truncate(limit);
        }
        Ok(docs)
    }

    /// The collections that exist, sorted.
    ///
    /// # Errors
    /// [`DashError::WrongFormat`] when the root also holds collections in the
    /// other format (a whole store in the other format is never empty);
    /// [`DashError::Io`] when the root cannot be listed.
    pub fn collections(&self) -> Result<Vec<String>, DashError> {
        let (mine, other) = match self.format {
            Format::Dir => (
                dir::collections(&self.root)?,
                jsonl::collections(&self.root)?,
            ),
            Format::Jsonl => {
                let mine = jsonl::collections(&self.root)?;
                let stray = dir::collections(&self.root)?
                    .into_iter()
                    .filter(|c| !mine.contains(c))
                    .collect();
                (mine, stray)
            }
        };
        if let Some(c) = other.first() {
            return Err(DashError::WrongFormat(format!(
                "{} holds collection {c} in the other format; this store is {}",
                self.root.display(),
                self.format
            )));
        }
        Ok(mine)
    }

    /// Every document of `colls`.
    ///
    /// # Errors
    /// [`Store::list`]'s errors.
    pub fn snapshot(&self, colls: &[&str]) -> Result<Snapshot, DashError> {
        colls
            .iter()
            .map(|c| Ok(((*c).to_owned(), self.list(c, &ListQuery::default())?)))
            .collect()
    }

    /// Applies `ops` all-or-nothing under the store lock.
    ///
    /// # Errors
    /// [`DashError::Conflict`] (stale `if_version`), [`DashError::NotFound`],
    /// [`DashError::Invalid`] (bad op, body, name, a case variant of an
    /// existing name, a delete with no archive, a batch with no secret
    /// scan), [`DashError::SecretRefused`], [`DashError::SchemaInvalid`]
    /// (enforce mode), [`DashError::Unavailable`] (lock wait passed), the
    /// archive's error, or an I/O or corrupt-file error. Nothing is written on
    /// any of these. A write that fails midway is rolled back; when a restore
    /// fails too the error is [`DashError::RollbackIncomplete`], naming the
    /// torn files. Rollback covers failures, not crashes. A committed batch
    /// whose change-log append fails is [`DashError::Unlogged`].
    pub fn batch(&self, ops: &[Op]) -> Result<Vec<OpResult>, DashError> {
        if ops.is_empty() {
            return Err(DashError::Invalid(
                "batch needs a non-empty list of ops".into(),
            ));
        }
        // #10063 PR 2: before the lock and before any message can quote a value.
        self.refuse_secrets(ops)?;
        let _lock = lock::acquire(&self.root, self.lock_wait)?;
        let mut staged = Staged::default();
        let results = ops
            .iter()
            .map(|op| self.stage(&mut staged, op))
            .collect::<Result<Vec<_>, _>>()?;
        self.check_schemas(&staged)?;
        let now = crate::classify::alerts::iso(self.clock.now_unix());
        self.archive_removed(&staged, &now)?;
        self.commit(&staged)?;
        // #10063 PR 2: after the commit; a failure is reported, never dropped.
        if let Err(cause) = changes::append(self, &staged, &now) {
            tracing::error!(error = %cause, "dashboard store: change log not written");
            return Err(DashError::Unlogged {
                results,
                cause: Box::new(cause),
            });
        }
        Ok(results)
    }

    /// The first secret-shaped op, refused with its shape only; every batch
    /// is refused when no scan is configured (fail closed, as a delete with
    /// no archive is).
    fn refuse_secrets(&self, ops: &[Op]) -> Result<(), DashError> {
        let scan: SecretScan<'_> = match &self.scan {
            Some(scan) => &**scan,
            None => {
                return Err(DashError::Invalid(
                    "write refused: no secret scan is configured for this store".into(),
                ));
            }
        };
        for (i, op) in ops.iter().enumerate() {
            let (coll, id, data) = op.scan_parts();
            let shape = scan(coll)
                .or_else(|| scan(id))
                .or_else(|| data.and_then(|d| scan_strings(d, scan)));
            if let Some(shape) = shape {
                let n = i + 1;
                tracing::warn!(op = n, shape = %shape, "dashboard store: secret-shaped value refused");
                return Err(DashError::SecretRefused(format!(
                    "op {n} refused: secret-shaped value ({shape}); write a location, never a value"
                )));
            }
        }
        Ok(())
    }

    /// `set` one document.
    ///
    /// # Errors
    /// [`Store::batch`]'s errors.
    pub fn set(
        &self,
        coll: &str,
        id: &str,
        data: Value,
        if_version: Option<u64>,
    ) -> Result<OpResult, DashError> {
        let op = Op::Set {
            collection: coll.into(),
            id: id.into(),
            data,
            if_version,
        };
        self.one(op)
    }

    /// `update` one document.
    ///
    /// # Errors
    /// [`Store::batch`]'s errors.
    pub fn update(
        &self,
        coll: &str,
        id: &str,
        data: Value,
        if_version: Option<u64>,
    ) -> Result<OpResult, DashError> {
        let op = Op::Update {
            collection: coll.into(),
            id: id.into(),
            data,
            if_version,
        };
        self.one(op)
    }

    /// `delete` one document.
    ///
    /// # Errors
    /// [`Store::batch`]'s errors.
    pub fn delete(
        &self,
        coll: &str,
        id: &str,
        if_version: Option<u64>,
    ) -> Result<OpResult, DashError> {
        self.one(Op::Delete {
            collection: coll.into(),
            id: id.into(),
            if_version,
        })
    }

    fn one(&self, op: Op) -> Result<OpResult, DashError> {
        let mut out = self.batch(std::slice::from_ref(&op))?;
        out.pop()
            .ok_or_else(|| DashError::Invalid("batch returned no result".into()))
    }

    fn check_schemas(&self, staged: &Staged) -> Result<(), DashError> {
        let Some((schemas, mode)) = &self.schemas else {
            return Ok(());
        };
        if *mode == SchemaMode::Off {
            return Ok(());
        }
        let mut touched: Vec<_> = staged
            .slots
            .iter()
            .filter(|s| s.dirty && s.exists)
            .collect();
        touched.sort_by(|a, b| (&a.coll, &a.id).cmp(&(&b.coll, &b.id)));
        let mut errors = Vec::new();
        for s in touched {
            errors.extend(schemas.validate(&s.coll, &s.id, &s.data)?);
        }
        if errors.is_empty() {
            return Ok(());
        }
        if *mode == SchemaMode::Enforce {
            return Err(DashError::SchemaInvalid(errors));
        }
        for e in &errors {
            tracing::warn!(problem = %e, "dashboard store: schema warning");
        }
        Ok(())
    }

    fn archive_removed(&self, staged: &Staged, now: &str) -> Result<(), DashError> {
        let removed: Vec<Removed<'_>> = staged
            .slots
            .iter()
            .filter(|s| s.dirty && !s.exists)
            .filter_map(|s| {
                s.old.as_ref().map(|(version, data)| Removed {
                    collection: &s.coll,
                    id: &s.id,
                    version: *version,
                    data,
                    deleted_at: now,
                    by: &self.actor,
                })
            })
            .collect();
        if removed.is_empty() {
            return Ok(());
        }
        // #10063: fail closed; a delete never runs without its archive record.
        let archive = self.archive.as_ref().ok_or_else(|| {
            DashError::Invalid(
                "delete refused: no prune archive is configured for this store".into(),
            )
        })?;
        archive.record(&removed)
    }
}

/// The prototype's list sort key: numbers, then strings, then containers (by
/// their `json.dumps` text), then booleans and null (by Python's `str`).
enum SortKey {
    Number(f64),
    Text(String),
    Container(String),
    Other(&'static str),
}

impl SortKey {
    fn rank(&self) -> u8 {
        match self {
            Self::Number(_) => 0,
            Self::Text(_) => 1,
            Self::Container(_) => 2,
            Self::Other(_) => 3,
        }
    }
}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (Self::Text(a), Self::Text(b)) | (Self::Container(a), Self::Container(b)) => a.cmp(b),
            (Self::Other(a), Self::Other(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for SortKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for SortKey {}

fn sort_key(v: &Value) -> SortKey {
    match v {
        Value::Number(n) => SortKey::Number(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => SortKey::Text(s.clone()),
        Value::Array(_) | Value::Object(_) => SortKey::Container(python_dumps(v, true)),
        Value::Bool(true) => SortKey::Other("True"),
        Value::Bool(false) => SortKey::Other("False"),
        Value::Null => SortKey::Other("None"),
    }
}

/// UTF-8 entry names of `dir`; empty when it does not exist.
pub(super) fn list_names(dir: &Path) -> Result<Vec<String>, DashError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(DashError::io(dir)(e)),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(DashError::io(dir))?;
        if let Ok(name) = entry.file_name().into_string() {
            out.push(name);
        }
    }
    Ok(out)
}
