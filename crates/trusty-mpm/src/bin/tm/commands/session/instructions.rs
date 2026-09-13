//! The instruction-pipeline half of the `tm sessions` command group.
//!
//! Why: split out of `session.rs` (#7422) to bring that file back under the
//! 500-SLOC production cap, following the `start`/`catchup` submodule pattern
//! already used there. The move is mechanical — no behaviour change — except
//! for [`print_excluded_scope`], which #7422 adds.
//! What: [`compose_session_instructions`] and
//! [`compose_session_instructions_with_roster`] (the one source of truth for
//! the PM prompt a session receives, and the `last-instructions.md` stash), plus
//! [`print_excluded_scope`], the stderr companion naming what default-deny
//! scoping leaves out.
//! Test: `compose_session_instructions_*` in `session_tests.rs`.

/// Print, on stderr, what this project's sessions will not load (#7422).
///
/// Why: `tm session instructions` is where an operator goes to see what a
/// session actually receives, and since default-deny scoping that answer
/// includes an absence — the shared MCP servers and installed plugins this
/// project does not opt into. stderr, not stdout, because the prompt on stdout
/// is piped into files and diffs.
/// What: one line per excluded server and plugin, then the `[session]` keys
/// that opt them back in. Silent when nothing is excluded, and silent when the
/// managed config dir does not resolve (there is no shared map to scope).
/// Test: `session_scope::check_session_scope` covers the same derivation;
/// `print_excluded_scope_is_silent_when_nothing_is_excluded`.
pub(crate) fn print_excluded_scope(project_dir: &std::path::Path) {
    let Some(config_dir) = trusty_mpm::core::trusty_tools_config::managed_claude_config_dir()
    else {
        return;
    };
    let scope = trusty_mpm::core::session_mcp_scope::resolve_scope(project_dir, &config_dir);
    // #7422: the granted list, not the declared one — an untrusted project's
    // `[session] plugins` entry grants nothing and must report as scoped out.
    let plugins = trusty_mpm::core::session_plugin_scope::excluded_plugins(
        &config_dir,
        &trusty_mpm::core::session_mcp_scope::granted_plugins(project_dir),
    );
    if scope.excluded.is_empty() && plugins.is_empty() {
        return;
    }
    eprintln!();
    eprintln!("scoped out for this project (#7422):");
    for name in &scope.excluded {
        eprintln!("  mcp server  {name}");
    }
    for name in &plugins {
        eprintln!("  plugin      {name}");
    }
    eprintln!(
        "  opt in via [session] mcp_servers / plugins in {}",
        trusty_mpm::core::project_config::PROJECT_CONFIG_FILE
    );
}

/// Run the instruction merge pipeline and stash the override-resolved PM prompt.
///
/// Why: `session start` and `session instructions` both need the effective PM
/// prompt — the text actually delivered to `claude --append-system-prompt-file`.
/// The old code returned `output.merged` (the legacy pipeline: INSTRUCTIONS.md +
/// delegation authority + CLAUDE.md) for display, while stashing `resolve_pm_prompt`
/// separately. That caused `tm session instructions` to print content that differed
/// from what Claude received, which is exactly the divergence issue #382 describes.
/// The single source of truth for "what claude receives" is `resolve_pm_prompt`;
/// the display and the stash must both come from it.
/// #4832: `fw` is gone — the pipeline no longer reads a framework path, so the
/// parameter had no remaining use.
/// What: builds a `PipelineInput` and runs `build_instructions` to ensure
/// `CLAUDE.md` is seeded (the side-effect we still need); resolves the PM prompt
/// via `crate::core::instruction_overrides::resolve_pm_prompt`; writes it to
/// `<project>/.trusty-mpm/last-instructions.md`; returns the resolved prompt text,
/// the `PipelineOutput` metadata flags, and the stash path.
/// Test: `compose_session_instructions_display_matches_stash`,
/// `compose_session_instructions_display_matches_live_prompt`.
pub(crate) fn compose_session_instructions(
    project_dir: &std::path::Path,
) -> anyhow::Result<(
    String,
    trusty_mpm::core::instruction_pipeline::PipelineOutput,
    std::path::PathBuf,
)> {
    compose_session_instructions_with_roster(project_dir, None)
}

/// [`compose_session_instructions`] with the deployed-agent roster supplied.
///
/// Why (#5544): the equality this function's tests assert —
/// `compose_session_instructions(p).0 == build_system_prompt_for(p)` — puts two
/// independent scans of the three LIVE agent tiers on either side of an
/// `assert_eq!`. Those tiers are machine-global mutable state, so the two scans
/// can legitimately disagree and the test fails with a message that reads as a
/// prompt regression. Injecting one roster into both sides is the fix. The
/// alternative — pinning `$HOME` and `$CLAUDE_CONFIG_DIR` around the test — is
/// a PROCESS-GLOBAL write every sibling in the `tm` bin target can observe
/// mid-scan, which is the flake class #5544 tracks rather than a cure for it.
/// What: identical to [`compose_session_instructions`] except that a
/// `Some(roster)` routes the prompt through
/// [`trusty_mpm::core::session_launch::build_system_prompt_for_with_roster`].
/// `None` — every production caller — takes the live scan, unchanged.
/// Test: `compose_session_instructions_display_matches_live_prompt`,
/// `compose_session_instructions_display_matches_live_prompt_with_override`.
pub(crate) fn compose_session_instructions_with_roster(
    project_dir: &std::path::Path,
    roster: Option<String>,
) -> anyhow::Result<(
    String,
    trusty_mpm::core::instruction_pipeline::PipelineOutput,
    std::path::PathBuf,
)> {
    let mut stdin_prompt = stdin_git_init_prompt(project_dir);
    let should_init: Option<&mut dyn FnMut() -> bool> = match stdin_prompt.as_mut() {
        Some(boxed) => Some(boxed.as_mut()),
        None => None,
    };
    compose_session_instructions_with_roster_and_init(project_dir, roster, should_init)
}

/// The exact text [`stdin_git_init_prompt`] shows, naming the directory that
/// would be initialised (#7673 round 3 review, LOW).
///
/// Why: split so the wording is assertable without a real TTY — the prior
/// text never named a directory at all, which cost nothing at this call site
/// (there is exactly one directory in view), but is wasted ambiguity on a
/// prompt an operator actually reads.
/// What: `"<dir> is not a git repository; initialise one? [y/N] "`.
/// Test: `git_init_prompt_text_names_the_directory`.
fn git_init_prompt_text(dir: &std::path::Path) -> String {
    format!(
        "{} is not a git repository; initialise one? [y/N] ",
        dir.display()
    )
}

/// A real yes/no `git init` prompt, built only when stdin is a TTY (#7673
/// round 3 follow-up, owner ruling 2026-09-13).
///
/// Why: `tm sessions instructions --dir <dir>` is the one production entry
/// point `load_or_create_claude_md` reaches with no prior git-project check
/// (`session start`'s in-place path already refuses a non-git directory
/// before it gets here; `tm launch`/bare `tm` already run the pre-existing,
/// unconditional `commands::auto_git_init::ensure_git_repo` before project
/// detection, so the seed site never sees a non-git directory from there
/// either — see the module-choice note on [`compose_session_instructions_with_roster_and_init`]).
/// A non-interactive caller (no TTY — piped, `--yes`-style automation, a
/// daemon or MCP caller) must get the existing silent-decline default;
/// asking a question nobody can answer would hang the process.
/// What: `None` when stdin is not a terminal. Otherwise a closure that prints
/// [`git_init_prompt_text`] — naming `dir` — on stderr, reads one line, and
/// classifies it through the SAME confirm helper the picker's other yes/no
/// prompts use ([`crate::commands::picker_delete::confirm_is_yes`]) rather
/// than a new stdin reader.
/// Test: the decline/accept DECISION is tested directly through
/// [`compose_session_instructions_with_roster_and_init`]'s injected closure
/// (`instructions_tests.rs`); actually reading stdin is terminal interaction,
/// not exercised in CI, matching every other confirm prompt in this crate.
fn stdin_git_init_prompt(dir: &std::path::Path) -> Option<Box<dyn FnMut() -> bool>> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return None;
    }
    let dir = dir.to_path_buf();
    Some(Box::new(move || {
        use std::io::Write as _;
        eprint!("{}", git_init_prompt_text(&dir));
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        crate::commands::picker_delete::confirm_is_yes(&line)
    }))
}

/// [`compose_session_instructions_with_roster`] with the git-init-offer
/// prompt seam threaded through explicitly, so the decline/accept decision is
/// testable without a real TTY or stdin (#7673 round 3 follow-up).
///
/// Why this entry point, and not `session start`'s in-place path or `tm
/// launch`: `start_session_in_place` only runs once
/// `refuse_outside_a_git_project` has already certified the target is inside
/// a git working tree, so `load_or_create_claude_md`'s seed site there can
/// never see a non-git directory — there is nothing for this prompt to offer.
/// `tm launch`/bare `tm` call `commands::auto_git_init::ensure_git_repo`
/// (#6274) before project detection even runs, which already turns a plain
/// directory into an ordinary no-origin git project (or refuses Home/
/// filesystem-root) — by the time either reaches the seed path the directory
/// is already resolved, so wiring a second, different git-init decision onto
/// an already-working flow would be the parallel path the reuse note warns
/// against, not a fix. This function is the one production call that reaches
/// `load_or_create_claude_md` with NEITHER guard in front of it — the
/// documented `tm sessions instructions --dir <dir>` first-touch shape.
/// What: identical to [`compose_session_instructions_with_roster`] except
/// `should_init` is passed straight through to
/// [`trusty_mpm::core::instruction_pipeline::build_instructions_with_init`]
/// instead of being derived from stdin here.
/// Test: `compose_session_instructions_declines_git_init_without_a_prompt`,
/// `compose_session_instructions_runs_git_init_when_the_prompt_accepts`.
fn compose_session_instructions_with_roster_and_init(
    project_dir: &std::path::Path,
    roster: Option<String>,
    should_init: Option<&mut dyn FnMut() -> bool>,
) -> anyhow::Result<(
    String,
    trusty_mpm::core::instruction_pipeline::PipelineOutput,
    std::path::PathBuf,
)> {
    use trusty_mpm::core::instruction_pipeline::{PipelineInput, build_instructions_with_init};

    // Run the legacy pipeline for its side-effects: seed CLAUDE.md if absent
    // and populate the metadata flags (agent_count, claude_md_created, …).
    let input = PipelineInput {
        // #4588: the roster is resolved from the project by the one shared
        // resolver, not from a directory named here — naming one tier is what
        // made the printed count disagree with the delivered roster.
        project_dir: project_dir.to_path_buf(),
        claude_md_path: project_dir.join("CLAUDE.md"),
        // #7673: the seed-site guard's home. This is the CLI boundary, so the
        // ambient read belongs here rather than inside the guard.
        home: dirs::home_dir(),
    };
    let output = build_instructions_with_init(&input, should_init)?;

    // The single source of truth for the live PM prompt is
    // `build_system_prompt_for`, NOT the bare `resolve_pm_prompt`. The launcher
    // applies HR-4 output-style version-fallback injection on top of the resolved
    // prompt (issue #1409), so `tm session instructions` must show — and the
    // stash must hold — that SAME injected text. Writing the pre-injection
    // `resolve_pm_prompt` here made the display/stash diverge from the real launch
    // prompt whenever `claude` was absent/old (injection fires), the same #382
    // divergence this function was written to prevent. Routing through
    // `build_system_prompt_for` keeps display, stash, and launch identical
    // regardless of Claude Code version.
    let resolved_prompt = match roster {
        Some(roster) => trusty_mpm::core::session_launch::build_system_prompt_for_with_roster(
            project_dir,
            Some(roster),
        ),
        None => trusty_mpm::core::session_launch::build_system_prompt_for(project_dir),
    };
    // #4832: the harness ROOT, not `project_dir` — a worktree must never grow
    // its own `.trusty-mpm/`.
    let stash_dir = trusty_mpm::core::harness_root::harness_dir(project_dir);
    std::fs::create_dir_all(&stash_dir)?;
    let stash = stash_dir.join("last-instructions.md");
    std::fs::write(&stash, &resolved_prompt)?;

    Ok((resolved_prompt, output, stash))
}

#[cfg(test)]
#[path = "instructions_tests.rs"]
mod tests;
