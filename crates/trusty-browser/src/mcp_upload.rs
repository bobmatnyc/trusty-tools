//! `page_upload`'s upload root: which local files the MCP server may send.
//!
//! Why: the browser cannot read the agent's disk, and a hostile page must not
//! steer the agent into sending `~/.ssh` or `.env` (Python `agent_mcp.py`
//! `upload_file`, PR #11305 review). The checks and their refusal texts must
//! match Python 0.7.0.
//! What: [`upload_root`] resolves `--upload-root`. [`upload_files`] refuses
//! more than [`UPLOAD_FILES`] paths before any read, then reads each file one
//! component at a time from a root directory fd with `O_NOFOLLOW`, so no
//! symlink below the root is followed. The total is capped at [`UPLOAD_MAX`]
//! while reading. Each entry is `{name, type, data}`.
//! Test: the `page_upload` steps of `connect_exchange_replays_byte_identically`
//! and `noroot_exchange_replays_byte_identically` in `tests/parity_mcp.rs`.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::mcp_mime::guess_type;
use crate::pyjson::Json;

/// Total bytes one `page_upload` may carry (`UPLOAD_MAX`).
pub const UPLOAD_MAX: usize = 512 * 1024;
/// Most paths one `page_upload` may name (`UPLOAD_FILES`).
pub const UPLOAD_FILES: usize = 10;
/// The `UploadsDisabled` message.
pub const UPLOADS_DISABLED: &str =
    "Uploads are off: start the MCP server with --upload-root DIR to allow files under DIR";

/// A local refusal frame `{"ok":false,"error":…,"message":…}` (Python `local`).
pub fn local(error: &str, message: impl Into<String>) -> Json {
    Json::object([
        ("ok", Json::Bool(false)),
        ("error", Json::from(error)),
        ("message", Json::Str(message.into())),
    ])
}

/// Resolve `--upload-root` to the real path of an existing directory.
pub fn upload_root(raw: &str) -> Result<PathBuf, String> {
    match fs::canonicalize(raw) {
        Ok(p) if p.is_dir() => Ok(p),
        _ => Err(format!("--upload-root {raw} is not a directory")),
    }
}

/// `os.path.realpath` (non-strict): the longest existing prefix is resolved,
/// the rest is appended as written.
fn realpath(p: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        if let Ok(real) = fs::canonicalize(&cur) {
            return tail
                .iter()
                .rev()
                .fold(real, |acc: PathBuf, c: &std::ffi::OsString| acc.join(c));
        }
        match (cur.file_name().map(ToOwned::to_owned), cur.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                cur = parent.to_path_buf();
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// `PurePosixPath(raw).parts`, without the root: `.` and empty parts dropped.
fn parts(raw: &str) -> Vec<&str> {
    raw.split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect()
}

/// The components of `raw` below `root` (Python `_inside`).
fn inside(root: &Path, raw: &str) -> Result<Vec<String>, &'static str> {
    let ps = parts(raw);
    if ps.contains(&"..") {
        return Err("contains a '..' component");
    }
    let rel: Vec<String> = if !raw.starts_with('/') {
        ps.iter().map(|s| (*s).to_owned()).collect()
    } else if realpath(Path::new(raw)) == root {
        Vec::new()
    } else {
        // The caller may name the root through an alias (/tmp vs /private/tmp),
        // so resolve only the parent; the final component is never followed.
        let (dir, base) = match raw.rfind('/') {
            Some(0) => ("/", &raw[1..]),
            Some(i) => (&raw[..i], &raw[i + 1..]),
            None => ("", raw),
        };
        let parent = realpath(Path::new(dir));
        let below = parent
            .strip_prefix(root)
            .map_err(|_| "is outside the upload root")?;
        let mut rel: Vec<String> = below
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        rel.push(base.to_owned());
        rel
    };
    let comps: Vec<String> = rel
        .into_iter()
        .filter(|c| !c.is_empty() && c != ".")
        .collect();
    if comps.is_empty() {
        return Err("is not a regular file");
    }
    Ok(comps)
}

fn openat(dir: Option<&OwnedFd>, name: &str, flags: libc::c_int) -> io::Result<OwnedFd> {
    let c = CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let at = dir.map_or(libc::AT_FDCWD, AsRawFd::as_raw_fd);
    // SAFETY: `c` is a NUL-terminated path and `at` is AT_FDCWD or a live fd we own.
    let fd = unsafe { libc::openat(at, c.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by openat and is owned by nothing else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

enum Read {
    Data(Vec<u8>),
    NotRegular,
}

/// Open `comps` below `root` with no symlink followed and read at most `limit` bytes.
fn read_below(root: &Path, comps: &[String], limit: usize) -> io::Result<Read> {
    let root_s = root
        .to_str()
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    let mut dir = openat(None, root_s, libc::O_RDONLY | libc::O_DIRECTORY)?;
    let (leaf, dirs) = comps
        .split_last()
        .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
    for comp in dirs {
        dir = openat(
            Some(&dir),
            comp,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )?;
    }
    // O_NONBLOCK: opening a FIFO must not hang the tool.
    let fd = openat(
        Some(&dir),
        leaf,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
    )?;
    let file = fs::File::from(fd);
    if !file.metadata()?.file_type().is_file() {
        return Ok(Read::NotRegular);
    }
    let mut data = Vec::new();
    io::Read::read_to_end(&mut io::Read::take(&file, limit as u64 + 1), &mut data)?;
    Ok(Read::Data(data))
}

/// One `page_upload` entry from a file under `root`, reading at most `budget`
/// bytes (Python `upload_file`). Errors are refusal frames.
pub fn upload_file(raw: &Json, root: Option<&Path>, budget: usize) -> Result<Json, Json> {
    entry(raw, root, budget).map(|(e, _)| e)
}

/// [`upload_file`] plus the number of bytes read.
fn entry(raw: &Json, root: Option<&Path>, budget: usize) -> Result<(Json, usize), Json> {
    let root = root.ok_or_else(|| local("UploadsDisabled", UPLOADS_DISABLED))?;
    let raw = match raw {
        Json::Str(s) if !s.is_empty() && !s.contains('\0') => s.as_str(),
        _ => {
            return Err(local(
                "InvalidRequest",
                "paths must be non-empty file path strings",
            ));
        }
    };
    let refuse = |reason: &str| local("FileRefused", format!("{raw}: {reason}"));
    let comps = inside(root, raw).map_err(refuse)?;
    let data = match read_below(root, &comps, budget) {
        Ok(Read::Data(d)) => d,
        Ok(Read::NotRegular) => return Err(refuse("is not a regular file")),
        Err(e) => {
            return Err(refuse(match e.raw_os_error() {
                Some(libc::ENOENT) => "does not exist",
                Some(libc::ENOTDIR) => "is a symlink or not under a directory",
                Some(libc::ELOOP) => "is a symlink",
                _ => "cannot be read",
            }));
        }
    };
    if data.len() > budget {
        if budget < UPLOAD_MAX {
            return Err(refuse(&format!(
                "puts the upload over {UPLOAD_MAX} bytes in total"
            )));
        }
        return Err(refuse(&format!("is larger than {UPLOAD_MAX} bytes")));
    }
    let name = comps.last().cloned().unwrap_or_default();
    let kind = guess_type(&name).unwrap_or_else(|| "application/octet-stream".to_owned());
    let size = data.len();
    let entry = Json::object([
        ("name", Json::Str(name)),
        ("type", Json::Str(kind)),
        ("data", Json::Str(STANDARD.encode(&data))),
    ]);
    Ok((entry, size))
}

/// Every `page_upload` entry, refusing a bad path list before any read
/// (Python `upload_files`).
pub fn upload_files(paths: &Json, root: Option<&Path>) -> Result<Json, Json> {
    if root.is_none() {
        return Err(local("UploadsDisabled", UPLOADS_DISABLED));
    }
    let list = match paths {
        Json::Array(items) if (1..=UPLOAD_FILES).contains(&items.len()) => items,
        _ => {
            return Err(local(
                "InvalidRequest",
                format!("paths must list 1 to {UPLOAD_FILES} files"),
            ));
        }
    };
    let mut left = UPLOAD_MAX;
    let mut entries = Vec::new();
    for raw in list {
        let (one, used) = entry(raw, root, left)?;
        left = left.saturating_sub(used);
        entries.push(one);
    }
    Ok(Json::Array(entries))
}
