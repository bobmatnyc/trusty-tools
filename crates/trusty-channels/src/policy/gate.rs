//! The git load gates: a route file takes effect only when the bytes parsed
//! are the bytes a reviewed commit holds.
//!
//! Why: a route change must go through a reviewed commit. The commit is not
//! the approval; the gate only keeps an unreviewed edit from taking effect.
//! `git status` is not that check: `--assume-unchanged`, `--skip-worktree`, a
//! clean filter or `core.fsmonitor` each hide an edit from it, and a file
//! read before the check can change between the read and the check.
//! What: two gates over the caller's bytes. [`check_committed_at_head`] is
//! gchat's #9448 gate: the bytes equal the blob at `HEAD`. [`check_default_branch`]
//! is the #8454 Db1 gate for the policy loader: the project dir is its
//! repo's top level, `HEAD` is a symbolic ref to the default branch, and the
//! bytes equal the blob at that branch's commit, resolved once. Each git call
//! has every `GIT_*` variable removed and `core.fsmonitor` off; bytes are
//! hashed with `hash-object --no-filters`. Any git failure, a missing git,
//! unexpected git output or a step that outlasts [`GIT_TIMEOUT`] refuses.
//! Test: `src/policy/tests/gate.rs`, and gchat's
//! `load_gate_refuses_untracked_modified_and_staged`.

use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

/// The route file's path inside a project repo, from its top level.
pub const ROUTES_REL_PATH: &str = ".trusty-channels/routes.toml";

/// The longest one git step may run before it is killed.
///
/// Why: git opens an `include.path` file with a blocking open, so a FIFO
/// there, or any stalled git, would hold the loader and every consumer
/// behind it (#8454).
/// What: 10 s. A local `rev-parse`, `symbolic-ref` or `hash-object` takes
/// milliseconds, so the bound leaves two orders of magnitude for a loaded
/// host while keeping a stalled reload short.
/// Test: `git_blocked_on_a_fifo_config_include_times_out`.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
thread_local! {
    static TEST_TIMEOUT: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with git steps on this thread bounded by `limit`.
#[cfg(test)]
pub(crate) fn with_git_timeout<T>(limit: Duration, f: impl FnOnce() -> T) -> T {
    let before = TEST_TIMEOUT.with(|t| t.replace(Some(limit)));
    let out = f();
    TEST_TIMEOUT.with(|t| t.set(before));
    out
}

/// [`GIT_TIMEOUT`], or a test's shorter bound.
fn git_timeout() -> Duration {
    #[cfg(test)]
    if let Some(limit) = TEST_TIMEOUT.with(std::cell::Cell::get) {
        return limit;
    }
    GIT_TIMEOUT
}

/// Why a route file's bytes were refused by a load gate.
///
/// Why: every refusal has a named reason (#8454, Architect 19:05Z); the
/// type is channel-neutral so gchat and the policy loader share it.
/// What: one variant per rule. Branch names come from the repo's own refs;
/// no variant carries file content.
/// Test: `src/policy/tests/gate.rs`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum GateError {
    /// The route path has no parent directory or file name.
    #[error("path has no parent directory")]
    NoParent,
    /// git could not be started.
    #[error("cannot run git: {reason}")]
    GitUnavailable {
        /// The I/O error kind.
        reason: String,
    },
    /// A git step exited with an unexpected status or printed unexpected
    /// output.
    #[error("git {step} failed or printed unexpected output")]
    GitFailed {
        /// The git step.
        step: &'static str,
    },
    /// A git step ran longer than [`GIT_TIMEOUT`] and was killed.
    #[error("git {step} did not finish in time and was stopped")]
    GitTimedOut {
        /// The git step.
        step: &'static str,
    },
    /// The project dir is not in a git repository git will read.
    #[error("the project dir is not in a git repository")]
    NotARepository,
    /// The project dir is inside another repo, not that repo's top level.
    #[error("the project dir is not the top level of its own git repository")]
    NotTopLevel,
    /// No `origin/HEAD`, and both or neither of `main` and `master` exist.
    #[error(
        "the default branch is unknown: no origin/HEAD, and not exactly one of main and master"
    )]
    DefaultBranchUnknown,
    /// `HEAD` is detached, even at the default branch's tip.
    #[error("HEAD is detached; check out the default branch")]
    DetachedHead,
    /// `HEAD` is on a branch other than the default branch.
    #[error("HEAD is on {head}, not the default branch {default}")]
    NotOnDefaultBranch {
        /// The branch `HEAD` names.
        head: String,
        /// The default branch.
        default: String,
    },
    /// The default branch has no commit.
    #[error("the default branch {branch} has no commit")]
    NoDefaultCommit {
        /// The default branch.
        branch: String,
    },
    /// The file is absent from the reviewed commit.
    #[error("not committed at {at} (commit it through a reviewed PR)")]
    NotCommitted {
        /// `HEAD` or the default branch name.
        at: String,
    },
    /// The bytes read differ from the reviewed commit's blob.
    #[error("content differs from the version committed at {at}")]
    ContentDiffers {
        /// `HEAD` or the default branch name.
        at: String,
    },
}

/// Refuse unless `bytes` equal the blob committed at `HEAD` for `path`.
///
/// Why: gchat's #9448 gate, kept HEAD-only until S3 applies Db1 to
/// gchat-mcp (#8454 G1). The caller passes the bytes it will parse, so the
/// check and the parse see the same content.
/// What: runs git in the file's directory. No commit, a path absent from
/// `HEAD`, bytes that differ from the committed blob, or git missing each
/// refuse. `--no-filters` hashes the raw bytes.
/// Test: `load_gate_refuses_untracked_modified_and_staged`,
/// `load_gate_refuses_edits_hidden_by_assume_unchanged_or_skip_worktree`,
/// `load_gate_checks_the_bytes_read_not_the_file_after`.
pub fn check_committed_at_head(path: &Path, bytes: &[u8]) -> Result<(), GateError> {
    let (dir, file) = match (path.parent(), path.file_name()) {
        (Some(d), Some(f)) => (d, f),
        _ => return Err(GateError::NoParent),
    };
    let mut spec = OsString::from("HEAD:./");
    spec.push(file);
    let committed = run(
        git(dir)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(&spec),
        "rev-parse",
    )?;
    let at = || "HEAD".to_string();
    if !committed.status.success() {
        return Err(GateError::NotCommitted { at: at() });
    }
    let read = hash_bytes(dir, bytes)?;
    if object_id(&committed, "rev-parse")? != object_id(&read, "hash-object")? {
        return Err(GateError::ContentDiffers { at: at() });
    }
    Ok(())
}

/// Where a project repo's `HEAD` and default branch point, read locally.
///
/// Why: the reload check re-gates when either moves, so a branch switch
/// with identical file bytes is still re-gated (#8454 S2b §3).
/// What: the default branch name, `HEAD`'s symbolic ref (`None` when
/// detached) and the default branch's commit.
/// Test: `branch_switch_with_identical_bytes_regates`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchState {
    /// The default branch, e.g. `main`.
    pub default: String,
    /// `HEAD`'s target, e.g. `refs/heads/main`; `None` when detached.
    pub head: Option<String>,
    /// The commit `refs/heads/<default>` names, resolved once.
    pub commit: String,
}

/// Read the project repo's branch state, refusing a non-top-level dir.
///
/// Why: Db1 checks run against ONE resolved commit (S2b plan §1).
/// What: `rev-parse --show-toplevel` must equal `project_dir` (both
/// canonical); then the default branch ([`default_branch`]), `HEAD`'s
/// symbolic ref and `rev-parse refs/heads/<default>^{commit}`.
/// Test: `project_dir_nested_in_a_parent_repo_refused`,
/// `default_branch_unknown_refused`, `origin_head_names_the_default_branch`.
pub fn branch_state(project_dir: &Path) -> Result<BranchState, GateError> {
    let top = run(
        git(project_dir).args(["rev-parse", "--show-toplevel"]),
        "rev-parse --show-toplevel",
    )?;
    if !top.status.success() {
        return Err(GateError::NotARepository);
    }
    let top = PathBuf::from(line(&top, "rev-parse --show-toplevel")?);
    let canonical = |p: &Path| std::fs::canonicalize(p).ok();
    // #8454 Db1: a dir nested in a parent repo would read the parent's HEAD.
    match (canonical(&top), canonical(project_dir)) {
        (Some(t), Some(d)) if t == d => {}
        _ => return Err(GateError::NotTopLevel),
    }
    let default = default_branch(project_dir)?;
    let head = symbolic_ref(project_dir, "HEAD")?;
    let spec = format!("refs/heads/{default}^{{commit}}");
    let commit = run(
        git(project_dir).args(["rev-parse", "--verify", "--quiet", &spec]),
        "rev-parse <default>",
    )?;
    if !commit.status.success() {
        return Err(GateError::NoDefaultCommit { branch: default });
    }
    let commit = object_id(&commit, "rev-parse <default>")?.to_string();
    Ok(BranchState {
        default,
        head,
        commit,
    })
}

/// Refuse unless `bytes` equal the routes file at the default branch's
/// commit and `HEAD` is a symbolic ref to that branch.
///
/// Why: Bob Db1 + Architect G2/G3. Reading the blob at the resolved commit,
/// not at `HEAD`, keeps both checks on one commit if `HEAD` moves.
/// What: [`branch_state`], then `HEAD` must be `refs/heads/<default>`
/// (detached refused even at the tip), then `<C>:<ROUTES_REL_PATH>` must
/// exist and equal `hash-object --no-filters` of `bytes`. Returns the state
/// it checked against.
/// Test: `routes_file_refused_when_head_is_on_a_feature_branch`,
/// `routes_file_refused_on_detached_head_at_default_tip`,
/// `local_commit_on_default_branch_ahead_of_origin_is_accepted`,
/// `local_only_repo_uses_main_without_origin`,
/// `routes_edit_outside_the_reviewed_source_has_no_effect`.
pub fn check_default_branch(project_dir: &Path, bytes: &[u8]) -> Result<BranchState, GateError> {
    let state = branch_state(project_dir)?;
    let want = format!("refs/heads/{}", state.default);
    match state.head.as_deref() {
        None => return Err(GateError::DetachedHead),
        Some(h) if h != want => {
            return Err(GateError::NotOnDefaultBranch {
                head: h.strip_prefix("refs/heads/").unwrap_or(h).to_string(),
                default: state.default,
            })
        }
        Some(_) => {}
    }
    let spec = format!("{}:{ROUTES_REL_PATH}", state.commit);
    let committed = run(
        git(project_dir).args(["rev-parse", "--verify", "--quiet", &spec]),
        "rev-parse <blob>",
    )?;
    let at = || state.default.clone();
    if !committed.status.success() {
        return Err(GateError::NotCommitted { at: at() });
    }
    let read = hash_bytes(project_dir, bytes)?;
    if object_id(&committed, "rev-parse <blob>")? != object_id(&read, "hash-object")? {
        return Err(GateError::ContentDiffers { at: at() });
    }
    Ok(state)
}

/// The default branch, resolved locally (Architect G2): `origin/HEAD`'s
/// target with `origin/` stripped; else exactly one of `main`, `master`.
/// `init.defaultBranch` is not read. No remote is contacted.
fn default_branch(dir: &Path) -> Result<String, GateError> {
    const ORIGIN_HEAD: &str = "refs/remotes/origin/HEAD";
    if let Some(target) = symbolic_ref(dir, ORIGIN_HEAD)? {
        return target
            .strip_prefix("refs/remotes/origin/")
            .filter(|b| !b.is_empty() && *b != "HEAD")
            .map(str::to_string)
            .ok_or(GateError::DefaultBranchUnknown);
    }
    // #8454: an origin/HEAD that exists but is not symbolic names no branch.
    if ref_exists(dir, ORIGIN_HEAD)? {
        return Err(GateError::DefaultBranchUnknown);
    }
    match (
        ref_exists(dir, "refs/heads/main")?,
        ref_exists(dir, "refs/heads/master")?,
    ) {
        (true, false) => Ok("main".into()),
        (false, true) => Ok("master".into()),
        _ => Err(GateError::DefaultBranchUnknown),
    }
}

/// `git symbolic-ref -q <name>`: `Some(target)`, or `None` when the ref is
/// absent, detached or not symbolic (exit 1). Any other status refuses.
fn symbolic_ref(dir: &Path, name: &str) -> Result<Option<String>, GateError> {
    let out = run(git(dir).args(["symbolic-ref", "-q", name]), "symbolic-ref")?;
    match out.status.code() {
        Some(0) => {
            let target = line(&out, "symbolic-ref")?;
            if !target.starts_with("refs/") {
                return Err(GateError::GitFailed {
                    step: "symbolic-ref",
                });
            }
            Ok(Some(target))
        }
        Some(1) => Ok(None),
        _ => Err(GateError::GitFailed {
            step: "symbolic-ref",
        }),
    }
}

/// `git show-ref --verify -q <name>`: exit 0 present, 1 absent.
fn ref_exists(dir: &Path, name: &str) -> Result<bool, GateError> {
    let out = run(
        git(dir).args(["show-ref", "--verify", "-q", name]),
        "show-ref",
    )?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(GateError::GitFailed { step: "show-ref" }),
    }
}

/// `git hash-object --no-filters --stdin` over `bytes`, in `dir`'s repo (so
/// the repository's object format applies).
fn hash_bytes(dir: &Path, bytes: &[u8]) -> Result<Output, GateError> {
    let out = run_with_input(
        git(dir).args(["hash-object", "--no-filters", "--stdin"]),
        "hash-object",
        Some(bytes),
    )?;
    if !out.status.success() {
        return Err(GateError::GitFailed {
            step: "hash-object",
        });
    }
    Ok(out)
}

/// The object id a successful git call printed: 40 or 64 lowercase hex.
pub(super) fn object_id<'a>(out: &'a Output, step: &'static str) -> Result<&'a str, GateError> {
    let id =
        std::str::from_utf8(out.stdout.trim_ascii()).map_err(|_| GateError::GitFailed { step })?;
    // #8454: unexpected git output refuses; it never compares equal by accident.
    let hex = id
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hex || !(id.len() == 40 || id.len() == 64) {
        return Err(GateError::GitFailed { step });
    }
    Ok(id)
}

/// One non-empty UTF-8 line of output.
fn line(out: &Output, step: &'static str) -> Result<String, GateError> {
    let text =
        std::str::from_utf8(out.stdout.trim_ascii()).map_err(|_| GateError::GitFailed { step })?;
    if text.is_empty() || text.contains('\n') {
        return Err(GateError::GitFailed { step });
    }
    Ok(text.to_string())
}

fn run(cmd: &mut Command, step: &'static str) -> Result<Output, GateError> {
    run_with_input(cmd, step, None)
}

/// Run one git step, feeding it `input` (or no stdin) and capturing stdout,
/// and kill it once [`git_timeout`] has passed.
///
/// Why: #8454: a git that blocks (a FIFO in its config) must not block the
/// caller; std's `output()` waits forever.
/// What: stdin and stdout each get a thread, so a full pipe never stalls
/// the wait; the child is polled until the deadline, then killed and
/// reaped. stderr is discarded. A thread that cannot start kills the child.
/// Test: `git_blocked_on_a_fifo_config_include_times_out`.
fn run_with_input(
    cmd: &mut Command,
    step: &'static str,
    input: Option<&[u8]>,
) -> Result<Output, GateError> {
    let deadline = Instant::now() + git_timeout();
    let stdin = if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let mut child = cmd
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(unavailable)?;
    let (tx_in, rx_in) = mpsc::channel();
    let (tx_out, rx_out) = mpsc::channel();
    let feed = match (child.stdin.take(), input) {
        (Some(mut pipe), Some(bytes)) => {
            let bytes = bytes.to_vec();
            // Dropping the pipe after the write closes stdin: git sees EOF.
            thread::Builder::new()
                .spawn(move || {
                    let _ = tx_in.send(pipe.write_all(&bytes));
                })
                .map(drop)
        }
        _ => {
            let _ = tx_in.send(Ok(()));
            Ok(())
        }
    };
    let piped = feed.and_then(|()| match child.stdout.take() {
        Some(mut pipe) => thread::Builder::new()
            .spawn(move || {
                let mut buf = Vec::new();
                let _ = tx_out.send(pipe.read_to_end(&mut buf).map(|_| buf));
            })
            .map(drop),
        None => Err(std::io::Error::other("git stdout unavailable")),
    });
    let status = match piped.and_then(|()| wait_until(&mut child, deadline)) {
        Ok(Some(status)) => status,
        Ok(None) => {
            kill(&mut child);
            return Err(GateError::GitTimedOut { step });
        }
        Err(e) => {
            kill(&mut child);
            return Err(unavailable(e));
        }
    };
    let left = || deadline.saturating_duration_since(Instant::now());
    let written = rx_in.recv_timeout(left());
    let stdout = rx_out.recv_timeout(left());
    match (written, stdout) {
        (Ok(Ok(())), Ok(Ok(stdout))) => Ok(Output {
            status,
            stdout,
            stderr: Vec::new(),
        }),
        (Ok(Err(e)), _) | (_, Ok(Err(e))) => Err(unavailable(e)),
        (Err(RecvTimeoutError::Timeout), _) | (_, Err(RecvTimeoutError::Timeout)) => {
            Err(GateError::GitTimedOut { step })
        }
        _ => Err(GateError::GitFailed { step }),
    }
}

/// Poll `child` until it exits (`Some`) or `deadline` passes (`None`).
fn wait_until(child: &mut Child, deadline: Instant) -> std::io::Result<Option<ExitStatus>> {
    let mut pause = Duration::from_millis(1);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        thread::sleep(pause.min(deadline - now));
        pause = (pause * 2).min(Duration::from_millis(20));
    }
}

/// Kill and reap a git step that is no longer wanted.
fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn unavailable(e: std::io::Error) -> GateError {
    GateError::GitUnavailable {
        reason: e.kind().to_string(),
    }
}

/// A git command in `dir` with every `GIT_*` variable removed and
/// `core.fsmonitor` off (#8454: the fsmonitor hook can hide an edit).
fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir).args(["-c", "core.fsmonitor=false"]);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    cmd
}
