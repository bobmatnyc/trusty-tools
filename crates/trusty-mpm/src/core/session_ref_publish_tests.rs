//! Real-git tests for the ADR-0062 session-ref publisher (#7830).
//!
//! Why: the publisher's contract is entirely about what a real remote does —
//! whether the ref advances, whether `main` stays put, whether a lease is
//! honoured. A mocked git would assert the argv and prove none of it. Every
//! fixture is a bare `origin` inside a fresh `tempfile::TempDir`; nothing here
//! touches a real repository or the network.
//! What: one fixture ([`Fixture`]) plus one test per #7830 closure condition
//! and per failure arm.
//! Test: itself.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;

/// Run `git -C <dir> <args>`, panicking with git's own stderr on failure.
fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = trusty_common::git::command_in(dir)
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

/// The user id every test publishes under, so no test depends on this host's
/// `gh` configuration (see [`publish_session_ref_as`]).
const USER: &str = "octocat";

/// A bare `origin` plus one checkout with a pushed `main` (#7830).
struct Fixture {
    _tmp: TempDir,
    remote: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let remote = tmp.path().join("remote.git");
        let repo = tmp.path().join("work");
        std::fs::create_dir_all(&repo).unwrap();
        git_ok(
            tmp.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        git_ok(&repo, &["init", "--initial-branch=main"]);
        git_ok(&repo, &["config", "user.email", "refs@example.invalid"]);
        git_ok(&repo, &["config", "user.name", "Ref Tester"]);
        git_ok(&repo, &["config", "commit.gpgsign", "false"]);
        git_ok(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        std::fs::write(repo.join("README.md"), b"seed\n").unwrap();
        git_ok(&repo, &["add", "README.md"]);
        git_ok(&repo, &["commit", "-m", "seed"]);
        git_ok(&repo, &["push", "origin", "main"]);
        Self {
            _tmp: tmp,
            remote,
            repo,
        }
    }

    /// Write a snapshot into the local cache and return its absolute path.
    fn snapshot(&self, session_id: &str, stamp: &str, body: &str) -> PathBuf {
        let path = sessions_store(&self.repo)
            .join(session_id)
            .join(format!("session-{stamp}.md"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        path
    }

    /// A publish request for `snapshot`.
    fn request<'a>(&'a self, session_id: &'a str, snapshot: &'a Path) -> SessionRefRequest<'a> {
        SessionRefRequest {
            repo: &self.repo,
            session_id,
            snapshot_path: snapshot,
            timestamp: chrono::Utc::now(),
        }
    }

    /// `git ls-remote origin <pattern>` as `(sha, ref)` pairs.
    fn ls_remote(&self, pattern: &str) -> Vec<(String, String)> {
        git_ok(&self.repo, &["ls-remote", "origin", pattern])
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(sha, name)| (sha.trim().to_string(), name.trim().to_string()))
            .collect()
    }

    /// Make `origin` refuse every push, without breaking its reads.
    fn reject_pushes(&self) {
        let hooks = self.remote.join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-receive");
        std::fs::write(&hook, "#!/bin/sh\necho 'refused by policy' >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

/// A snapshot body with no credential-shaped content.
const PLAIN: &str = "# Session Pause - 2026-09-13T10:00:00+00:00\n\n## Summary\nParked mid-review.\n\n## Git Context\nBranch: feat/7830-session-history-git-refs\n";

/// Why: #7830 closure condition (a) — after one pause the session's own ref
/// exists on `origin` with exactly one commit, that commit is an ORPHAN (no
/// parent, no history shared with `main`), and `main` has not moved. This is
/// the whole point of ADR-0062: session history that never lands on a branch.
/// What: record `main`'s remote sha, publish once, then read `ls-remote` for
/// both namespaces.
/// Test: itself.
#[test]
fn a_first_pause_creates_an_orphan_commit_and_leaves_main_alone() {
    let fx = Fixture::new();
    let main_before = fx.ls_remote("refs/heads/main");
    assert_eq!(main_before.len(), 1);

    let snap = fx.snapshot("s-alpha", "20260913-100000", PLAIN);
    let out = publish_session_ref_as(&fx.request("s-alpha", &snap), Some(USER)).unwrap();

    assert_eq!(out.ref_name, "refs/tm/sessions/octocat/s-alpha");
    assert_eq!(out.parent, None, "the chain's first commit is an orphan");

    let refs = fx.ls_remote("refs/tm/sessions/*");
    assert_eq!(refs.len(), 1, "{refs:?}");
    assert_eq!(refs[0], (out.commit.clone(), out.ref_name.clone()));

    assert_eq!(
        fx.ls_remote("refs/heads/main"),
        main_before,
        "a session ref write must never move a branch"
    );
    assert_eq!(
        git_ok(&fx.repo, &["rev-list", "--count", &out.commit]),
        "1",
        "the commit shares no history with main"
    );
    // The tree carries the snapshot and nothing else — ADR-0062 decision 7
    // keeps the jsonl event stream out of the ref.
    let tree = git_ok(&fx.repo, &["ls-tree", "-r", "--name-only", &out.commit]);
    assert_eq!(
        tree,
        ".trusty-mpm/sessions/s-alpha/session-20260913-100000.md"
    );
}

/// Why: #7830 closure condition (b) — a second pause from the same session
/// APPENDS. A replaced ref would lose the earlier snapshot, which is the whole
/// reason the ref is a chain rather than a pointer.
/// What: two publishes; the second's parent is the first's commit, the remote
/// ref advanced to the second, and the chain is two commits long.
/// Test: itself.
#[test]
fn a_second_pause_appends_to_the_same_ref() {
    let fx = Fixture::new();
    let first_snap = fx.snapshot("s-alpha", "20260913-100000", PLAIN);
    let first = publish_session_ref_as(&fx.request("s-alpha", &first_snap), Some(USER)).unwrap();

    let second_snap = fx.snapshot("s-alpha", "20260913-110000", PLAIN);
    let second = publish_session_ref_as(&fx.request("s-alpha", &second_snap), Some(USER)).unwrap();

    assert_eq!(second.ref_name, first.ref_name, "same session, same ref");
    assert_eq!(second.parent.as_deref(), Some(first.commit.as_str()));

    let refs = fx.ls_remote("refs/tm/sessions/*");
    assert_eq!(refs.len(), 1, "one session is still one ref: {refs:?}");
    assert_eq!(refs[0].0, second.commit);
    assert_eq!(
        git_ok(&fx.repo, &["rev-list", "--count", &second.commit]),
        "2"
    );
}

/// Why: #7830 closure condition (c) / ADR-0062 decision 3 — one writer per ref,
/// enforced by the remote. A writer that read the tip before another writer
/// advanced it must be REJECTED, and the rejection must never be papered over
/// with `--force`.
/// What: publish once, then push a second commit with the lease that writer
/// would have held BEFORE the first push (the ref must not exist). The push is
/// rejected as [`RefPublishError::StaleLease`] and the remote tip is unchanged.
/// Test: itself.
#[test]
fn a_stale_lease_is_rejected_and_never_forced() {
    let fx = Fixture::new();
    let snap = fx.snapshot("s-alpha", "20260913-100000", PLAIN);
    let landed = publish_session_ref_as(&fx.request("s-alpha", &snap), Some(USER)).unwrap();

    // A second writer's commit, built on the stale knowledge that the ref did
    // not exist yet.
    let tree = git_ok(
        &fx.repo,
        &["rev-parse", &format!("{}^{{tree}}", landed.commit)],
    );
    let racing = git_ok(
        &fx.repo,
        &["commit-tree", "--no-gpg-sign", &tree, "-m", "racing pause"],
    );
    git_ok(&fx.repo, &["update-ref", &landed.ref_name, &racing]);

    let err = push_session_ref(&fx.repo, &landed.ref_name, None).unwrap_err();
    let RefPublishError::StaleLease { ref_name, expected } = &err else {
        panic!("expected a stale lease, got {err:?}");
    };
    assert_eq!(ref_name, &landed.ref_name);
    assert_eq!(expected, "<absent>");

    let refs = fx.ls_remote("refs/tm/sessions/*");
    assert_eq!(
        refs[0].0, landed.commit,
        "a rejected lease must leave the remote tip exactly where it was"
    );
}

/// Why: #7830 closure condition (g) — a push that the remote refuses is
/// reported, never swallowed. A refusal that is not a lease loss must not be
/// misreported as one, because the two have opposite remedies.
/// What: a `pre-receive` hook rejects every push; the publish fails with a
/// `Git { step: "push" }` error naming the remote's own message, and no session
/// ref exists on `origin`.
/// Test: itself.
#[test]
fn a_rejecting_remote_surfaces_the_push_failure() {
    let fx = Fixture::new();
    fx.reject_pushes();
    let snap = fx.snapshot("s-alpha", "20260913-100000", PLAIN);

    let err = publish_session_ref_as(&fx.request("s-alpha", &snap), Some(USER)).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("refused by policy"),
        "the remote's own reason must reach the caller: {message}"
    );
    assert!(fx.ls_remote("refs/tm/sessions/*").is_empty());
}

/// Why: ADR-0062 decision 9 — the pre-push credential gate applies to this
/// refspec exactly as it applies to a branch. A credential in a snapshot must
/// refuse the PUBLISH, not the pause: the local cache keeps the file.
/// What: a snapshot carrying a GitHub-token-shaped string; the publish is
/// refused before any object is written, the remote has no session ref, and the
/// local snapshot is still on disk.
/// Test: itself.
#[test]
fn a_credential_in_the_snapshot_refuses_the_publish() {
    let fx = Fixture::new();
    let body = format!(
        "{PLAIN}\n## Next Steps\n- rotate ghp_{}\n",
        "A1b2C3d4E5f6G7h8i9"
    );
    let snap = fx.snapshot("s-alpha", "20260913-100000", &body);

    let err = publish_session_ref_as(&fx.request("s-alpha", &snap), Some(USER)).unwrap_err();
    let RefPublishError::CredentialDetected(hit) = &err else {
        panic!("expected a credential refusal, got {err:?}");
    };
    assert_eq!(hit, "ghp_…", "the preview must not echo the secret");

    assert!(fx.ls_remote("refs/tm/sessions/*").is_empty());
    assert!(snap.is_file(), "the local cache keeps the snapshot");
}

/// Why: the credential gate is on the pause hot path, so a false positive would
/// silently stop every session's history from publishing. An ordinary snapshot
/// — branch names, SHAs, prose — must never trip it.
/// What: the plain fixture body plus the shapes most likely to look secret-ish.
/// Test: itself.
#[test]
fn an_ordinary_snapshot_is_not_flagged() {
    assert_eq!(scan_for_credentials(PLAIN), None);
    for benign in [
        "Last commit: acfd47e369497a4e234d073b17d28817d28efe0d feat(x): y",
        "the AKIA prefix is what the scanner keys on",
        "see https://github.com/bobmatnyc/trusty-tools/pull/7831",
        "sk-ant-",
    ] {
        assert_eq!(scan_for_credentials(benign), None, "{benign:?}");
    }
    assert!(scan_for_credentials("-----BEGIN RSA PRIVATE KEY-----").is_some());
}

/// Why: #7830 (decided) — with no user id there is no ref key, so the publish
/// is SKIPPED with `ref_error` set. The local snapshot still writes; a pause
/// that failed because `gh` was logged out would be a regression.
/// What: the explicit-user seam with `None`; the error is
/// [`RefPublishError::NoUserId`] and no ref reaches `origin`.
/// Test: itself.
#[test]
fn a_missing_user_id_skips_the_publish() {
    let fx = Fixture::new();
    let snap = fx.snapshot("s-alpha", "20260913-100000", PLAIN);

    let err = publish_session_ref_as(&fx.request("s-alpha", &snap), None).unwrap_err();
    assert!(matches!(err, RefPublishError::NoUserId), "{err:?}");
    assert!(fx.ls_remote("refs/tm/sessions/*").is_empty());
    assert!(snap.is_file());
}

/// Why: #7830 closure condition (h) — `[session_refs] enabled = false` must
/// restore pre-ADR-0062 behaviour exactly. Not "publishes but ignores the
/// result": no ref, no commit, and no `remote.origin.fetch` line either, since
/// a config the operator did not ask for is itself a behaviour change.
/// What: `publish_receipt(.., false)` against a live fixture, then read back
/// the remote, the local ref namespace, and `remote.origin.fetch`.
/// Test: itself.
#[test]
fn a_disabled_section_publishes_nothing() {
    let fx = Fixture::new();
    let snap = fx.snapshot("s-alpha", "20260913-100000", PLAIN);
    let fetch_before = git_ok(&fx.repo, &["config", "--get-all", "remote.origin.fetch"]);

    let receipt = publish_receipt(&fx.request("s-alpha", &snap), false);
    assert_eq!(receipt, SessionRefReceipt::default());
    assert!(!receipt.published);
    assert_eq!(receipt.ref_name, None);
    assert_eq!(receipt.error, None);

    assert!(fx.ls_remote("refs/tm/sessions/*").is_empty());
    assert_eq!(
        trusty_common::catchup::session_refs::list_session_refs(&fx.repo).unwrap(),
        vec![]
    );
    assert_eq!(
        git_ok(&fx.repo, &["config", "--get-all", "remote.origin.fetch"]),
        fetch_before,
        "a disabled section must not register the fetch refspec"
    );
}

/// Why: an enabled publish reports the ref it landed on, so `/tm-session-pause`
/// can show the operator where the durable copy went.
/// What: the receipt of a successful publish names the ref, reports `published`
/// and carries no error.
/// Test: itself.
#[test]
fn an_enabled_receipt_names_the_ref_it_published() {
    let fx = Fixture::new();
    let snap = fx.snapshot("s-alpha", "20260913-100000", PLAIN);
    // The receipt path resolves the user id from this host, so assert against
    // whatever it resolves rather than pinning a login into the test.
    let expected = session_ref_for(&fx.repo, "s-alpha")
        .expect("the fixture pins `git config user.name`, so a user id always resolves");

    let receipt = publish_receipt(&fx.request("s-alpha", &snap), true);
    assert!(receipt.published, "{receipt:?}");
    assert_eq!(receipt.ref_name.as_deref(), Some(expected.as_str()));
    assert_eq!(receipt.error, None);
    assert_eq!(fx.ls_remote("refs/tm/sessions/*").len(), 1);
}

/// Why: both halves of the ref key come from outside, and a ref name is a path.
/// A component that is empty, reserved, or traversing after filtering must fail
/// the publish rather than collide with another writer's ref.
/// What: the pure sanitizer over the accept and reject sets.
/// Test: itself.
#[test]
fn ref_components_are_sanitized_and_rejected() {
    assert_eq!(
        sanitize_ref_component("octocat").as_deref(),
        Some("octocat")
    );
    assert_eq!(
        sanitize_ref_component("Robert (Masa) Matsuoka").as_deref(),
        Some("RobertMasaMatsuoka"),
    );
    assert_eq!(
        sanitize_ref_component("tmux-window-230").as_deref(),
        Some("tmux-window-230")
    );
    for bad in [
        "", "   ", ".", "..", "a..b", ".hidden", "-lead", "trail.", "x.lock", "@@@",
    ] {
        assert_eq!(sanitize_ref_component(bad), None, "{bad:?}");
    }
}

/// Why: #7830 (decided) — `tmux-window-230` is the SAME id on every machine the
/// operator works from, so two hosts would write one ref and fight over its
/// lease forever. A managed session id is already globally unique and must be
/// left alone.
/// What: a tmux-derived id gains this host's name as a prefix; a managed UUID
/// does not.
/// Test: itself.
#[test]
fn a_tmux_session_key_is_hostname_qualified() {
    let managed = "7bd5c27a-475b-41df-9e9f-a6f630801717";
    assert_eq!(session_key(managed).as_deref(), Some(managed));

    let key = session_key("tmux-window-230").expect("a tmux id always keys a ref");
    assert!(key.ends_with("tmux-window-230"), "{key}");
    if let Some(host) = sysinfo::System::host_name().and_then(|h| sanitize_ref_component(&h)) {
        assert_eq!(key, format!("{host}-tmux-window-230"));
        assert_ne!(key, "tmux-window-230", "two hosts must not share one ref");
    }
}

/// Why: a snapshot written outside `.trusty-mpm/sessions/` has no store-relative
/// path, so it cannot be stored under one the reader would find. Refusing is
/// what keeps the write and read sides on one naming rule.
/// What: a path outside the store is [`RefPublishError::SnapshotOutsideStore`].
/// Test: itself.
#[test]
fn a_snapshot_outside_the_store_is_refused() {
    let fx = Fixture::new();
    let stray = fx.repo.join("session-20260913-100000.md");
    std::fs::write(&stray, PLAIN).unwrap();

    let err = publish_session_ref_as(&fx.request("s-alpha", &stray), Some(USER)).unwrap_err();
    assert!(
        matches!(err, RefPublishError::SnapshotOutsideStore(_)),
        "{err:?}"
    );
}

/// Why: the reader rebuilds `sessions-log.jsonl` from these trailers, and the
/// ref key is deliberately NOT the session id — a commit that stopped carrying
/// them would hydrate bytes nothing could attribute, and the snapshot would
/// resolve for nobody (#5272's rule, reached from the ref side).
/// What: publish once, read the commit message back with `cat-file`, and check
/// all four trailers name the SESSION rather than the ref key.
/// Test: itself.
#[test]
fn the_ref_commit_carries_the_attribution_trailers() {
    let fx = Fixture::new();
    let snap = fx.snapshot("s-alpha", "20260913-100000", PLAIN);
    let out = publish_session_ref_as(&fx.request("s-alpha", &snap), Some(USER)).unwrap();

    let message = git_ok(&fx.repo, &["cat-file", "commit", &out.commit]);
    assert!(message.contains("Session-Id: s-alpha"), "{message}");
    assert!(message.contains("Session-Event: pause"), "{message}");
    assert!(
        message.contains("Session-Snapshot: s-alpha/session-20260913-100000.md"),
        "{message}"
    );
    assert!(message.contains("Session-Timestamp: "), "{message}");
}
