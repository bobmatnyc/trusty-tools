//! Keep tm-regenerated harness scaffolding out of a project's git history
//! (issue #3427).
//!
//! Why: `tm` writes composed agents (`.claude/agents/`), skills
//! (`.claude/skills/`), and output styles (`.claude/output-styles/`) directly
//! into a project's working tree — a managed workspace deploys all three
//! project-locally, and the `#2125` project-tier output-style deploy runs
//! unconditionally for every session regardless of managed/standalone. If a
//! project ever commits one of those paths (accidentally, in a PR, or from a
//! pre-#3427 install with no `.gitignore` guard), the NEXT session's
//! regeneration writes the same path back as local content — and a later
//! `git merge --ff-only origin/main` aborts with "your local changes would be
//! overwritten", exactly the `duettoresearch/duetto-eve-agents` reproduction
//! (28 commits stranded, resolved only by `git rm -r --cached` + a manual
//! `.gitignore`, duetto-eve-agents#111). This is Option 1 (prevent) of #3427;
//! [`crate::daemon::doctor_scaffold_tracking`] is Option 2 (detect + exact
//! remediation for a project that already committed these paths — a
//! `.gitignore` entry added AFTER the fact does not untrack anything already
//! in the index).
//! What: [`ensure_scaffold_gitignored`] appends a small, clearly-delimited
//! managed block to `<project_dir>/.gitignore` naming exactly the three
//! harness-owned subdirectories tm writes into — never the broader
//! `.claude/` (which also holds `settings.json` / project `.mcp.json`,
//! legitimately shared config the issue explicitly calls out as NOT in
//! scope). Gated on `project_dir` actually being a git working tree (a
//! `.git` entry present) — mirroring the existing
//! `daemon::managed_routes::inproject::ensure_worktrees_gitignored`
//! precedent — so a non-git project (or a bare library/test call) never
//! grows a stray `.gitignore`. Idempotent: a repeat call is a no-op once the
//! managed block's begin marker is present, and an existing `.gitignore`
//! with unrelated content is preserved byte-for-byte apart from the
//! appended block. This only edits the WORKING TREE file — it never `git
//! add`s or commits it, so the change goes through the project's normal
//! review/diff flow like any other tm-authored edit.
//! Test: `writes_block_to_fresh_gitignore`, `idempotent_on_repeat_call`,
//! `preserves_unrelated_existing_content`, `noop_when_not_a_git_repo`,
//! `appends_newline_when_existing_file_lacks_trailing_newline`.

use std::io::Write as _;
use std::path::Path;

/// First line of the managed block — its presence is the sole idempotency
/// check (a repeat call skips entirely once this line exists anywhere in the
/// file).
pub const SCAFFOLD_GITIGNORE_BEGIN: &str =
    "# >>> trusty-mpm harness scaffolding (auto-managed by tm, issue #3427) >>>";

/// Last line of the managed block.
pub const SCAFFOLD_GITIGNORE_END: &str = "# <<< trusty-mpm harness scaffolding <<<";

/// The exact harness-owned subdirectories tm writes into, and nothing else.
///
/// Why: surgical by design — `.claude/settings.json` and the project
/// `.mcp.json` are legitimately shared, tracked config (issue #3427's
/// explicit "Important Note") and must never be swept up by this list.
/// What: trailing-slash directory patterns so only the three harness-owned
/// subtrees are ignored, not sibling files under `.claude/`.
pub const SCAFFOLD_IGNORED_PATHS: &[&str] = &[
    ".claude/agents/",
    ".claude/skills/",
    ".claude/output-styles/",
];

/// Ensure `<project_dir>/.gitignore` carries the tm-scaffolding managed
/// block, when `project_dir` is a git working tree.
///
/// Why/What: see the module doc. Returns `Ok(true)` when the block was
/// (freshly) written, `Ok(false)` when it was already present or
/// `project_dir` is not a git repo (both are legitimate no-ops, not
/// failures). Propagates genuine I/O errors (permissions, disk full) so the
/// non-fatal caller can log them rather than silently swallowing a real
/// problem.
/// Test: `writes_block_to_fresh_gitignore`, `idempotent_on_repeat_call`,
/// `preserves_unrelated_existing_content`, `noop_when_not_a_git_repo`.
pub fn ensure_scaffold_gitignored(project_dir: &Path) -> std::io::Result<bool> {
    if !project_dir.join(".git").exists() {
        return Ok(false);
    }

    let gitignore_path = project_dir.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore_path).unwrap_or_default();

    // Idempotent: the begin marker's presence anywhere in the file means a
    // prior run already installed the block — never append a duplicate.
    if existing
        .lines()
        .any(|line| line == SCAFFOLD_GITIGNORE_BEGIN)
    {
        return Ok(false);
    }

    let mut block = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        block.push('\n');
    }
    if !existing.is_empty() {
        block.push('\n');
    }
    block.push_str(SCAFFOLD_GITIGNORE_BEGIN);
    block.push('\n');
    for path in SCAFFOLD_IGNORED_PATHS {
        block.push_str(path);
        block.push('\n');
    }
    block.push_str(SCAFFOLD_GITIGNORE_END);
    block.push('\n');

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&gitignore_path)?;
    file.write_all(block.as_bytes())?;

    tracing::info!(
        path = %gitignore_path.display(),
        "added tm harness-scaffolding block to .gitignore (issue #3427)"
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_git_repo(dir: &Path) {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
    }

    #[test]
    fn writes_block_to_fresh_gitignore() {
        let tmp = crate::test_support::hermetic_temp_dir();
        init_git_repo(tmp.path());

        let wrote = ensure_scaffold_gitignored(tmp.path()).unwrap();
        assert!(wrote, "first call must write the block");

        let content = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(content.contains(SCAFFOLD_GITIGNORE_BEGIN));
        assert!(content.contains(SCAFFOLD_GITIGNORE_END));
        for path in SCAFFOLD_IGNORED_PATHS {
            assert!(
                content.contains(path),
                "missing {path} in written .gitignore:\n{content}"
            );
        }
        // Never blanket-ignore `.claude/` itself — settings.json / .mcp.json
        // must remain trackable.
        assert!(!content.lines().any(|l| l.trim() == ".claude/"));
    }

    #[test]
    fn idempotent_on_repeat_call() {
        let tmp = crate::test_support::hermetic_temp_dir();
        init_git_repo(tmp.path());

        assert!(ensure_scaffold_gitignored(tmp.path()).unwrap());
        let first = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();

        // A second (and third) call must be a no-op — no duplicated block.
        assert!(!ensure_scaffold_gitignored(tmp.path()).unwrap());
        assert!(!ensure_scaffold_gitignored(tmp.path()).unwrap());
        let second = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();

        assert_eq!(first, second, "repeat calls must not modify the file");
        assert_eq!(
            second.matches(SCAFFOLD_GITIGNORE_BEGIN).count(),
            1,
            "the begin marker must appear exactly once"
        );
    }

    #[test]
    fn preserves_unrelated_existing_content() {
        let tmp = crate::test_support::hermetic_temp_dir();
        init_git_repo(tmp.path());
        let gitignore_path = tmp.path().join(".gitignore");
        std::fs::write(&gitignore_path, "node_modules/\ntarget/\n").unwrap();

        ensure_scaffold_gitignored(tmp.path()).unwrap();

        let content = std::fs::read_to_string(&gitignore_path).unwrap();
        assert!(content.starts_with("node_modules/\ntarget/\n"));
        assert!(content.contains(SCAFFOLD_GITIGNORE_BEGIN));
    }

    #[test]
    fn appends_newline_when_existing_file_lacks_trailing_newline() {
        let tmp = crate::test_support::hermetic_temp_dir();
        init_git_repo(tmp.path());
        let gitignore_path = tmp.path().join(".gitignore");
        // Deliberately no trailing newline.
        std::fs::write(&gitignore_path, "node_modules/").unwrap();

        ensure_scaffold_gitignored(tmp.path()).unwrap();

        let content = std::fs::read_to_string(&gitignore_path).unwrap();
        assert!(content.starts_with("node_modules/\n"));
        assert!(
            !content.contains("node_modules/#"),
            "must not glue lines together"
        );
    }

    #[test]
    fn noop_when_not_a_git_repo() {
        let tmp = crate::test_support::hermetic_temp_dir();
        // No `.git` created.

        let wrote = ensure_scaffold_gitignored(tmp.path()).unwrap();
        assert!(!wrote, "must not write when project_dir is not a git repo");
        assert!(!tmp.path().join(".gitignore").exists());
    }
}
