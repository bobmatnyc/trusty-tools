//! Unit tests for [`super`] (#2196).
//!
//! Why: split into a sibling `tests.rs` so the production `untracked_sync.rs`
//! stays comfortably under the 500-SLOC cap while these get the generous
//! 1500-SLOC test-file cap (mirrors `inproject/tests.rs`).
//! What: glob-matcher unit tests, `collect_matches`/`sync_untracked_files`
//! behaviour against real temp directories, the size cap, the path-escape
//! guard, a real-git-worktree round trip proving `.git/info/exclude` resolution
//! survives the linked-worktree gitlink indirection, and the #4733 refusals (a
//! broken repo copies nothing; a tracked path is never overwritten).
//! Test: this IS the test module.

use std::path::Path;

use super::*;

// ── glob_match ───────────────────────────────────────────────────────────

#[test]
fn glob_match_exact() {
    assert!(glob_match(".env", ".env"));
    assert!(!glob_match(".env", ".env.local"));
}

#[test]
fn glob_match_prefix() {
    assert!(glob_match(".env.*", ".env.local"));
    assert!(glob_match(".env.*", ".env.production"));
    assert!(!glob_match(".env.*", ".envrc"));
    assert!(
        !glob_match(".env.*", ".env"),
        "trailing '*' requires >=0 more chars after the dot"
    );
}

#[test]
fn glob_match_suffix() {
    assert!(glob_match("*.env", "prod.env"));
    assert!(glob_match("*.env", ".env"));
    assert!(!glob_match("*.env", "prod.env.bak"));
}

#[test]
fn glob_match_no_match() {
    assert!(!glob_match(".env", "not-env"));
    assert!(!glob_match(".env.*", "other-file.txt"));
}

// ── test fixtures ────────────────────────────────────────────────────────

/// Build a source checkout root with the fixture files the #2196 issue's
/// test plan names: `.env`, `.env.local`, `.env.local.bak` (must NOT match
/// `.env.local` exactly, nor `.env.*` since it has no trailing wildcard
/// continuation issue — `.env.local.bak` DOES start with `.env.` so it is
/// deliberately given a name that also proves the "only allowlisted
/// patterns, nothing broader" guarantee: `random.txt` never matches any
/// default pattern), an oversized file, and a `node_modules/` directory
/// (which must never be recursed into or copied as a whole).
fn make_source_fixture() -> tempfile::TempDir {
    let src = tempfile::TempDir::new().expect("src tmp dir");
    std::fs::write(src.path().join(".env"), b"SECRET=1").expect("write .env");
    std::fs::write(src.path().join(".env.local"), b"SECRET=2").expect("write .env.local");
    std::fs::write(src.path().join("random.txt"), b"not a secret").expect("write random.txt");

    std::fs::create_dir_all(src.path().join("node_modules/some-pkg")).expect("mkdir node_modules");
    std::fs::write(
        src.path().join("node_modules/some-pkg/.env"),
        b"nested, must not be touched by a top-level pattern",
    )
    .expect("write nested .env");

    let oversized = vec![0u8; (MAX_SYNC_FILE_BYTES + 1) as usize];
    std::fs::write(src.path().join(".env.oversized"), &oversized).expect("write oversized file");

    src
}

/// Default `.env*` allowlist used by most tests here (mirrors
/// `DEFAULT_UNTRACKED_SYNC_PATTERNS`).
fn default_patterns() -> Vec<String> {
    vec![".env".into(), ".env.local".into(), ".env.*".into()]
}

// ── sync_untracked_files: matching + non-matching + size cap + no-recurse ──

#[test]
fn syncs_matching_files_only() {
    let src = make_source_fixture();
    let dest = tempfile::TempDir::new().expect("dest tmp dir");

    sync_untracked_files(src.path(), dest.path(), &default_patterns());

    assert!(dest.path().join(".env").is_file(), ".env must be copied");
    assert!(
        dest.path().join(".env.local").is_file(),
        ".env.local must be copied"
    );
    assert!(
        !dest.path().join("random.txt").exists(),
        "non-matching file must NOT be copied"
    );
    assert!(
        !dest.path().join("node_modules").exists(),
        "node_modules/ must NOT be copied (directories are never matched)"
    );
    assert!(
        !dest.path().join(".env.oversized").exists(),
        "oversized file must be skipped, not copied"
    );

    assert_eq!(
        std::fs::read(dest.path().join(".env")).expect("read copied .env"),
        b"SECRET=1"
    );
}

#[test]
fn oversized_file_is_skipped() {
    let src = tempfile::TempDir::new().expect("src tmp dir");
    let oversized = vec![b'x'; (MAX_SYNC_FILE_BYTES + 1) as usize];
    std::fs::write(src.path().join(".env"), &oversized).expect("write oversized .env");
    let dest = tempfile::TempDir::new().expect("dest tmp dir");

    sync_untracked_files(src.path(), dest.path(), &[".env".to_string()]);

    assert!(
        !dest.path().join(".env").exists(),
        "a file over MAX_SYNC_FILE_BYTES must never be copied"
    );
}

#[test]
fn empty_patterns_copies_nothing() {
    let src = make_source_fixture();
    let dest = tempfile::TempDir::new().expect("dest tmp dir");

    sync_untracked_files(src.path(), dest.path(), &[]);

    assert!(
        !dest.path().join(".env").exists(),
        "an empty allowlist must copy nothing (mirrors the caller's enabled=false gate)"
    );
}

// ── non-fatal on missing/unreadable source ──────────────────────────────

#[test]
fn missing_source_root_is_non_fatal() {
    let dest = tempfile::TempDir::new().expect("dest tmp dir");
    let missing_source = Path::new("/this/path/does/not/exist/anywhere/2196");

    // Must not panic; must simply copy nothing.
    sync_untracked_files(missing_source, dest.path(), &default_patterns());

    assert!(
        std::fs::read_dir(dest.path())
            .expect("dest still readable")
            .next()
            .is_none(),
        "dest worktree must stay empty when the source root is missing"
    );
}

#[test]
fn missing_source_file_is_non_fatal() {
    // A pattern that matches nothing existing must not panic or error.
    let src = tempfile::TempDir::new().expect("src tmp dir");
    let dest = tempfile::TempDir::new().expect("dest tmp dir");

    sync_untracked_files(src.path(), dest.path(), &[".env.nonexistent".to_string()]);

    assert!(
        std::fs::read_dir(dest.path())
            .expect("dest still readable")
            .next()
            .is_none(),
        "no candidate files exist; sync must be a silent no-op"
    );
}

// ── path-escape guard ────────────────────────────────────────────────────

#[test]
fn path_escape_pattern_matches_nothing() {
    let src = tempfile::TempDir::new().expect("src tmp dir");
    // A secret OUTSIDE the source root that a malicious/misconfigured
    // pattern might try to reach via `..`.
    let outside = tempfile::TempDir::new().expect("outside tmp dir");
    std::fs::write(outside.path().join(".env"), b"must never be reachable")
        .expect("write outside .env");
    let dest = tempfile::TempDir::new().expect("dest tmp dir");

    let escape_pattern = format!(
        "../{}/.env",
        outside
            .path()
            .file_name()
            .expect("outside dir has a name")
            .to_string_lossy()
    );

    sync_untracked_files(src.path(), dest.path(), &[escape_pattern]);

    assert!(
        std::fs::read_dir(dest.path())
            .expect("dest still readable")
            .next()
            .is_none(),
        "a pattern with a `..`-escaping directory component must copy nothing"
    );
}

#[test]
fn absolute_pattern_matches_nothing() {
    let src = tempfile::TempDir::new().expect("src tmp dir");
    let dest = tempfile::TempDir::new().expect("dest tmp dir");

    sync_untracked_files(src.path(), dest.path(), &["/etc/.env".to_string()]);

    assert!(
        std::fs::read_dir(dest.path())
            .expect("dest still readable")
            .next()
            .is_none(),
        "an absolute pattern must copy nothing"
    );
}

// ── subdirectory pattern (explicit path separator) ──────────────────────

#[test]
fn subdirectory_pattern_matches_one_level_non_recursively() {
    let src = tempfile::TempDir::new().expect("src tmp dir");
    std::fs::create_dir_all(src.path().join("config")).expect("mkdir config");
    std::fs::write(src.path().join("config/.env"), b"nested secret").expect("write config/.env");
    std::fs::create_dir_all(src.path().join("config/nested")).expect("mkdir config/nested");
    std::fs::write(src.path().join("config/nested/.env"), b"too deep")
        .expect("write config/nested/.env");
    let dest = tempfile::TempDir::new().expect("dest tmp dir");

    sync_untracked_files(src.path(), dest.path(), &["config/.env".to_string()]);

    assert!(
        dest.path().join("config/.env").is_file(),
        "an explicit subdirectory pattern must copy that one file"
    );
    assert!(
        !dest.path().join("config/nested").exists(),
        "a subdirectory pattern must not recurse further than the named directory"
    );
}

// ── real linked-worktree .git/info/exclude round trip ───────────────────

/// Init a minimal, committed git repo at `path` (mirrors the setup already
/// used by `inproject::tests`).
fn init_base_repo(path: &Path) {
    let init = std::process::Command::new("git")
        .args(["init", path.to_str().expect("utf8")])
        .output()
        .expect("git init");
    assert!(init.status.success(), "git init failed");
    for (k, v) in [("user.email", "t@example.com"), ("user.name", "T")] {
        let ok = std::process::Command::new("git")
            .args(["-C", path.to_str().expect("utf8"), "config", k, v])
            .status()
            .expect("git config");
        assert!(ok.success(), "git config {k} failed");
    }
    std::fs::write(path.join("README"), b"init").expect("write README");
    let add = std::process::Command::new("git")
        .args(["-C", path.to_str().expect("utf8"), "add", "."])
        .status()
        .expect("git add");
    assert!(add.success(), "git add failed");
    let commit = std::process::Command::new("git")
        .args(["-C", path.to_str().expect("utf8"), "commit", "-m", "init"])
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit failed");
}

#[test]
fn copied_files_are_added_to_shared_worktree_exclude() {
    let base_tmp = tempfile::TempDir::new().expect("base tmp dir");
    let base = base_tmp.path();
    init_base_repo(base);

    let worktree = super::super::create_session_worktree(
        base,
        "untracked-sync-exclude-test",
        &crate::session_manager::ManagedSessionId::new(),
    )
    .expect("create_session_worktree must succeed against a real, committed base repo");
    assert!(
        worktree.join(".git").is_file(),
        "sanity: a linked worktree's .git must be a FILE (gitlink), not a directory"
    );

    let src = make_source_fixture();
    sync_untracked_files(src.path(), &worktree, &default_patterns());

    assert!(worktree.join(".env").is_file(), ".env must be copied");
    assert!(worktree.join(".env.local").is_file());

    // The exclude entry must land in the SHARED common dir's info/exclude —
    // NOT at <worktree>/.git/info/exclude (that path cannot exist: .git is a
    // gitlink file, not a directory).
    let exclude_out = std::process::Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()
        .expect("git rev-parse");
    assert!(exclude_out.status.success());
    let raw = String::from_utf8_lossy(&exclude_out.stdout)
        .trim()
        .to_string();
    let exclude_path = if Path::new(&raw).is_absolute() {
        std::path::PathBuf::from(raw)
    } else {
        worktree.join(raw)
    };
    let content = std::fs::read_to_string(&exclude_path).expect("exclude file readable");
    assert!(
        content.lines().any(|l| l.trim() == ".env"),
        "exclude file must list .env: {content}"
    );
    assert!(
        content.lines().any(|l| l.trim() == ".env.local"),
        "exclude file must list .env.local: {content}"
    );
}

#[test]
fn append_to_git_exclude_is_idempotent() {
    let base_tmp = tempfile::TempDir::new().expect("base tmp dir");
    let base = base_tmp.path();
    init_base_repo(base);
    let worktree = super::super::create_session_worktree(
        base,
        "untracked-sync-idempotent-test",
        &crate::session_manager::ManagedSessionId::new(),
    )
    .expect("create_session_worktree must succeed");

    let exclude = match resolve_git_exclude(&worktree) {
        ExcludeTarget::Registered(p) => p,
        _ => panic!("a real linked worktree must resolve its shared info/exclude path"),
    };
    append_paths_to_exclude(&exclude, &[".env".to_string()]).expect("first append");
    append_paths_to_exclude(&exclude, &[".env".to_string()]).expect("second append");

    let exclude_out = std::process::Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()
        .expect("git rev-parse");
    let raw = String::from_utf8_lossy(&exclude_out.stdout)
        .trim()
        .to_string();
    let exclude_path = if Path::new(&raw).is_absolute() {
        std::path::PathBuf::from(raw)
    } else {
        worktree.join(raw)
    };
    let content = std::fs::read_to_string(&exclude_path).expect("exclude readable");
    let count = content.lines().filter(|l| l.trim() == ".env").count();
    assert_eq!(
        count, 1,
        "repeated appends must not duplicate the entry: {content}"
    );
}

// ── #4733: a failed git probe must not leave a secret unregistered ──────────

/// Regression guard for #4733, git-history leg.
///
/// Why: before this fix the exclude-append ran AFTER the copy and a failure was
/// only a `warn!`. A destination whose `.git` git merely declined to read — a
/// stale worktree gitlink, `detected dubious ownership`, an unreadable `.git` —
/// therefore ended up holding the operator's `.env` with no exclude entry, where
/// a later `git add -A && git commit` stages it into history. Against the
/// pre-fix implementation this test fails on its first assertion (`.env` IS
/// copied).
/// What: a destination whose `.git` is a gitlink pointing nowhere — git 2.54.0
/// answers `fatal: not a git repository: (null)`, which contains the shorter
/// phrase `not a git repository` while meaning the opposite. Asserts nothing is
/// copied at all.
/// Test: this test itself.
#[test]
fn broken_repo_copies_nothing() {
    let src = make_source_fixture();
    let dest = tempfile::TempDir::new().expect("dest tmp dir");
    std::fs::write(dest.path().join(".git"), "gitdir: /nonexistent/xyz-4733\n")
        .expect("write stale gitlink");

    sync_untracked_files(src.path(), dest.path(), &default_patterns());

    assert!(
        !dest.path().join(".env").exists(),
        "a secret must never be written into a repository git could not confirm \
         would ignore it (#4733)"
    );
    assert!(
        !dest.path().join(".env.local").exists(),
        ".env.local must be refused for the same reason (#4733)"
    );
}

/// Why: the guard must not over-refuse. A destination with no repository at all
/// has no history for a secret to leak into, and this is the shape every
/// existing caller-level test uses.
/// What: a plain temp directory still receives the allowlisted files.
/// Test: this test itself.
#[test]
fn plain_directory_destination_still_copies() {
    let src = make_source_fixture();
    let dest = tempfile::TempDir::new().expect("dest tmp dir");

    sync_untracked_files(src.path(), dest.path(), &default_patterns());

    assert!(
        dest.path().join(".env").is_file(),
        "a non-repository destination has no history to leak into — copying stays allowed"
    );
}

/// Why: appending to `info/exclude` succeeds unconditionally and proves nothing
/// about a path the repo already TRACKS — git deliberately reports tracked paths
/// as not-ignored, and `git add -A` stages their modifications regardless. The
/// `check-ignore` re-verification is what catches that, mirroring
/// `native_mcp::is_env_local_actually_ignored`.
/// What: commits a `.env` into the base repo, then syncs a DIFFERENT `.env` from
/// a source checkout. Asserts the tracked file is left untouched.
/// Test: this test itself.
#[test]
fn tracked_secret_is_not_overwritten_in_worktree() {
    let base_tmp = tempfile::TempDir::new().expect("base tmp dir");
    let base = base_tmp.path();
    init_base_repo(base);
    std::fs::write(base.join(".env"), b"COMMITTED=placeholder").expect("write tracked .env");
    for args in [vec!["add", "-A"], vec!["commit", "-m", "track env"]] {
        let ok = std::process::Command::new("git")
            .args(["-C", base.to_str().expect("utf8")])
            .args(&args)
            .status()
            .expect("git");
        assert!(ok.success(), "git {args:?} failed");
    }

    let worktree = super::super::create_session_worktree(
        base,
        "untracked-sync-tracked-test",
        &crate::session_manager::ManagedSessionId::new(),
    )
    .expect("create_session_worktree must succeed");

    let src = make_source_fixture();
    sync_untracked_files(src.path(), &worktree, &default_patterns());

    assert_eq!(
        std::fs::read(worktree.join(".env")).expect("tracked .env present in worktree"),
        b"COMMITTED=placeholder",
        "a TRACKED path is never ignored no matter what info/exclude says — \
         overwriting it would stage the operator's real secret (#4733)"
    );
    assert!(
        worktree.join(".env.local").is_file(),
        "the untracked sibling is still delivered — the refusal is per-path"
    );
}

/// Why: git prints [`NO_REPO_STDERR`] for an unreadable `.git` just as readily
/// as for a genuinely empty directory, so the message is a necessary and never a
/// sufficient condition; the filesystem witness decides.
/// What: asserts the classifier refuses when an ancestor carries a `.git` entry
/// despite the "no repository" wording, concedes `NoRepo` only without one, and
/// never believes the stale-worktree near-miss wording.
/// Test: this test itself.
#[test]
fn classify_rev_parse_failure_corroborates_the_no_repo_message() {
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    let msg = format!("fatal: {NO_REPO_STDERR}: .git");

    assert!(
        matches!(
            classify_rev_parse_failure(tmp.path(), &msg),
            ExcludeTarget::NoRepo
        ),
        "no ancestor .git witness → the message is believed"
    );
    assert!(
        matches!(
            classify_rev_parse_failure(tmp.path(), "fatal: not a git repository: (null)"),
            ExcludeTarget::Unknown(_)
        ),
        "the stale-worktree near-miss contains the short phrase but means the opposite"
    );

    std::fs::write(tmp.path().join(".git"), "gitdir: /somewhere\n").expect("gitlink");
    assert!(
        matches!(
            classify_rev_parse_failure(tmp.path(), &msg),
            ExcludeTarget::Unknown(_)
        ),
        "a .git witness contradicts the message — a disagreement is 'cannot be asked'"
    );
}

/// Why: registration and verification are different jobs, and a broken
/// `info/exclude` defeats both — `git check-ignore` refuses to run at all
/// (`fatal: cannot use .git/info/exclude as an exclude file`), so the question
/// "would `git add -A` stage this?" becomes unanswerable. An unanswerable
/// question must be a refusal, not a copy. Against an implementation that
/// treats the append as best-effort and copies regardless, this test fails:
/// `.env` lands in the worktree unignored.
/// What: a real linked worktree whose resolved `info/exclude` path is replaced
/// by a DIRECTORY. Asserts nothing is copied.
/// Test: this test itself.
#[test]
fn broken_exclude_file_still_refuses_unignored_paths() {
    let base_tmp = tempfile::TempDir::new().expect("base tmp dir");
    let base = base_tmp.path();
    init_base_repo(base);
    let worktree = super::super::create_session_worktree(
        base,
        "untracked-sync-broken-exclude-test",
        &crate::session_manager::ManagedSessionId::new(),
    )
    .expect("create_session_worktree must succeed");

    let exclude = match resolve_git_exclude(&worktree) {
        ExcludeTarget::Registered(p) => p,
        _ => panic!("a real linked worktree must resolve its shared info/exclude path"),
    };
    let _ = std::fs::remove_file(&exclude);
    std::fs::create_dir_all(&exclude).expect("replace info/exclude with a directory");

    let src = make_source_fixture();
    sync_untracked_files(src.path(), &worktree, &default_patterns());

    assert!(
        !worktree.join(".env").exists(),
        "with registration broken and no .gitignore covering it, `git check-ignore` \
         is the only remaining guarantee and it must refuse (#4733)"
    );
    assert!(
        !worktree.join(".env.local").exists(),
        ".env.local must be refused for the same reason (#4733)"
    );
}

/// Why: git is not always on `PATH` — a stripped container, a broken shim, a
/// daemon started with a sanitised environment. A spawn failure tells us
/// nothing about whether the destination is a repository, so it must refuse.
/// This gate runs BEFORE any copy and before `is_path_git_ignored`, so an
/// unspawnable git means nothing is written at all.
/// What: a real linked worktree (so the only variable is the binary) resolved
/// with a program name that cannot be spawned. Asserts `Unknown` — which
/// `broken_repo_copies_nothing` separately pins as "copy nothing".
/// Test: this test itself.
#[test]
fn missing_git_binary_copies_nothing() {
    let base_tmp = tempfile::TempDir::new().expect("base tmp dir");
    let base = base_tmp.path();
    init_base_repo(base);
    let worktree = super::super::create_session_worktree(
        base,
        "untracked-sync-missing-git-test",
        &crate::session_manager::ManagedSessionId::new(),
    )
    .expect("create_session_worktree must succeed");

    // Sanity: with a real git this worktree resolves its exclude file.
    assert!(
        matches!(resolve_git_exclude(&worktree), ExcludeTarget::Registered(_)),
        "sanity: a real linked worktree resolves with a working git"
    );

    assert!(
        matches!(
            resolve_git_exclude_with(&worktree, "trusty-no-such-git-binary-4733"),
            ExcludeTarget::Unknown(_)
        ),
        "an unspawnable git answers nothing — it must never be read as 'no repository', \
         which would let a secret be copied into a live worktree (#4733)"
    );
}

/// Why: `Path::ancestors` walks LEXICALLY. Without `.canonicalize()` a path
/// reached through a symlink (or a relative one) yields a chain that is not its
/// real parentage, so the destination's `.git` is never visited and the
/// permissive `NoRepo` wins — which copies the operator's `.env` into a live
/// repository unregistered. Dropping the call passes every other test in this
/// file; this is the one that fails.
/// What: `link -> repo/sub`, with `.git` on `repo` only. The lexical ancestors
/// of `link` never include `repo`; the canonicalised ones do.
/// Test: this test itself.
#[test]
fn classify_rev_parse_failure_canonicalises_before_walking_ancestors() {
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("sub")).expect("mkdir repo/sub");
    std::fs::write(repo.join(".git"), "gitdir: /somewhere\n").expect("gitlink");
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(repo.join("sub"), &link).expect("symlink");

    assert!(
        matches!(
            classify_rev_parse_failure(&link, &format!("fatal: {NO_REPO_STDERR}: .git")),
            ExcludeTarget::Unknown(_)
        ),
        "the real parent carries a .git — only a canonicalised ancestor walk sees it"
    );
}
