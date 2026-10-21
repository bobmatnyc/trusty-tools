//! Store layout, launch-dir anchoring, the per-host code registry and `init`.
//!
//! Why: two PMs in one checkout overwrote one shared prototype store; #9757
//! gives each session its own store, anchored to the session's launch
//! directory (not an agent's worktree cwd), with a code no other live session
//! holds.
//! What: [`launch_dir`] walks up from a cwd and maps a git worktree to its
//! main checkout; [`StorePaths`] names the files of
//! `<launch>/.trusty-mpm/dbatg/<session>/` (LDG-2.1.3); [`init_store`] checks
//! the code against the registry under an exclusive `flock`, then writes the
//! registry and an empty schema-1 `state.json` (temp file + rename). Same code
//! and session again is a no-op. [`load_for_write`] reads a store a writer
//! may change. Nothing here is wired into `tm` yet (#9756 P0).
//! Test: `ledger_two_sessions_in_one_checkout_get_two_stores`,
//! `ledger_init_refuses_a_code_held_by_another_live_session`,
//! `ledger_store_anchors_to_the_launch_dir_from_a_worktree`.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::ids::Code;
use super::limits::{check_writable, normalize_ts};
use super::model::State;
use crate::error::LedgerError;

/// The stores' directory under a launch directory (#10068: `dbatg`, not `datg`).
pub const STORE_ROOT: &str = ".trusty-mpm/dbatg";

/// The files of one session's store.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // each field is the file of the same name
pub struct StorePaths {
    pub dir: PathBuf,
    pub state: PathBuf,
    pub log: PathBuf,
    pub archive: PathBuf,
    pub outbox: PathBuf,
    pub acks: PathBuf,
    pub lock: PathBuf,
}

impl StorePaths {
    /// The store of `session` under `launch_dir`.
    ///
    /// # Errors
    /// [`LedgerError::Invalid`] unless `session` matches
    /// `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$` (so it cannot leave the root).
    pub fn new(launch_dir: &Path, session: &str) -> Result<Self, LedgerError> {
        let mut chars = session.chars();
        let first = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
        let rest = chars.all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
        if !(first && rest && session.len() <= 64) {
            return Err(LedgerError::Invalid(format!(
                "bad session name {session:?}"
            )));
        }
        let dir = launch_dir.join(STORE_ROOT).join(session);
        let file = |name: &str| dir.join(name);
        Ok(Self {
            state: file("state.json"),
            log: file("log.jsonl"),
            archive: file("archive.jsonl"),
            outbox: file("outbox.jsonl"),
            acks: file("acks.json"),
            lock: file(".lock"),
            dir,
        })
    }
}

/// The session's launch directory for a `cwd`: the nearest directory with a
/// `.git` directory, or, for a git worktree (a `.git` file naming
/// `<main>/.git/worktrees/<name>`), the main checkout. No repository: `cwd`.
pub fn launch_dir(cwd: &Path) -> PathBuf {
    for dir in cwd.ancestors() {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return dir.to_path_buf();
        }
        let Ok(text) = fs::read_to_string(&dot_git) else {
            continue;
        };
        let Some(gitdir) = text.trim().strip_prefix("gitdir:") else {
            continue;
        };
        let gitdir = dir.join(gitdir.trim());
        let main = gitdir
            .parent()
            .filter(|p| p.ends_with("worktrees"))
            .and_then(Path::parent)
            .filter(|p| p.ends_with(".git"))
            .and_then(Path::parent);
        return main.unwrap_or(dir).to_path_buf();
    }
    cwd.to_path_buf()
}

/// One code registry entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    /// The session that holds the code.
    pub session: String,
    /// Its launch directory.
    pub project: PathBuf,
    /// When it was registered.
    pub registered_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    codes: BTreeMap<String, Registration>,
}

/// Creates the store of `session` under `launch_dir` with `code`, recording
/// the code in the host registry at `registry` (LDG-2.2.2).
///
/// `is_live` says whether another registration's session still runs; a dead
/// holder's code is taken over. The caller supplies `now` (UTC, minute or
/// second precision) so tests pin it.
///
/// # Errors
/// [`LedgerError::CodeTaken`] when a live session holds `code`, or
/// [`LedgerError::Invalid`] for a bad code, session, time or a store that
/// exists with another code; nothing is written then. [`LedgerError::Io`] /
/// [`LedgerError::Json`] when a file cannot be read or written.
pub fn init_store(
    registry: &Path,
    launch_dir: &Path,
    session: &str,
    code: &str,
    now: &str,
    is_live: &dyn Fn(&Registration) -> bool,
) -> Result<StorePaths, LedgerError> {
    let code = Code::parse(code)?;
    let paths = StorePaths::new(launch_dir, session)?;
    let now = normalize_ts(now)?;
    let _lock = lock(&registry.with_extension("lock"))?;
    let mut reg: Registry = match fs::read_to_string(registry) {
        Ok(text) => serde_json::from_str(&text)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Registry::default(),
        Err(e) => return Err(LedgerError::io(registry)(e)),
    };
    let mine = |r: &Registration| r.session == session && r.project == launch_dir;
    if let Some(holder) = reg
        .codes
        .get(code.as_str())
        .filter(|r| !mine(r) && is_live(r))
    {
        return Err(LedgerError::CodeTaken {
            code: code.as_str().to_owned(),
            session: holder.session.clone(),
        });
    }
    let existing = match fs::read_to_string(&paths.state) {
        Ok(text) => Some(State::from_json(&text)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(LedgerError::io(&paths.state)(e)),
    };
    if let Some(other) = existing.as_ref().filter(|s| s.code != code) {
        return Err(LedgerError::Invalid(format!(
            "store {} exists with code {}",
            paths.dir.display(),
            other.code.as_str()
        )));
    }
    if !reg.codes.get(code.as_str()).is_some_and(mine) {
        let entry = Registration {
            session: session.to_owned(),
            project: launch_dir.to_path_buf(),
            registered_at: now,
        };
        reg.codes.insert(code.as_str().to_owned(), entry);
        write_atomic(registry, &(serde_json::to_string_pretty(&reg)? + "\n"))?;
    }
    if existing.is_none() {
        fs::create_dir_all(&paths.dir).map_err(LedgerError::io(&paths.dir))?;
        write_atomic(&paths.state, &State::new(code).to_json()?)?;
    }
    Ok(paths)
}

/// Reads the store at `paths` for a write: parsed, and refused when its
/// schema is newer than [`super::SCHEMA_VERSION`]. Reads only.
///
/// # Errors
/// [`LedgerError::NewerSchema`], [`LedgerError::Json`] or [`LedgerError::Io`].
pub fn load_for_write(paths: &StorePaths) -> Result<State, LedgerError> {
    let text = fs::read_to_string(&paths.state).map_err(LedgerError::io(&paths.state))?;
    let state = State::from_json(&text)?;
    check_writable(&state)?;
    Ok(state)
}

/// An exclusive `flock` on `path`, held until the file drops.
fn lock(path: &Path) -> Result<File, LedgerError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(LedgerError::io(parent))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(LedgerError::io(path))?;
    // SAFETY: the fd is open for the life of `file`; flock touches nothing else.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(LedgerError::io(path)(std::io::Error::last_os_error()));
    }
    Ok(file)
}

/// Writes `text` to a temp file beside `path`, fsyncs it and renames it over.
fn write_atomic(path: &Path, text: &str) -> Result<(), LedgerError> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let result = File::create(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()))
        .and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(LedgerError::io(path))
}
