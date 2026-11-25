//! The durable tracked-PR table (#11673 AC1).
//!
//! Why: the daemon loop must survive a restart without re-waking a PM, so the
//! per-PR head, verdict and delivery marks have to be on disk. One JSON file
//! under the data dir is the smallest store that holds them; no existing store
//! keys rows by (repo, PR).
//! What: [`Table`] is `<data dir>/pr-watch.json`, a versioned list of
//! [`WatchRow`]. Every write takes an exclusive lock on `pr-watch.lock`, reads
//! the file, applies one change and renames a temp file over it, so a
//! `tm pr watch rm` and a daemon update never interleave and a row removed by
//! the CLI is never written back. Reads take no lock: the rename is atomic.
//! Test: `table_upsert_keeps_delivery_marks_on_the_same_head`,
//! `table_update_after_remove_writes_nothing`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use super::verdict::Verdict;

/// The table's file name under the data dir.
pub const TABLE_FILE: &str = "pr-watch.json";

/// The lock file every write holds.
pub const LOCK_FILE: &str = "pr-watch.lock";

/// The on-disk format version.
const FORMAT_V: u32 = 1;

/// One watched PR.
///
/// Why: the three `*_key` marks record which delivery steps already ran for a
/// transition, so a retry after a failed step resumes at that step and a
/// restart repeats none of them.
/// What: identity (repo, PR), what the last completed transition saw (head,
/// required contexts, verdict), who to wake (owner, and the owner's ledger
/// launch dir), and the delivery marks.
/// Test: `table_upsert_keeps_delivery_marks_on_the_same_head`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchRow {
    /// `owner/repo`.
    pub repo: String,
    /// The PR number.
    pub pr: u64,
    /// The head SHA the last completed transition saw.
    pub head: String,
    /// The base branch the required contexts were read from.
    pub base: String,
    /// The required contexts read for `head`; empty when none are required.
    #[serde(default)]
    pub required: Vec<String>,
    /// The owner PM's tmux session name.
    pub owner: String,
    /// The owner's ledger launch dir; `None` skips the ledger update.
    #[serde(default)]
    pub project: Option<PathBuf>,
    /// The last verdict judged on `head`; `None` until the first judgement.
    #[serde(default)]
    pub verdict: Option<Verdict>,
    /// The transition key last emitted to the bus.
    #[serde(default)]
    pub emitted_key: Option<String>,
    /// The transition key last written to the owner's ledger.
    #[serde(default)]
    pub ledger_key: Option<String>,
    /// The transition key whose pane wake was delivered.
    #[serde(default)]
    pub last_woken_key: Option<String>,
    /// #11673: transitions delivered; the next key carries `seq + 1`.
    #[serde(default)]
    pub seq: u64,
    /// RFC 3339 time the row was added.
    pub added_at: String,
}

impl WatchRow {
    /// Whether this row is (`repo`, `pr`).
    pub fn is(&self, repo: &str, pr: u64) -> bool {
        self.pr == pr && self.repo.eq_ignore_ascii_case(repo)
    }
}

/// The file body.
#[derive(Debug, Default, Serialize, Deserialize)]
struct TableFile {
    #[serde(default)]
    v: u32,
    #[serde(default)]
    rows: Vec<WatchRow>,
}

/// What [`Table::upsert`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    /// A new row.
    Added,
    /// An existing row, updated in place.
    Updated,
}

/// The table at one directory.
#[derive(Debug, Clone)]
pub struct Table {
    dir: PathBuf,
}

impl Table {
    /// The table in `dir`.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The table under the `trusty-mpm` data dir.
    ///
    /// # Errors
    /// The data dir cannot be resolved.
    pub fn production() -> anyhow::Result<Self> {
        Ok(Self::at(trusty_common::resolve_data_dir("trusty-mpm")?))
    }

    /// The table file's path.
    pub fn path(&self) -> PathBuf {
        self.dir.join(TABLE_FILE)
    }

    /// Every row; an absent file is an empty table.
    ///
    /// # Errors
    /// The file exists but cannot be read or parsed.
    pub fn list(&self) -> anyhow::Result<Vec<WatchRow>> {
        Ok(read(&self.path())?.rows)
    }

    /// Add `row`, or update the row with its (repo, PR).
    ///
    /// What: an update takes the new owner, project, base and, on a changed
    /// head, the new head and required list with the verdict cleared. The
    /// delivery marks are kept, so re-adding never re-wakes a delivered key.
    ///
    /// # Errors
    /// The lock, read or write failed.
    pub fn upsert(&self, row: WatchRow) -> anyhow::Result<Upsert> {
        self.write(
            |rows| match rows.iter_mut().find(|r| r.is(&row.repo, row.pr)) {
                Some(old) => {
                    old.owner = row.owner;
                    old.project = row.project;
                    old.base = row.base;
                    if old.head != row.head {
                        old.head = row.head;
                        old.required = row.required;
                        old.verdict = None;
                    }
                    Upsert::Updated
                }
                None => {
                    rows.push(row);
                    Upsert::Added
                }
            },
        )
    }

    /// Apply `change` to the row (`repo`, `pr`); `false` when it is gone.
    ///
    /// # Errors
    /// The lock, read or write failed.
    pub fn update(
        &self,
        repo: &str,
        pr: u64,
        change: impl FnOnce(&mut WatchRow),
    ) -> anyhow::Result<bool> {
        self.write(|rows| match rows.iter_mut().find(|r| r.is(repo, pr)) {
            Some(row) => {
                change(row);
                true
            }
            None => false,
        })
    }

    /// Remove the row (`repo`, `pr`); `false` when there was none.
    ///
    /// # Errors
    /// The lock, read or write failed.
    pub fn remove(&self, repo: &str, pr: u64) -> anyhow::Result<bool> {
        self.write(|rows| {
            let before = rows.len();
            rows.retain(|r| !r.is(repo, pr));
            rows.len() != before
        })
    }

    /// Lock, read, apply `f`, write back, unlock.
    fn write<T>(&self, f: impl FnOnce(&mut Vec<WatchRow>) -> T) -> anyhow::Result<T> {
        create_private_dir(&self.dir)?;
        let lock_path = self.dir.join(LOCK_FILE);
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("cannot open {}", lock_path.display()))?;
        lock.lock()
            .with_context(|| format!("cannot lock {}", lock_path.display()))?;
        let path = self.path();
        let mut file = read(&path)?;
        let out = f(&mut file.rows);
        file.v = FORMAT_V;
        write_atomic(&path, &file)?;
        drop(lock);
        Ok(out)
    }
}

/// Read the table file; absent is empty.
fn read(path: &Path) -> anyhow::Result<TableFile> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("cannot parse the PR watch table {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TableFile::default()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Write `file` to a temp file beside `path`, then rename it over `path`.
fn write_atomic(path: &Path, file: &TableFile) -> anyhow::Result<()> {
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = (|| -> anyhow::Result<()> {
        let mut out = options.open(&tmp)?;
        out.write_all(&serde_json::to_vec_pretty(file)?)?;
        out.flush()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.with_context(|| format!("cannot write {}", path.display()))
}

/// `dir` and its missing parents, mode 0700 on unix.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}
