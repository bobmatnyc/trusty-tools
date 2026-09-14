//! Write side of ADR-0062: append one pause snapshot to this session's own
//! `refs/tm/sessions/<user-id>/<session-key>` ref and lease-push it (#7830).
//!
//! Why: `.trusty-mpm/sessions/` is gitignored (ADR-0061's 2026-09-13
//! amendment), so a pause had no durable copy anywhere. The branch+PR publish
//! in [`crate::core::session_pause_pr`] serves projects that TRACK their store
//! and short-circuits with `not_tracked` here; it is untouched and stays.
//! ADR-0062's ruling is that session history lives OUTSIDE the branch
//! namespace: one orphan, append-only commit chain per session, never a commit
//! on a code branch, never a PR, never a merge-queue slot.
//! What: [`publish_session_ref`] scans the snapshot for credential-shaped
//! content (decision 9), resolves the ref key, builds a one-file commit with
//! git plumbing against a scratch `GIT_INDEX_FILE` in a per-call
//! [`tempfile::TempDir`] — the same shape `session_pause_pr` uses, so HEAD, the
//! shared index and the working tree are never touched — parents it on the
//! ref's current REMOTE tip, and pushes with `--force-with-lease` against that
//! same tip (decision 3). A lease mismatch is [`RefPublishError::StaleLease`]
//! and is never retried with `--force`.
//!
//! Every spawn goes through [`trusty_common::git::command_in`] (#7171). There
//! is no `git2` dependency.
//! Test: `session_ref_publish_tests.rs`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::core::attribution::ATTRIBUTION_FOOTER;

/// Namespace every session ref lives under (ADR-0062 decision 1).
pub const SESSION_REF_ROOT: &str = "refs/tm/sessions";

/// Blob mode for a regular non-executable file, as `update-index` spells it.
const BLOB_MODE: &str = "100644";

/// Why a pause could not publish its session ref.
///
/// Why: the pause is fail-open — the local snapshot is the primary write — but
/// the caller still has to tell a lease loss (another writer moved the ref;
/// ADR-0062 decision 3) from a credential refusal (decision 9) from an ordinary
/// transport failure. Collapsing them into one string would make the one arm
/// that must NEVER be retried with `--force` indistinguishable from the arms
/// that a later pause simply repeats.
/// What: `thiserror`, so the `Display` text is what reaches the pause response's
/// `ref_error` field verbatim.
/// Test: `session_ref_publish_tests.rs`.
#[derive(Debug, thiserror::Error)]
pub enum RefPublishError {
    /// Neither a `gh` login nor `git config user.name` named this writer.
    #[error(
        "no user id resolved for the session ref: `gh` names no active github.com \
         account and `git config user.name` is unset or unusable as a ref component"
    )]
    NoUserId,

    /// The session id cannot be spelled as a git ref component.
    #[error("session id {0:?} cannot be used as a git ref component")]
    NoSessionKey(String),

    /// The snapshot carries credential-shaped bytes; the publish is refused.
    #[error(
        "the pause snapshot carries credential-shaped content ({0}); the session ref \
         was not published and the snapshot stays in the local cache only"
    )]
    CredentialDetected(String),

    /// `origin` moved since this writer read its tip.
    #[error(
        "{ref_name} moved on origin since this pause read its tip ({expected}); the \
         lease-checked push was rejected and is NEVER retried with --force"
    )]
    StaleLease {
        /// The ref whose lease was lost.
        ref_name: String,
        /// The tip this writer expected, or `<absent>` for a first commit.
        expected: String,
    },

    /// This directory is not a checkout with an `origin` remote.
    #[error("{0} is not a git checkout with an `origin` remote")]
    NoOrigin(String),

    /// A git step failed for a reason with no dedicated arm.
    #[error("git {step} failed: {message}")]
    Git {
        /// Which plumbing step failed.
        step: &'static str,
        /// git's own stderr (or stdout when stderr was silent).
        message: String,
    },

    /// The snapshot could not be read, or a scratch file could not be made.
    #[error("session ref publish io error: {0}")]
    Io(#[from] std::io::Error),

    /// The snapshot is not inside this project's `.trusty-mpm/sessions/` store.
    #[error("snapshot {0} is not inside the project's .trusty-mpm/sessions/ store")]
    SnapshotOutsideStore(String),
}

/// What a caller must supply to append one pause to a session's ref.
///
/// Why: the pause writer already knows all four values; re-deriving any of them
/// here would be a second answer to a settled question.
/// What: the checkout, the session id the snapshot was filed under, the
/// snapshot's absolute path, and the pause timestamp (which also becomes the
/// commit's author/committer date, so the chain is ordered by pause time rather
/// than by push time).
/// Test: every test in `session_ref_publish_tests.rs`.
#[derive(Debug, Clone)]
pub struct SessionRefRequest<'a> {
    /// The project checkout the pause happened in.
    pub repo: &'a Path,
    /// The session id the snapshot was filed under.
    pub session_id: &'a str,
    /// Absolute path of the snapshot `.md` just written.
    pub snapshot_path: &'a Path,
    /// The pause timestamp.
    pub timestamp: DateTime<Utc>,
}

/// One appended session-ref commit.
///
/// What: the ref that advanced, the commit now at its tip, and that commit's
/// parent — `None` for the first commit in an orphan chain.
/// Test: `a_first_pause_creates_an_orphan_commit_and_leaves_main_alone`,
/// `a_second_pause_appends_to_the_same_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRefOutcome {
    /// The full ref name, e.g. `refs/tm/sessions/octocat/host-tmux-window-230`.
    pub ref_name: String,
    /// The commit the ref now points at.
    pub commit: String,
    /// The commit's parent, or `None` for the chain's first commit.
    pub parent: Option<String>,
}

/// Keep only the characters a session ref component may contain.
///
/// Why (#7830): both halves of the ref key come from outside — a GitHub login,
/// a `git config user.name`, an MCP-supplied session id — and a ref name is a
/// path. `[A-Za-z0-9._-]` is the decided alphabet; anything else is dropped
/// rather than escaped, because a component that is empty or reserved after
/// filtering must fail the publish rather than collide with another writer's.
/// What: `Some(filtered)` when the result is non-empty, is not `.` or `..`,
/// contains no `..`, and neither starts with `.`/`-` nor ends with `.` or
/// `.lock` — the subset of `git check-ref-format` that this alphabet can still
/// violate.
/// Test: `ref_components_are_sanitized_and_rejected`.
pub fn sanitize_ref_component(raw: &str) -> Option<String> {
    let filtered: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    if filtered.is_empty()
        || filtered == "."
        || filtered == ".."
        || filtered.contains("..")
        || filtered.starts_with('.')
        || filtered.starts_with('-')
        || filtered.ends_with('.')
        || filtered.ends_with(".lock")
    {
        return None;
    }
    Some(filtered)
}

/// The `<user-id>` half of a session ref key.
///
/// Why (#7830, decided): the GitHub login of the authenticated account names
/// the writer everywhere else in this workspace, so it names it here too. A
/// host with no `gh` falls back to `git config user.name`. It is NEVER an email
/// address — an address filtered into `[A-Za-z0-9._-]` would silently become a
/// different, colliding id (`bob@example.com` → `bobexample.com`), so a value
/// containing `@` is rejected outright rather than mangled.
/// What: `gh`'s active github.com login from its own `hosts.yml` (a file read,
/// no subprocess — this runs on every pause), else `git config user.name`, each
/// through [`sanitize_ref_component`]. `None` when neither resolves, which
/// skips the publish with `ref_error` set and leaves the local snapshot alone.
/// Test: `ref_components_are_sanitized_and_rejected`,
/// `a_missing_user_id_skips_the_publish`.
pub fn resolve_user_id(repo: &Path) -> Option<String> {
    if let Some(login) = crate::core::gh_account::gh_account_status_local()
        .and_then(|status| status.active)
        .filter(|login| !login.contains('@'))
        .and_then(|login| sanitize_ref_component(&login))
    {
        return Some(login);
    }
    let out = trusty_common::git::command_in(repo)
        .args(["config", "user.name"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.contains('@') {
        return None;
    }
    sanitize_ref_component(&name)
}

/// The `<session-key>` half of a session ref key.
///
/// Why (#7830, decided): a managed session id is globally unique, so it keys
/// the ref as-is. A tmux-derived id is only `tmux-window-<N>` — the SAME string
/// on every machine the operator works from — so two hosts would write one ref
/// and fight over its lease forever. Prefixing the hostname separates them.
/// What: `<hostname>-<session-id>` when `session_id` carries
/// [`trusty_common::catchup::session_id::TMUX_WINDOW_ID_PREFIX`] and a hostname
/// resolves, else the sanitized session id alone.
/// Test: `a_tmux_session_key_is_hostname_qualified`.
pub fn session_key(session_id: &str) -> Option<String> {
    use trusty_common::catchup::session_id::TMUX_WINDOW_ID_PREFIX;
    let base = sanitize_ref_component(session_id)?;
    if !session_id.starts_with(TMUX_WINDOW_ID_PREFIX) {
        return Some(base);
    }
    match sysinfo::System::host_name()
        .as_deref()
        .and_then(sanitize_ref_component)
    {
        Some(host) => sanitize_ref_component(&format!("{host}-{base}")),
        None => Some(base),
    }
}

/// The full ref a session's pauses append to, when both halves resolve.
///
/// Why: the pause response reports the ref name even when the publish fails, so
/// resolving the name is a separate, cheap step from publishing to it.
/// What: `refs/tm/sessions/<user-id>/<session-key>`, or `None` when either half
/// is unresolvable.
/// Test: `a_missing_user_id_skips_the_publish`.
pub fn session_ref_for(repo: &Path, session_id: &str) -> Option<String> {
    let user = resolve_user_id(repo)?;
    let key = session_key(session_id)?;
    Some(format!("{SESSION_REF_ROOT}/{user}/{key}"))
}

/// Credential prefixes a snapshot may not carry to `origin` (ADR-0062 #9).
///
/// Why: this mirrors `trusty_common::memory_core::filter::secret`'s
/// `find_secret_token`, which is the workspace's real credential detector — but
/// that module sits behind trusty-common's heavy `memory-core` feature (HNSW,
/// redb, bundled ONNX), and trusty-mpm's default build deliberately does not
/// pay for it (see `trusty-mpm/Cargo.toml`, `sm-memory`). A pause runs on the
/// PM's hot path, so the dependency edge is not worth it for a gate that only
/// has to refuse a snapshot. This is the prefix half of that detector: the
/// known provider key shapes, which is the part with effectively no false
/// positives. The entropy/charset half is deliberately NOT reproduced here.
/// What: `(prefix, minimum trailing credential characters)`. A bare occurrence
/// of the word `AKIA` in prose does not match; `AKIA` plus 16 key characters
/// does.
const CREDENTIAL_PREFIXES: &[(&str, usize)] = &[
    ("ghp_", 16),
    ("gho_", 16),
    ("ghu_", 16),
    ("ghs_", 16),
    ("ghr_", 16),
    ("github_pat_", 16),
    ("glpat-", 16),
    ("sk-ant-", 16),
    ("sk-proj-", 16),
    ("sk-or-v1-", 16),
    ("xoxb-", 16),
    ("xoxp-", 16),
    ("xoxa-", 16),
    ("xoxs-", 16),
    ("AKIA", 16),
    ("ASIA", 16),
    ("AIza", 16),
    ("npm_", 16),
];

/// Literal needles that are a credential on sight, with no trailing charset run.
const CREDENTIAL_LITERALS: &[&str] = &[
    "-----BEGIN OPENSSH PRIVATE KEY",
    "-----BEGIN RSA PRIVATE KEY",
    "-----BEGIN PRIVATE KEY",
    "-----BEGIN EC PRIVATE KEY",
];

/// A redacted preview of the first credential-shaped token in `content`.
///
/// Why: ADR-0062 decision 9 keeps the pre-push credential gate on this refspec.
/// The preview must name the shape without echoing the secret, so the operator
/// can find the line without the error message becoming a second leak.
/// What: `Some("<prefix>…")` for the first match, else `None`.
/// Test: `a_credential_in_the_snapshot_refuses_the_publish`,
/// `an_ordinary_snapshot_is_not_flagged`.
pub fn scan_for_credentials(content: &str) -> Option<String> {
    for literal in CREDENTIAL_LITERALS {
        if content.contains(literal) {
            return Some((*literal).to_string());
        }
    }
    for (prefix, min_tail) in CREDENTIAL_PREFIXES {
        let mut rest = content;
        while let Some(at) = rest.find(prefix) {
            let tail = &rest[at + prefix.len()..];
            let run = tail
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                .count();
            if run >= *min_tail {
                return Some(format!("{prefix}…"));
            }
            rest = &rest[at + prefix.len()..];
        }
    }
    None
}

/// Run `git -C <repo> <args>`, optionally against a scratch index.
fn git(
    repo: &Path,
    args: &[&str],
    index_file: Option<&Path>,
    env: &[(&str, String)],
    step: &'static str,
) -> Result<String, RefPublishError> {
    let mut cmd = trusty_common::git::command_in(repo);
    cmd.args(args);
    // A pause runs inside the daemon, with no terminal: an https or ssh remote
    // that wants credentials must fail fast into `ref_error`, never block the
    // pause on a `/dev/tty` prompt nobody can answer.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    if let Some(idx) = index_file {
        cmd.env("GIT_INDEX_FILE", idx);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(RefPublishError::Git {
            step,
            message: failure_text(&out),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// git's stderr, or its stdout when stderr said nothing.
fn failure_text(out: &std::process::Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if err.is_empty() {
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        err
    }
}

/// The commit `ref_name` points at on `origin`, or `None` when it does not exist.
///
/// Why: this value is BOTH the parent of the commit about to be written and the
/// lease the push is checked against, so reading it once is what makes the two
/// agree. Reading it from `origin` rather than from the local ref is what makes
/// the lease meaningful — a local ref left ahead by a previously failed push
/// would otherwise lease against itself forever.
/// What: `git ls-remote origin <ref>`; an empty answer means the ref does not
/// exist yet and the next commit starts an orphan chain.
/// Test: `a_first_pause_creates_an_orphan_commit_and_leaves_main_alone`.
pub fn remote_tip(repo: &Path, ref_name: &str) -> Result<Option<String>, RefPublishError> {
    let out = git(
        repo,
        &["ls-remote", "origin", ref_name],
        None,
        &[],
        "ls-remote",
    )?;
    Ok(out
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .map(str::to_string))
}

/// Lease-push `ref_name` to `origin`, expecting it to be at `expected_tip`.
///
/// Why: ADR-0062 decision 3 — one writer per ref, enforced by the remote rather
/// than by hope. This is a separate entry point so the stale-lease arm can be
/// driven directly: the real race (read tip, another writer advances it, push)
/// is not reproducible from one process without injecting the tip.
/// What: `git push --force-with-lease=<ref>:<expected> origin <ref>:<ref>`. An
/// `expected_tip` of `None` renders the empty expectation, which git reads as
/// "this ref must not exist" — verified against real git, not assumed. A
/// rejection is [`RefPublishError::StaleLease`]; the caller never retries with
/// `--force`.
/// Test: `a_stale_lease_is_rejected_and_never_forced`,
/// `a_first_pause_creates_an_orphan_commit_and_leaves_main_alone`.
pub fn push_session_ref(
    repo: &Path,
    ref_name: &str,
    expected_tip: Option<&str>,
) -> Result<(), RefPublishError> {
    let lease = format!(
        "--force-with-lease={ref_name}:{}",
        expected_tip.unwrap_or("")
    );
    let refspec = format!("{ref_name}:{ref_name}");
    match git(
        repo,
        &["push", &lease, "origin", &refspec],
        None,
        &[],
        "push",
    ) {
        Ok(_) => Ok(()),
        Err(RefPublishError::Git {
            step: "push",
            message,
        }) if is_lease_rejection(&message) => Err(RefPublishError::StaleLease {
            ref_name: ref_name.to_string(),
            expected: expected_tip.unwrap_or("<absent>").to_string(),
        }),
        Err(e) => Err(e),
    }
}

/// Whether git's push stderr describes a lost lease rather than a transport error.
fn is_lease_rejection(message: &str) -> bool {
    message.contains("stale info")
        || message.contains("non-fast-forward")
        || message.contains("[rejected]")
}

/// Append one pause snapshot to its session's ref and lease-push it.
///
/// Why: the single write-side entry point for ADR-0062. Wiring it into
/// `session_context_pause` — the one path every pause goes through — is what
/// makes "session history is durable and never lands on a code branch" true by
/// construction (#7830).
/// What, in order: read and credential-scan the snapshot (decision 9, refused
/// BEFORE any object is written); resolve `refs/tm/sessions/<user>/<key>`;
/// register the fetch refspec idempotently (decision 5); read the ref's remote
/// tip, which is both the parent and the lease; fetch that tip locally so
/// `commit-tree` can parent on it; `hash-object -w` the snapshot; build a
/// one-entry tree with `read-tree --empty` + `update-index --cacheinfo` +
/// `write-tree` against a scratch `GIT_INDEX_FILE` in a per-call `TempDir`;
/// `commit-tree` (no parent for the chain's first commit); `update-ref`; and
/// lease-push. Nothing here runs `git add`, `git commit`, `git checkout` or
/// `git stash`, so HEAD, the shared index and the working tree are untouched
/// and no branch moves.
/// Test: `a_first_pause_creates_an_orphan_commit_and_leaves_main_alone`,
/// `a_second_pause_appends_to_the_same_ref`,
/// `a_stale_lease_is_rejected_and_never_forced`,
/// `a_credential_in_the_snapshot_refuses_the_publish`,
/// `a_missing_user_id_skips_the_publish`,
/// `a_rejecting_remote_surfaces_the_push_failure`.
pub fn publish_session_ref(
    req: &SessionRefRequest<'_>,
) -> Result<SessionRefOutcome, RefPublishError> {
    publish_session_ref_as(req, resolve_user_id(req.repo).as_deref())
}

/// [`publish_session_ref`] with the writer's user id supplied explicitly.
///
/// Why: [`resolve_user_id`] reads this HOST's `gh` configuration, so a test
/// cannot drive the "no user id resolved" arm through the public entry point —
/// on a developer machine `gh` always answers, and on CI it never does. This
/// seam makes both arms deterministic without an env-var mutation that every
/// other test in the binary would share.
/// What: identical to [`publish_session_ref`]; `None` is
/// [`RefPublishError::NoUserId`].
/// Test: `a_missing_user_id_skips_the_publish`,
/// `a_first_pause_creates_an_orphan_commit_and_leaves_main_alone`.
pub fn publish_session_ref_as(
    req: &SessionRefRequest<'_>,
    user_id: Option<&str>,
) -> Result<SessionRefOutcome, RefPublishError> {
    let snapshot = std::fs::read_to_string(req.snapshot_path)?;
    if let Some(hit) = scan_for_credentials(&snapshot) {
        return Err(RefPublishError::CredentialDetected(hit));
    }

    let rel = store_relative(req.repo, req.snapshot_path)?;
    let tree_path = format!(
        "{}{rel}",
        trusty_common::catchup::session_refs::SESSIONS_STORE_PREFIX
    );

    let user = user_id
        .and_then(sanitize_ref_component)
        .ok_or(RefPublishError::NoUserId)?;
    let key = session_key(req.session_id)
        .ok_or_else(|| RefPublishError::NoSessionKey(req.session_id.to_string()))?;
    let ref_name = format!("{SESSION_REF_ROOT}/{user}/{key}");

    // `origin` must exist before any remote step; without it this is a local
    // directory that happens to be a checkout, not a publishing target.
    git(
        req.repo,
        &["remote", "get-url", "origin"],
        None,
        &[],
        "remote",
    )
    .map_err(|_| RefPublishError::NoOrigin(req.repo.display().to_string()))?;
    // Decision 5: the refspec is what makes a later catch-up see this ref at all.
    if let Err(e) = trusty_common::catchup::session_refs::ensure_fetch_refspec(req.repo) {
        tracing::warn!(repo = %req.repo.display(), "could not register the session fetch refspec: {e}");
    }

    let parent = remote_tip(req.repo, &ref_name)?;
    if parent.is_some() {
        // Force-fetch the tip into the local ref so `commit-tree -p` can reach
        // it AND so a local ref left ahead by an earlier failed push is reset.
        let refspec = format!("+{ref_name}:{ref_name}");
        git(req.repo, &["fetch", "origin", &refspec], None, &[], "fetch")?;
    }

    let blob = git(
        req.repo,
        // `--no-filters`, and deliberately NOT `--path` (git rejects the pair):
        // the blob must be byte-exact with the file the reader writes back, and
        // `--path` is what makes git apply the `.gitattributes` clean filter or
        // CRLF conversion for a path this store never commits.
        &[
            "hash-object",
            "-w",
            "--no-filters",
            "--",
            &req.snapshot_path.to_string_lossy(),
        ],
        None,
        &[],
        "hash-object",
    )?;

    let scratch = tempfile::TempDir::new()?;
    let index = scratch.path().join("index");
    let idx = Some(index.as_path());
    git(req.repo, &["read-tree", "--empty"], idx, &[], "read-tree")?;
    let cacheinfo = format!("{BLOB_MODE},{blob},{tree_path}");
    git(
        req.repo,
        &["update-index", "--add", "--cacheinfo", &cacheinfo],
        idx,
        &[],
        "update-index",
    )?;
    let tree = git(req.repo, &["write-tree"], idx, &[], "write-tree")?;

    let message = commit_message(req, &rel);
    // `--no-gpg-sign`: a host with `commit.gpgsign = true` would otherwise
    // prompt for a passphrase from inside the daemon and hang the pause.
    let mut args: Vec<&str> = vec!["commit-tree", "--no-gpg-sign", &tree];
    if let Some(p) = &parent {
        args.push("-p");
        args.push(p);
    }
    args.push("-m");
    args.push(&message);
    let commit = git(
        req.repo,
        &args,
        None,
        &commit_identity(&user, req.timestamp),
        "commit-tree",
    )?;

    git(
        req.repo,
        &["update-ref", &ref_name, &commit],
        None,
        &[],
        "update-ref",
    )?;
    push_session_ref(req.repo, &ref_name, parent.as_deref())?;

    Ok(SessionRefOutcome {
        ref_name,
        commit,
        parent,
    })
}

/// The snapshot's path relative to `<repo>/.trusty-mpm/sessions/`.
///
/// Why: the ref's tree stores the snapshot at its repo-relative path, so
/// hydration mirrors a tree entry straight back onto disk. A snapshot outside
/// the store has no such path and is refused rather than stored under a made-up
/// one.
fn store_relative(repo: &Path, snapshot: &Path) -> Result<String, RefPublishError> {
    let store = sessions_store(repo);
    snapshot
        .strip_prefix(&store)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .map_err(|_| RefPublishError::SnapshotOutsideStore(snapshot.display().to_string()))
}

/// The ref commit's message, carrying the trailers hydration reads back.
///
/// Why: the reader rebuilds `sessions-log.jsonl` from the commit, and the ref
/// KEY is deliberately not the session id (it is hostname-qualified, and the
/// user half is a login). Stamping the attribution into the message is what
/// keeps the two ends from having to re-derive each other's naming rules.
/// What: a one-line subject plus `Session-Id` / `Session-Event` /
/// `Session-Snapshot` / `Session-Timestamp` trailers and the shared attribution
/// footer.
/// Test: `the_ref_commit_carries_the_attribution_trailers`; the reader half is
/// `catchup::session_refs`'s `hydration_restores_a_deleted_snapshot_and_its_log_line`
/// in trusty-common.
fn commit_message(req: &SessionRefRequest<'_>, rel: &str) -> String {
    let ts = req.timestamp.to_rfc3339();
    format!(
        "pause {session} {ts}\n\n\
         Session-Id: {session}\n\
         Session-Event: pause\n\
         Session-Snapshot: {rel}\n\
         Session-Timestamp: {ts}\n\n\
         {ATTRIBUTION_FOOTER}\n",
        session = req.session_id,
    )
}

/// Author/committer environment for the ref commit.
///
/// Why: the daemon may run where no git identity is configured at all, and a
/// `commit-tree` that fails on a missing `user.email` would turn a working
/// pause into a reported ref error for no reason. The ref is machine data, so
/// naming the resolved writer is both sufficient and more accurate than an
/// ambient identity.
/// What: the user id as the name, its GitHub no-reply address as the email, and
/// the PAUSE timestamp as both dates — so the chain orders by pause time rather
/// than by whenever the push happened to succeed.
fn commit_identity(user: &str, timestamp: DateTime<Utc>) -> Vec<(&'static str, String)> {
    let email = format!("{user}@users.noreply.github.com");
    let date = timestamp.to_rfc3339();
    vec![
        ("GIT_AUTHOR_NAME", user.to_string()),
        ("GIT_AUTHOR_EMAIL", email.clone()),
        ("GIT_AUTHOR_DATE", date.clone()),
        ("GIT_COMMITTER_NAME", user.to_string()),
        ("GIT_COMMITTER_EMAIL", email),
        ("GIT_COMMITTER_DATE", date),
    ]
}

/// A pause's session-ref receipt, as the MCP response reports it.
///
/// Why: ADR-0062 fail-open — a ref failure must not fail the pause, and must
/// not be silent either (#7830 closure condition 6). Three fields is what a
/// caller needs to tell "off", "published", and "failed, here is why" apart.
/// What: the ref name when one could be resolved, whether the push landed, and
/// the failure text otherwise.
/// Test: `a_failed_ref_publish_is_reported_and_never_fails_the_pause`
/// (`daemon::mcp_context`), `a_disabled_section_publishes_nothing`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRefReceipt {
    /// The ref this pause targeted, when both key halves resolved.
    pub ref_name: Option<String>,
    /// Whether the lease-checked push landed.
    pub published: bool,
    /// Why it did not, when it did not.
    pub error: Option<String>,
}

/// Publish the session ref, converting every failure into a receipt.
///
/// Why: the ONE place ADR-0062's fail-open rule is implemented, so no caller
/// can accidentally make a ref failure fatal to the pause — the local snapshot
/// is already on disk and is the primary write.
/// What: returns an all-`false`/`None` receipt when `enabled` is false (pause
/// then behaves exactly as before ADR-0062, including writing no git config);
/// otherwise runs [`publish_session_ref`] and, on failure, logs a
/// `tracing::warn!` naming the project, the ref and the error before returning
/// it in the receipt.
/// Test: `a_disabled_section_publishes_nothing`,
/// `a_rejecting_remote_surfaces_the_push_failure`,
/// `a_failed_ref_publish_is_reported_and_never_fails_the_pause`.
pub fn publish_receipt(req: &SessionRefRequest<'_>, enabled: bool) -> SessionRefReceipt {
    if !enabled {
        return SessionRefReceipt::default();
    }
    // Resolved ONCE: `resolve_user_id` reads `gh`'s config and may shell out to
    // `git config`, and the receipt needs the same answer the publish uses.
    let user_id = resolve_user_id(req.repo);
    let ref_name = user_id
        .as_deref()
        .zip(session_key(req.session_id))
        .map(|(user, key)| format!("{SESSION_REF_ROOT}/{user}/{key}"));
    match publish_session_ref_as(req, user_id.as_deref()) {
        Ok(out) => SessionRefReceipt {
            ref_name: Some(out.ref_name),
            published: true,
            error: None,
        },
        Err(e) => {
            let message = e.to_string();
            tracing::warn!(
                project = %req.repo.display(),
                session_ref = ref_name.as_deref().unwrap_or("<unresolved>"),
                "session ref publish failed: {message}"
            );
            SessionRefReceipt {
                ref_name,
                published: false,
                error: Some(message),
            }
        }
    }
}

/// The absolute path of a project's session store, for callers that build one.
///
/// Why: three call sites spelled `.trusty-mpm/sessions` by hand; one place is
/// enough.
pub fn sessions_store(repo: &Path) -> PathBuf {
    repo.join(".trusty-mpm").join("sessions")
}

#[cfg(test)]
#[path = "session_ref_publish_tests.rs"]
mod tests;
