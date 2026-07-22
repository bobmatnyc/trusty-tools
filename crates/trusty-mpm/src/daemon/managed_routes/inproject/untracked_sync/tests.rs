//! Unit tests for [`super`] (#2196).
//!
//! Why: split into a sibling `tests.rs` so the production `untracked_sync.rs`
//! stays comfortably under the 500-SLOC cap while these get the generous
//! 1500-SLOC test-file cap (mirrors `inproject/tests.rs`).
//! What: glob-matcher unit tests, `collect_matches`/`sync_untracked_files`
//! behaviour against real temp directories, the size cap, the path-escape
//! guard, and a real-git-worktree round trip proving `.git/info/exclude`
//! resolution survives the linked-worktree gitlink indirection.
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

    append_to_git_exclude(&worktree, &[".env".to_string()]).expect("first append");
    append_to_git_exclude(&worktree, &[".env".to_string()]).expect("second append");

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
