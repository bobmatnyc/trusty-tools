//! Where an attachment lands: `attach_dir`, `_safe_dir` and `dashboard_il`'s schedule read (#12029 P2h).
//!
//! Why: an owner upload must land beside the Intent Ledger of the project
//! that owns the item, and never through a symlinked directory, so a link
//! planted in the tree cannot redirect the write.
//! What: [`read_schedule`] (`_read_schedule`: the `sessions` entries with a
//! non-empty string `name` and `project`; an unreadable file gives none),
//! [`session_ok`] (`SESSION_RE`), [`attach_dir`] (the `(dir, il label)`
//! rule, `architect-fallback` when no entry places the session) and
//! [`safe_dir`] (the link and plain-directory checks around one `makedirs`).
//! The schedule path is the caller's: the poller's `IL_SCHEDULE` resolution
//! is not needed here.
//! Test: `attach_dir_follows_the_schedule_and_falls_back`,
//! `a_symlinked_item_dir_is_refused_and_nothing_is_written`.

use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use crate::dashboard::pyjsonl::join;
use crate::dashboard::store::py::{PyStoreError, abspath, dirname};
use crate::pyjson_file::read_text;
use crate::pyjson_load::{PyValue, loads};
use crate::pyval::get;

/// One schedule entry: `{name, project, code}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The tmux session name.
    pub name: String,
    /// The project directory.
    pub project: String,
    /// The short code, `""` when absent or not a string.
    pub code: String,
}

/// `_read_schedule(path)`: the `sessions` list of `il-schedule.json`.
pub fn read_schedule(path: &Path) -> Vec<Session> {
    let Ok(Ok(data)) = read_text(path).map(|t| loads(&t)) else {
        return Vec::new();
    };
    let Some(PyValue::List(rows)) = get(&data, "sessions") else {
        return Vec::new();
    };
    let text = |e: &PyValue, k: &str| match get(e, k) {
        Some(PyValue::Str(s)) => Some(s.clone()),
        _ => None,
    };
    rows.iter()
        .filter(|e| matches!(e, PyValue::Dict(_)))
        .filter_map(|e| {
            let name = text(e, "name").filter(|s| !s.is_empty())?;
            let project = text(e, "project").filter(|s| !s.is_empty())?;
            let code = text(e, "code").unwrap_or_default();
            Some(Session {
                name,
                project,
                code,
            })
        })
        .collect()
}

/// `SESSION_RE.match(s)`: `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`, `$` also before a final `\n`.
pub fn session_ok(s: &str) -> bool {
    let s = s.strip_suffix('\n').unwrap_or(s);
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_alphanumeric())
        && s.chars().count() <= 64
        && c.all(|x| x.is_ascii_alphanumeric() || "._-".contains(x))
}

/// Why: the item's session picks the project whose ledger holds the file.
/// What: `attach_dir(session, schedule_path, architect_il, store_root)`:
/// a session-mode server (`store_root`) writes beside its store
/// (`"session"`); else the first schedule entry named `session` that places
/// it (`.trusty-mpm/il`, the Architect's own IL dir, or
/// `.trusty-mpm/datg/<session>` for a valid name) gives `"project"`; else
/// `<architect_il>/attachments`, `"architect-fallback"`. A `session` of
/// `None` (the item's value is not a string) names no entry.
/// Test: `attach_dir_follows_the_schedule_and_falls_back`.
pub fn attach_dir(
    session: Option<&str>,
    schedule: &Path,
    architect_il: &str,
    store_root: Option<&str>,
    cwd: &Path,
) -> (String, &'static str) {
    if let Some(root) = store_root.filter(|r| !r.is_empty()) {
        return (join(dirname(&abspath(root, cwd)), "attachments"), "session");
    }
    let arch = abspath(architect_il, cwd);
    for e in read_schedule(schedule) {
        let Some(session) = session.filter(|s| *s == e.name) else {
            continue;
        };
        let proj = abspath(&e.project, cwd);
        let il = join(&join(&proj, ".trusty-mpm"), "il");
        if Path::new(&il).is_dir() {
            return (join(&il, "attachments"), "project");
        }
        if proj == arch {
            return (join(&proj, "attachments"), "project");
        }
        if session_ok(session) {
            let d = join(&join(&join(&proj, ".trusty-mpm"), "datg"), session);
            return (join(&d, "attachments"), "project");
        }
    }
    (join(&arch, "attachments"), "architect-fallback")
}

/// `os.path.lexists(p)`.
fn lexists(p: &str) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// `os.path.islink(p)`.
fn islink(p: &str) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

fn not_plain() -> PyStoreError {
    PyStoreError::Store("attachment path is not a plain directory".into())
}

/// `os.path.realpath(p)` of an existing path.
pub(crate) fn realpath(p: &str) -> Result<String, PyStoreError> {
    std::fs::canonicalize(p)
        .map(|r| r.to_string_lossy().into_owned())
        .map_err(|e| PyStoreError::os(p, &e))
}

/// Why: the item directory is created without following a link at any component.
/// What: `_safe_dir(base, item_id)`: the deepest existing ancestor of `base`,
/// `base` and `base/<item_id>` must each be a real directory where they
/// exist; then one `makedirs(mode 0o755)`, a link check on every component
/// it created, and the realpath check. `base` is absolute.
/// Test: `a_symlinked_item_dir_is_refused_and_nothing_is_written`.
///
/// # Errors
/// [`PyStoreError::Store`] for a link or a non-directory, an `OSError` from `makedirs`.
pub fn safe_dir(base: &str, item_id: &str) -> Result<String, PyStoreError> {
    let target = join(base, item_id);
    let mut anc = base.to_owned();
    while !lexists(&anc) {
        let up = dirname(&anc).to_owned();
        if up == anc || up.is_empty() {
            break;
        }
        anc = up;
    }
    for p in [&anc, &base.to_owned(), &target] {
        if lexists(p) && (islink(p) || !Path::new(p).is_dir()) {
            return Err(not_plain());
        }
    }
    makedirs(&target)?;
    let rel = relpath(&target, &anc);
    let mut probe = anc.clone();
    for part in rel.split('/').filter(|p| !p.is_empty() && *p != ".") {
        probe = join(&probe, part);
        if islink(&probe) {
            return Err(not_plain());
        }
    }
    if realpath(&target)? != join(&realpath(&anc)?, &rel) {
        return Err(PyStoreError::Store(
            "attachment path resolves outside its directory".into(),
        ));
    }
    Ok(target)
}

/// `os.makedirs(target, mode=0o755, exist_ok=True)`: the parents at the
/// default mode, the leaf at `0o755` (umask applied).
fn makedirs(target: &str) -> Result<(), PyStoreError> {
    let os = |e: std::io::Error| PyStoreError::os(target, &e);
    let parent = dirname(target);
    if !parent.is_empty() {
        std::fs::create_dir_all(parent).map_err(os)?;
    }
    match std::fs::DirBuilder::new().mode(0o755).create(target) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && Path::new(target).is_dir() => {
            Ok(())
        }
        r => r.map_err(os),
    }
}

/// `os.path.relpath(path, start)` for two normalised absolute paths.
fn relpath(path: &str, start: &str) -> String {
    let p: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let s: Vec<&str> = start.split('/').filter(|s| !s.is_empty()).collect();
    let common = p.iter().zip(&s).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = vec![".."; s.len() - common];
    parts.extend(&p[common..]);
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}
