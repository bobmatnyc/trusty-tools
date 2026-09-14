//! Real-git tests for the ADR-0062 session-ref reader (#7830).
//!
//! Why: this module's whole job is to turn `refs/tm/sessions/**` back into
//! cache files, against a real remote with a real refspec. A mocked git would
//! test the parsing and none of the behaviour that matters — whether a plain
//! fetch sees the refs at all, whether a ref this checkout does not own is
//! refused, and whether the restored files resolve through the unchanged
//! reader.
//! What: every fixture here lives inside a fresh `tempfile::TempDir` — a bare
//! `origin` plus one or two checkouts. A ref's source blobs are hashed from a
//! scratch directory OUTSIDE the store, so "nothing was written" assertions
//! mean what they say. Nothing touches a real repository.
//! Test: itself.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::catchup::session_log;

/// The user id every fixture ref is published under.
const USER: &str = "octocat";

/// The host name every fixture identity claims.
const HOST: &str = "testhost";

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

/// The one ref a test checkout is allowed to hydrate.
fn target(user: &str, key: &str, session_id: &str) -> SessionRefTarget {
    SessionRefTarget {
        user_id: user.to_string(),
        session_key: key.to_string(),
        session_id: session_id.to_string(),
    }
}

/// The common case: a managed-style session whose key IS its session id.
fn alpha_target() -> SessionRefTarget {
    target(USER, "s-alpha", "s-alpha")
}

/// One blob a fixture ref's tree should carry, as `(tree path, content)`.
type Blob<'a> = (&'a str, &'a str);

/// Everything a fixture ref commit varies.
struct RefCommit<'a> {
    /// The `<user-id>` half of the ref key.
    user: &'a str,
    /// The `<session-key>` half of the ref key.
    key: &'a str,
    /// What the commit's `Session-Id` trailer claims — normally the session the
    /// key names, but a forged ref claims someone else's.
    trailer_session_id: &'a str,
    /// The store-relative path the `Session-Snapshot` trailer names.
    snapshot_rel: &'a str,
    /// The snapshot's content.
    body: &'a str,
    /// Additional tree entries, by full repo-relative path.
    extra: &'a [Blob<'a>],
}

/// A bare `origin` plus one checkout with a pushed `main`.
struct RemoteFixture {
    _tmp: TempDir,
    remote: PathBuf,
    work: PathBuf,
    /// Scratch space for blob sources, deliberately OUTSIDE the session store.
    scratch: PathBuf,
}

impl RemoteFixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let remote = tmp.path().join("remote.git");
        let work = tmp.path().join("work");
        let scratch = tmp.path().join("scratch");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
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
            scratch,
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

    /// This checkout's session store.
    fn sessions_dir(&self) -> PathBuf {
        self.work.join(".trusty-mpm/sessions")
    }

    /// Push one orphan session-ref commit, exactly as the write side in
    /// `trusty_mpm::core::session_ref_publish` builds it.
    fn push_ref(&self, spec: &RefCommit<'_>) -> String {
        let tree_path = format!("{SESSIONS_STORE_PREFIX}{}", spec.snapshot_rel);
        let mut entries: Vec<(String, String)> = vec![(tree_path, self.blob(spec.body))];
        for (path, content) in spec.extra {
            entries.push(((*path).to_string(), self.blob(content)));
        }

        let scratch = TempDir::new().unwrap();
        let index = scratch.path().join("index");
        let idx = index.to_str().unwrap();
        self.indexed(idx, &["read-tree", "--empty"]);
        for (path, blob) in &entries {
            self.indexed(
                idx,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("100644,{blob},{path}"),
                ],
            );
        }
        let tree = String::from_utf8_lossy(
            &crate::git::command_in(&self.work)
                .args(["write-tree"])
                .env("GIT_INDEX_FILE", idx)
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();

        let ref_name = format!("{SESSION_REF_PREFIX}{}/{}", spec.user, spec.key);
        let parent = crate::git::command_in(&self.work)
            .args(["rev-parse", "--verify", &format!("{ref_name}^{{commit}}")])
            .output()
            .unwrap();
        let parent = parent
            .status
            .success()
            .then(|| String::from_utf8_lossy(&parent.stdout).trim().to_string());

        let message = format!(
            "pause {id} 2026-09-13T10:00:00+00:00\n\n\
             Session-Id: {id}\n\
             Session-Event: pause\n\
             Session-Snapshot: {rel}\n\
             Session-Timestamp: 2026-09-13T10:00:00+00:00\n",
            id = spec.trailer_session_id,
            rel = spec.snapshot_rel,
        );
        let mut args: Vec<&str> = vec!["commit-tree", "--no-gpg-sign", &tree];
        if let Some(p) = &parent {
            args.push("-p");
            args.push(p);
        }
        args.push("-m");
        args.push(&message);
        let commit = git_ok(&self.work, &args);
        git_ok(&self.work, &["update-ref", &ref_name, &commit]);
        git_ok(
            &self.work,
            &["push", "origin", &format!("{ref_name}:{ref_name}")],
        );
        commit
    }

    /// Hash `content` into the object store from a file outside the store.
    fn blob(&self, content: &str) -> String {
        let src = self.scratch.join(format!("blob-{}", next_id()));
        std::fs::write(&src, content).unwrap();
        git_ok(
            &self.work,
            &[
                "hash-object",
                "-w",
                "--no-filters",
                "--",
                src.to_str().unwrap(),
            ],
        )
    }

    /// Run a git command against the scratch index at `idx`.
    fn indexed(&self, idx: &str, args: &[&str]) {
        let out = crate::git::command_in(&self.work)
            .args(args)
            .env("GIT_INDEX_FILE", idx)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "fixture: `git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A monotonically increasing id, so two blobs in one fixture never share a
/// source filename.
fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Pin an identity and disable signing so `commit`/`commit-tree` never prompt.
fn configure(repo: &Path) {
    git_ok(repo, &["config", "user.email", "refs@example.invalid"]);
    git_ok(repo, &["config", "user.name", "Ref Tester"]);
    git_ok(repo, &["config", "commit.gpgsign", "false"]);
}

/// The store-relative snapshot path session `s-alpha` owns.
const ALPHA: &str = "s-alpha/session-20260913-100000.md";

const BODY: &str =
    "# Session Pause - 2026-09-13T10:00:00+00:00\n\n## Summary\nParked mid-review.\n";

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
    fx.push_ref(&RefCommit {
        user: USER,
        key: "s-alpha",
        trailer_session_id: "s-alpha",
        snapshot_rel: ALPHA,
        body: BODY,
        extra: &[],
    });

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
        fx.push_ref(&RefCommit {
            user: USER,
            key: &format!("{HOST}-tmux-window-23{n}"),
            trailer_session_id: &format!("tmux-window-23{n}"),
            snapshot_rel: &format!("tmux-window-23{n}/session-2026091{n}-100000.md"),
            body: BODY,
            extra: &[],
        });
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
/// What: push a session ref, hydrate into an empty store, and resolve the
/// snapshot for that session id.
/// Test: itself.
#[test]
fn hydration_restores_a_deleted_snapshot_and_its_log_line() {
    let fx = RemoteFixture::new();
    let rel = ALPHA;
    fx.push_ref(&RefCommit {
        user: USER,
        key: "s-alpha",
        trailer_session_id: "s-alpha",
        snapshot_rel: rel,
        body: BODY,
        extra: &[],
    });
    assert!(!fx.sessions_dir().exists(), "the store starts empty");

    let outcome = hydrate_session_cache(&fx.work, &alpha_target()).unwrap();
    assert_eq!(outcome.refs_seen, 1, "{outcome:?}");
    assert_eq!(outcome.snapshots_written.len(), 1, "{outcome:?}");
    assert_eq!(outcome.log_entries_added, 1, "{outcome:?}");

    let sessions_dir = fx.sessions_dir();
    assert_eq!(
        std::fs::read_to_string(sessions_dir.join(rel)).unwrap(),
        BODY
    );
    let resolved = session_log::resolve_session_snapshot(&sessions_dir, "s-alpha", "md")
        .expect("the unchanged reader must resolve the hydrated snapshot");
    assert_eq!(resolved, sessions_dir.join(rel));
}

/// Why: a live session's working copy is newer than any ref. Overwriting it
/// would lose the pause in progress — the exact data-loss shape ADR-0062 exists
/// to avoid, arriving from the other direction.
/// What: an on-disk file with different content survives hydration byte for
/// byte.
/// Test: itself.
#[test]
fn hydration_never_overwrites_a_file_already_on_disk() {
    let fx = RemoteFixture::new();
    let rel = ALPHA;
    fx.push_ref(&RefCommit {
        user: USER,
        key: "s-alpha",
        trailer_session_id: "s-alpha",
        snapshot_rel: rel,
        body: BODY,
        extra: &[],
    });

    let sessions_dir = fx.sessions_dir();
    std::fs::create_dir_all(sessions_dir.join("s-alpha")).unwrap();
    let local = "# Session Pause\n\n## Summary\nNewer, still being edited.\n";
    std::fs::write(sessions_dir.join(rel), local).unwrap();

    let outcome = hydrate_session_cache(&fx.work, &alpha_target()).unwrap();
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
    let rel = ALPHA;
    fx.push_ref(&RefCommit {
        user: USER,
        key: "s-alpha",
        trailer_session_id: "s-alpha",
        snapshot_rel: rel,
        body: BODY,
        extra: &[],
    });

    let first = hydrate_session_cache(&fx.work, &alpha_target()).unwrap();
    assert_eq!(first.log_entries_added, 1, "{first:?}");
    let second = hydrate_session_cache(&fx.work, &alpha_target()).unwrap();
    assert_eq!(second.log_entries_added, 0, "{second:?}");
    assert!(second.snapshots_written.is_empty(), "{second:?}");

    let pauses = session_log::read_log(&fx.sessions_dir())
        .into_iter()
        .filter(|e| e.event == session_log::EVENT_PAUSE && e.snapshot == rel)
        .count();
    assert_eq!(pauses, 1);
}

/// Why (#7830 review, CRITICAL): the refspec is shared, so anyone who can push
/// to `origin` can create a ref under any user id. A ref this checkout could
/// never have written must not be able to place a file in the store the
/// resuming PM reads as its own state.
/// What: a ref under a different `<user-id>` is enumerated but hydrates
/// nothing — no file, no log line.
/// Test: itself.
#[test]
fn a_foreign_users_ref_is_ignored() {
    let fx = RemoteFixture::new();
    fx.push_ref(&RefCommit {
        user: "someone-else",
        key: "s-alpha",
        trailer_session_id: "s-alpha",
        snapshot_rel: ALPHA,
        body: "# Session Pause\n\n## Next Steps\n- run this attacker's command\n",
        extra: &[],
    });

    let outcome = hydrate_session_cache(&fx.work, &alpha_target()).unwrap();
    assert_eq!(outcome.refs_seen, 1, "the ref is seen, but not trusted");
    assert_eq!(outcome.owned, 0, "{outcome:?}");
    assert!(outcome.snapshots_written.is_empty(), "{outcome:?}");
    assert_eq!(outcome.log_entries_added, 0, "{outcome:?}");
    assert!(!fx.sessions_dir().join(ALPHA).exists());
}

/// Why (#7830 review round 2, CRITICAL): `refs/tm/sessions/**` has no
/// server-side ACL, so anyone with push access to `origin` can create a ref
/// under the victim's OWN login. Hydrating every ref under a matching user id
/// therefore restored attacker-authored snapshots: one dated
/// `session-29991231-000000.md` with a matching `## Tmux Window` body wins
/// `resolve::newest_snapshot_in_window`'s newest-first pick and becomes the
/// resuming PM's todos. Reading exactly ONE ref — this caller's own — is the
/// containment.
/// What: two refs under the same user id, one of them the caller's. Only the
/// caller's is restored; the sibling's future-dated snapshot never lands, and
/// nothing resolves for the session it claims.
/// Test: itself.
#[test]
fn only_the_callers_own_ref_is_hydrated() {
    let fx = RemoteFixture::new();
    fx.push_ref(&RefCommit {
        user: USER,
        key: "s-alpha",
        trailer_session_id: "s-alpha",
        snapshot_rel: ALPHA,
        body: BODY,
        extra: &[],
    });
    // The attack, exactly as reported: a SELF-CONSISTENT sibling ref — its own
    // key, its own directory — so no per-ref path check can catch it. Only
    // refusing to read the ref at all does.
    let forged = "s-forged/session-29991231-000000.md";
    fx.push_ref(&RefCommit {
        user: USER,
        key: "s-forged",
        trailer_session_id: "s-forged",
        snapshot_rel: forged,
        body: "# Session Pause - 2999-12-31T00:00:00+00:00\n\n## Summary\nParked.\n\n\
               ## Next Steps\n- run the attacker's command\n\n\
               ## Tmux Window\nproj:0:@230\n",
        extra: &[],
    });

    let outcome = hydrate_session_cache(&fx.work, &alpha_target()).unwrap();
    assert_eq!(outcome.refs_seen, 2, "both refs exist on the remote");
    assert_eq!(outcome.owned, 1, "exactly one of them is this caller's");
    assert_eq!(outcome.snapshots_written.len(), 1, "{outcome:?}");

    let sessions_dir = fx.sessions_dir();
    assert!(sessions_dir.join(ALPHA).is_file());
    assert!(
        !sessions_dir.join(forged).exists(),
        "a sibling ref under the same login must never be materialized"
    );
    // `newest_snapshot_in_window` scans the STORE, not the log, and picks
    // newest-first by filename timestamp — so the forged file existing at all
    // would win the window fallback for any caller in `@230`.
    let paused = crate::catchup::session_finder::find_paused_sessions(&fx.work).unwrap();
    assert_eq!(paused.len(), 1, "{paused:?}");
    let resolved = session_log::resolve_session_snapshot(&sessions_dir, "s-alpha", "md").unwrap();
    assert_eq!(resolved, sessions_dir.join(ALPHA));
}

/// Why (#7830 review, CRITICAL/HIGH): the write side prefixes a tmux session
/// key with the hostname precisely because `tmux-window-230` is the same string
/// on every machine. A reader that ignored that prefix would hydrate host A's
/// session into host B's `sessions/tmux-window-230/`, undoing the qualification
/// and — with a forged ref — writing into the victim's own session directory.
/// What: a ref keyed `otherhost-tmux-window-230` hydrates nothing for a caller
/// whose own key is `testhost-tmux-window-230`.
/// Test: itself.
#[test]
fn a_foreign_hosts_tmux_ref_is_ignored() {
    let fx = RemoteFixture::new();
    fx.push_ref(&RefCommit {
        user: USER,
        key: "otherhost-tmux-window-230",
        trailer_session_id: "tmux-window-230",
        snapshot_rel: "tmux-window-230/session-20260913-100000.md",
        body: BODY,
        extra: &[],
    });

    let mine = target(USER, &format!("{HOST}-tmux-window-230"), "tmux-window-230");
    let outcome = hydrate_session_cache(&fx.work, &mine).unwrap();
    assert_eq!(outcome.refs_seen, 1);
    assert_eq!(outcome.owned, 0, "{outcome:?}");
    assert!(outcome.snapshots_written.is_empty(), "{outcome:?}");
    assert_eq!(outcome.log_entries_added, 0, "{outcome:?}");
    assert!(!fx.sessions_dir().exists());
}

/// Why: the qualification must be reversible — this host's OWN tmux ref has to
/// hydrate, under the bare session id the local writer and reader both derive.
/// What: a ref keyed `<this host>-tmux-window-230` restores
/// `sessions/tmux-window-230/session-*.md` and attributes it to
/// `tmux-window-230`, not to the ref key.
/// Test: itself.
#[test]
fn a_host_qualified_tmux_ref_hydrates_under_the_bare_session_id() {
    let fx = RemoteFixture::new();
    let rel = "tmux-window-230/session-20260913-100000.md";
    fx.push_ref(&RefCommit {
        user: USER,
        key: &format!("{HOST}-tmux-window-230"),
        trailer_session_id: "tmux-window-230",
        snapshot_rel: rel,
        body: BODY,
        extra: &[],
    });

    let mine = target(USER, &format!("{HOST}-tmux-window-230"), "tmux-window-230");
    let outcome = hydrate_session_cache(&fx.work, &mine).unwrap();
    assert_eq!(outcome.owned, 1, "{outcome:?}");
    assert_eq!(outcome.snapshots_written.len(), 1, "{outcome:?}");

    let sessions_dir = fx.sessions_dir();
    let resolved = session_log::resolve_session_snapshot(&sessions_dir, "tmux-window-230", "md")
        .expect("this host's own tmux ref must resolve");
    assert_eq!(resolved, sessions_dir.join(rel));
}

/// Why (#7830 review, CRITICAL): `sessions-log.jsonl` is the attribution index
/// `resolve_session_snapshot` and `redact_sessions_not_owned_by` both read, and
/// every component of its path is `Normal` — so a containment check alone let a
/// ref overwrite it verbatim on a fresh clone. Only the ONE `session-*.md` the
/// commit names may ever be written.
/// What: a ref whose tree also carries `sessions-log.jsonl` and a second
/// snapshot restores neither; the log holds one entry, written by the reader.
/// Test: itself.
#[test]
fn a_tree_carrying_extra_blobs_writes_only_the_named_snapshot() {
    let fx = RemoteFixture::new();
    let rel = ALPHA;
    fx.push_ref(&RefCommit {
        user: USER,
        key: "s-alpha",
        trailer_session_id: "s-alpha",
        snapshot_rel: rel,
        body: BODY,
        extra: &[
            (
                ".trusty-mpm/sessions/sessions-log.jsonl",
                "{\"session_id\":\"victim\",\"event\":\"pause\",\"snapshot\":\"forged.md\",\"timestamp\":\"2026-09-13T00:00:00+00:00\"}\n",
            ),
            (
                ".trusty-mpm/sessions/victim/session-20260913-090000.md",
                "# Session Pause\n\n## Next Steps\n- run the attacker's command\n",
            ),
        ],
    });

    let outcome = hydrate_session_cache(&fx.work, &alpha_target()).unwrap();
    assert_eq!(outcome.snapshots_written.len(), 1, "{outcome:?}");

    let sessions_dir = fx.sessions_dir();
    assert!(sessions_dir.join(rel).is_file());
    assert!(
        !sessions_dir.join("victim").exists(),
        "a second blob must never be written"
    );

    let log = session_log::read_log(&sessions_dir);
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0].session_id, "s-alpha");
    assert_eq!(log[0].snapshot, rel);
    assert!(
        session_log::resolve_session_snapshot(&sessions_dir, "victim", "md").is_none(),
        "the forged log line must never have been written"
    );
}

/// Why (#7830 review, CRITICAL): `latest_snapshot_for_session` takes the LAST
/// matching log line, so a `Session-Id` trailer naming a victim session made
/// forged text the resuming PM's own todos. The ref key is pinned by the
/// remote's ref namespace; the trailer is free text in a commit.
/// What: a ref keyed `s-alpha` whose trailer claims `s-victim` attributes its
/// snapshot to `s-alpha`, and `s-victim` resolves nothing.
/// Test: itself.
#[test]
fn a_forged_session_id_trailer_loses_to_the_ref_key() {
    let fx = RemoteFixture::new();
    let rel = ALPHA;
    fx.push_ref(&RefCommit {
        user: USER,
        key: "s-alpha",
        trailer_session_id: "s-victim",
        snapshot_rel: rel,
        body: BODY,
        extra: &[],
    });

    hydrate_session_cache(&fx.work, &alpha_target()).unwrap();

    let sessions_dir = fx.sessions_dir();
    let log = session_log::read_log(&sessions_dir);
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(
        log[0].session_id, "s-alpha",
        "the ref key decides attribution, never the trailer"
    );
    assert!(session_log::resolve_session_snapshot(&sessions_dir, "s-victim", "md").is_none());
}

/// Why (#7830 review round 2): the flat-root arm used to be an unconditional
/// `true`, so a ref could place a snapshot at the store ROOT for a session that
/// owns a directory. `session_finder` sees a root-level `session-*.md` for every
/// session, so that file competes in the window fallback for all of them.
/// What: the writer's own condition — flat is legal only when the session id has
/// no legal directory name (`pause.rs`) — held both ways.
/// Test: itself.
#[test]
fn a_flat_snapshot_is_refused_for_a_session_that_owns_a_directory() {
    assert!(session_log::session_dir_name("s-alpha").is_some());
    assert!(
        !is_own_snapshot_path("session-20260913-100000.md", "s-alpha"),
        "a session with a legal directory name never writes flat"
    );

    let unsafe_id = "proj:0:@230";
    assert!(session_log::session_dir_name(unsafe_id).is_none());
    assert!(
        is_own_snapshot_path("session-20260913-100000.md", unsafe_id),
        "an id that cannot be a directory name is exactly when the writer goes flat"
    );
}

/// Why: `rel` comes from a commit message, so a ref could otherwise name
/// `sessions-log.jsonl`, a traversing path, or another session's directory.
/// What: the pure predicate, over the accept and reject sets.
/// Test: itself.
#[test]
fn a_traversing_tree_path_is_refused() {
    assert!(is_own_snapshot_path(
        "s-alpha/session-20260913-100000.md",
        "s-alpha"
    ));
    for bad in [
        "",
        "../escape.md",
        "s-alpha/../../escape.md",
        "/abs/session-1.md",
        "sessions-log.jsonl",
        "s-alpha/sessions-log.jsonl",
        "s-victim/session-20260913-100000.md",
        "a/b/session-20260913-100000.md",
        "s-alpha/notes.md",
        "session-20260913-100000.md",
    ] {
        assert!(!is_own_snapshot_path(bad, "s-alpha"), "{bad:?}");
    }
}

/// Why: hydration reads the snapshot path and the timestamp from the commit, so
/// the trailer parse is what makes a restored snapshot findable at all.
/// What: an exact trailer, one with surrounding whitespace, a missing key and an
/// empty value.
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

/// Why: the target names exactly one ref, so its rendering is what the whole
/// own-ref-only rule is compared against.
/// What: both halves reach the ref name in order, under the ADR-0062 prefix.
/// Test: itself.
#[test]
fn a_target_renders_its_one_ref_name() {
    assert_eq!(
        target("octocat", "host-tmux-window-230", "tmux-window-230").ref_name(),
        "refs/tm/sessions/octocat/host-tmux-window-230"
    );
    assert!(alpha_target().ref_name().starts_with(SESSION_REF_PREFIX));
}

/// Why (#7830 review round 2): `git config --add remote.origin.fetch <spec>` on
/// a repo with NO origin does not fail — it CREATES a phantom `origin` with an
/// empty URL, and every later `git fetch origin` in that checkout then breaks.
/// A catch-up must not mutate a project it has nothing to say about.
/// What: hydration against a checkout with no remote errors, and `git remote`
/// still lists nothing afterwards.
/// Test: itself.
#[test]
fn catchup_on_a_repo_with_no_origin_creates_none() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("bare-checkout");
    std::fs::create_dir_all(&repo).unwrap();
    git_ok(&repo, &["init", "--initial-branch=main"]);
    configure(&repo);

    let err = hydrate_session_cache(&repo, &alpha_target()).unwrap_err();
    assert!(err.to_string().contains("origin"), "{err}");

    let remotes = git_ok(&repo, &["remote"]);
    assert!(
        remotes.is_empty(),
        "a phantom remote was created: {remotes:?}"
    );
    let configured = crate::git::command_in(&repo)
        .args(["config", "--get-all", "remote.origin.fetch"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&configured.stdout)
            .trim()
            .is_empty(),
        "no refspec may be written into a repo with no origin"
    );
}
