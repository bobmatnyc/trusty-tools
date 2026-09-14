//! Read side of ADR-0062: hydrate the local `.trusty-mpm/sessions/` cache from
//! the per-session git refs a pause publishes (#7830).
//!
//! Why: ADR-0061's 2026-09-13 amendment made `.trusty-mpm/sessions/`
//! gitignored, so the store became a machine-local cache with no durable copy.
//! ADR-0062 puts the durable copy in `refs/tm/sessions/<user-id>/<session-key>`
//! — an orphan, append-only commit chain per session, invisible to a plain
//! clone. A fresh clone therefore starts with an EMPTY cache, and the whole
//! resume path (`session_finder::find_paused_sessions`,
//! `session_log::resolve_session_snapshot`) reads the cache, never the refs.
//! This module is the one place that turns refs back into cache files, so the
//! read path stays unchanged.
//!
//! **A ref is remote input, and the cache it feeds becomes the resuming PM's
//! todos.** Every trust decision here is therefore taken from the REF KEY,
//! which the remote's own ref namespace pins, and never from the commit's own
//! text: [`LocalRefIdentity`] decides which refs are this writer's at all, the
//! synthesized `session_id` is derived from the key, and exactly one blob — the
//! `session-*.md` the commit names, inside that session's own directory — is
//! ever written. A tree carrying `sessions-log.jsonl`, a second snapshot, or a
//! `Session-Id` trailer disagreeing with the key changes nothing on disk.
//! What: [`ensure_fetch_refspec`] adds `+refs/tm/sessions/*:refs/tm/sessions/*`
//! to `remote.origin.fetch` idempotently — without it a plain fetch sees no
//! session refs at all (ADR-0062 decision 5). [`list_session_refs`] is the
//! `git for-each-ref` aggregator of decision 6. [`hydrate_session_cache`]
//! composes the two under one [`LocalRefIdentity`].
//!
//! Errors are `anyhow`, matching every sibling in this module
//! ([`super::pause`], [`super::session_log`]); the typed, branch-on-it error is
//! on the WRITE side, in `trusty_mpm::core::session_ref_publish`, where a
//! caller has to tell a stale lease from a transport failure.
//! Test: `session_refs_tests.rs`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

use super::session_id::TMUX_WINDOW_ID_PREFIX;
use super::session_log::{self, SessionLogEntry};

/// Namespace every session ref lives under (ADR-0062 decision 1).
pub const SESSION_REF_PREFIX: &str = "refs/tm/sessions/";

/// The dedicated fetch refspec session refs travel on (ADR-0062 decision 5).
///
/// Why: `refs/tm/sessions/**` is outside `refs/heads/**`, so the default
/// `+refs/heads/*:refs/remotes/origin/*` never carries it. A clone that does
/// not add this line sees no session data — the intended default for machine
/// state, and what test `a_plain_fetch_without_the_refspec_sees_no_session_refs`
/// pins.
pub const SESSION_REF_FETCH_REFSPEC: &str = "+refs/tm/sessions/*:refs/tm/sessions/*";

/// Repo-relative root of the session cache, as it appears inside a ref's tree.
///
/// Why: the write side stores each snapshot at its repo-relative path, so the
/// reader can mirror the named tree entry straight back onto disk with no
/// second naming convention to keep in sync.
pub const SESSIONS_STORE_PREFIX: &str = ".trusty-mpm/sessions/";

/// `remote.origin.fetch` — the config key the refspec is appended to.
const FETCH_CONFIG_KEY: &str = "remote.origin.fetch";

/// Who this checkout is, for deciding which session refs are its own.
///
/// Why: hydration writes into the store the resuming PM reads as its own state,
/// so "which refs may write here" is a trust decision and must not be taken
/// from data the remote controls. The ref KEY carries both halves of the
/// answer, and this is what it is compared against. `trusty-common` cannot
/// resolve a `gh` login (that lives in trusty-mpm alongside the write side), so
/// the caller supplies it.
/// What: the sanitized user id the write side publishes under, and this host's
/// sanitized name — `None` only when no hostname resolves, which is also the
/// case in which the write side leaves a tmux session key unqualified.
/// Test: `a_foreign_users_ref_is_ignored`, `a_foreign_hosts_tmux_ref_is_ignored`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRefIdentity {
    /// The `<user-id>` half of the refs this checkout owns.
    pub user_id: String,
    /// This host's sanitized name, as the write side prefixes a tmux key with.
    pub host: Option<String>,
}

/// One `refs/tm/sessions/**` ref and the commit it points at.
///
/// Why: the aggregator of ADR-0062 decision 6 answers "which sessions exist and
/// where is each one's tip" in one pass, so a caller never re-runs
/// `for-each-ref` per session.
/// What: the full ref name and its tip commit id.
/// Test: `list_session_refs_enumerates_every_session`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRefTip {
    /// Full ref name, e.g. `refs/tm/sessions/octocat/host-tmux-window-230`.
    pub name: String,
    /// The commit the ref points at.
    pub commit: String,
}

/// What one [`hydrate_session_cache`] pass restored.
///
/// Why: hydration is fail-open and mostly a no-op, so a caller that logs it
/// needs to tell "nothing to do" from "restored four snapshots" without
/// re-listing the directory.
/// What: how many refs the aggregator found (including refs this identity does
/// not own and therefore skipped), which snapshot files were written, and how
/// many `sessions-log.jsonl` pause lines were synthesized.
/// Test: `hydration_restores_a_deleted_snapshot_and_its_log_line`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HydrationOutcome {
    /// Number of `refs/tm/sessions/**` refs the aggregator found.
    pub refs_seen: usize,
    /// Absolute paths of the snapshot files this pass created.
    pub snapshots_written: Vec<PathBuf>,
    /// Number of `pause` entries appended to `sessions-log.jsonl`.
    pub log_entries_added: usize,
}

/// One completed `git` invocation, as this module needs it.
struct GitOut {
    success: bool,
    stdout: Vec<u8>,
    stderr: String,
}

impl GitOut {
    /// stdout as UTF-8, trimmed.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }
}

/// Run `git -C <repo> <args>` through the workspace's single git entry point.
///
/// Why (#7171): every git spawn in the workspace goes through
/// [`crate::git::command_in`], which pins `maintenance.auto=false` /
/// `gc.auto=0` on argv so a fleet of worktrees cannot each trigger background
/// maintenance against one shared object store.
/// What: captures raw stdout bytes — a snapshot blob is copied through this,
/// and lossy UTF-8 would corrupt it — plus the exit status and stderr.
/// Hydration runs inside the daemon, which has no terminal, so
/// `GIT_TERMINAL_PROMPT=0` and an SSH `BatchMode=yes` are pinned on EVERY
/// spawn: a remote that wants a credential must fail fast, never block a
/// catch-up on a `/dev/tty` prompt nobody can answer.
/// Test: exercised by every test in `session_refs_tests.rs`.
fn git(repo: &Path, args: &[&str]) -> Result<GitOut> {
    let out = crate::git::command_in(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
        .output()
        .with_context(|| format!("could not run `git {}`", args.join(" ")))?;
    Ok(GitOut {
        success: out.status.success(),
        stdout: out.stdout,
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

/// Run `git -C <repo> <args>` and fail with git's own stderr on a non-zero exit.
fn git_ok(repo: &Path, args: &[&str], step: &str) -> Result<GitOut> {
    let out = git(repo, args)?;
    if !out.success {
        bail!("{step}: `git {}` failed: {}", args.join(" "), out.stderr);
    }
    Ok(out)
}

/// Add the session-ref fetch refspec to `remote.origin.fetch`, once.
///
/// Why: ADR-0062 decision 5 — session refs travel on their own refspec, and a
/// checkout that has not opted in must keep seeing nothing. Adding it on the
/// first pause or catch-up is what makes the opt-in automatic for the tooling
/// that knows to ask, while a plain clone stays clean.
/// What: reads `git config --get-all remote.origin.fetch` FIRST and returns
/// `Ok(false)` when the line is already there, so repeated pauses never
/// accumulate duplicates; otherwise appends it with `--add` and returns
/// `Ok(true)`. A directory with no `origin` remote is an error, not a silent
/// success — the caller decides whether that is fatal.
/// Test: `ensure_fetch_refspec_is_idempotent`,
/// `a_plain_fetch_without_the_refspec_sees_no_session_refs`.
pub fn ensure_fetch_refspec(repo: &Path) -> Result<bool> {
    let existing = git(repo, &["config", "--get-all", FETCH_CONFIG_KEY])?;
    // Exit 1 with no output is git's answer for "the key is unset", not a
    // failure; only a louder error deserves to stop the caller.
    if !existing.success && !existing.stderr.is_empty() {
        bail!("could not read {FETCH_CONFIG_KEY}: {}", existing.stderr);
    }
    let already = String::from_utf8_lossy(&existing.stdout)
        .lines()
        .any(|line| line.trim() == SESSION_REF_FETCH_REFSPEC);
    if already {
        return Ok(false);
    }
    git_ok(
        repo,
        &[
            "config",
            "--add",
            FETCH_CONFIG_KEY,
            SESSION_REF_FETCH_REFSPEC,
        ],
        "add-refspec",
    )?;
    Ok(true)
}

/// Every `refs/tm/sessions/**` ref in this checkout, with its tip.
///
/// Why: ADR-0062 decision 6 — the daemon reports who is doing what by reading
/// refs, never by committing. `for-each-ref` is that read.
/// What: `git for-each-ref --format=<sha> <ref> refs/tm/sessions/`, parsed into
/// [`SessionRefTip`]s in git's own (lexicographic) order. An empty namespace
/// yields an empty vec, not an error. This enumerates EVERY session ref,
/// including other users'; deciding which are this checkout's own is
/// [`hydrate_session_cache`]'s job.
/// Test: `list_session_refs_enumerates_every_session`.
pub fn list_session_refs(repo: &Path) -> Result<Vec<SessionRefTip>> {
    let out = git_ok(
        repo,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            SESSION_REF_PREFIX,
        ],
        "for-each-ref",
    )?;
    let mut refs = Vec::new();
    for line in out.text().lines() {
        let Some((commit, name)) = line.split_once(' ') else {
            continue;
        };
        refs.push(SessionRefTip {
            name: name.trim().to_string(),
            commit: commit.trim().to_string(),
        });
    }
    Ok(refs)
}

/// The blob sha of `want` in `commit`'s tree, if the tree carries it.
///
/// Why: only ONE path is ever materialized — the `session-*.md` the commit
/// names — so the tree is queried for that path rather than walked. A tree
/// carrying `sessions-log.jsonl`, a second snapshot, or anything else
/// contributes nothing (#7830 review, CRITICAL).
/// What: `git ls-tree <commit> -- <want>` lines are `<mode> <type> <sha>\t<path>`;
/// the sha is returned only for a `blob` row whose path is exactly `want`.
/// Test: `a_tree_carrying_extra_blobs_writes_only_the_named_snapshot`.
fn blob_at(repo: &Path, commit: &str, want: &str) -> Result<Option<String>> {
    let out = git_ok(repo, &["ls-tree", commit, "--", want], "ls-tree")?;
    for line in out.text().lines() {
        let Some((meta, path)) = line.split_once('\t') else {
            continue;
        };
        let fields: Vec<&str> = meta.split_whitespace().collect();
        if fields.len() >= 3 && fields[1] == "blob" && path.trim() == want {
            return Ok(Some(fields[2].to_string()));
        }
    }
    Ok(None)
}

/// The trailer value for `key` in a ref commit's message, if present.
///
/// Why: the write side stamps `Session-Snapshot` / `Session-Timestamp` into the
/// commit message so hydration knows WHICH path in the tree is the snapshot and
/// when the pause happened. `Session-Id` is deliberately NOT read from here —
/// see [`session_id_for_key`].
/// What: scans `git cat-file commit <sha>` output for `<key>: <value>`.
/// Test: `commit_trailers_are_read_back`.
fn commit_trailer(message: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    message.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(&needle)?;
        let value = rest.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// Split `refs/tm/sessions/<user>/<key>` into its two halves.
fn split_ref_key(ref_name: &str) -> Option<(&str, &str)> {
    let rest = ref_name.strip_prefix(SESSION_REF_PREFIX)?;
    let (user, key) = rest.split_once('/')?;
    (!user.is_empty() && !key.is_empty() && !key.contains('/')).then_some((user, key))
}

/// The session id a ref key names, when this identity owns that ref.
///
/// Why (#7830 review, CRITICAL): the session id decides which directory a
/// snapshot lands in AND which session `sessions-log.jsonl` attributes it to —
/// which is what `resolve_session_snapshot` and `redact_sessions_not_owned_by`
/// both read. Taking it from the commit's `Session-Id` trailer let any writer
/// with push access to the refspec file forged text under a victim session's
/// id, and the resuming PM would read it as its own todos. The ref KEY is
/// pinned by the remote's ref namespace, so it is the only trustworthy source.
/// The same rule fixes a benign bug: host A's `hostA-tmux-window-230` used to
/// hydrate into host B's `sessions/tmux-window-230/`, undoing exactly the
/// hostname qualification the write side adds.
/// What, in order: (1) a key prefixed with THIS host's name followed by
/// `tmux-window-` is ours, and the session id is the key with that prefix
/// removed; (2) a BARE `tmux-window-N` key is ours only when no hostname
/// resolves here, because that is the one case in which the write side leaves
/// it unqualified; (3) any other key mentioning `tmux-window-` belongs to some
/// other host and is refused; (4) anything else is a globally unique managed
/// session id and is its own key.
/// Test: `a_foreign_hosts_tmux_ref_is_ignored`,
/// `a_host_qualified_tmux_ref_hydrates_under_the_bare_session_id`,
/// `a_managed_session_key_is_its_own_session_id`.
fn session_id_for_key(key: &str, identity: &LocalRefIdentity) -> Option<String> {
    if let Some(host) = &identity.host
        && let Some(rest) = key.strip_prefix(&format!("{host}-"))
        && rest.starts_with(TMUX_WINDOW_ID_PREFIX)
    {
        return Some(rest.to_string());
    }
    if key.starts_with(TMUX_WINDOW_ID_PREFIX) {
        return identity.host.is_none().then(|| key.to_string());
    }
    if key.contains(TMUX_WINDOW_ID_PREFIX) {
        return None;
    }
    Some(key.to_string())
}

/// Whether `rel` is a snapshot path `session_id` is allowed to own.
///
/// Why (#7830 review, CRITICAL): `rel` comes from a commit message, so without
/// this a ref could name `sessions-log.jsonl` — whose every component is
/// `Normal`, so containment alone passes — and overwrite the attribution index
/// on a fresh clone, or write into another session's directory.
/// What: every component is an ordinary name; the basename is `session-*.md`,
/// the shape [`super::pause::write_pause_snapshot`] writes and
/// `session_log::snapshot_path_in` reads; and the path is either flat at the
/// store root (the pre-#5272 / unsafe-id layout) or one level under this
/// session's OWN directory.
/// Test: `a_tree_carrying_extra_blobs_writes_only_the_named_snapshot`,
/// `a_traversing_tree_path_is_refused`.
fn is_own_snapshot_path(rel: &str, session_id: &str) -> bool {
    let path = Path::new(rel);
    let components: Vec<&std::ffi::OsStr> = path
        .components()
        .map(|c| match c {
            std::path::Component::Normal(n) => Some(n),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default();
    if components.is_empty() || components.len() > 2 {
        return false;
    }
    let Some(name) = components.last().and_then(|n| n.to_str()) else {
        return false;
    };
    if !(name.starts_with("session-") && name.ends_with(".md")) {
        return false;
    }
    match components.len() {
        1 => true,
        _ => components[0].to_str() == Some(session_id),
    }
}

/// Restore this project's session cache from the session refs this identity owns.
///
/// Why: ADR-0062 decision 4 makes `.trusty-mpm/sessions/` a cache and the ref
/// the durable copy, so a fresh clone (or a deleted cache) has to be able to
/// rebuild what resume reads. Running this BEFORE
/// `session_finder::find_paused_sessions` is what lets the entire read path
/// stay unchanged (#7830 closure condition 4).
/// What: [`ensure_fetch_refspec`], then a fetch SCOPED to
/// [`SESSION_REF_FETCH_REFSPEC`] — a catch-up must not pull every branch on the
/// remote as a side effect — then, for every ref [`list_session_refs`] reports
/// whose key [`session_id_for_key`] says this identity owns, the tip commit's
/// `Session-Snapshot` path, and ONLY that path, is written back when it is
/// missing on disk and passes [`is_own_snapshot_path`]. The matching `pause`
/// line is appended when the log has none. An existing file is NEVER
/// overwritten: a live session's working copy is newer than any ref, and
/// clobbering it would lose the pause in progress.
///
/// Only the tip is read. Each ref commit's tree carries exactly the one
/// snapshot that pause wrote (ADR-0062 decision 7 keeps the chain's commits
/// coarse and the jsonl out of the tree), and the tip is the newest pause —
/// which is the only one `resolve_session_snapshot` would resolve. Walking the
/// whole chain to rehydrate a session's full history is deliberately out of
/// scope, alongside the retention policy ADR-0062 decision 8 defers.
/// Test: `hydration_restores_a_deleted_snapshot_and_its_log_line`,
/// `hydration_never_overwrites_a_file_already_on_disk`,
/// `hydration_is_idempotent_across_two_runs`,
/// `a_foreign_users_ref_is_ignored`, `a_foreign_hosts_tmux_ref_is_ignored`,
/// `a_tree_carrying_extra_blobs_writes_only_the_named_snapshot`,
/// `a_forged_session_id_trailer_loses_to_the_ref_key`.
pub fn hydrate_session_cache(repo: &Path, identity: &LocalRefIdentity) -> Result<HydrationOutcome> {
    ensure_fetch_refspec(repo)?;
    git_ok(
        repo,
        &["fetch", "origin", SESSION_REF_FETCH_REFSPEC],
        "fetch",
    )?;

    let sessions_dir = repo.join(".trusty-mpm").join("sessions");
    let mut outcome = HydrationOutcome::default();
    // Read the log ONCE: it was re-read per tree entry per ref, which is
    // O(refs x log lines) on a store whose whole point is to accumulate.
    let mut recorded: HashSet<String> = session_log::read_log(&sessions_dir)
        .into_iter()
        .filter(|e| e.event == session_log::EVENT_PAUSE)
        .map(|e| e.snapshot)
        .collect();

    for tip in list_session_refs(repo)? {
        outcome.refs_seen += 1;
        let Some((user, key)) = split_ref_key(&tip.name) else {
            continue;
        };
        if user != identity.user_id {
            continue;
        }
        let Some(session_id) = session_id_for_key(key, identity) else {
            continue;
        };
        let message = git_ok(repo, &["cat-file", "commit", &tip.commit], "cat-file")?.text();
        let Some(rel) = commit_trailer(&message, "Session-Snapshot") else {
            continue;
        };
        if !is_own_snapshot_path(&rel, &session_id) {
            tracing::warn!(
                ref_name = %tip.name,
                snapshot = %rel,
                "session ref names a snapshot path it does not own; skipped"
            );
            continue;
        }
        let tree_path = format!("{SESSIONS_STORE_PREFIX}{rel}");
        let Some(blob) = blob_at(repo, &tip.commit, &tree_path)? else {
            continue;
        };

        let target = sessions_dir.join(&rel);
        if !target.exists() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = git_ok(repo, &["cat-file", "blob", &blob], "cat-file")?;
            std::fs::write(&target, &bytes.stdout)?;
            outcome.snapshots_written.push(target);
        }
        if !recorded.contains(&rel) {
            let timestamp = commit_trailer(&message, "Session-Timestamp")
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
            session_log::append_entry(
                &sessions_dir,
                &SessionLogEntry {
                    session_id,
                    event: session_log::EVENT_PAUSE.to_string(),
                    snapshot: rel.clone(),
                    timestamp,
                },
            )?;
            recorded.insert(rel);
            outcome.log_entries_added += 1;
        }
    }
    Ok(outcome)
}

#[cfg(test)]
#[path = "session_refs_tests.rs"]
mod tests;
