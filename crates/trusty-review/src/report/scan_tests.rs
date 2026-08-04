//! Tests for the built-in repository scanner (#2342.3).
//!
//! Why: the scanner is the `measured` baseline that makes a bare run
//! substantive; its LoC counting, language attribution, file count, and manifest
//! framework detection must be pinned against a real fixture repository.
//! What: builds a tiny git repo in a temp dir (via `git init`/`add`) and asserts
//! the computed baseline; also covers the non-repo and empty-dir degradations,
//! plus the #4733 git-fail-open guard (a broken repo must refuse the walk, never
//! downgrade to a `.gitignore`-blind directory walk).
//! Test: included as `#[cfg(test)] mod tests` from `scan.rs`.

use std::path::Path;
use std::process::Command;

use super::{Corpus, NO_REPO_STDERR, classify_ls_files_failure, list_tracked_files, scan_repo};

/// Initialise a git repo at `dir` and stage everything, best-effort.
fn git_init_add(dir: &Path) {
    let _ = Command::new("git").arg("-C").arg(dir).arg("init").output();
    let _ = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["add", "-A"])
        .output();
}

/// Why: the scan must count blank-excluded LoC per language and the file total.
/// What: writes Rust + TypeScript sources (with blank lines) plus a data file,
/// then asserts the LoC total excludes blanks, the language mix is ordered, and
/// the file count includes all tracked files.
/// Test: this test itself.
#[test]
fn scan_counts_loc_and_languages() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();

    // 3 non-blank Rust lines (one blank line excluded).
    std::fs::write(dir.join("main.rs"), "fn main() {\n\n    work();\n}\n").expect("rs");
    // 2 non-blank TypeScript lines.
    std::fs::write(dir.join("app.ts"), "const x = 1;\nexport {};\n").expect("ts");
    // A data file: counted in file_count, excluded from LoC (not a source ext).
    std::fs::write(dir.join("data.json"), "{ \"a\": 1 }\n").expect("json");
    git_init_add(dir);

    let scan = scan_repo(dir).expect("scan produces a baseline");
    assert_eq!(scan.total_loc, 5, "3 Rust + 2 TS non-blank lines");
    assert_eq!(scan.file_count, 3, "all three tracked files counted");
    // Rust (3) sorts before TypeScript (2).
    assert_eq!(scan.by_language[0].language, "Rust");
    assert_eq!(scan.by_language[0].loc, 3);
    assert_eq!(scan.primary_languages(2), vec!["Rust", "TypeScript"]);
    assert!(!scan.is_empty());
}

/// Why: naming the build manifest + top dependencies is the key `measured`
/// signal; it must be parsed from the manifests actually present.
/// What: writes a Cargo.toml and a package.json, then asserts both frameworks are
/// detected with their project names and declared dependency names.
/// Test: this test itself.
#[test]
fn scan_detects_frameworks() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();

    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"acme-core\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1\"\ntokio = \"1\"\n",
    )
    .expect("cargo");
    std::fs::write(
        dir.join("package.json"),
        "{ \"name\": \"acme-web\", \"dependencies\": { \"react\": \"18\", \"next\": \"15\" } }",
    )
    .expect("pkg");
    // A source file so the scan is non-empty even without recognised LoC.
    std::fs::write(dir.join("lib.rs"), "pub fn f() {}\n").expect("rs");
    git_init_add(dir);

    let scan = scan_repo(dir).expect("scan");
    let cargo = scan
        .frameworks
        .iter()
        .find(|f| f.manifest == "Cargo.toml")
        .expect("cargo detected");
    assert_eq!(cargo.name, "acme-core");
    assert!(cargo.deps.contains(&"serde".to_string()));
    assert!(cargo.deps.contains(&"tokio".to_string()));

    let pkg = scan
        .frameworks
        .iter()
        .find(|f| f.manifest == "package.json")
        .expect("package.json detected");
    assert_eq!(pkg.name, "acme-web");
    assert!(pkg.deps.contains(&"react".to_string()));
}

/// Why: a non-existent path or empty directory must degrade gracefully to `None`
/// so the report falls back to declared/inferred data rather than aborting.
/// What: asserts a missing path and an empty directory both scan to `None`.
/// Test: this test itself.
#[test]
fn scan_non_repo_dir_is_empty() {
    assert!(scan_repo(Path::new("/nonexistent/xyz-123")).is_none());
    let tmp = tempfile::TempDir::new().expect("tempdir");
    assert!(scan_repo(tmp.path()).is_none(), "empty dir → no baseline");
}

/// Why: a non-git directory must still produce a baseline via the filtered walk
/// fallback (a substantive report should not require a git repo).
/// What: writes a source file into a plain (non-git) dir and asserts the scan
/// counts it.
/// Test: this test itself.
#[test]
fn scan_non_git_dir_uses_walk_fallback() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("s.py"), "x = 1\ny = 2\n").expect("py");
    let scan = scan_repo(tmp.path()).expect("walk fallback scan");
    assert_eq!(scan.total_loc, 2);
    assert_eq!(scan.by_language[0].language, "Python");
}

// ── #4733: a failed git probe must not downgrade to a gitignore-blind walk ──

/// Why: this corpus is sent to an LLM. Before #4733, ANY `git ls-files` failure
/// fell through to a walk that consults `SKIP_DIRS` and nothing else — so a repo
/// whose `.git` is a stale worktree pointer (git still exits 128, but its
/// `.gitignore` is entirely real) contributed its ignored `.env` to the corpus.
/// What: builds a repo with `.env` gitignored, then replaces `.git` with a
/// gitlink pointing nowhere — git 2.54.0 answers `fatal: not a git repository:
/// (null)`, which contains the shorter phrase `not a git repository` while
/// meaning the opposite of "there is no repository here". Asserts the scanner
/// refuses outright rather than walking.
/// Test: this test itself.
#[test]
fn scan_refuses_when_git_is_broken_rather_than_walking() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();
    std::fs::write(dir.join("lib.rs"), "pub fn f() {}\n").expect("rs");
    std::fs::write(dir.join(".gitignore"), ".env\n").expect("gitignore");
    std::fs::write(dir.join(".env"), "AWS_SECRET_ACCESS_KEY=hunter2\n").expect("env");
    // A stale worktree pointer: `.git` is a FILE naming a gitdir that is gone.
    std::fs::write(dir.join(".git"), "gitdir: /nonexistent/xyz-4733\n").expect("gitlink");

    let files = list_tracked_files(dir);
    assert!(
        !files.iter().any(|p| p.to_string_lossy().contains(".env")),
        "a gitignored .env must never reach the LLM corpus when git merely \
         failed to answer: {files:?}"
    );
    assert!(
        files.is_empty(),
        "an unreadable repository yields no corpus at all, not a walked one: {files:?}"
    );
    assert!(
        scan_repo(dir).is_none(),
        "the scan degrades to no measured baseline rather than a leaky one"
    );
}

/// Why: a repository with nothing committed yet has an EMPTY tracked corpus and
/// a fully live `.gitignore`. Treating "git listed zero files" as "not a git
/// repo" (the pre-#4733 `if files.is_empty() { None }` branch) walked it and
/// swept up every untracked, ignored file.
/// What: `git init`s a repo, writes a gitignored `.env` and an unstaged source
/// file, and asserts the scanner reports no corpus rather than walking.
/// Test: this test itself.
#[test]
fn scan_empty_git_repo_does_not_walk_untracked_files() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();
    let init = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("init")
        .output()
        .expect("git init");
    assert!(init.status.success(), "git init failed");
    std::fs::write(dir.join(".gitignore"), ".env\n").expect("gitignore");
    std::fs::write(dir.join(".env"), "OPENROUTER_API_KEY=sk-live\n").expect("env");
    std::fs::write(dir.join("lib.rs"), "pub fn f() {}\n").expect("rs");

    let files = list_tracked_files(dir);
    assert!(
        files.is_empty(),
        "an empty tracked corpus is an empty corpus, not a walk trigger: {files:?}"
    );
    assert!(scan_repo(dir).is_none(), "no tracked files → no baseline");
}

/// Why: git prints [`NO_REPO_STDERR`] for an unreadable `.git` just as readily
/// as for a genuinely empty directory, so the message alone is a necessary
/// condition and never a sufficient one. The filesystem witness is what makes it
/// decidable.
/// What: asserts the classifier refuses when an ancestor carries a `.git` entry
/// despite the "no repository" wording, and only concedes `NoRepo` when no such
/// witness exists.
/// Test: this test itself.
#[test]
fn classify_ls_files_failure_corroborates_the_no_repo_message() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();

    assert!(
        matches!(
            classify_ls_files_failure(dir, &format!("fatal: {NO_REPO_STDERR}: .git")),
            Corpus::NoRepo
        ),
        "no ancestor .git witness → the message is believed"
    );

    std::fs::write(dir.join(".git"), "gitdir: /somewhere\n").expect("gitlink");
    assert!(
        matches!(
            classify_ls_files_failure(dir, &format!("fatal: {NO_REPO_STDERR}: .git")),
            Corpus::Refused
        ),
        "a .git entry contradicts the message — a disagreement is 'cannot be asked'"
    );
}

/// Why: `fatal: not a git repository: (null)` (stale worktree pointer) contains
/// the substring `not a git repository` while meaning the opposite. Matching the
/// short phrase is the trap; only the parenthesised form is diagnostic.
/// What: asserts the stale-pointer wording classifies as `Refused` even in a
/// directory with no `.git` witness at all, and that an unrelated fatal
/// (`dubious ownership`) does too.
/// Test: this test itself.
#[test]
fn classify_ls_files_failure_rejects_near_miss_and_unknown_wordings() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    for stderr in [
        "fatal: not a git repository: (null)",
        "fatal: detected dubious ownership in repository at '/x'",
        "",
    ] {
        assert!(
            matches!(
                classify_ls_files_failure(tmp.path(), stderr),
                Corpus::Refused
            ),
            "unrecognised failure must refuse, not walk: {stderr:?}"
        );
    }
}
