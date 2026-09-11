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
    let plugins = trusty_mpm::core::session_plugin_scope::excluded_plugins(
        &config_dir,
        &trusty_mpm::core::session_mcp_scope::opt_in_plugins(project_dir),
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
    use trusty_mpm::core::instruction_pipeline::{PipelineInput, build_instructions};

    // Run the legacy pipeline for its side-effects: seed CLAUDE.md if absent
    // and populate the metadata flags (agent_count, claude_md_created, …).
    let input = PipelineInput {
        // #4588: the roster is resolved from the project by the one shared
        // resolver, not from a directory named here — naming one tier is what
        // made the printed count disagree with the delivered roster.
        project_dir: project_dir.to_path_buf(),
        claude_md_path: project_dir.join("CLAUDE.md"),
    };
    let output = build_instructions(&input)?;

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
