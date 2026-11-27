//! The throwaway directory a recorded or replayed run sees as its whole world.
//!
//! Why: a recorder that runs a script on live state can mutate it (#12026
//! risk). Every run here gets its own directory of copies, as cwd and `HOME`,
//! under a cleared environment.
//! What: [`Sandbox`] creates and removes the directory, copies seeds in, lays
//! recorded files out, snapshots it, and runs a command in it with only the
//! given environment, `HOME` and the fixed [`SANDBOX_PATH`].
//! Test: `record_captures_argv_stdin_env_files_and_writes`,
//! `record_keeps_env_outside_the_allowlist_out`, `record_refuses_an_oversized_seed`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::{ROOT_TOKEN, ReplayError, SANDBOX_PATH};
use crate::process::{Finished, RunFailure, run_bounded_input};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// File path (relative, `/`-separated) to its bytes.
pub(super) type Snapshot = BTreeMap<String, Vec<u8>>;

fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> ReplayError {
    let what = what.into();
    move |source| ReplayError::Io { what, source }
}

/// A fresh directory under the system temp dir, removed on drop.
pub(super) struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    pub(super) fn new(tag: &str) -> Result<Self, ReplayError> {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("trusty-replay-{tag}-{}-{n}", std::process::id()));
        // A leftover from a crashed run with the same pid is stale; start clean.
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).map_err(io("create sandbox"))?;
        let root = std::fs::canonicalize(&base).map_err(io("canonicalize sandbox"))?;
        Ok(Self { root })
    }

    /// The canonical sandbox root.
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// `s` with every [`ROOT_TOKEN`] replaced by this sandbox's root.
    pub(super) fn expand(&self, s: &str) -> String {
        s.replace(ROOT_TOKEN, &self.root.to_string_lossy())
    }

    /// The absolute path of relative `rel`, refusing `..` and absolute forms.
    fn join(&self, rel: &str) -> Result<PathBuf, ReplayError> {
        let p = Path::new(rel);
        let plain = p
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
        if rel.is_empty() || !plain {
            return Err(ReplayError::BadArg(format!(
                "not a plain relative path: {rel}"
            )));
        }
        Ok(self.root.join(p))
    }

    /// Copies file or directory `src` (live state, read only) to `rel`.
    /// Fails once the copied bytes would pass `budget`.
    pub(super) fn seed(&self, src: &Path, rel: &str, budget: &mut u64) -> Result<(), ReplayError> {
        copy_tree(src, &self.join(rel)?, budget)
    }

    /// Writes `content` (with [`ROOT_TOKEN`] expanded) to `rel`.
    pub(super) fn put(&self, rel: &str, content: &str) -> Result<(), ReplayError> {
        let dest = self.join(rel)?;
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).map_err(io("create input dir"))?;
        }
        std::fs::write(&dest, self.expand(content)).map_err(io("write input file"))
    }

    /// Every regular file under the root, by relative path.
    pub(super) fn snapshot(&self) -> Result<Snapshot, ReplayError> {
        let mut out = Snapshot::new();
        walk(&self.root, &self.root, &mut out)?;
        Ok(out)
    }

    /// Runs `program prefix.. argv..` in the sandbox with exactly `env` plus
    /// `HOME` (the root) and `PATH` ([`SANDBOX_PATH`]); every value expanded.
    pub(super) fn run(
        &self,
        program: &Path,
        prefix: &[String],
        argv: &[String],
        env: &BTreeMap<String, String>,
        stdin: &str,
        timeout: Duration,
    ) -> Result<Finished, ReplayError> {
        let mut cmd = Command::new(program);
        cmd.args(prefix)
            .args(argv.iter().map(|a| self.expand(a)))
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", &self.root)
            .env("PATH", SANDBOX_PATH);
        for (k, v) in env {
            cmd.env(k, self.expand(v));
        }
        run_bounded_input(cmd, Some(self.expand(stdin).into_bytes()), timeout).map_err(
            |e| match e {
                RunFailure::Spawn(e) | RunFailure::Wait(e) => ReplayError::Spawn(e),
                RunFailure::Timeout => ReplayError::Timeout,
            },
        )
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn copy_tree(src: &Path, dest: &Path, budget: &mut u64) -> Result<(), ReplayError> {
    let meta = std::fs::metadata(src).map_err(io("read seed"))?;
    if meta.is_dir() {
        std::fs::create_dir_all(dest).map_err(io("create seed dir"))?;
        for entry in std::fs::read_dir(src).map_err(io("list seed dir"))? {
            let entry = entry.map_err(io("list seed dir"))?;
            copy_tree(&entry.path(), &dest.join(entry.file_name()), budget)?;
        }
        return Ok(());
    }
    *budget = budget
        .checked_sub(meta.len())
        .ok_or_else(|| ReplayError::BadArg("seeds pass the size limit; seed fewer files".into()))?;
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(io("create seed dir"))?;
    }
    std::fs::copy(src, dest)
        .map(|_| ())
        .map_err(io("copy seed"))
}

fn walk(root: &Path, dir: &Path, out: &mut Snapshot) -> Result<(), ReplayError> {
    for entry in std::fs::read_dir(dir).map_err(io("list sandbox"))? {
        let path = entry.map_err(io("list sandbox"))?.path();
        let meta = std::fs::symlink_metadata(&path).map_err(io("stat sandbox file"))?;
        if meta.is_dir() {
            walk(root, &path, out)?;
        } else if meta.is_file() {
            let rel = rel_key(root, &path);
            out.insert(rel, std::fs::read(&path).map_err(io("read sandbox file"))?);
        }
    }
    Ok(())
}

/// `path` relative to `root`, `/`-separated.
pub(super) fn rel_key(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let parts: Vec<String> = rel
        .iter()
        .map(|c| c.to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

/// Files `after` has that `before` lacks or holds differently (`Some`), and
/// files `before` has that `after` lost (`None`).
pub(super) fn changes(before: &Snapshot, after: &Snapshot) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for (k, v) in after {
        if before.get(k) != Some(v) {
            out.insert(k.clone(), Some(v.clone()));
        }
    }
    for k in before.keys().filter(|k| !after.contains_key(*k)) {
        out.insert(k.clone(), None);
    }
    out
}
