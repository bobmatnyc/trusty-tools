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
//! `crate::daemon::doctor_scaffold_tracking` is Option 2 (detect + exact
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

/// The exact harness-owned paths tm writes into, and nothing else.
///
/// Why: surgical by design — `.claude/settings.json` and the project
/// `.mcp.json` are legitimately shared, tracked config (issue #3427's
/// explicit "Important Note") and must never be swept up by this list.
///
/// #4832 added the `.trusty-mpm/` runtime-output paths. `tm` writes a
/// per-session compiled prompt and a `last-instructions.md` inspection stash
/// into the project's harness directory on every launch, and neither was
/// covered here — a target project that ran `tm` grew untracked noise (or, if
/// committed once, the same "your local changes would be overwritten" merge
/// abort this module exists to prevent). Note what is deliberately NOT listed:
/// `.trusty-mpm/framework/` holds the operator-authored `manifest.toml`
/// override, and `.trusty-mpm/config.toml` is written by `tm project init` —
/// both are project config an operator MAY want tracked, so ignoring them would
/// repeat the over-broad `.claude/` mistake called out above.
///
/// #7762 added the two `settings*.json.lock` sidecars. Every managed launch now
/// runs its settings write under an `flock(2)` sidecar
/// ([`crate::core::settings_lock`]), and that sidecar is created beside a file
/// the project legitimately tracks — so without these two lines the very fix
/// that stopped the launch dropping a `.bak` starts dropping a `.lock` into
/// `git status` instead. The settings files themselves stay absent from this
/// list, exactly as the "Important Note" above requires: only the harness's own
/// lock artifact is ignored, never the config it guards.
/// What: trailing-slash directory patterns, the one stash FILE, and the two lock
/// sidecars — so only the harness-owned subtrees and artifacts are ignored, not
/// sibling config.
/// Test: `writes_block_to_fresh_gitignore`,
/// `block_covers_session_output_but_not_project_config`,
/// `lock_entries_match_the_settings_lock_sidecar`.
pub const SCAFFOLD_IGNORED_PATHS: &[&str] = &[
    ".claude/agents/",
    ".claude/skills/",
    ".claude/output-styles/",
    ".claude/settings.json.lock",
    ".claude/settings.local.json.lock",
    ".trusty-mpm/sessions/",
    ".trusty-mpm/logs/",
    ".trusty-mpm/last-instructions.md",
];

/// Ensure `<project_dir>/.gitignore` carries the tm-scaffolding managed
/// block, when `project_dir` is a git working tree.
///
/// Why/What: see the module doc. Returns `Ok(true)` when the block was written
/// or refreshed, `Ok(false)` when it was already up to date or `project_dir` is
/// not a git repo (both are legitimate no-ops, not failures). Propagates genuine
/// I/O errors (permissions, disk full) so the non-fatal caller can log them
/// rather than silently swallowing a real problem.
///
/// #7762: the begin marker alone used to be the whole idempotency check, so a
/// project scaffolded before a path was added to [`SCAFFOLD_IGNORED_PATHS`]
/// never gained it — the new entry reached fresh projects only. A block whose
/// body is missing a managed path is now re-rendered in place, which is what
/// carries the two `settings*.json.lock` entries to the projects that already
/// have a block.
/// Test: `writes_block_to_fresh_gitignore`, `idempotent_on_repeat_call`,
/// `preserves_unrelated_existing_content`, `noop_when_not_a_git_repo`,
/// `an_existing_block_gains_newly_managed_paths`.
pub fn ensure_scaffold_gitignored(project_dir: &Path) -> std::io::Result<bool> {
    if !project_dir.join(".git").exists() {
        return Ok(false);
    }

    let gitignore_path = project_dir.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore_path).unwrap_or_default();

    // The begin marker's presence anywhere in the file means a prior run already
    // installed the block — never append a duplicate; refresh that one instead.
    if existing
        .lines()
        .any(|line| line == SCAFFOLD_GITIGNORE_BEGIN)
    {
        return refresh_block(&gitignore_path, &existing);
    }

    let mut block = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        block.push('\n');
    }
    if !existing.is_empty() {
        block.push('\n');
    }
    block.push_str(&render_block());

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

/// The managed block exactly as it is written: begin, every managed path, end.
///
/// What: newline-terminated throughout, including the end marker, so a caller
/// can append it or splice it between two slices of an existing file.
/// Test: `writes_block_to_fresh_gitignore`.
fn render_block() -> String {
    let mut block = String::new();
    block.push_str(SCAFFOLD_GITIGNORE_BEGIN);
    block.push('\n');
    for path in SCAFFOLD_IGNORED_PATHS {
        block.push_str(path);
        block.push('\n');
    }
    block.push_str(SCAFFOLD_GITIGNORE_END);
    block.push('\n');
    block
}

/// Re-render an already-installed block that is missing a managed path (#7762).
///
/// Why: this is the upgrade path. Every entry added to
/// [`SCAFFOLD_IGNORED_PATHS`] after a project was first scaffolded would
/// otherwise reach only projects that never ran tm before.
/// What: locates the begin/end markers, and rewrites the whole file with a
/// freshly [`render_block`]ed body between them when any managed path is absent
/// from the current one. Content OUTSIDE the markers is carried through
/// verbatim, including operator lines added between them and the block — the
/// entire file is rewritten, so the trailing-newline shape of the original is
/// preserved deliberately rather than incidentally. A block whose END marker was
/// hand-deleted is left alone: rewriting it would have to guess where the
/// operator's own lines resume.
/// Test: `an_existing_block_gains_newly_managed_paths`,
/// `idempotent_on_repeat_call`, `a_block_with_no_end_marker_is_left_alone`.
fn refresh_block(gitignore_path: &Path, existing: &str) -> std::io::Result<bool> {
    let lines: Vec<&str> = existing.lines().collect();
    let Some(begin) = lines.iter().position(|l| *l == SCAFFOLD_GITIGNORE_BEGIN) else {
        return Ok(false);
    };
    let Some(end) = lines[begin..]
        .iter()
        .position(|l| *l == SCAFFOLD_GITIGNORE_END)
        .map(|offset| begin + offset)
    else {
        tracing::warn!(
            path = %gitignore_path.display(),
            "tm scaffolding block has no end marker — leaving it as the operator edited it"
        );
        return Ok(false);
    };

    let current = &lines[begin..=end];
    if SCAFFOLD_IGNORED_PATHS
        .iter()
        .all(|managed| current.iter().any(|line| line.trim() == *managed))
    {
        return Ok(false);
    }

    let mut rebuilt = String::new();
    for line in &lines[..begin] {
        rebuilt.push_str(line);
        rebuilt.push('\n');
    }
    rebuilt.push_str(&render_block());
    for line in &lines[end + 1..] {
        rebuilt.push_str(line);
        rebuilt.push('\n');
    }
    if !existing.ends_with('\n') {
        rebuilt.pop();
    }
    std::fs::write(gitignore_path, rebuilt)?;

    tracing::info!(
        path = %gitignore_path.display(),
        "refreshed the tm harness-scaffolding block in .gitignore (issue #7762)"
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
    fn block_covers_session_output_but_not_project_config() {
        // #4832: `tm` writes per-session output into `.trusty-mpm/` on every
        // launch, so the block must name it — but must NOT swallow the
        // operator-authored `framework/manifest.toml` or `config.toml`.
        let tmp = crate::test_support::hermetic_temp_dir();
        init_git_repo(tmp.path());
        ensure_scaffold_gitignored(tmp.path()).unwrap();
        let content = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();

        for covered in [
            ".trusty-mpm/sessions/",
            ".trusty-mpm/logs/",
            ".trusty-mpm/last-instructions.md",
        ] {
            assert!(
                content.lines().any(|l| l.trim() == covered),
                "missing {covered} in:\n{content}"
            );
        }
        for spared in [
            ".trusty-mpm/",
            ".trusty-mpm/framework/",
            ".trusty-mpm/config.toml",
            // #5207: the committed project config MUST stay trackable — being
            // reviewable in a PR is the entire reason it exists. Scaffolding an
            // ignore rule over it would silently defeat the feature.
            crate::core::project_config::PROJECT_CONFIG_FILE,
        ] {
            assert!(
                !content.lines().any(|l| l.trim() == spared),
                "{spared} is operator config and must stay trackable"
            );
        }
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

    /// The two lock entries are spelled by `settings_lock`, not by hand (#7762).
    ///
    /// Why: the sidecar name is decided in `settings_lock::lock_sidecar` and
    /// written here as a literal. This is what stops the two drifting — if the
    /// sidecar suffix ever changes, this fails instead of the operator's
    /// `git status` quietly growing an untracked file.
    #[test]
    fn lock_entries_match_the_settings_lock_sidecar() {
        for settings in [".claude/settings.json", ".claude/settings.local.json"] {
            let sidecar = crate::core::settings_lock::lock_sidecar(Path::new(settings));
            let expected = sidecar.to_str().expect("a UTF-8 fixture path");
            assert!(
                SCAFFOLD_IGNORED_PATHS.contains(&expected),
                "{expected} must be in SCAFFOLD_IGNORED_PATHS: {SCAFFOLD_IGNORED_PATHS:?}"
            );
        }
        // The guarded files themselves stay trackable — only the lock artifact
        // is ignored (issue #3427's "Important Note").
        for spared in [".claude/settings.json", ".claude/settings.local.json"] {
            assert!(
                !SCAFFOLD_IGNORED_PATHS.contains(&spared),
                "{spared} is project config and must stay trackable"
            );
        }
    }

    /// A project scaffolded before #7762 gains the new entries on the next pass.
    ///
    /// Why: the begin marker alone used to be the whole idempotency check, so an
    /// existing block never gained an entry added later — the fix would have
    /// reached only projects that had never run tm.
    #[test]
    fn an_existing_block_gains_newly_managed_paths() {
        let tmp = crate::test_support::hermetic_temp_dir();
        init_git_repo(tmp.path());
        let gitignore_path = tmp.path().join(".gitignore");
        // A pre-#7762 block: the markers plus the paths managed at that time.
        std::fs::write(
            &gitignore_path,
            format!(
                "node_modules/\n\n{SCAFFOLD_GITIGNORE_BEGIN}\n\
                 .claude/agents/\n.claude/skills/\n.claude/output-styles/\n\
                 {SCAFFOLD_GITIGNORE_END}\ncustom-tail/\n"
            ),
        )
        .unwrap();

        let refreshed = ensure_scaffold_gitignored(tmp.path()).unwrap();

        assert!(refreshed, "a stale block must be refreshed");
        let content = std::fs::read_to_string(&gitignore_path).unwrap();
        for managed in SCAFFOLD_IGNORED_PATHS {
            assert!(
                content.lines().any(|l| l.trim() == *managed),
                "missing {managed} after the refresh:\n{content}"
            );
        }
        assert_eq!(
            content.matches(SCAFFOLD_GITIGNORE_BEGIN).count(),
            1,
            "the refresh must replace the block, not add a second one"
        );
        assert!(
            content.starts_with("node_modules/\n"),
            "content before the block must survive:\n{content}"
        );
        assert!(
            content.ends_with("custom-tail/\n"),
            "content after the block must survive:\n{content}"
        );
        // And the refresh is itself idempotent.
        assert!(!ensure_scaffold_gitignored(tmp.path()).unwrap());
    }

    /// A block whose end marker was hand-deleted is not rewritten.
    ///
    /// Why: the refresh needs both markers to know where the managed lines stop.
    /// Guessing would delete whatever the operator wrote below the block.
    #[test]
    fn a_block_with_no_end_marker_is_left_alone() {
        let tmp = crate::test_support::hermetic_temp_dir();
        init_git_repo(tmp.path());
        let gitignore_path = tmp.path().join(".gitignore");
        let mangled = format!("{SCAFFOLD_GITIGNORE_BEGIN}\n.claude/agents/\nmine/\n");
        std::fs::write(&gitignore_path, &mangled).unwrap();

        let changed = ensure_scaffold_gitignored(tmp.path()).unwrap();

        assert!(!changed);
        assert_eq!(
            std::fs::read_to_string(&gitignore_path).unwrap(),
            mangled,
            "an operator-edited block must be left byte-for-byte alone"
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
