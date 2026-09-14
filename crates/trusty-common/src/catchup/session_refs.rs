//! Read side of ADR-0062: hydrate the local `.trusty-mpm/sessions/` cache from
//! the caller's own per-session git ref (#7830).
//!
//! Why: ADR-0061's 2026-09-13 amendment made `.trusty-mpm/sessions/`
//! gitignored, so the store became a machine-local cache with no durable copy.
//! ADR-0062 puts the durable copy in `refs/tm/sessions/<user-id>/<session-key>`
//! — an orphan, append-only commit chain per session, invisible to a plain
//! clone. A fresh clone therefore starts with an EMPTY cache, and the whole
//! resume path (`session_finder::find_paused_sessions`,
//! `session_log::resolve_session_snapshot`,
//! `resolve::newest_snapshot_in_window`) reads the cache, never the refs. This
//! module is the one place that turns a ref back into cache files, so the read
//! path stays unchanged.
//!
//! # Trust boundary — read this before widening anything here
//!
//! **`refs/tm/sessions/**` carries no server-side access control. Anyone with
//! push access to `origin` can create a ref under ANY user id and any session
//! key, with any content.** The owner's 2026-09-14 ruling (ADR-0062 decision
//! 10) accepts that: a collaborator with push access is trusted to write
//! session history, by the same permission that lets them push a branch. The
//! gate below is DEFENCE IN DEPTH, not authentication — it limits which ref is
//! read and cannot verify who wrote it. Two consequences follow, and both are
//! why the scope is as narrow as it is:
//!
//! - Hydration reads exactly ONE ref — the caller's own
//!   `<user-id>/<session-key>` ([`SessionRefTarget`]) — and never enumerates
//!   "every ref under my user id". A forged sibling ref under the same login
//!   would otherwise be materialized, and a snapshot with a future-dated
//!   filename and a matching `## Tmux Window` body wins
//!   `resolve::newest_snapshot_in_window`'s newest-first pick, making attacker
//!   text the resuming PM's todos (#7830 review round 2).
//! - Within that one ref, only the single `session-*.md` the commit names, at a
//!   path that session is allowed to own, is written. The tree is never walked,
//!   so a ref carrying `sessions-log.jsonl` — whose components are all `Normal`,
//!   so a containment check alone passes — cannot overwrite the attribution
//!   index, and the synthesized `session_id` comes from the CALLER, never from
//!   the commit's `Session-Id` trailer.
//!
//! What: [`ensure_fetch_refspec`] adds `+refs/tm/sessions/*:refs/tm/sessions/*`
//! to `remote.origin.fetch` idempotently — without it a plain fetch sees no
//! session refs at all (ADR-0062 decision 5). [`list_session_refs`] is the
//! `git for-each-ref` aggregator of decision 6, which reports what EXISTS
//! without implying any of it is trusted. [`hydrate_session_cache`] composes
//! them for one [`SessionRefTarget`].
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
/// reader can mirror the named tree entry straight back onto disk with no
/// second naming convention to keep in sync.
pub const SESSIONS_STORE_PREFIX: &str = ".trusty-mpm/sessions/";

/// `remote.origin.fetch` — the config key the refspec is appended to.
const FETCH_CONFIG_KEY: &str = "remote.origin.fetch";

/// The ONE ref a hydration pass may read, and the session it belongs to.
///
/// Why: see the module's trust-boundary section. The ref namespace is
/// unauthenticated, so the narrowest useful scope is the caller's own ref —
/// the one this very session's pauses write. Widening this to a prefix scan
/// reopens the forged-sibling-ref attack, so the type carries an exact key
/// rather than a pattern.
/// What: the `<user-id>` and `<session-key>` halves of the ref name, plus the
/// session id those pauses were filed under locally. `session_id` is what the
/// synthesized `sessions-log.jsonl` line is attributed to; it comes from the
/// caller, never from the commit.
/// Test: `only_the_callers_own_ref_is_hydrated`, `a_foreign_users_ref_is_ignored`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRefTarget {
    /// The `<user-id>` half of the ref this caller publishes under.
    pub user_id: String,
    /// The `<session-key>` half — host-qualified for a tmux-derived session.
    pub session_key: String,
    /// The local session id the restored snapshot is attributed to.
    pub session_id: String,
}

impl SessionRefTarget {
    /// The full ref name this target names.
    pub fn ref_name(&self) -> String {
        format!("{SESSION_REF_PREFIX}{}/{}", self.user_id, self.session_key)
    }
}

/// One `refs/tm/sessions/**` ref and the commit it points at.
///
/// Why: the aggregator of ADR-0062 decision 6 answers "which session refs exist
/// and where is each one's tip" in one pass. It reports what EXISTS on the
/// remote — not what is trusted; see the module's trust boundary.
/// What: the full ref name and its tip commit id.
/// Test: `list_session_refs_enumerates_every_session`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRefTip {
    /// Full ref name, e.g. `refs/tm/sessions/octocat/host-tmux-window-230`.
    pub name: String,
    /// The commit the ref points at.
    pub commit: String,
}

/// What one [`hydrate_session_cache`] pass produced.
///
/// Why (#7830 review round 2): `refs_seen` alone cannot be read as success. A
/// host that changed its name, or an operator who switched `gh` account, owns
/// none of its old refs any more — every pass then sees five refs, restores
/// nothing, and reports the same shape as a pass with nothing to do.
/// `own_ref_found` and the written-snapshot count are what separate the two.
/// What: how many refs the aggregator enumerated, whether the caller's own ref
/// was among them, which snapshot files were written, and how many
/// `sessions-log.jsonl` pause lines were synthesized.
///
/// `own_ref_found` is deliberately NOT called `owned`: `sessions[].owned` in the
/// same catch-up response is a per-session boolean about attribution, and one
/// paragraph carrying both would read as one concept (#7830 review round 3).
/// Test: `hydration_restores_a_deleted_snapshot_and_its_log_line`,
/// `only_the_callers_own_ref_is_hydrated`, `a_foreign_users_ref_is_ignored`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HydrationOutcome {
    /// Number of `refs/tm/sessions/**` refs the aggregator found, trusted or not.
    pub refs_seen: usize,
    /// Whether the caller's own ref was among them.
    pub own_ref_found: bool,
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

/// Fail unless `repo` is a checkout with an `origin` remote.
///
/// Why (#7830 review round 2): `git config --add remote.origin.fetch <spec>` on
/// a repo with NO origin does not fail — it CREATES a phantom `origin` with an
/// empty URL, which then breaks every later `git fetch origin` in that
/// checkout. Probing first is what keeps a catch-up from mutating a project it
/// has nothing to say about. Same order the write side uses
/// (`session_ref_publish::publish_session_ref_as`).
/// Test: `catchup_on_a_repo_with_no_origin_creates_none`.
fn require_origin(repo: &Path) -> Result<()> {
    let out = git(repo, &["remote", "get-url", "origin"])?;
    if !out.success {
        bail!(
            "{} is not a git checkout with an `origin` remote",
            repo.display()
        );
    }
    Ok(())
}

/// Add the session-ref fetch refspec to `remote.origin.fetch`, once.
///
/// Why: ADR-0062 decision 5 — session refs travel on their own refspec, and a
/// checkout that has not opted in must keep seeing nothing. Adding it on the
/// first pause or catch-up is what makes the opt-in automatic for the tooling
/// that knows to ask, while a plain clone stays clean.
/// What: refuses a checkout with no `origin` FIRST (see [`require_origin`]),
/// then reads `git config --get-all remote.origin.fetch` and returns `Ok(false)`
/// when the line is already there, so repeated pauses never accumulate
/// duplicates; otherwise appends it with `--add` and returns `Ok(true)`.
/// Test: `ensure_fetch_refspec_is_idempotent`,
/// `a_plain_fetch_without_the_refspec_sees_no_session_refs`,
/// `catchup_on_a_repo_with_no_origin_creates_none`.
pub fn ensure_fetch_refspec(repo: &Path) -> Result<bool> {
    require_origin(repo)?;
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
/// Why: ADR-0062 decision 6 — the daemon reports which session refs exist by
/// reading them, never by committing. This is an INVENTORY, not a trust
/// decision: the namespace is unauthenticated, so a ref appearing here says
/// only that somebody pushed it.
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

/// The blob sha of `want` in `commit`'s tree, if the tree carries it.
///
/// Why: only ONE path is ever materialized — the `session-*.md` the commit
/// names — so the tree is queried for that path rather than walked. A tree
/// carrying `sessions-log.jsonl`, a second snapshot, or anything else
/// contributes nothing (#7830 review).
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
/// when the pause happened. `Session-Id` is deliberately never read: the
/// session a restored snapshot is attributed to comes from
/// [`SessionRefTarget::session_id`], which the caller supplies.
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

/// Whether `rel` is a snapshot path `session_id` is allowed to own.
///
/// Why (#7830 review): `rel` comes from a commit message, so without this a ref
/// could name `sessions-log.jsonl` — whose every component is `Normal`, so
/// containment alone passes — and overwrite the attribution index on a fresh
/// clone, or write into another session's directory.
/// What: every component is an ordinary name; the basename is `session-*.md`,
/// the shape [`super::pause::write_pause_snapshot`] writes and
/// `session_log::snapshot_path_in` reads; and the path is either one level under
/// this session's OWN directory, or flat at the store root — the latter ONLY for
/// a session id that has no legal directory name, which is exactly the condition
/// under which the writer falls back to the root (`pause.rs`).
/// Test: `a_traversing_tree_path_is_refused`,
/// `a_flat_snapshot_is_refused_for_a_session_that_owns_a_directory`.
fn is_own_snapshot_path(rel: &str, session_id: &str) -> bool {
    let components: Vec<&std::ffi::OsStr> = Path::new(rel)
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
        // The writer only writes flat when the id cannot be a directory name.
        1 => session_log::session_dir_name(session_id).is_none(),
        _ => components[0].to_str() == Some(session_id),
    }
}

/// Restore this caller's own session snapshot from its own session ref.
///
/// Why: ADR-0062 decision 4 makes `.trusty-mpm/sessions/` a cache and the ref
/// the durable copy, so a fresh clone (or a deleted cache) has to be able to
/// rebuild what resume reads. Running this BEFORE
/// `session_finder::find_paused_sessions` is what lets the entire read path
/// stay unchanged (#7830 closure condition 4).
/// What: [`require_origin`] and [`ensure_fetch_refspec`], then a fetch SCOPED to
/// [`SESSION_REF_FETCH_REFSPEC`] — a catch-up must not pull every branch on the
/// remote as a side effect. [`list_session_refs`] then gives the inventory, and
/// EXACTLY ONE of those refs — [`SessionRefTarget::ref_name`] — is read: its tip
/// commit's `Session-Snapshot` path, and only that path, is written back when it
/// is missing on disk and passes [`is_own_snapshot_path`]. The matching `pause`
/// line is appended when the log has none, attributed to
/// [`SessionRefTarget::session_id`]. An existing file is NEVER overwritten: a
/// live session's working copy is newer than any ref, and clobbering it would
/// lose the pause in progress.
///
/// Only the tip is read. Each ref commit's tree carries exactly the one snapshot
/// that pause wrote (ADR-0062 decision 7 keeps the chain's commits coarse and
/// the jsonl out of the tree), and the tip is the newest pause — which is the
/// only one `resolve_session_snapshot` would resolve. Walking the whole chain to
/// rehydrate a session's full history is deliberately out of scope, alongside
/// the retention policy ADR-0062 decision 8 defers.
/// Test: `hydration_restores_a_deleted_snapshot_and_its_log_line`,
/// `hydration_never_overwrites_a_file_already_on_disk`,
/// `hydration_is_idempotent_across_two_runs`,
/// `only_the_callers_own_ref_is_hydrated`, `a_foreign_users_ref_is_ignored`,
/// `a_foreign_hosts_tmux_ref_is_ignored`,
/// `a_tree_carrying_extra_blobs_writes_only_the_named_snapshot`,
/// `a_forged_session_id_trailer_loses_to_the_ref_key`.
pub fn hydrate_session_cache(repo: &Path, target: &SessionRefTarget) -> Result<HydrationOutcome> {
    ensure_fetch_refspec(repo)?;
    git_ok(
        repo,
        &["fetch", "origin", SESSION_REF_FETCH_REFSPEC],
        "fetch",
    )?;

    let want = target.ref_name();
    let inventory = list_session_refs(repo)?;
    let mut outcome = HydrationOutcome {
        refs_seen: inventory.len(),
        ..HydrationOutcome::default()
    };
    // See #7830 — ADR-0062 decision 10: push access is the trust boundary, so
    // this exact-name match is defence in depth, not authentication.
    let Some(tip) = inventory.iter().find(|t| t.name == want) else {
        if outcome.refs_seen > 0 {
            // #7830 review round 2: five refs and nothing restored is what a
            // renamed host or a switched `gh` account looks like, forever.
            tracing::warn!(
                refs_seen = outcome.refs_seen,
                expected_ref = %want,
                own_ref_found = false,
                "session refs exist on this remote but none is this caller's; \
                 nothing was restored"
            );
        }
        return Ok(outcome);
    };
    outcome.own_ref_found = true;

    let sessions_dir = repo.join(".trusty-mpm").join("sessions");
    let message = git_ok(repo, &["cat-file", "commit", &tip.commit], "cat-file")?.text();
    let Some(rel) = commit_trailer(&message, "Session-Snapshot") else {
        return Ok(outcome);
    };
    if !is_own_snapshot_path(&rel, &target.session_id) {
        tracing::warn!(
            ref_name = %want,
            snapshot = %rel,
            "session ref names a snapshot path it does not own; skipped"
        );
        return Ok(outcome);
    }
    let tree_path = format!("{SESSIONS_STORE_PREFIX}{rel}");
    let Some(blob) = blob_at(repo, &tip.commit, &tree_path)? else {
        return Ok(outcome);
    };

    let file = sessions_dir.join(&rel);
    if !file.exists() {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = git_ok(repo, &["cat-file", "blob", &blob], "cat-file")?;
        std::fs::write(&file, &bytes.stdout)?;
        outcome.snapshots_written.push(file);
    }
    let recorded = session_log::read_log(&sessions_dir)
        .iter()
        .any(|e| e.event == session_log::EVENT_PAUSE && e.snapshot == rel);
    if !recorded {
        let timestamp = commit_trailer(&message, "Session-Timestamp")
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        session_log::append_entry(
            &sessions_dir,
            &SessionLogEntry {
                session_id: target.session_id.clone(),
                event: session_log::EVENT_PAUSE.to_string(),
                snapshot: rel,
                timestamp,
            },
        )?;
        outcome.log_entries_added += 1;
    }
    Ok(outcome)
}

#[cfg(test)]
#[path = "session_refs_tests.rs"]
mod tests;
