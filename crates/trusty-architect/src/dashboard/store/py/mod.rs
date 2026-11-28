//! The port of `dashboard_store.py`: the prototype's document store, byte for byte (#12029 P2c).
//!
//! Why: the frozen prototype's callers (`dashboard-db.py`, the owner-answer
//! and question writers) are ported in later slices onto this store, and
//! during the overlap the prototype and the port write the same files, so
//! the port must give the same bytes, versions, refusals and error text. The
//! #10063 [`super::Store`] keeps its stricter rules by design (secret scan,
//! letter-case refusal, its own change log); this module reuses its lock and
//! its writer-class guard and changes none of its paths.
//! What: `PyStore` over an absolute root in `dir` or `jsonl`: reads
//! (`PyStore::get`, `PyStore::list_docs`, `PyStore::collections`) take
//! no lock, as the prototype's do (a replace is atomic). Writes live in
//! `write`: one `flock` on `<root>/.lock` (shared with the prototype), the
//! AC2 claim (`writer::admit`, class [`WriterClass::Python`]),
//! staging, the prune log, then the commit with rollback. Values are
//! `PyValue`s so Python's types survive.
//! Known gaps: the lock wait is bounded ([`super::LOCK_WAIT`]; the prototype
//! waits forever, #9758) and ends in [`PyStoreError::Unavailable`]; the
//! jsonl parse cache is not kept (it only skips re-reading an unchanged
//! file); a big integer or a non-finite float reaches the schema check as a
//! float or `null`; the pytest live-store refusal has no Rust twin.
//! Test: `dashboard_store_recordings_replay_clean_against_the_port`,
//! `a_second_writer_class_on_one_collection_is_refused`, and the error-arm
//! tests in `tests`.

mod cases;
mod fsio;
mod jsonl;
#[cfg(test)]
mod tests;
mod value;
mod write;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use cases::replay_cases;
// #12029 P2d: the ids and qual driver mirrors share these.
pub(crate) use cases::{error_line, file_op};
pub(crate) use fsio::makedirs;
pub(crate) use value::{normalize_timestamps, sorted, with_key};

use super::writer::WriterClass;
use crate::clock::Clock;
use crate::dashboard::pyjsonl::{self, JsonlError, join};
use crate::pyjson_file::{LoadError, read_text};
use crate::pyjson_load::{PyInt, PyValue, loads};
use crate::pystr::py_repr;
use crate::pyval::get;
use value::{check_id, cmp_keys, sort_key};

/// An exception the prototype raises, by class.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PyStoreError {
    /// `StoreError` (400 `invalid_argument`).
    #[error("{0}")]
    Store(String),
    /// `NotFound` (404).
    #[error("{0}")]
    NotFound(String),
    /// `Conflict` (409).
    #[error("{0}")]
    Conflict(String),
    /// `SchemaInvalid` (400): every problem.
    #[error("{}", summarize(.0))]
    SchemaInvalid(Vec<String>),
    /// `dashboard_schema.SchemaError`.
    #[error("{0}")]
    Schema(String),
    /// An `OSError` the prototype lets through: its class, errno and `str()`.
    #[error("{msg}")]
    Os {
        /// `type(e).__name__`.
        class: String,
        /// `e.errno`.
        errno: i32,
        /// `str(e)`.
        msg: String,
    },
    /// #12029 AC2: the collection belongs to the other writer class.
    #[error("{0}")]
    WriterClass(String),
    /// The lock wait passed (the prototype has no such outcome).
    #[error("{0}")]
    Unavailable(String),
}

impl PyStoreError {
    /// `(status, code)` of a `StoreError` subclass; `None` for the others.
    pub fn http(&self) -> Option<(u16, &'static str)> {
        match self {
            Self::Store(_) => Some((400, "invalid_argument")),
            Self::NotFound(_) => Some((404, "not_found")),
            Self::Conflict(_) => Some((409, "conflict")),
            Self::SchemaInvalid(_) => Some((400, "schema_invalid")),
            _ => None,
        }
    }

    /// `type(e).__name__`.
    pub fn class(&self) -> &str {
        match self {
            Self::Store(_) => "StoreError",
            Self::NotFound(_) => "NotFound",
            Self::Conflict(_) => "Conflict",
            Self::SchemaInvalid(_) => "SchemaInvalid",
            Self::Schema(_) => "SchemaError",
            Self::Os { class, .. } => class,
            Self::WriterClass(_) => "WriterClassError",
            Self::Unavailable(_) => "Unavailable",
        }
    }

    /// The `OSError` Python raises for `e` on `path`.
    pub(crate) fn os(path: &str, e: &std::io::Error) -> Self {
        let errno = e.raw_os_error().unwrap_or(0);
        Self::Os {
            class: os_class(errno).into(),
            errno,
            msg: crate::writers::errno_text(e, path),
        }
    }

    /// An `OSError` known only by its `str()` (`[Errno N] ...`).
    fn os_text(msg: String) -> Self {
        let errno = msg
            .strip_prefix("[Errno ")
            .and_then(|r| r.split_once(']'))
            .and_then(|(n, _)| n.parse().ok())
            .unwrap_or(0);
        Self::Os {
            class: os_class(errno).into(),
            errno,
            msg,
        }
    }
}

/// The `OSError` subclass for `errno`.
fn os_class(errno: i32) -> &'static str {
    match errno {
        libc::ENOENT => "FileNotFoundError",
        libc::EEXIST => "FileExistsError",
        libc::EISDIR => "IsADirectoryError",
        libc::ENOTDIR => "NotADirectoryError",
        libc::EACCES | libc::EPERM => "PermissionError",
        _ => "OSError",
    }
}

/// `SchemaInvalid`'s text: the first five problems, then a count.
fn summarize(errors: &[String]) -> String {
    let head = errors
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    match errors.len() {
        n if n > 5 => format!("{head} (+{} more)", n - 5),
        _ => head,
    }
}

/// A [`JsonlError`] as the prototype re-raises it: a `StoreError`, or the
/// `OSError` it lets through.
fn from_jsonl(e: JsonlError) -> PyStoreError {
    match e {
        JsonlError::Bad(m) => PyStoreError::Store(m),
        JsonlError::Os(m) => PyStoreError::os_text(m),
    }
}

/// The store format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fmt {
    /// `<root>/<coll>/<id>.json`.
    Dir,
    /// `<root>/<coll>.jsonl` plus `changes.jsonl`.
    Jsonl,
}

impl Fmt {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "dir" => Some(Self::Dir),
            "jsonl" => Some(Self::Jsonl),
            _ => None,
        }
    }

    /// `"dir"` or `"jsonl"`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Dir => "dir",
            Self::Jsonl => "jsonl",
        }
    }
}

/// `default_format(env)`: `DASHBOARD_FORMAT`, `dir` when unset or empty.
///
/// # Errors
/// [`PyStoreError::Store`] for any other value.
pub(crate) fn default_format(env: &dyn Fn(&str) -> Option<String>) -> Result<Fmt, PyStoreError> {
    let value = env("DASHBOARD_FORMAT")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "dir".into());
    Fmt::parse(&value).ok_or_else(|| {
        PyStoreError::Store(format!(
            "DASHBOARD_FORMAT={}: use dir or jsonl",
            py_repr(&value)
        ))
    })
}

/// `os.path.abspath(path)` against `cwd`.
pub(crate) fn abspath(path: &str, cwd: &Path) -> String {
    let full = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{}/{path}", cwd.display())
    };
    let mut parts: Vec<&str> = Vec::new();
    for p in full.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

/// `os.path.dirname(path)`.
pub(crate) fn dirname(path: &str) -> &str {
    let Some(i) = path.rfind('/') else {
        return "";
    };
    let head = &path[..=i];
    if head.bytes().all(|b| b == b'/') {
        head
    } else {
        head.trim_end_matches('/')
    }
}

/// The constructor's arguments, `None` where the prototype's caller passes none.
#[derive(Debug, Clone, Default)]
pub(crate) struct Options {
    pub(crate) fmt: Option<String>,
    pub(crate) actor: Option<String>,
    pub(crate) schema_mode: Option<String>,
    pub(crate) prune_log: Option<String>,
}

/// A stored document's `(version, data)`.
pub(crate) type Found = (PyInt, Vec<(String, PyValue)>);

/// One document as read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PyDocOut {
    pub(crate) id: String,
    pub(crate) version: PyInt,
    pub(crate) data: PyValue,
}

/// `Store(root, fmt, actor, schema_mode, clock, prune_log)`.
pub(crate) struct PyStore {
    pub(crate) root: String,
    pub(crate) fmt: Fmt,
    pub(crate) actor: String,
    pub(crate) schema_mode: String,
    prune_log: Option<String>,
    schema_dir: PathBuf,
    clock: Arc<dyn Clock + Send + Sync>,
    lock_wait: Duration,
    /// What the prototype writes to stderr (schema and change-log warnings).
    diag: std::sync::Mutex<String>,
}

impl PyStore {
    /// Appends a line the prototype writes to `sys.stderr`.
    fn warn(&self, line: &str) {
        if let Ok(mut d) = self.diag.lock() {
            d.push_str(line);
        }
    }

    /// The stderr text written since the last call.
    pub(crate) fn take_stderr(&self) -> String {
        self.diag
            .lock()
            .map(|mut d| std::mem::take(&mut *d))
            .unwrap_or_default()
    }

    /// Why: the prototype resolves its format, actor and schema mode from
    /// its arguments, then the environment, then its defaults.
    /// What: `Store.__init__`; `root` is made absolute against `cwd`;
    /// `schema_dir` stands for `dashboard_schema.SCHEMA_DIR`.
    /// Test: `dashboard_store_recordings_replay_clean_against_the_port` (`dir`, `jsonl`).
    ///
    /// # Errors
    /// [`PyStoreError::Store`] for a bad format, [`PyStoreError::Schema`] for
    /// a bad `DASHBOARD_SCHEMA`.
    pub(crate) fn new(
        root: &str,
        opts: Options,
        env: &dyn Fn(&str) -> Option<String>,
        cwd: &Path,
        schema_dir: PathBuf,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Result<Self, PyStoreError> {
        let some = |v: Option<String>| v.filter(|s| !s.is_empty());
        let fmt = match some(opts.fmt) {
            Some(f) => Fmt::parse(&f).ok_or_else(|| {
                PyStoreError::Store(format!(
                    "unknown store format {}: use dir or jsonl",
                    py_repr(&f)
                ))
            })?,
            None => default_format(env)?,
        };
        let actor = some(opts.actor)
            .or_else(|| some(env("DASHBOARD_ACTOR")))
            .unwrap_or_else(|| "unknown".into());
        let schema_mode = match some(opts.schema_mode) {
            Some(m) => m,
            None => crate::dashboard::schema_check::script::mode(env)
                .map_err(|e| PyStoreError::Schema(e.to_string()))?
                .map_or_else(
                    || if fmt == Fmt::Jsonl { "warn" } else { "off" }.to_owned(),
                    |m| {
                        use crate::dashboard::schema_check::SchemaMode;
                        match m {
                            SchemaMode::Off => "off",
                            SchemaMode::Warn => "warn",
                            SchemaMode::Enforce => "enforce",
                        }
                        .to_owned()
                    },
                ),
        };
        Ok(Self {
            root: abspath(root, cwd),
            fmt,
            actor,
            schema_mode,
            prune_log: opts.prune_log,
            schema_dir,
            clock,
            lock_wait: super::LOCK_WAIT,
            diag: std::sync::Mutex::default(),
        })
    }

    /// The writer class every batch of this store claims (#12029 AC2).
    pub(crate) const CLASS: WriterClass = WriterClass::Python;

    /// Bounds the wait for the store lock.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_lock_wait(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    /// `prune_log_path()`: `<root>/../owner-queue-archive/pruned.jsonl` unless given.
    pub(crate) fn prune_log_path(&self) -> String {
        self.prune_log.clone().unwrap_or_else(|| {
            join(
                &join(dirname(&self.root), "owner-queue-archive"),
                "pruned.jsonl",
            )
        })
    }

    /// `now_iso()` on this store's clock.
    pub(crate) fn now_iso(&self) -> String {
        crate::classify::alerts::iso(self.clock.now_unix().floor())
    }

    /// `_path(coll, doc_id)`.
    fn path(&self, coll: &PyValue, id: &PyValue) -> Result<String, PyStoreError> {
        let coll = check_id("collection", coll)?;
        let id = check_id("doc id", id)?;
        Ok(join(&join(&self.root, coll), &format!("{id}.json")))
    }

    /// `_guard(coll)`: refuse a store laid out in the other format.
    fn guard<'a>(&self, coll: &'a PyValue) -> Result<&'a str, PyStoreError> {
        let coll = check_id("collection", coll)?;
        let has_jsonl = Path::new(&pyjsonl::coll_path(&self.root, coll)).exists();
        let cdir = join(&self.root, coll);
        match self.fmt {
            Fmt::Dir if has_jsonl => Err(PyStoreError::Store(format!(
                "{} holds {coll}.jsonl: set DASHBOARD_FORMAT=jsonl",
                self.root
            ))),
            Fmt::Jsonl if reserved(coll) => Err(PyStoreError::Store(format!(
                "collection name {} is reserved for the change log",
                py_repr(coll)
            ))),
            Fmt::Jsonl if !has_jsonl && Path::new(&cdir).is_dir() => {
                if listdir(&cdir)?.iter().any(|n| n.ends_with(".json")) {
                    return Err(PyStoreError::Store(format!(
                        "{cdir} holds per-document files and no {coll}.jsonl: run scripts/dashboard-migrate.py"
                    )));
                }
                Ok(coll)
            }
            _ => Ok(coll),
        }
    }

    /// `_load(path)`: `(version, data)`, `None` when absent.
    fn load(&self, path: &str) -> Result<Option<Found>, PyStoreError> {
        let unreadable =
            |e: String| PyStoreError::Store(format!("unreadable document {path}: {e}"));
        let text = match read_text(Path::new(path)) {
            Ok(t) => t,
            Err(LoadError::NotFound(_)) => return Ok(None),
            Err(LoadError::Other(e)) => return Err(unreadable(e)),
        };
        let raw = loads(&text).map_err(|e| unreadable(e.py_str()))?;
        let PyValue::Dict(mut pairs) = raw else {
            return Err(PyStoreError::Store(format!(
                "document {path} is not a JSON object"
            )));
        };
        let version = match pairs.iter().position(|(k, _)| k == "version") {
            Some(i) => pairs.remove(i).1,
            None => PyValue::Int(PyInt::Small(1)),
        };
        let version = match version {
            PyValue::Int(v) if crate::pyval::cmp_int(&v, &PyInt::Small(1)).is_ge() => v,
            _ => PyInt::Small(1),
        };
        Ok(Some((version, pairs)))
    }

    /// `read_coll` for this store, a `JsonlError` re-raised as a `StoreError`.
    fn read_coll(&self, coll: &str) -> Result<Vec<pyjsonl::Line>, PyStoreError> {
        pyjsonl::read_coll(&self.root, coll).map_err(from_jsonl)
    }

    /// `_jcoll(coll)`: the collection's lines (no parse cache).
    fn jcoll(&self, coll: &PyValue) -> Result<Vec<pyjsonl::Line>, PyStoreError> {
        let coll = self.guard(coll)?;
        let path = pyjsonl::coll_path(&self.root, coll);
        match std::fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(PyStoreError::os(&path, &e)),
            Ok(_) => {}
        }
        self.read_coll(coll)
    }

    /// `get(coll, doc_id)`.
    ///
    /// # Errors
    /// [`PyStoreError::NotFound`] when absent; a bad name, the other format,
    /// an unreadable file.
    pub(crate) fn get(&self, coll: &PyValue, id: &PyValue) -> Result<PyDocOut, PyStoreError> {
        let found = match self.fmt {
            Fmt::Jsonl => {
                let id = check_id("doc id", id)?;
                self.jcoll(coll)?
                    .into_iter()
                    .find(|(i, _, _)| i == id)
                    .map(|(_, v, d)| (v, d))
            }
            Fmt::Dir => {
                self.guard(coll)?;
                self.load(&self.path(coll, id)?)?
                    .map(|(v, d)| (v, PyValue::Dict(d)))
            }
        };
        let (coll, id) = (check_id("collection", coll)?, check_id("doc id", id)?);
        let (version, data) =
            found.ok_or_else(|| PyStoreError::NotFound(format!("{coll}/{id} not found")))?;
        Ok(PyDocOut {
            id: id.to_owned(),
            version,
            data,
        })
    }

    /// `collections()`.
    ///
    /// # Errors
    /// The `OSError` a listing raises (a missing root is empty).
    pub(crate) fn collections(&self) -> Result<Vec<String>, PyStoreError> {
        if self.fmt == Fmt::Jsonl {
            return pyjsonl::collections(&self.root).map_err(from_jsonl);
        }
        let mut names = match std::fs::read_dir(&self.root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            _ => listdir(&self.root)?,
        };
        names.sort();
        let mut out = Vec::new();
        for n in names {
            let dir = join(&self.root, &n);
            if n.starts_with('.') || n.ends_with(".json.d") || !Path::new(&dir).is_dir() {
                continue;
            }
            if listdir(&dir)?
                .iter()
                .any(|f| f.ends_with(".json") && !f.starts_with('.'))
            {
                out.push(n);
            }
        }
        Ok(out)
    }

    /// `_dir_docs(coll)`.
    fn dir_docs(&self, coll: &str) -> Result<Vec<PyDocOut>, PyStoreError> {
        let cdir = join(&self.root, coll);
        let mut names: Vec<String> = match std::fs::read_dir(&cdir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            _ => listdir(&cdir)?,
        };
        names.retain(|n| n.ends_with(".json") && !n.starts_with('.'));
        names.sort();
        let mut out = Vec::new();
        for n in names {
            if let Some((version, data)) = self.load(&join(&cdir, &n))? {
                let id = n.strip_suffix(".json").unwrap_or(&n).to_owned();
                out.push(PyDocOut {
                    id,
                    version,
                    data: PyValue::Dict(data),
                });
            }
        }
        Ok(out)
    }

    /// `list_docs(coll, status, order_by, desc, limit)`.
    ///
    /// # Errors
    /// A bad name, the other format, an unreadable file.
    pub(crate) fn list_docs(
        &self,
        coll: &PyValue,
        status: Option<&PyValue>,
        order_by: Option<&str>,
        desc: bool,
        limit: Option<i64>,
    ) -> Result<Vec<PyDocOut>, PyStoreError> {
        let name = self.guard(coll)?;
        let mut docs = match self.fmt {
            Fmt::Jsonl => {
                let mut lines = self.jcoll(coll)?;
                lines.sort_by(|a, b| a.0.cmp(&b.0));
                lines
                    .into_iter()
                    .map(|(id, version, data)| PyDocOut { id, version, data })
                    .collect()
            }
            Fmt::Dir => self.dir_docs(name)?,
        };
        if let Some(status) = status {
            docs.retain(|d| {
                crate::dashboard::pyderive::py_eq(get(&d.data, "status"), Some(status))
            });
        }
        if let Some(key) = order_by.filter(|k| !k.is_empty()) {
            let (mut present, absent): (Vec<PyDocOut>, Vec<PyDocOut>) =
                docs.into_iter().partition(|d| get(&d.data, key).is_some());
            let key_of = |d: &PyDocOut| sort_key(get(&d.data, key).unwrap_or(&PyValue::Null));
            present.sort_by(|a, b| {
                let ord = cmp_keys(&key_of(a), &key_of(b));
                if desc { ord.reverse() } else { ord }
            });
            docs = present.into_iter().chain(absent).collect();
        }
        if let Some(limit) = limit {
            docs.truncate(usize::try_from(limit.max(0)).unwrap_or(0));
        }
        Ok(docs)
    }
}

/// `RESERVED.match(coll)`, with Python's `$` before a final newline.
fn reserved(coll: &str) -> bool {
    let c = value::before_final_newline(coll);
    let Some(rest) = c.strip_prefix("changes") else {
        return false;
    };
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    rest.is_empty()
        || rest
            .strip_prefix('-')
            .and_then(|r| r.split_once('-'))
            .is_some_and(|(y, m)| digits(y, 4) && digits(m, 2))
}

/// `os.listdir(path)`, with Python's `OSError`.
fn listdir(path: &str) -> Result<Vec<String>, PyStoreError> {
    let os = |e: std::io::Error| PyStoreError::os(path, &e);
    let mut out = Vec::new();
    for entry in std::fs::read_dir(path).map_err(os)? {
        out.push(
            entry
                .map_err(os)?
                .file_name()
                .to_string_lossy()
                .into_owned(),
        );
    }
    Ok(out)
}
