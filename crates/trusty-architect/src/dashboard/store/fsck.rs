//! `fsck`: the store's files against its change log.
//!
//! Why: DOC-82 LDG-2.8.3 says `fsck` verifies `seq` continuity and that the
//! last version of each document matches its change line (#10063 AC 4), so a
//! lost append or a hand edit is found. The prototype's `fsck` ran on `jsonl`
//! stores only; the Rust log covers both formats, so `fsck` does too.
//! What: [`Store::fsck`] returns problem lines, empty when clean: a corrupt
//! document or collection file, a `jsonl` file whose bytes are not
//! canonical, the change log's own problems (torn or unreadable lines, a
//! `seq` gap or reversal), and per document whose last change line
//! disagrees with the store: logged deleted but present, logged but missing,
//! a version that differs, or a hash that differs from the bytes on disk.
//! Once a change log exists, a document on disk that no line names is
//! reported too: it was written while the log was not (#10063 df-G01, the
//! `Unlogged` error says "run fsck"). Messages the prototype also prints
//! keep its words. `dir` files are not checked for canonical bytes: the
//! page's mirror writes them too.
//! Test: `fsck_reports_gap`, `fsck_reports_version_mismatch`,
//! `fsck_clean_after_writes`, `fsck_reports_torn_change_line`,
//! `fsck_reports_documents_with_no_change_line`, `fsck_missing_root_is_an_error`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use super::super::error::DashError;
use super::{Format, Store, changes, dir, jsonl};
use crate::sha256::sha256_hex;

/// `(version, sha256 of the bytes on disk)` per `(collection, id)`.
type OnDisk = BTreeMap<(String, String), (u64, String)>;

impl Store {
    /// Problems in the store, as lines; empty when it is consistent.
    ///
    /// # Errors
    /// [`DashError::NotFound`] when the root does not exist (a missing store
    /// is never clean), [`DashError::WrongFormat`], [`DashError::Io`].
    pub fn fsck(&self) -> Result<Vec<String>, DashError> {
        require_root(&self.root)?;
        let mut problems = Vec::new();
        let mut disk = OnDisk::new();
        for coll in self.collections()? {
            match self.format {
                Format::Jsonl => self.fsck_jsonl(&coll, &mut disk, &mut problems)?,
                Format::Dir => self.fsck_dir(&coll, &mut disk, &mut problems)?,
            }
        }
        let (last, log_problems) = changes::read_all(&self.root)?;
        problems.extend(log_problems);
        let logged = !changes::logs(&self.root)?.is_empty();
        let keys: BTreeSet<&(String, String)> = disk.keys().chain(last.keys()).collect();
        for key in keys {
            problems.extend(doc_problem(key, disk.get(key), last.get(key), logged));
        }
        Ok(problems)
    }

    fn fsck_jsonl(
        &self,
        coll: &str,
        disk: &mut OnDisk,
        problems: &mut Vec<String>,
    ) -> Result<(), DashError> {
        let path = jsonl::coll_path(&self.root, coll);
        let blob = fs::read(&path).map_err(DashError::io(&path))?;
        let docs = match jsonl::parse_file(&blob, &path) {
            Ok(d) => d,
            Err(e) => {
                problems.push(e.to_string());
                return Ok(());
            }
        };
        if blob != jsonl::render_file(&docs).as_bytes() {
            problems.push(format!(
                "{}: bytes are not canonical (re-render differs)",
                path.display()
            ));
        }
        for (id, (version, data)) in &docs {
            let line = jsonl::render_line(id, *version, data);
            let sha = sha256_hex(line.as_bytes());
            disk.insert((coll.to_owned(), id.clone()), (*version, sha));
        }
        Ok(())
    }

    fn fsck_dir(
        &self,
        coll: &str,
        disk: &mut OnDisk,
        problems: &mut Vec<String>,
    ) -> Result<(), DashError> {
        for id in dir::ids(&self.root, coll)? {
            let path = dir::doc_path(&self.root, coll, &id);
            let blob = fs::read(&path).map_err(DashError::io(&path))?;
            match dir::load(&path) {
                Ok(Some((version, _))) => {
                    disk.insert((coll.to_owned(), id), (version, sha256_hex(&blob)));
                }
                Ok(None) => {}
                Err(e) => problems.push(e.to_string()),
            }
        }
        Ok(())
    }
}

/// The problem with `coll/id`, given its `(version, sha256)` on disk and its
/// last change line; `logged` says a change log exists.
fn doc_problem(
    (coll, id): &(String, String),
    have: Option<&(u64, String)>,
    entry: Option<&changes::Entry>,
    logged: bool,
) -> Option<String> {
    let Some(e) = entry else {
        // #10063 df-G01: a document no line names was written unlogged.
        return (logged && have.is_some())
            .then(|| format!("{coll}/{id}: present but no change line"));
    };
    let seq = e.seq;
    match have {
        Some(_) if e.op == "delete" => Some(format!(
            "{coll}/{id}: logged deleted (seq {seq}) but present in the file"
        )),
        None if e.op == "delete" => None,
        None => Some(format!(
            "{coll}/{id}: logged at version {} (seq {seq}) but missing",
            e.version
        )),
        Some((version, _)) if *version != e.version => Some(format!(
            "{coll}/{id}: version {version} differs from last logged version {} (seq {seq})",
            e.version
        )),
        Some((_, sha)) if e.sha256.as_deref() == Some(sha) => None,
        Some(_) => Some(format!(
            "{coll}/{id}: file line hash differs from last logged hash (seq {seq})"
        )),
    }
}

/// Refuses a store root that does not exist or is not a directory.
pub(super) fn require_root(root: &Path) -> Result<(), DashError> {
    match fs::metadata(root) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(DashError::Invalid(format!(
            "store root {} is not a directory",
            root.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(DashError::NotFound(format!(
            "store root {} does not exist",
            root.display()
        ))),
        Err(e) => Err(DashError::io(root)(e)),
    }
}
