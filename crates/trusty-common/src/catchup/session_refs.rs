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
//! What: [`ensure_fetch_refspec`] adds `+refs/tm/sessions/*:refs/tm/sessions/*`
//! to `remote.origin.fetch` idempotently — without it a plain fetch sees no
//! session refs at all (ADR-0062 decision 5). [`list_session_refs`] is the
//! `git for-each-ref` aggregator of decision 6. [`hydrate_session_cache`]
//! composes the two: fetch, enumerate, read each tip's tree, and materialize
//! every snapshot missing on disk plus the `sessions-log.jsonl` pause line that
//! attributes it. Nothing here ever overwrites a file that already exists —
//! the working tree is the live copy while a session is running.
//!
//! Errors are `anyhow`, matching every sibling in this module
//! ([`super::pause`], [`super::session_log`]); the typed, branch-on-it error is
//! on the WRITE side, in `trusty_mpm::core::session_ref_publish`, where a
//! caller has to tell a stale lease from a transport failure.
//! Test: `session_refs_tests.rs`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

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
/// reader can mirror a tree entry straight back onto disk with no second
/// naming convention to keep in sync.
pub const SESSIONS_STORE_PREFIX: &str = ".trusty-mpm/sessions/";

/// `remote.origin.fetch` — the config key the refspec is appended to.
const FETCH_CONFIG_KEY: &str = "remote.origin.fetch";

/// One `refs/tm/sessions/**` ref and the commit it points at.
///
/// Why: the aggregator of ADR-0062 decision 6 answers "which sessions exist and
/// where is each one's tip" in one pass, so a caller never re-runs
/// `for-each-ref` per session.
/// What: the full ref name and its tip commit id.
/// Test: `list_session_refs_enumerates_every_session`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRefTip {
    /// Full ref name, e.g. `refs/tm/sessions/octocat/tmux-window-230`.
    pub name: String,
    /// The commit the ref points at.
    pub commit: String,
}

/// What one [`hydrate_session_cache`] pass restored.
///
/// Why: hydration is fail-open and mostly a no-op, so a caller that logs it
/// needs to tell "nothing to do" from "restored four snapshots" without
/// re-listing the directory.
/// What: how many refs were seen, which snapshot files were written, and how
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
/// Test: exercised by every test in `session_refs_tests.rs`.
fn git(repo: &Path, args: &[&str]) -> Result<GitOut> {
    let out = crate::git::command_in(repo)
        .args(args)
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
/// yields an empty vec, not an error.
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

/// One `<path>` → `<blob sha>` entry from a ref tip's tree.
type TreeEntry = (String, String);

/// The regular-file entries in `commit`'s tree, recursively.
///
/// What: `git ls-tree -r <commit>` lines are `<mode> <type> <sha>\t<path>`;
/// only `blob` rows are returned, as `(path, sha)`.
fn tree_entries(repo: &Path, commit: &str) -> Result<Vec<TreeEntry>> {
    let out = git_ok(repo, &["ls-tree", "-r", commit], "ls-tree")?;
    let mut entries = Vec::new();
    for line in out.text().lines() {
        let Some((meta, path)) = line.split_once('\t') else {
            continue;
        };
        let fields: Vec<&str> = meta.split_whitespace().collect();
        if fields.len() < 3 || fields[1] != "blob" {
            continue;
        }
        entries.push((path.trim().to_string(), fields[2].to_string()));
    }
    Ok(entries)
}

/// The trailer value for `key` in a ref commit's message, if present.
///
/// Why: the write side stamps `Session-Id`/`Session-Snapshot`/`Session-Timestamp`
/// into the commit message so hydration can rebuild the `sessions-log.jsonl`
/// attribution line without inferring it from the ref name — the ref key is the
/// user plus a hostname-qualified session key, which is deliberately NOT the
/// session id (#7830).
/// What: scans `git cat-file commit <sha>` output for `<key>: <value>`.
fn commit_trailer(message: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    message.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(&needle)?;
        let value = rest.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// Restore this project's session cache from its session refs.
///
/// Why: ADR-0062 decision 4 makes `.trusty-mpm/sessions/` a cache and the ref
/// the durable copy, so a fresh clone (or a deleted cache) has to be able to
/// rebuild what resume reads. Running this BEFORE
/// `session_finder::find_paused_sessions` is what lets the entire read path
/// stay unchanged (#7830 closure condition 4).
/// What: [`ensure_fetch_refspec`], then a plain `git fetch origin` — which now
/// carries the session refspec because of that config line, and would carry
/// nothing without it — then, for every ref [`list_session_refs`] reports, reads
/// the TIP commit's tree and writes back any `.trusty-mpm/sessions/**` blob
/// that is missing on disk, plus the matching `pause` line in
/// `sessions-log.jsonl` when the log has none for that snapshot. An existing
/// file is NEVER overwritten: a live session's working copy is newer than any
/// ref, and clobbering it would lose the pause in progress.
///
/// Only the tip is read. Each ref commit's tree carries exactly the one
/// snapshot that pause wrote (ADR-0062 decision 7 keeps the chain's commits
/// coarse and the jsonl out of the tree), and the tip is the newest pause —
/// which is the only one `resolve_session_snapshot` would resolve. Walking the
/// whole chain to rehydrate a session's full history is deliberately out of
/// scope, alongside the retention policy ADR-0062 decision 8 defers.
/// Test: `hydration_restores_a_deleted_snapshot_and_its_log_line`,
/// `hydration_never_overwrites_a_file_already_on_disk`,
/// `hydration_is_idempotent_across_two_runs`.
pub fn hydrate_session_cache(repo: &Path) -> Result<HydrationOutcome> {
    ensure_fetch_refspec(repo)?;
    git_ok(repo, &["fetch", "origin"], "fetch")?;

    let sessions_dir = repo.join(".trusty-mpm").join("sessions");
    let mut outcome = HydrationOutcome::default();

    for tip in list_session_refs(repo)? {
        outcome.refs_seen += 1;
        let message = git_ok(repo, &["cat-file", "commit", &tip.commit], "cat-file")?.text();
        for (path, blob) in tree_entries(repo, &tip.commit)? {
            let Some(rel) = path.strip_prefix(SESSIONS_STORE_PREFIX) else {
                continue;
            };
            if !is_contained_relative(rel) {
                tracing::warn!(ref_name = %tip.name, path = %path, "session ref carries a path outside the store; skipped");
                continue;
            }
            let target = sessions_dir.join(rel);
            if !target.exists() {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let blob_bytes = git_ok(repo, &["cat-file", "blob", &blob], "cat-file")?;
                std::fs::write(&target, &blob_bytes.stdout)?;
                outcome.snapshots_written.push(target);
            }
            if synthesize_log_entry(&sessions_dir, &message, rel)? {
                outcome.log_entries_added += 1;
            }
        }
    }
    Ok(outcome)
}

/// Whether `rel` is a plain relative path that stays inside the store.
///
/// Why: a ref is remote input. A tree entry naming `../../.ssh/authorized_keys`
/// would otherwise be written outside `.trusty-mpm/sessions/`, which is the
/// same containment rule `session_log::snapshot_path_in` applies to the log's
/// own untrusted `snapshot` field.
fn is_contained_relative(rel: &str) -> bool {
    !rel.is_empty()
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Append the `pause` line that attributes `rel` to its session, if absent.
///
/// Why: `resolve_session_snapshot` reads `sessions-log.jsonl`, not the
/// directory — a snapshot with no pause line belongs to nobody and resolves for
/// nobody (#5272). A hydrated file without its log line would restore the bytes
/// and still fail the resume, so the two are restored together.
/// What: reads the commit's `Session-Id` / `Session-Timestamp` trailers and
/// appends a [`SessionLogEntry`] whose `snapshot` is `rel` — store-relative,
/// exactly as [`super::pause::write_pause_snapshot`] records it. Returns
/// `Ok(false)` when the log already carries a `pause` line for this snapshot,
/// or when the commit names no session id.
/// Test: `hydration_restores_a_deleted_snapshot_and_its_log_line`,
/// `hydration_is_idempotent_across_two_runs`.
fn synthesize_log_entry(sessions_dir: &Path, message: &str, rel: &str) -> Result<bool> {
    let Some(session_id) = commit_trailer(message, "Session-Id") else {
        return Ok(false);
    };
    let already = session_log::read_log(sessions_dir)
        .iter()
        .any(|e| e.event == session_log::EVENT_PAUSE && e.snapshot == rel);
    if already {
        return Ok(false);
    }
    let timestamp = commit_trailer(message, "Session-Timestamp")
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    session_log::append_entry(
        sessions_dir,
        &SessionLogEntry {
            session_id,
            event: session_log::EVENT_PAUSE.to_string(),
            snapshot: rel.to_string(),
            timestamp,
        },
    )?;
    Ok(true)
}

#[cfg(test)]
#[path = "session_refs_tests.rs"]
mod tests;
