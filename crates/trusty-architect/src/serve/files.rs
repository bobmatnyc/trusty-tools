//! The service's file watch: mtime polling of the inbox and the transcripts.
//!
//! Why: the crate's dependencies are std, serde, thiserror, tracing and libc
//! (#9171 A1), so `notify` is out. kqueue through libc would need one open
//! descriptor per file to see an append (a directory vnode fires only on
//! entry add/remove) and a second implementation for Linux. A stat of a few
//! dozen inbox files and the hot transcripts once a second costs less than
//! one cycle and is the same code on every host.
//! What: [`FileWatch::scan`] compares each [`WatchRoot`]'s `(mtime, len)`
//! stamps with the last scan and reports per root `Ok(changed)` or the
//! root's read error. A `hot_only` root (the transcripts) stats only the
//! files modified in the last [`HOT_WINDOW`] on a quick scan; a full scan
//! (start and tick) lists the whole tree and finds new files. Excluded paths
//! (the shadow directory, `poll.log`, `poll.lock`) are never read.
//! Test: `inbox_write_triggers_one_cycle_within_debounce`,
//! `burst_of_inbox_writes_is_one_cycle`, `transcript_append_is_an_event`,
//! `watch_root_that_is_not_a_directory_is_a_fault` (tests/serve_loop.rs).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A transcript written within this window is re-statted every quick scan.
pub const HOT_WINDOW: Duration = Duration::from_secs(15 * 60);

/// `(mtime, len)` of one file.
type Stamp = (Option<SystemTime>, u64);

/// One watched directory tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchRoot {
    /// Names the fault kind: `watch-<name>`.
    pub name: String,
    /// The directory.
    pub path: PathBuf,
    /// Levels read: 1 is the directory's own files, 2 adds its subdirectories'.
    pub depth: usize,
    /// Only files with this suffix count (`.jsonl` for transcripts).
    pub suffix: Option<String>,
    /// Quick scans stat only recently written files.
    pub hot_only: bool,
    /// Paths never read (the service's own outputs).
    pub exclude: Vec<PathBuf>,
}

impl WatchRoot {
    /// The inbox: two levels, every file, `exclude` skipped.
    pub fn inbox(path: &Path, exclude: Vec<PathBuf>) -> Self {
        Self {
            name: "inbox".to_owned(),
            path: path.to_owned(),
            depth: 2,
            suffix: None,
            hot_only: false,
            exclude,
        }
    }

    /// Claude Code's transcripts: `<base>/<project>/<id>.jsonl`, hot only.
    pub fn transcripts(base: &Path) -> Self {
        Self {
            name: "transcripts".to_owned(),
            path: base.to_owned(),
            depth: 2,
            suffix: Some(".jsonl".to_owned()),
            hot_only: true,
            exclude: Vec::new(),
        }
    }

    /// The fault kind of this root.
    pub fn kind(&self) -> String {
        format!("watch-{}", self.name)
    }
}

#[derive(Debug)]
struct Watched {
    root: WatchRoot,
    seen: Option<BTreeMap<PathBuf, Stamp>>,
}

/// The mtime watch over several roots; see the module docs.
#[derive(Debug)]
pub struct FileWatch {
    roots: Vec<Watched>,
}

impl FileWatch {
    /// A watch with no baseline yet; the first scan sets it.
    pub fn new(roots: Vec<WatchRoot>) -> Self {
        let roots = roots
            .into_iter()
            .map(|root| Watched { root, seen: None })
            .collect();
        Self { roots }
    }

    /// Re-reads every root. The first good read of a root is its baseline
    /// (never a change). Returns `(fault kind, Ok(changed) | Err(detail))`.
    pub fn scan(&mut self, full: bool, now: SystemTime) -> Vec<(String, Result<bool, String>)> {
        self.roots
            .iter_mut()
            .map(|w| (w.root.kind(), w.scan(full, now)))
            .collect()
    }
}

impl Watched {
    fn scan(&mut self, full: bool, now: SystemTime) -> Result<bool, String> {
        let quick = !full && self.root.hot_only && self.seen.is_some();
        let fresh = if quick {
            self.restat_hot(now)
        } else {
            let mut out = BTreeMap::new();
            list(&self.root, &self.root.path, self.root.depth, &mut out)
                .map_err(|e| format!("cannot read {}: {e}", self.root.path.display()))?;
            out
        };
        let Some(seen) = self.seen.as_mut() else {
            self.seen = Some(fresh);
            return Ok(false);
        };
        if quick {
            let changed = fresh.iter().any(|(p, s)| seen.get(p) != Some(s));
            seen.extend(fresh);
            return Ok(changed);
        }
        let changed = *seen != fresh;
        *seen = fresh;
        Ok(changed)
    }

    /// The hot files' current stamps; a vanished file is a change.
    fn restat_hot(&self, now: SystemTime) -> BTreeMap<PathBuf, Stamp> {
        let Some(seen) = &self.seen else {
            return BTreeMap::new();
        };
        seen.iter()
            .filter(|(_, (mtime, _))| is_hot(*mtime, now))
            .map(|(p, old)| {
                let stamp = fs::metadata(p).map_or((None, old.1), |m| stamp(&m));
                (p.clone(), stamp)
            })
            .collect()
    }
}

fn is_hot(mtime: Option<SystemTime>, now: SystemTime) -> bool {
    mtime.is_some_and(|t| now.duration_since(t).map_or(true, |age| age <= HOT_WINDOW))
}

fn stamp(meta: &fs::Metadata) -> Stamp {
    (meta.modified().ok(), meta.len())
}

/// Lists `dir` into `out` down to `depth` levels. Only the root's own read
/// error is returned; a file that vanishes mid-listing is skipped.
fn list(
    root: &WatchRoot,
    dir: &Path,
    depth: usize,
    out: &mut BTreeMap<PathBuf, Stamp>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if root.exclude.iter().any(|x| x == &path) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if depth > 1 {
                let _ = list(root, &path, depth - 1, out);
            }
        } else if root
            .suffix
            .as_deref()
            .is_none_or(|s| path.to_string_lossy().ends_with(s))
        {
            out.insert(path, stamp(&meta));
        }
    }
    Ok(())
}
