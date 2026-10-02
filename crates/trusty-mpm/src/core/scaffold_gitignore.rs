//! The harness-scaffolding path list and the legacy `.gitignore` block markers
//! (issues #3427, #8758).
//!
//! Why: `tm` writes composed agents, skills, output styles and per-session
//! output into a project's working tree. If a project commits one of those
//! paths, the next session's regeneration collides with it on merge (#3427).
//! #8758: the launch used to append these paths to the project's TRACKED
//! `.gitignore` as a managed block, which dirtied `git status` after every
//! launch and rewrote operator lines and CRLF endings inside the block. That
//! writer is removed; no production path edits a `.gitignore` now.
//! What: [`SCAFFOLD_IGNORED_PATHS`] is the one list of paths. The launch writes
//! it to `.git/info/exclude` through [`crate::core::harness_exclude`]. The
//! begin/end markers stay so the decommission gate and the doctor can still
//! recognise a block an older tm left in a `.gitignore`.
//! Test: `lock_entries_match_the_settings_lock_sidecar`,
//! `block_covers_session_output_but_not_project_config`,
//! `a_project_style_stays_trackable_and_generated_styles_are_ignored`,
//! `launch_leaves_a_tracked_gitignore_untouched`.

/// First line of the managed block an older tm wrote into a `.gitignore`.
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
/// generated `*.tm-floor.md` composites.
/// What: trailing-slash directory patterns, the one skills glob, the generated
/// style files, the one stash FILE, and the two lock sidecars — so only the
/// harness-owned subtrees and artifacts are ignored, not sibling config.
/// Test: `block_covers_session_output_but_not_project_config`,
/// `lock_entries_match_the_settings_lock_sidecar`,
/// `a_tracked_skill_survives_the_exclude_entries`,
/// `a_project_style_stays_trackable_and_generated_styles_are_ignored`.
pub const SCAFFOLD_IGNORED_PATHS: &[&str] = &[
    ".claude/agents/",
    // #7932: glob, never the directory form — see the constant's doc.
    ".claude/skills/*",
    // #8533: one line per bundled style file (`OUTPUT_STYLES`), plus composites.
    ".claude/output-styles/trusty-mpm.md",
    ".claude/output-styles/trusty-mpm-teacher.md",
    ".claude/output-styles/trusty-mpm-research.md",
    // #8453: the supervisor profile's style.
    ".claude/output-styles/trusty-mpm-supervisor.md",
    ".claude/output-styles/*.tm-floor.md",
    ".claude/settings.json.lock",
    ".claude/settings.local.json.lock",
    ".trusty-mpm/sessions/",
    ".trusty-mpm/logs/",
    ".trusty-mpm/last-instructions.md",
];

/// Append the legacy managed block to `<dir>/.gitignore`, as a pre-#8758 launch
/// did.
///
/// Why: the decommission tests need the dirt an older tm left behind; nothing
/// in production writes it any more.
/// What: a blank separator line when the file is non-empty, then the markers
/// around every [`SCAFFOLD_IGNORED_PATHS`] entry.
/// Test: the decommission tests that call it, for example
/// `force_decommission_removes_a_tree_with_an_untracked_scaffold_gitignore`.
#[cfg(test)]
pub(crate) fn append_legacy_block(dir: &std::path::Path) {
    let path = dir.join(".gitignore");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(SCAFFOLD_GITIGNORE_BEGIN);
    text.push('\n');
    for entry in SCAFFOLD_IGNORED_PATHS {
        text.push_str(entry);
        text.push('\n');
    }
    text.push_str(SCAFFOLD_GITIGNORE_END);
    text.push('\n');
    std::fs::write(path, text).expect("write the legacy block");
}

#[cfg(test)]
#[path = "scaffold_gitignore_tests.rs"]
mod tests;
