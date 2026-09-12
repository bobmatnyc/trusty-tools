//! The launch-time `--append-system-prompt` composers.
//!
//! Why: these four functions are one concern — turn a project directory into
//! the exact prompt text `claude` is launched with — and they were the last
//! cohesive block in [`super`], which reached the 500-SLOC production cap when
//! #7688 added the prompt-feedback addendum to two of them. Splitting them out
//! separates prompt COMPOSITION from launch ORCHESTRATION, which is the seam
//! the rest of `session_launch` is already organised around (`settings`,
//! `skills`, `project_hooks`, `sync_assets` each own one stage).
//!
//! What: [`build_system_prompt_for`] and its three seams. Each layers, in
//! order, the override-resolved PM prompt
//! ([`crate::core::instruction_overrides::resolve_pm_prompt`]), the HR-4
//! output-style version fallback
//! ([`crate::core::output_style::apply_output_style_to_prompt_with_native`]),
//! and the #7688 prompt-feedback addendum
//! ([`crate::core::prompt_self_improvement::append_to_pm_prompt`], a no-op when
//! the flag is off).
//!
//! The three `_with_*` seams exist so a test can pin a decision that otherwise
//! reads machine-global state — the output-style probe (#1409) and the live
//! agent-tier scan (#5544). Production calls the bare entry point.
//! Test: `build_system_prompt_for_applies_project_override`,
//! `build_system_prompt_for_no_override_matches_bundled_sections`,
//! `prepare_session_stash_reflects_override`,
//! `compose_session_instructions_display_matches_live_prompt`.

use std::path::Path;

/// Build the `--append-system-prompt` text for `project_dir`, applying any
/// project-level instruction overrides.
///
/// Why: `BASE_PM.md` advertises project-level overrides under
/// `<project>/.trusty-mpm/` (issue #381). The *live* prompt delivered to
/// `claude` must reflect them, and it must be resolved with the same
/// [`crate::core::instruction_overrides::resolve_pm_prompt`] function the
/// inspectable stash uses so the two never diverge (the #382 concern). This is
/// the launch-site entry point; it always returns a usable prompt — there is no
/// home-directory dependency because the prompt is composed from compiled-in
/// bundled assets plus the project's own override files.
/// What: delegates to [`build_system_prompt_for_with_style`] with no explicit
/// style override.
/// Test: `build_system_prompt_for_applies_project_override`.
pub fn build_system_prompt_for(project_dir: &Path) -> String {
    build_system_prompt_for_with_style(project_dir, None)
}

/// [`build_system_prompt_for`] with an explicit output-style override.
///
/// Why: on Claude Code builds older than `1.0.83` the native `outputStyle`
/// settings key is ignored, so the active output style only takes effect if its
/// content is folded into the `--append-system-prompt` text. This is the single
/// launch seam where that injection happens, so both the CLI (`tm launch`) and
/// the client (`/connect`) get identical behaviour. The `--style` flag flows in
/// as `explicit_style`.
/// What: probes `claude --version` for native support, then delegates. The
/// active style is `explicit_style` > `[style] active` config > professional
/// default; an unknown id falls back to the default.
/// Test: the injection logic is unit-tested in
/// `crate::core::output_style::tests`; this composition is covered by
/// `build_system_prompt_for_applies_project_override` (which asserts the PM
/// prompt is preserved regardless of the version gate).
pub fn build_system_prompt_for_with_style(
    project_dir: &Path,
    explicit_style: Option<&str>,
) -> String {
    let native = crate::core::output_style::claude_supports_native_output_style();
    build_system_prompt_for_with_style_and_native(project_dir, explicit_style, native)
}

/// [`build_system_prompt_for_with_style`] with the `native_supported` decision
/// supplied explicitly (no live `claude --version` probe).
///
/// Why: the public wrapper probes `claude --version`, so any test (or caller)
/// that exercises the launch prompt is silently coupled to whether `claude` is
/// installed on the host — the host-dependence that broke
/// `prepare_session_stash_reflects_override` on CI (issue #1409). This seam
/// pins the decision so the stash/launch invariant can be asserted
/// deterministically under BOTH `native_supported = true` (no injection) and
/// `false` (injection fires). Production code keeps real detection above.
/// What: resolve → style-inject → #7688 addendum.
/// Test: `prepare_session_stash_reflects_override`.
pub fn build_system_prompt_for_with_style_and_native(
    project_dir: &Path,
    explicit_style: Option<&str>,
    native_supported: bool,
) -> String {
    let prompt = crate::core::instruction_overrides::resolve_pm_prompt(project_dir);
    let styled = crate::core::output_style::apply_output_style_to_prompt_with_native(
        project_dir,
        explicit_style,
        prompt,
        native_supported,
    );
    // #7688: the flag-gated addendum; a no-op when the flag is off.
    crate::core::prompt_self_improvement::append_to_pm_prompt(project_dir, styled)
}

/// [`build_system_prompt_for`] with the deployed-agent roster supplied by the
/// caller.
///
/// Why (#5544): [`build_system_prompt_for`] rescans the three live agent tiers
/// on every call — `<project>/.claude/agents`, `$CLAUDE_CONFIG_DIR/agents`, and
/// `$HOME/.claude/agents`. Two successive calls can therefore disagree, so any
/// test comparing this prompt against another composition of it is racing
/// machine-global state, and it fails with a message indistinguishable from a
/// genuine regression. The remedy is to give both sides ONE roster value, not
/// to pin `$HOME` — pinning it is a PROCESS-GLOBAL write every sibling test in
/// the same target can observe mid-scan, which is the flake class #5544 tracks.
/// [`crate::core::instruction_overrides::resolve_pm_prompt_with_roster`] is the
/// matching seam one layer down; this is its launch-site counterpart.
/// What: identical to [`build_system_prompt_for`] except the rendered
/// `## Delegation Authority` block comes from `roster` instead of a live scan.
/// Callers build `roster` with
/// [`crate::core::delegation_authority::roster_section_from_dirs`] over tier
/// directories they own.
/// Test: `compose_session_instructions_display_matches_live_prompt` and its
/// `_with_override` sibling (`tests_behavior_b_tests.rs`).
pub fn build_system_prompt_for_with_roster(project_dir: &Path, roster: Option<String>) -> String {
    let (prompt, _source) =
        crate::core::instruction_overrides::resolve_pm_prompt_with_roster(project_dir, || roster);
    let native = crate::core::output_style::claude_supports_native_output_style();
    let styled = crate::core::output_style::apply_output_style_to_prompt_with_native(
        project_dir,
        None,
        prompt,
        native,
    );
    // #7688: the roster seam composes the same delivered prompt, so it owes the
    // same addendum — otherwise `tm session instructions` would print a prompt
    // the session did not receive.
    crate::core::prompt_self_improvement::append_to_pm_prompt(project_dir, styled)
}
