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

use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;

use crate::core::agent_manifest::{ManifestError, atomic_write};

/// First line of the managed block — its presence anywhere in the file is what
/// routes a repeat call into [`refresh_block`] instead of appending a second
/// block.
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
/// #7932: `.claude/skills/*` is the glob form on purpose. A trailing-slash
/// DIRECTORY pattern stops git descending into the directory at all, so a
/// project that tracks one skill and re-includes it below the block
/// (`!/.claude/skills/cargo-commands/`, PR #7916) has a dead negation — the
/// tracked file goes ignored the moment the block is regenerated. The glob
/// ignores each child instead, which leaves the negation reachable. It applies
/// only to `skills`: `agents` has no tracked children in this repo
/// (`git ls-files .claude`), so it stays in the cheaper directory form until
/// one does.
/// #8533: `.claude/output-styles/` holds a project's own `<id>.md` style,
/// which the committed `.trusty-mpm.toml` names, so the directory is not
/// ignored. Only what tm writes there is: the bundled style files and the
/// generated `*.tm-floor.md` composites. A block carrying the old directory
/// line is re-rendered without it by [`refresh_block`]; the same line written
/// by hand outside the block is the operator's and is left alone.
/// What: trailing-slash directory patterns, the one skills glob, the generated
/// style files, the one stash FILE, and the two lock sidecars — so only the
/// harness-owned subtrees and artifacts are ignored, not sibling config.
/// Test: `writes_block_to_fresh_gitignore`,
/// `block_covers_session_output_but_not_project_config`,
/// `lock_entries_match_the_settings_lock_sidecar`,
/// `a_tracked_skill_survives_a_refresh_of_the_block`,
/// `a_project_style_stays_trackable_and_generated_styles_are_ignored`,
/// `an_old_block_ignoring_all_styles_is_migrated`.
pub const SCAFFOLD_IGNORED_PATHS: &[&str] = &[
    ".claude/agents/",
    // #7932: glob, never the directory form — see the constant's doc.
    ".claude/skills/*",
    // #8533: one line per bundled style file (`OUTPUT_STYLES`), plus composites.
    ".claude/output-styles/trusty-mpm.md",
    ".claude/output-styles/trusty-mpm-teacher.md",
    ".claude/output-styles/trusty-mpm-research.md",
    ".claude/output-styles/*.tm-floor.md",
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
///
/// #7875: the rendered block omits any managed path the project already spells
/// outside the block, so a launch against a project that hand-added one of them
/// converges instead of appending a duplicate every time.
/// Test: `writes_block_to_fresh_gitignore`, `idempotent_on_repeat_call`,
/// `preserves_unrelated_existing_content`, `noop_when_not_a_git_repo`,
/// `an_existing_block_gains_newly_managed_paths`,
/// `this_repos_committed_block_matches_the_generator`,
/// `an_unreadable_gitignore_is_reported_not_treated_as_empty`.
pub fn ensure_scaffold_gitignored(project_dir: &Path) -> std::io::Result<bool> {
    if !project_dir.join(".git").exists() {
        return Ok(false);
    }

    let gitignore_path = project_dir.join(".gitignore");
    // #7932: only a MISSING file reads as empty. A present-but-unreadable one
    // used to as well, which appends a second managed block over a first one
    // this call could not see — the same fail-open family as the half-written
    // refresh below.
    let existing = match std::fs::read_to_string(&gitignore_path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };

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
    // #7875: rendered against the file it is about to join, so an entry the
    // project already spells outside the block is not appended a second time.
    block.push_str(&render_block_for(&existing));

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

/// Inclusive line-index range of the managed block within `lines`, if present.
///
/// What: the begin marker's first occurrence, and the first end marker at or
/// after it. `None` when either marker is missing.
/// Test: `an_existing_block_gains_newly_managed_paths`,
/// `a_block_with_no_end_marker_is_left_alone`.
fn block_range(lines: &[&str]) -> Option<(usize, usize)> {
    let begin = lines.iter().position(|l| *l == SCAFFOLD_GITIGNORE_BEGIN)?;
    let end = lines[begin..]
        .iter()
        .position(|l| *l == SCAFFOLD_GITIGNORE_END)
        .map(|offset| begin + offset)?;
    Some((begin, end))
}

/// The rule a `.gitignore` line expresses, normalized for comparison (#7875).
///
/// Why: the block must not re-add an entry the project already spells
/// elsewhere in the file. `/.claude/settings.json.lock` (added by hand in PR
/// #7860) and the block's `.claude/settings.json.lock` are the same rule —
/// only the anchoring slash differs — and without this the block appended a
/// duplicate on every launch.
/// What: trims, then rejects blanks, comments, and negations, and strips one
/// leading `/`. A negation is the OPPOSITE of a rule, so it can never count as
/// coverage — that is what keeps `!/.claude/skills/cargo-commands/` from
/// suppressing a managed path.
/// Test: `an_entry_spelled_outside_the_block_is_not_duplicated`,
/// `a_negation_outside_the_block_is_not_treated_as_coverage`.
fn normalized_rule(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
        return None;
    }
    Some(line.strip_prefix('/').unwrap_or(line))
}

/// The managed block as it must read inside `existing`: begin, every managed
/// path the rest of the file does not already carry, end.
///
/// Why (#7875): rendering the block is file-dependent, not constant — an entry
/// the project already spells outside the block is omitted rather than
/// duplicated. Rendering and the idempotency check therefore share this one
/// function: what a refresh would write IS what it compares against.
/// What: newline-terminated throughout, including the end marker, so a caller
/// can append it or splice it between two slices of an existing file. Only
/// lines OUTSIDE the current block count as coverage; the block's own entries
/// would otherwise erase themselves.
/// Test: `writes_block_to_fresh_gitignore`,
/// `an_entry_spelled_outside_the_block_is_not_duplicated`.
fn render_block_for(existing: &str) -> String {
    let lines: Vec<&str> = existing.lines().collect();
    let block = block_range(&lines);
    let covered: HashSet<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| block.is_none_or(|(begin, end)| *i < begin || *i > end))
        .filter_map(|(_, line)| normalized_rule(line))
        .collect();

    let mut rendered = String::new();
    rendered.push_str(SCAFFOLD_GITIGNORE_BEGIN);
    rendered.push('\n');
    for path in SCAFFOLD_IGNORED_PATHS {
        if covered.contains(path) {
            continue;
        }
        rendered.push_str(path);
        rendered.push('\n');
    }
    rendered.push_str(SCAFFOLD_GITIGNORE_END);
    rendered.push('\n');
    rendered
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
///
/// #7875: the check is now "does the block already read exactly as
/// [`render_block_for`] would write it", not "does it contain every managed
/// path". The old form could not see an entry the project spells outside the
/// block, so it re-added a duplicate on every launch and never converged.
///
/// #7440-class hazard: the rewrite publishes through the shared
/// [`atomic_write`] (stage to a sibling temp file, then rename), so a failed
/// write leaves the operator's `.gitignore` byte-for-byte intact and returns
/// `Err` — never a truncated file, and never `Ok`.
/// Test: `an_existing_block_gains_newly_managed_paths`,
/// `idempotent_on_repeat_call`, `a_block_with_no_end_marker_is_left_alone`,
/// `an_entry_spelled_outside_the_block_is_not_duplicated`,
/// `a_failed_write_leaves_the_gitignore_intact_and_reports_the_error`.
fn refresh_block(gitignore_path: &Path, existing: &str) -> std::io::Result<bool> {
    let lines: Vec<&str> = existing.lines().collect();
    if !lines.contains(&SCAFFOLD_GITIGNORE_BEGIN) {
        return Ok(false);
    }
    let Some((begin, end)) = block_range(&lines) else {
        tracing::warn!(
            path = %gitignore_path.display(),
            "tm scaffolding block has no end marker — leaving it as the operator edited it"
        );
        return Ok(false);
    };

    let desired = render_block_for(existing);
    let current: String = lines[begin..=end]
        .iter()
        .flat_map(|line| [*line, "\n"])
        .collect();
    if current == desired {
        return Ok(false);
    }

    let mut rebuilt = String::new();
    for line in &lines[..begin] {
        rebuilt.push_str(line);
        rebuilt.push('\n');
    }
    rebuilt.push_str(&desired);
    for line in &lines[end + 1..] {
        rebuilt.push_str(line);
        rebuilt.push('\n');
    }
    if !existing.ends_with('\n') {
        rebuilt.pop();
    }
    atomic_write(gitignore_path, &rebuilt).map_err(|e| match e {
        ManifestError::Io(io) => io,
        other => std::io::Error::other(other.to_string()),
    })?;

    tracing::info!(
        path = %gitignore_path.display(),
        "refreshed the tm harness-scaffolding block in .gitignore (issue #7762)"
    );
    Ok(true)
}

#[cfg(test)]
#[path = "scaffold_gitignore_tests.rs"]
mod tests;
