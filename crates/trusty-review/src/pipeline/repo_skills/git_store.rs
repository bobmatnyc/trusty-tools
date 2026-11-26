//! Read a skill blob at the base commit from a local git object store (#11899).
//!
//! Why: a PR can edit its own skill file, so the skill must come from the
//! base commit, never the head or the working tree. Reading through git's
//! object store (not the filesystem) makes the working tree irrelevant, and
//! a tree lookup never follows a symlink, so a link cannot reach outside the
//! repository.
//! What: [`BaseTree`] is the read seam; [`GitObjectStore`] implements it with
//! `git ls-tree` (mode and type), `git cat-file -s` (size, checked against the
//! cap before any read) and `git cat-file blob`. [`resolve_base_commit`] turns
//! an operator-named base ref into a commit SHA.
//! Test: `base_sha_is_read_not_head_or_worktree`, `symlinked_skill_is_not_regular`,
//! `option_like_base_ref_is_refused`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What one blob lookup at the base commit found (#11899).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BlobRead {
    /// No entry at that path in the base tree.
    Missing,
    /// The entry exists but is not a regular file; carries the git mode.
    NotRegular(String),
    /// A regular file larger than the cap; carries its size. Not read.
    TooLarge(usize),
    /// The blob's bytes (at most the cap).
    Bytes(Vec<u8>),
    /// The read failed; carries the cause, prefixed with its source.
    Error(String),
}

/// The base-commit tree a skill is read from (#11899 seam).
///
/// Why: lets the loader be tested against a scripted tree and keeps the git
/// calls in one place.
/// What: `read` looks up a repo-relative `path` and returns at most `cap` bytes.
/// Test: `base_sha_is_read_not_head_or_worktree`.
pub(crate) trait BaseTree {
    /// Read `path` (repo-relative, `/`-separated) at the base commit.
    fn read(&self, path: &str, cap: usize) -> BlobRead;
}

/// A [`BaseTree`] backed by `git` in a local checkout at one commit.
#[derive(Debug, Clone)]
pub(crate) struct GitObjectStore {
    repo_root: PathBuf,
    commit: String,
}

impl GitObjectStore {
    /// A store reading `commit` (a validated hex SHA) in `repo_root`.
    pub(crate) fn new(repo_root: &Path, commit: &str) -> Self {
        Self {
            repo_root: repo_root.to_path_buf(),
            commit: commit.to_string(),
        }
    }

    fn git(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        run_git(&self.repo_root, args)
    }

    /// `NotRegular` when a directory on `path` is a symlink at the commit.
    fn symlinked_parent(&self, path: &str) -> Option<BlobRead> {
        let mut prefix = String::new();
        for part in path.split('/').collect::<Vec<_>>().split_last()?.1 {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            let listing = self
                .git(&["ls-tree", "-z", &self.commit, "--", &prefix])
                .ok()?;
            match parse_ls_tree(&listing) {
                Some((mode, _, _)) if mode == "120000" => return Some(BlobRead::NotRegular(mode)),
                Some(_) => {}
                None => return None,
            }
        }
        None
    }
}

impl BaseTree for GitObjectStore {
    fn read(&self, path: &str, cap: usize) -> BlobRead {
        // #11899: an error names its source, as the hosted reader's do.
        match self.read_blob(path, cap) {
            BlobRead::Error(e) => BlobRead::Error(format!("git: {e}")),
            read => read,
        }
    }
}

impl GitObjectStore {
    fn read_blob(&self, path: &str, cap: usize) -> BlobRead {
        // `-z` keeps the path unquoted; the commit is validated hex, so it can
        // never be read as an option.
        let listing = match self.git(&["ls-tree", "-z", &self.commit, "--", path]) {
            Ok(out) => out,
            Err(e) => return BlobRead::Error(e),
        };
        let Some((mode, kind, oid)) = parse_ls_tree(&listing) else {
            // #11899 (DOC-93 RS-18): a tree lookup does not pass through a
            // symlinked directory; name that case rather than "missing".
            return self.symlinked_parent(path).unwrap_or(BlobRead::Missing);
        };
        // 100644 and 100755 are the only regular-file blob modes.
        if kind != "blob" || !(mode == "100644" || mode == "100755") {
            return BlobRead::NotRegular(mode);
        }
        let size = match self.git(&["cat-file", "-s", &oid]) {
            Ok(out) => String::from_utf8_lossy(&out).trim().parse::<usize>(),
            Err(e) => return BlobRead::Error(e),
        };
        match size {
            Ok(n) if n > cap => BlobRead::TooLarge(n),
            Ok(_) => match self.git(&["cat-file", "blob", &oid]) {
                Ok(bytes) => BlobRead::Bytes(bytes),
                Err(e) => BlobRead::Error(e),
            },
            Err(e) => BlobRead::Error(format!("unreadable blob size: {e}")),
        }
    }
}

/// Parse one `git ls-tree -z` record into `(mode, type, oid)`.
fn parse_ls_tree(out: &[u8]) -> Option<(String, String, String)> {
    let record = out.split(|b| *b == 0).next()?;
    let text = std::str::from_utf8(record).ok()?;
    let (meta, _path) = text.split_once('\t')?;
    let mut parts = meta.split(' ');
    let mode = parts.next()?.to_string();
    let kind = parts.next()?.to_string();
    let oid = parts.next()?.to_string();
    is_hex_sha(&oid).then_some((mode, kind, oid))
}

/// True for a full 40- (SHA-1) or 64-character (SHA-256) hex object id.
pub(crate) fn is_hex_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Resolve an operator-named base ref to a commit SHA in `repo_root`.
///
/// Why: a `--base` range names a ref; the skill must be read at the commit it
/// names now, and a ref beginning with `-` must never reach git as an option.
/// What: refuses an empty or `-`-prefixed ref, then runs
/// `git rev-parse --verify --quiet <ref>^{commit}` and returns the hex SHA.
/// Test: `option_like_base_ref_is_refused`, `base_sha_is_read_not_head_or_worktree`.
pub(crate) fn resolve_base_commit(repo_root: &Path, base: &str) -> Result<String, String> {
    if base.is_empty() || base.starts_with('-') {
        return Err(format!("base ref {base:?} is not a ref name"));
    }
    let spec = format!("{base}^{{commit}}");
    let out = run_git(repo_root, &["rev-parse", "--verify", "--quiet", &spec])
        .map_err(|_| format!("base ref {base:?} does not name a commit in the checkout"))?;
    let sha = String::from_utf8_lossy(&out).trim().to_string();
    if is_hex_sha(&sha) {
        Ok(sha)
    } else {
        Err(format!("base ref {base:?} did not resolve to a commit SHA"))
    }
}

/// True when `sha` (validated hex) is a commit present in `repo_root`.
pub(crate) fn has_commit(repo_root: &Path, sha: &str) -> bool {
    is_hex_sha(sha) && run_git(repo_root, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_ok()
}

/// Longest one git call may run; a slower call is killed and its read skipped.
pub(crate) const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Run `git -C <repo_root> <args>` within [`GIT_TIMEOUT`].
fn run_git(repo_root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    run_git_within(repo_root, args, GIT_TIMEOUT)
}

/// Run `git -C <repo_root> <args>`, killed after `timeout`.
///
/// Why: a hung git (a huge object, a lock, a slow filesystem) must not hold
/// the review; a timeout is a skipped read, never a failed review (#11899).
/// What: stdout and stderr are drained on threads so a full pipe cannot
/// stall the child; the child is polled until it exits or `timeout` passes,
/// then killed and reaped. Returns stdout on success, else stderr's first
/// line, or a timeout message.
/// Test: `git_call_past_its_timeout_is_an_error`.
pub(crate) fn run_git_within(
    repo_root: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run git: {e}"))?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git timed out after {} ms", timeout.as_millis()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(format!("could not wait for git: {e}")),
        }
    };
    let out = stdout.join().unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        let err = stderr.join().unwrap_or_default();
        let err = String::from_utf8_lossy(&err);
        Err(err.lines().next().unwrap_or("git failed").to_string())
    }
}
