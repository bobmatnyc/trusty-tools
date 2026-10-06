//! One indexed file's content and, optionally, its diff against `HEAD` (#9029).
//!
//! Why: the search dashboard opens a hit in a file viewer with a diff view, and
//! the daemon had no method that returned a whole file. The viewer must not
//! become a way to read arbitrary files, so only a file the index itself would
//! walk is served.
//! What: [`read_indexed_file`] resolves a path inside one index root, asks the
//! walker's admission ([`index_admission::admits`]), refuses sops-encrypted
//! content ([`crate::core::sops::is_sops_encrypted`]), caps content and diff,
//! and runs git through [`crate::core::git::run_git_bounded`] with a deadline.
//! A missing file and a file outside the index answer one identical 404 body.
//! Test: `content_only_returns_the_file_and_no_diff`,
//! `a_git_failure_is_a_diff_error_not_an_empty_diff` and their siblings in
//! `file_view_tests.rs`.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path};
use std::time::Duration;

use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::git::{run_git_bounded, GitRunError};
use crate::core::registry::IndexHandle;
use crate::service::index_admission::{self, Admission};

/// The most file content one call returns: 1 MiB.
pub const CONTENT_CAP_BYTES: usize = 1 << 20;
/// The most diff text one call returns: 256 KiB.
pub const DIFF_CAP_BYTES: usize = 256 << 10;
/// The deadline each git run gets.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(10);
/// How much of a truncated file's tail the sops check also reads; sops writes
/// its metadata block at the end of the document.
const SOPS_TAIL_BYTES: u64 = 64 << 10;

/// Which diff a caller wants beside the content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffMode {
    /// Content only.
    #[default]
    None,
    /// The working tree against `HEAD`.
    Head,
}

/// The caps and the git binary one read runs under.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Content byte cap.
    pub content_cap: usize,
    /// Diff byte cap.
    pub diff_cap: usize,
    /// Deadline per git run.
    pub git_timeout: Duration,
    /// The git binary.
    pub git_bin: String,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            content_cap: CONTENT_CAP_BYTES,
            diff_cap: DIFF_CAP_BYTES,
            git_timeout: GIT_TIMEOUT,
            git_bin: "git".to_owned(),
        }
    }
}

/// A refusal as `(status, body)`, the shape every socket method renders.
pub type Refusal = (StatusCode, Value);

/// The one 404 for a missing file, an excluded file, and a path outside the
/// index, so a caller cannot tell them apart.
fn not_found(index_id: &str) -> Refusal {
    (
        StatusCode::NOT_FOUND,
        json!({
            "error": "file_not_found",
            "index_id": index_id,
            "message": "no indexed file at this path",
        }),
    )
}

fn refusal(status: StatusCode, index_id: &str, error: &str, reason: &str, msg: &str) -> Refusal {
    let mut body = json!({
        "error": error,
        "index_id": index_id,
        "reason": reason,
        "message": msg,
    });
    if status == StatusCode::SERVICE_UNAVAILABLE {
        body["retryable"] = true.into();
    }
    (status, body)
}

/// Read one indexed file and, when asked, its diff against `HEAD`.
///
/// Why: see the module docs.
/// What: `path` is root-relative or absolute. An empty path, a NUL byte, or a
/// `..` segment is 400 `invalid_path`. An absolute path outside the root is
/// refused before the filesystem is touched. The path is then canonicalised;
/// a miss, a symlink resolving outside the root, a non-file, and a walker
/// exclusion are all [`not_found`]. An undecidable admission is 503. Content
/// over `limits.content_cap` is cut and `content_truncated` says so; sops
/// content is 403 `file_refused`. A requested diff that git could not produce
/// is a `diff.status: "error"` object beside the content, never an empty diff.
/// Blocking: call it from `spawn_blocking`.
/// Test: `content_only_returns_the_file_and_no_diff`,
/// `head_diff_reports_a_working_tree_change`,
/// `a_missing_file_and_an_outside_file_answer_one_body`,
/// `traversal_is_refused_before_the_filesystem`,
/// `a_symlink_escaping_the_root_is_not_found`, `sops_content_is_refused`,
/// `content_and_diff_over_their_caps_are_cut_and_flagged`,
/// `a_git_failure_is_a_diff_error_not_an_empty_diff`.
pub fn read_indexed_file(
    handle: &IndexHandle,
    path: &str,
    diff: DiffMode,
    limits: &Limits,
) -> Result<Value, Refusal> {
    let index_id = handle.id.0.as_str();
    let given = Path::new(path);
    if path.is_empty() || path.contains('\0') {
        return Err(refusal(
            StatusCode::BAD_REQUEST,
            index_id,
            "invalid_path",
            "empty_or_nul",
            "path must be a non-empty file path",
        ));
    }
    if given.components().any(|c| c == Component::ParentDir) {
        return Err(refusal(
            StatusCode::BAD_REQUEST,
            index_id,
            "invalid_path",
            "path_traversal",
            "path must not contain a `..` segment",
        ));
    }
    let root = handle.root_path.canonicalize().map_err(|e| {
        tracing::warn!(index_id, error = %e, "file.get: index root unavailable");
        refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            index_id,
            "index_root_unavailable",
            "root_unreadable",
            "the index root could not be resolved",
        )
    })?;
    if given.is_absolute() && !given.starts_with(&root) && !given.starts_with(&handle.root_path) {
        return Err(not_found(index_id));
    }
    // #9029: canonicalising resolves every symlink, so the containment check
    // below also catches a link that escapes the root.
    let file = root
        .join(given)
        .canonicalize()
        .map_err(|_| not_found(index_id))?;
    if !file.starts_with(&root) || !file.is_file() {
        return Err(not_found(index_id));
    }
    match index_admission::admits(handle, &file) {
        Admission::Included => {}
        Admission::Excluded => return Err(not_found(index_id)),
        Admission::Undetermined => {
            return Err(refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                index_id,
                "file_admission_undetermined",
                "admission_undetermined",
                "the filesystem could not say whether this path is indexed",
            ));
        }
    }
    let read = read_capped(&file, limits.content_cap).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => not_found(index_id),
        _ => refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            index_id,
            "file_read_failed",
            "io_error",
            &format!("the file could not be read: {e}"),
        ),
    })?;
    if crate::core::sops::is_sops_encrypted(&read.sops_probe) {
        tracing::warn!(index_id, "file.get refused a sops-encrypted file (#9029)");
        return Err(refusal(
            StatusCode::FORBIDDEN,
            index_id,
            "file_refused",
            "sops_encrypted",
            "the file is sops-encrypted, which is never served",
        ));
    }
    let rel = file
        .strip_prefix(&root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (content, lossy) = decode(read.bytes, read.truncated);
    let diff = match diff {
        DiffMode::None => Value::Null,
        DiffMode::Head => diff_head(&root, &rel, limits),
    };
    Ok(json!({
        "index_id": index_id,
        "path": rel,
        "size_bytes": read.size,
        "content": content,
        "content_truncated": read.truncated,
        "content_cap_bytes": limits.content_cap,
        "lossy_utf8": lossy,
        "diff": diff,
    }))
}

/// A capped read, plus the text the sops check runs over.
struct CappedRead {
    bytes: Vec<u8>,
    truncated: bool,
    size: u64,
    sops_probe: String,
}

/// Read at most `cap` bytes; a cut file also hands its tail to the sops check.
fn read_capped(file: &Path, cap: usize) -> std::io::Result<CappedRead> {
    let mut f = std::fs::File::open(file)?;
    let size = f.metadata()?.len();
    let mut bytes = Vec::new();
    (&mut f).take(cap as u64 + 1).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > cap;
    bytes.truncate(cap);
    let mut sops_probe = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        let mut tail = Vec::new();
        f.seek(SeekFrom::End(-(SOPS_TAIL_BYTES.min(size) as i64)))?;
        f.read_to_end(&mut tail)?;
        sops_probe.push('\n');
        sops_probe.push_str(&String::from_utf8_lossy(&tail));
    }
    Ok(CappedRead {
        bytes,
        truncated,
        size,
        sops_probe,
    })
}

/// Bytes to text. A character split by the cap is dropped, not replaced;
/// any other invalid byte is replaced and reported as lossy.
fn decode(bytes: Vec<u8>, truncated: bool) -> (String, bool) {
    match String::from_utf8(bytes) {
        Ok(text) => (text, false),
        Err(e) => {
            let err = e.utf8_error();
            let mut bytes = e.into_bytes();
            if truncated && err.error_len().is_none() {
                bytes.truncate(err.valid_up_to());
                return (String::from_utf8_lossy(&bytes).into_owned(), false);
            }
            (String::from_utf8_lossy(&bytes).into_owned(), true)
        }
    }
}

/// The working tree's diff against `HEAD` for one root-relative path.
///
/// What: `git ls-files` first, so an untracked file reads `untracked` rather
/// than an empty diff; then `git diff HEAD`. Either git run failing gives
/// `status: "error"` with `error` one of `git_timed_out`, `git_failed`,
/// `git_unavailable` (#9029 fail-closed).
fn diff_head(root: &Path, rel: &str, limits: &Limits) -> Value {
    let run = |args: &[&str], cap: usize| {
        run_git_bounded(root, &limits.git_bin, args, Some(limits.git_timeout), cap)
    };
    let tracked = match run(&["--literal-pathspecs", "ls-files", "-z", "--", rel], 4096) {
        Ok(out) => !out.stdout.is_empty(),
        Err(e) => return diff_error(&e),
    };
    let base = json!({ "base": "HEAD", "cap_bytes": limits.diff_cap });
    if !tracked {
        return with(base, "untracked", "", false);
    }
    let args = [
        "--no-pager",
        "--literal-pathspecs",
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "HEAD",
        "--",
        rel,
    ];
    match run(&args, limits.diff_cap) {
        Ok(out) if out.stdout.is_empty() => with(base, "unchanged", "", false),
        Ok(out) => {
            let (text, _) = decode(out.stdout, out.truncated);
            with(base, "changed", &text, out.truncated)
        }
        Err(e) => diff_error(&e),
    }
}

fn with(mut base: Value, status: &str, text: &str, truncated: bool) -> Value {
    base["status"] = status.into();
    base["text"] = text.into();
    base["truncated"] = truncated.into();
    base
}

fn diff_error(e: &GitRunError) -> Value {
    let error = match e {
        GitRunError::TimedOut(_) => "git_timed_out",
        GitRunError::Spawn(_) => "git_unavailable",
        GitRunError::Exit { .. } | GitRunError::Wait(_) => "git_failed",
    };
    tracing::warn!(error, "file.get diff failed (#9029): {e}");
    json!({ "base": "HEAD", "status": "error", "error": error, "message": e.to_string() })
}

#[cfg(test)]
#[path = "file_view_tests.rs"]
pub(crate) mod tests;
