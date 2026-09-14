//! Real-git tests for the ADR-0062 session-ref reader (#7830).
//!
//! Why: this module's whole job is to turn `refs/tm/sessions/**` back into
//! cache files, against a real remote with a real refspec. A mocked git would
//! test the parsing and none of the behaviour that matters — whether a plain
//! fetch sees the refs at all, and whether the restored files resolve through
//! the unchanged reader.
//! What: every fixture here lives inside a fresh `tempfile::TempDir` — a bare
//! `origin` plus one or two checkouts. Nothing touches a real repository.
//! Test: itself.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::catchup::session_log;

/// Run `git -C <dir> <args>`, panicking with git's own stderr on failure.
fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = crate::git::command_in(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("fixture: `git {}` could not be run: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "fixture: `git {}` failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A bare `origin` plus one checkout with a pushed `main`.
struct RemoteFixture {
    _tmp: TempDir,
    remote: PathBuf,
    work: PathBuf,
}

impl RemoteFixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let remote = tmp.path().join("remote.git");
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git_ok(
            tmp.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        git_ok(&work, &["init", "--initial-branch=main"]);
        configure(&work);
        git_ok(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        std::fs::write(work.join("README.md"), b"seed\n").unwrap();
        git_ok(&work, &["add", "README.md"]);
        git_ok(&work, &["commit", "-m", "seed"]);
        git_ok(&work, &["push", "origin", "main"]);
        Self {
            _tmp: tmp,
            remote,
            work,
        }
    }

    /// A second, plain clone of the same remote — no session refspec.
    fn plain_clone(&self, name: &str) -> PathBuf {
        let dest = self._tmp.path().join(name);
        git_ok(
            self._tmp.path(),
            &[
                "clone",
                self.remote.to_str().unwrap(),
                dest.to_str().unwrap(),
            ],
        );
        configure(&dest);
        dest
    }
}

/// Pin an identity and disable signing so `commit`/`commit-tree` never prompt.
fn configure(repo: &Path) {
    git_ok(repo, &["config", "user.email", "refs@example.invalid"]);
    git_ok(repo, &["config", "user.name", "Ref Tester"]);
    git_ok(repo, &["config", "commit.gpgsign", "false"]);
}

/// Push one orphan session-ref commit carrying `snapshot_rel`, exactly as the
/// write side in `trusty_mpm::core::session_ref_publish` builds it.
fn push_session_commit(
    repo: &Path,
    user: &str,
    key: &str,
    session_id: &str,
    snapshot_rel: &str,
    body: &str,
) -> String {
    let tree_path = format!("{SESSIONS_STORE_PREFIX}{snapshot_rel}");
    let abs = repo.join(".trusty-mpm/sessions").join(snapshot_rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, body).unwrap();

    let blob = git_ok(
        repo,
        &[
            "hash-object",
            "-w",
            "--path",
            &tree_path,
            "--",
            abs.to_str().unwrap(),
        ],
    );
    let scratch = TempDir::new().unwrap();
    let index = scratch.path().join("index");
    let idx = index.to_str().unwrap();
    for args in [
        vec!["read-tree", "--empty"],
        vec![
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},{tree_path}"),
        ],
    ] {
        let out = crate::git::command_in(repo)
            .args(&args)
            .env("GIT_INDEX_FILE", idx)
            .output()
            .unwrap();
        assert!(out.status.success(), "fixture: `git {args:?}` failed");
    }
    let tree = String::from_utf8_lossy(
        &crate::git::command_in(repo)
            .args(["write-tree"])
            .env("GIT_INDEX_FILE", idx)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();

    let ref_name = format!("{SESSION_REF_PREFIX}{user}/{key}");
    let parent = crate::git::command_in(repo)
        .args(["rev-parse", "--verify", &format!("{ref_name}^{{commit}}")])
        .output()
        .unwrap();
    let parent = parent
        .status
        .success()
        .then(|| String::from_utf8_lossy(&parent.stdout).trim().to_string());

    let message = format!(
        "pause {session_id} 2026-09-13T10:00:00+00:00\n\n\
         Session-Id: {session_id}\n\
         Session-Event: pause\n\
         Session-Snapshot: {snapshot_rel}\n\
         Session-Timestamp: 2026-09-13T10:00:00+00:00\n"
    );
    let mut args: Vec<&str> = vec!["commit-tree", "--no-gpg-sign", &tree];
    if let Some(p) = &parent {
        args.push("-p");
        args.push(p);
    }
    args.push("-m");
    args.push(&message);
    let commit = git_ok(repo, &args);
    git_ok(repo, &["update-ref", &ref_name, &commit]);
    git_ok(repo, &["push", "origin", &format!("{ref_name}:{ref_name}")]);
    commit
}

/// Why: #7830 closure condition 4 — the refspec is added on first use and never
/// again. A duplicate line would make every fetch carry the refspec N times and
/// would grow `.git/config` once per pause.
/// What: the first call reports it added the line, the second reports it did
/// not, and `--get-all` shows exactly one.
/// Test: itself.
#[test]
fn ensure_fetch_refspec_is_idempotent() {
    let fx = RemoteFixture::new();
    assert!(ensure_fetch_refspec(&fx.work).unwrap());
    assert!(!ensure_fetch_refspec(&fx.work).unwrap());
    assert!(!ensure_fetch_refspec(&fx.work).unwrap());

    let configured = git_ok(&fx.work, &["config", "--get-all", "remote.origin.fetch"]);
    let hits = configured
        .lines()
        .filter(|l| l.trim() == SESSION_REF_FETCH_REFSPEC)
        .count();
    assert_eq!(hits, 1, "{configured}");
}

/// Why: ADR-0062 decision 5 — session data is opt-in, so a clone that has not
/// added the refspec must see no session refs at all. This is the property that
/// keeps machine state out of an ordinary checkout and out of CI.
/// What: a session ref is pushed, then a PLAIN clone fetches and enumerates
/// nothing; adding the refspec and fetching again finds it.
/// Test: itself.
#[test]
fn a_plain_fetch_without_the_refspec_sees_no_session_refs() {
    let fx = RemoteFixture::new();
    push_session_commit(
        &fx.work,
        "octocat",
        "host-tmux-window-230",
        "tmux-window-230",
        "tmux-window-230/session-20260913-100000.md",
        "# Session Pause - 2026-09-13T10:00:00+00:00\n\n## Summary\nParked.\n",
    );

    let plain = fx.plain_clone("plain");
    git_ok(&plain, &["fetch", "origin"]);
    assert!(
        list_session_refs(&plain).unwrap().is_empty(),
        "a plain clone must see no session refs"
    );

    assert!(ensure_fetch_refspec(&plain).unwrap());
    git_ok(&plain, &["fetch", "origin"]);
    assert_eq!(
        list_session_refs(&plain).unwrap().len(),
        1,
        "the refspec is what makes the ref visible"
    );
}

/// Why: #7830 closure condition 6 — the daemon reports who is doing what by
/// enumerating refs. N sessions must be exactly N refs, with no ref shared and
/// none missed.
/// What: three sessions push their own ref; `for-each-ref` finds all three, and
/// every tip is a distinct commit.
/// Test: itself.
#[test]
fn list_session_refs_enumerates_every_session() {
    let fx = RemoteFixture::new();
    for n in 0..3 {
        push_session_commit(
            &fx.work,
            "octocat",
            &format!("host-tmux-window-23{n}"),
            &format!("tmux-window-23{n}"),
            &format!("tmux-window-23{n}/session-2026091{n}-100000.md"),
            &format!("# Session Pause\n\n## Summary\nSession {n}.\n"),
        );
    }
    let refs = list_session_refs(&fx.work).unwrap();
    assert_eq!(refs.len(), 3, "{refs:?}");

    let mut tips: Vec<&str> = refs.iter().map(|r| r.commit.as_str()).collect();
    tips.sort_unstable();
    tips.dedup();
    assert_eq!(tips.len(), 3, "each session owns its own chain: {refs:?}");
    assert!(refs.iter().all(|r| r.name.starts_with(SESSION_REF_PREFIX)));
}

/// Why: #7830 closure condition 4 (and ADR-0062 decision 4) — the working-tree
/// store is a cache, so deleting it must be recoverable. This is the whole read
/// side: after hydration the snapshot resolves through the UNCHANGED
/// `resolve_session_snapshot`, which reads the log, not the refs.
/// What: push a session ref, delete `.trusty-mpm/sessions/` entirely, hydrate,
/// and resolve the snapshot for that session id again.
/// Test: itself.
#[test]
fn hydration_restores_a_deleted_snapshot_and_its_log_line() {
    let fx = RemoteFixture::new();
    let rel = "s-alpha/session-20260913-100000.md";
    let body = "# Session Pause - 2026-09-13T10:00:00+00:00\n\n## Summary\nParked mid-review.\n";
    push_session_commit(&fx.work, "octocat", "s-alpha", "s-alpha", rel, body);

    let sessions_dir = fx.work.join(".trusty-mpm/sessions");
    std::fs::remove_dir_all(&sessions_dir).unwrap();
    assert!(!sessions_dir.exists());

    let outcome = hydrate_session_cache(&fx.work).unwrap();
    assert_eq!(outcome.refs_seen, 1, "{outcome:?}");
    assert_eq!(outcome.snapshots_written.len(), 1, "{outcome:?}");
    assert_eq!(outcome.log_entries_added, 1, "{outcome:?}");

    assert_eq!(
        std::fs::read_to_string(sessions_dir.join(rel)).unwrap(),
        body
    );
    let resolved = session_log::resolve_session_snapshot(&sessions_dir, "s-alpha", "md")
        .expect("the unchanged reader must resolve the hydrated snapshot");
    assert_eq!(resolved, sessions_dir.join(rel));
}

/// Why: a live session's working copy is newer than any ref. Overwriting it
/// would lose the pause in progress — the exact data-loss shape ADR-0062 exists
/// to avoid, arriving from the other direction.
/// What: an on-disk file with different content survives hydration byte for
/// byte, and no duplicate log line is appended for it.
/// Test: itself.
#[test]
fn hydration_never_overwrites_a_file_already_on_disk() {
    let fx = RemoteFixture::new();
    let rel = "s-alpha/session-20260913-100000.md";
    push_session_commit(
        &fx.work,
        "octocat",
        "s-alpha",
        "s-alpha",
        rel,
        "# Session Pause\n\n## Summary\nFrom the ref.\n",
    );

    let sessions_dir = fx.work.join(".trusty-mpm/sessions");
    let local = "# Session Pause\n\n## Summary\nNewer, still being edited.\n";
    std::fs::write(sessions_dir.join(rel), local).unwrap();

    let outcome = hydrate_session_cache(&fx.work).unwrap();
    assert!(outcome.snapshots_written.is_empty(), "{outcome:?}");
    assert_eq!(
        std::fs::read_to_string(sessions_dir.join(rel)).unwrap(),
        local
    );
}

/// Why: catch-up runs on every resume, so hydration is repeated constantly. A
/// second pass must add nothing — a duplicated `pause` line would make the log
/// grow without bound and would not change what resolves.
/// What: two hydrations in a row; the second writes no file and appends no log
/// entry, and the log still holds exactly one pause line for the snapshot.
/// Test: itself.
#[test]
fn hydration_is_idempotent_across_two_runs() {
    let fx = RemoteFixture::new();
    let rel = "s-alpha/session-20260913-100000.md";
    push_session_commit(
        &fx.work,
        "octocat",
        "s-alpha",
        "s-alpha",
        rel,
        "# Session Pause\n\n## Summary\nParked.\n",
    );
    let sessions_dir = fx.work.join(".trusty-mpm/sessions");
    std::fs::remove_dir_all(&sessions_dir).unwrap();

    let first = hydrate_session_cache(&fx.work).unwrap();
    assert_eq!(first.log_entries_added, 1, "{first:?}");
    let second = hydrate_session_cache(&fx.work).unwrap();
    assert_eq!(second.log_entries_added, 0, "{second:?}");
    assert!(second.snapshots_written.is_empty(), "{second:?}");

    let pauses = session_log::read_log(&sessions_dir)
        .into_iter()
        .filter(|e| e.event == session_log::EVENT_PAUSE && e.snapshot == rel)
        .count();
    assert_eq!(pauses, 1);
}

/// Why: a ref is remote input, so a tree entry naming `../` must never be
/// written outside the store — the same containment rule the log's own
/// `snapshot` field is held to.
/// What: the pure predicate, over the shapes a hostile tree could carry.
/// Test: itself.
#[test]
fn a_traversing_tree_path_is_refused() {
    assert!(is_contained_relative("s/session-1.md"));
    for bad in ["", "../escape.md", "s/../../escape.md", "/abs.md"] {
        assert!(!is_contained_relative(bad), "{bad:?}");
    }
}

/// Why: hydration rebuilds the attribution line from the commit message, so the
/// trailer parse is what makes a restored snapshot resolvable at all.
/// What: an exact trailer, a trailer with surrounding whitespace, a missing key
/// and an empty value.
/// Test: itself.
#[test]
fn commit_trailers_are_read_back() {
    let message =
        "pause s-alpha ts\n\n  Session-Id: s-alpha \nSession-Event: pause\nSession-Snapshot:\n";
    assert_eq!(
        commit_trailer(message, "Session-Id").as_deref(),
        Some("s-alpha")
    );
    assert_eq!(
        commit_trailer(message, "Session-Event").as_deref(),
        Some("pause")
    );
    assert_eq!(commit_trailer(message, "Session-Snapshot"), None);
    assert_eq!(commit_trailer(message, "Session-Absent"), None);
}
