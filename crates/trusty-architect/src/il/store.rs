//! Where a ledger lives and how its files are written safely.
//!
//! Why: the Python scripts block forever on `flock`, skip the directory fsync,
//! and write a log line in several buffered pieces. The port keeps the files,
//! the lock file and the resolution rules, and fixes those three gaps.
//! What: state-directory resolution (`--state-dir`, `$DATG_STATE_DIR`,
//! `--session`, `$DATG_SESSION`, tmux), a bounded exclusive lock on `.lock` (the
//! file the Python also locks, so mixed writers serialise), the atomic write
//! (temp, fsync file, rename, fsync dir), and the one-`write` append that
//! repairs a missing final newline first.
//! Test: `resolve_*`, `lock_times_out_when_held`, `atomic_write_replaces_and_leaves_no_temp`,
//! `append_lines_is_one_write_and_repairs_a_torn_tail`.

use super::error::{IlError, IlResult, invalid};
use super::json::Json;
use crate::pystr::py_repr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a writer waits for `.lock` before it gives up (exit 2, nothing changed).
pub const LOCK_WAIT: Duration = Duration::from_secs(10);

/// The inputs of directory resolution (`datg.py` `datg_dir`).
#[derive(Debug, Clone, Default)]
pub struct Where {
    /// `--project` (default: the working directory).
    pub project: PathBuf,
    /// `--session NAME`.
    pub session: Option<String>,
    /// `--state-dir DIR` or `$DATG_STATE_DIR`: used exactly.
    pub state_dir: Option<PathBuf>,
    /// `$DATG_SESSION`.
    pub env_session: Option<String>,
    /// Ask tmux for the pane's session when `$TMUX_PANE` is set.
    pub tmux_pane: Option<String>,
}

/// The files of one ledger directory.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The state directory.
    pub dir: PathBuf,
    /// `state.json`.
    pub state: PathBuf,
    /// `log.jsonl`.
    pub log: PathBuf,
    /// `archive.jsonl`.
    pub archive: PathBuf,
    /// `.lock`.
    pub lock: PathBuf,
    /// `stamp.json`.
    pub stamp: PathBuf,
}

impl Paths {
    /// The paths under `dir`.
    pub fn at(dir: PathBuf) -> Self {
        Self {
            state: dir.join("state.json"),
            log: dir.join("log.jsonl"),
            archive: dir.join("archive.jsonl"),
            lock: dir.join(".lock"),
            stamp: dir.join("stamp.json"),
            dir,
        }
    }
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`.
pub fn session_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn tmux_session(pane: &str) -> Option<String> {
    let out = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-t", pane, "#{session_name}"])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !name.is_empty()).then_some(name)
}

/// `per_pm_session`: the per-PM directory name in use, or `None` for the legacy directory.
///
/// # Errors
/// `Invalid` for a `--session` that is not a plain name.
pub fn per_pm_session(w: &Where) -> IlResult<Option<String>> {
    let base = w.project.join(".trusty-mpm").join("datg");
    let has_state = |name: &str| base.join(name).join("state.json").is_file();
    if let Some(s) = &w.session {
        if !session_ok(s) {
            return invalid(format!("bad session name {}", py_repr(s)));
        }
        return Ok(Some(s.clone()));
    }
    if let Some(e) = w
        .env_session
        .as_deref()
        .filter(|e| session_ok(e) && has_state(e))
    {
        return Ok(Some(e.to_owned()));
    }
    let Ok(entries) = fs::read_dir(&base) else {
        return Ok(None);
    };
    let has_subs = entries
        .flatten()
        .any(|e| e.path().is_dir() && e.path().join("state.json").is_file());
    if has_subs
        && let Some(pane) = &w.tmux_pane
        && let Some(t) = tmux_session(pane).filter(|t| session_ok(t) && has_state(t))
    {
        return Ok(Some(t));
    }
    Ok(None)
}

/// The paths of the ledger `w` names.
///
/// # Errors
/// `Invalid` for a bad session name.
pub fn resolve(w: &Where) -> IlResult<Paths> {
    if let Some(d) = w.state_dir.as_ref().filter(|d| !d.as_os_str().is_empty()) {
        return Ok(Paths::at(d.clone()));
    }
    let base = w.project.join(".trusty-mpm").join("datg");
    Ok(Paths::at(match per_pm_session(w)? {
        Some(s) => base.join(s),
        None => base,
    }))
}

/// An exclusive lock on `.lock`, released when dropped.
#[derive(Debug)]
pub struct LockGuard(File);

impl LockGuard {
    /// The lock file's descriptor, for tests.
    pub fn raw_fd(&self) -> i32 {
        self.0.as_raw_fd()
    }
}

/// Takes `LOCK_EX` on `<dir>/.lock`, creating the directory and file, waiting at most `wait`.
///
/// # Errors
/// `Locked` when another process holds it past `wait`; `Io` otherwise.
pub fn lock(paths: &Paths, wait: Duration) -> IlResult<LockGuard> {
    fs::create_dir_all(&paths.dir).map_err(IlError::io("create", &paths.dir))?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&paths.lock)
        .map_err(IlError::io("open", &paths.lock))?;
    let start = Instant::now();
    loop {
        // SAFETY: the descriptor is open for the life of `file`; flock touches nothing else.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(LockGuard(file));
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::WouldBlock {
            return Err(IlError::io("lock", &paths.lock)(err));
        }
        if start.elapsed() >= wait {
            return Err(IlError::Locked(format!(
                "ledger locked: {} is held by another writer (waited {} ms); nothing changed",
                paths.lock.display(),
                wait.as_millis()
            )));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn sync_dir(path: &Path) -> IlResult<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(IlError::io("fsync dir", dir))
}

/// Writes `bytes` to `<path>.tmp.<pid>`, fsyncs it, renames it over `path`, fsyncs the directory.
/// `fail_rename` simulates a failed rename for tests; the temp file is removed either way.
///
/// # Errors
/// `Io`; the old file is untouched on any error before the rename.
pub fn atomic_write(path: &Path, bytes: &[u8], fail_rename: bool) -> IlResult<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp.{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let result = File::create(&tmp)
        .and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
        .and_then(|()| {
            if fail_rename {
                Err(std::io::Error::other("injected rename failure"))
            } else {
                fs::rename(&tmp, path)
            }
        });
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(IlError::io("write", path)(e));
    }
    sync_dir(path)
}

/// Appends `bytes` to `path` with one `write` on an `O_APPEND` descriptor, then fsyncs.
/// A file that does not end in a newline gets one first, so a torn tail never merges with the new line.
///
/// # Errors
/// `Io`, with the file possibly unchanged.
pub fn append_lines(path: &Path, bytes: &[u8]) -> IlResult<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let mut f = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)
        .map_err(IlError::io("open", path))?;
    let len = f.metadata().map_err(IlError::io("stat", path))?.len();
    let mut buf = Vec::with_capacity(bytes.len() + 1);
    if len > 0 {
        let mut last = [0u8; 1];
        f.seek(SeekFrom::Start(len - 1))
            .map_err(IlError::io("seek", path))?;
        f.read_exact(&mut last).map_err(IlError::io("read", path))?;
        if last[0] != b'\n' {
            buf.push(b'\n');
        }
    }
    buf.extend_from_slice(bytes);
    f.write_all(&buf).map_err(IlError::io("append", path))?;
    f.sync_all().map_err(IlError::io("fsync", path))?;
    if len == 0 {
        sync_dir(path)?;
    }
    Ok(())
}

/// The size of `path` in bytes, 0 when it is missing.
pub fn size_of(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

/// `load_state`: the parsed `state.json`, refused when missing or badly shaped.
///
/// # Errors
/// `Invalid` when missing; `Corrupt` when unreadable, not JSON, or the wrong shape.
pub fn load_state(paths: &Paths) -> IlResult<Json> {
    let bytes = match fs::read(&paths.state) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return invalid("state.json missing; run init");
        }
        Err(e) => {
            return Err(IlError::Corrupt(format!(
                "state.json malformed ({e}); refusing to write"
            )));
        }
    };
    let corrupt =
        |why: &str| IlError::Corrupt(format!("state.json malformed ({why}); refusing to write"));
    let text = String::from_utf8(bytes).map_err(|e| corrupt(&e.to_string()))?;
    let st = Json::parse(&text).map_err(|e| corrupt(&e))?;
    let ok = matches!(&st, Json::Obj(_))
        && super::ids::is_code(st.s("code"))
        && st.get("v").and_then(Json::as_i64).is_some()
        && matches!(st.get("next"), Some(Json::Obj(_)))
        && ["goals", "items", "tasks"]
            .iter()
            .all(|k| matches!(st.get(k), Some(Json::Arr(_))))
        && matches!(
            st.get("session").unwrap_or(&Json::Obj(Vec::new())),
            Json::Obj(_)
        );
    if !ok {
        return Err(corrupt("bad shape"));
    }
    Ok(st)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn resolve_prefers_state_dir_then_session_then_legacy() {
        let t = tmp();
        let mut w = Where {
            project: t.path().to_path_buf(),
            ..Where::default()
        };
        assert_eq!(resolve(&w).unwrap().dir, t.path().join(".trusty-mpm/datg"));
        w.session = Some("pm-1".into());
        assert_eq!(
            resolve(&w).unwrap().dir,
            t.path().join(".trusty-mpm/datg/pm-1")
        );
        w.session = Some("bad/name".into());
        assert!(resolve(&w).is_err());
        w.state_dir = Some(t.path().join("exact"));
        assert_eq!(resolve(&w).unwrap().dir, t.path().join("exact"));
    }

    #[test]
    fn resolve_env_session_needs_an_existing_state_json() {
        let t = tmp();
        let sub = t.path().join(".trusty-mpm/datg/pm-2");
        fs::create_dir_all(&sub).unwrap();
        let w = Where {
            project: t.path().to_path_buf(),
            env_session: Some("pm-2".into()),
            ..Where::default()
        };
        assert_eq!(resolve(&w).unwrap().dir, t.path().join(".trusty-mpm/datg"));
        fs::write(sub.join("state.json"), "{}").unwrap();
        assert_eq!(resolve(&w).unwrap().dir, sub);
    }

    #[test]
    fn lock_times_out_when_held() {
        let t = tmp();
        let p = Paths::at(t.path().to_path_buf());
        let held = lock(&p, Duration::from_secs(1)).unwrap();
        let err = lock(&p, Duration::from_millis(60)).unwrap_err();
        assert!(matches!(err, IlError::Locked(_)), "{err}");
        drop(held);
        assert!(lock(&p, Duration::from_millis(60)).is_ok());
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let t = tmp();
        let f = t.path().join("state.json");
        fs::write(&f, "old").unwrap();
        atomic_write(&f, b"new", false).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "new");
        assert!(atomic_write(&f, b"newer", true).is_err());
        assert_eq!(fs::read_to_string(&f).unwrap(), "new");
        assert_eq!(fs::read_dir(t.path()).unwrap().count(), 1);
    }

    #[test]
    fn append_lines_is_one_write_and_repairs_a_torn_tail() {
        let t = tmp();
        let f = t.path().join("log.jsonl");
        append_lines(&f, b"{\"a\":1}\n").unwrap();
        fs::write(&f, "{\"a\":1}\n{\"torn\":").unwrap();
        append_lines(&f, b"{\"b\":2}\n").unwrap();
        assert_eq!(
            fs::read_to_string(&f).unwrap(),
            "{\"a\":1}\n{\"torn\":\n{\"b\":2}\n"
        );
    }
}
