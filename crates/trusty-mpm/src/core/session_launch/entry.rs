//! The public `prepare_session*` entry points.
//!
//! Why (#7685): `mod.rs` holds [`super::prepare_session_inner`], the long
//! preparation sequence, and sat at the 500-SLOC production cap — so the next
//! entry point could not be added beside the eight already there. The wrappers
//! are the cohesive half to move: every one of them is a thin translation of
//! its caller's knowledge (a style id, a clone URL, a managed session id, a
//! pinned home, a trusty-memory verdict) into one `prepare_session_inner` call,
//! and none of them carries preparation logic of its own.
//! What: the entry points, re-exported from `super` so every existing
//! `crate::core::session_launch::prepare_session*` path is unchanged. A pure
//! relocation — no behaviour and no signature changed with the move.
//! Test: every `prepare_session_*` test in `tests.rs`.

use std::path::Path;

use super::{HostInputs, PrepError, PrepReport, prepare_session_inner};
use crate::core::paths::FrameworkPaths;

/// Prepare a project directory for a fresh Claude Code session launch.
///
/// Why: launching `claude` is only correct if its custom instructions are in
/// place first — the composed agents must be deployed and the project
/// `CLAUDE.md` merged. This is the "custom instructions" step that makes a plain
/// `claude` process behave as a trusty-mpm session; both the CLI and the client
/// call this before sending `claude` into the tmux pane.
/// What: deploys composed agents from the framework agent source to
/// `~/.claude/agents/`, runs
/// [`build_instructions`](crate::core::instruction_pipeline::build_instructions) for
/// `project_dir` (which
/// loads or creates the project `CLAUDE.md`), writes the launch prompt — the
/// exact override-resolved AND output-style-injected text produced by
/// [`build_system_prompt_for_with_style`](crate::core::session_launch::build_system_prompt_for_with_style) — to
/// `<project_dir>/.trusty-mpm/last-instructions.md` so the inspectable stash
/// matches the live launch prompt byte-for-byte (issue #1409), and returns a
/// [`PrepReport`].
///
/// ORDERING CONTRACT (#4752, owner ruling 2026-08-04): **a session that starts
/// always has its instructions on disk, matching the text it received.** A
/// session depends on its instructions, so one that cannot get them must not
/// start.
///
/// Two steps establish them, and BOTH are fatal — the same condition reaching
/// two sites, reported as [`PrepError::Instructions`], which every spawning
/// caller refuses to launch on:
///   * [`build_instructions`](crate::core::instruction_pipeline::build_instructions)
///     composes the merged instructions; and
///   * the same resolved prompt is written to
///     `<project_dir>/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md`
///     ([`crate::core::instruction_pipeline::compiled_prompt_path`]) as this
///     function's LAST step.
///
/// There is no exception between them: nothing that can fail in between returns
/// early. The `.trusty-mpm/last-instructions.md` stash is the one write that
/// still degrades to a warning — it is an inspection copy, and letting it
/// short-circuit would have skipped the fatal write below it and started a
/// session whose instructions were never recorded, which is precisely what this
/// contract forbids.
///
/// This is the ONE preparation CONDITION that blocks a launch — one condition,
/// two sites, not two error classes. #2149's non-fatal-preparation design still
/// governs every other variant (a roster or skill deploy failure is surfaced via
/// [`PrepReport::roster_errors`] and the session still starts);
/// [`PrepError::is_fatal`] is the discriminator.
///
/// POSITION: the compiled write is deliberately LAST, so a refusal is not also a
/// half-provisioned workspace — see the inline comment at the call.
///
/// The resume path never calls this function. Both other entry points carry the
/// same fatal write: `daemon::managed_routes::lifecycle::resume_managed` (daemon
/// resume) and `instruction_pipeline::refresh_compiled_prompt` as called from
/// the bare-`tm` in-place relaunch in `tm::commands::guided_inplace`. See spec
/// §10.3.
/// Test: `prepare_session_writes_claude_md_and_stash`, `prepare_session_is_idempotent`,
/// `prepare_session_stash_reflects_override`,
/// `prepare_session_writes_the_compiled_prompt_before_returning`,
/// `prepare_session_fails_when_the_compiled_prompt_cannot_be_written`,
/// `prepare_session_refuses_when_the_instructions_cannot_be_built`.
pub fn prepare_session(fw: &FrameworkPaths, project_dir: &Path) -> Result<PrepReport, PrepError> {
    prepare_session_with_style(fw, project_dir, None)
}

/// Prepare a session, threading the cloned-from `repo_url` for palace pinning.
///
/// Why (issue #1605): a managed session cloned from `repo_url` lives under a
/// throwaway `<owner>/<repo>/<session-id>/` workspace whose basename is the
/// session-id, so palace derivation from that basename picks the WRONG name. The
/// provisioner knows the `repo_url` it cloned, so threading it here supplies the
/// canonical remote. Since ADR-0042 deleted the MCP injectors, what consumes it
/// is `maybe_register_palace_alias` — the #1939 healing that decides whether
/// the derived `owner-repo` palace should resolve to a pre-existing bare-repo
/// one. The flag-less [`prepare_session`] delegates with `None`, which falls
/// back to the workspace's own `git remote get-url origin`.
/// What: identical to [`prepare_session`] except the optional `repo_url` (from
/// `LaunchParams`/`SessionRecord`) is threaded down as the authoritative remote.
/// Real native-style detection is applied.
/// Test: `creates_alias_for_split_brain` and the sibling guards in
/// `palace_alias`, which cover what the threaded remote decides.
pub fn prepare_session_with_repo_url(
    fw: &FrameworkPaths,
    project_dir: &Path,
    repo_url: Option<&str>,
) -> Result<PrepReport, PrepError> {
    prepare_session_with_repo_url_and_exe(fw, project_dir, repo_url, None)
}

/// [`prepare_session_with_repo_url`] with the hook binary pinned by the caller.
///
/// Why (#7244): the project-tier hooks write refuses a build-artifact binary,
/// and a CI runner has no installed `tm` for the PATH fallback — so
/// [`crate::core::deploy_validate::validate_and_repair`]'s test could never
/// close the `HooksMissing` gap it asserts on. Pinning the path from the test
/// makes that assertion about the repair pipeline again rather than about
/// whether the host has `tm` installed.
/// What: identical to [`prepare_session_with_repo_url`] except `hook_exe`
/// reaches the settings writer as its `exe_override`. Production callers pass
/// `None` and keep resolving the running binary exactly as before.
/// Test: `repair_closes_gaps_on_incomplete_workspace`.
pub fn prepare_session_with_repo_url_and_exe(
    fw: &FrameworkPaths,
    project_dir: &Path,
    repo_url: Option<&str>,
    hook_exe: Option<&Path>,
) -> Result<PrepReport, PrepError> {
    // #7763: `None` is "probe the host"; the repair path passes what the launch
    // already resolved.
    prepare_session_for_repair(fw, project_dir, repo_url, hook_exe, None)
}

/// [`prepare_session_with_repo_url_and_exe`] reusing a resolved reachability.
///
/// Why (#7763): the deployment gate re-runs this pipeline as its repair step,
/// AFTER the same launch already probed trusty-memory once. With nothing to
/// thread, the repair probed again, so a launch against a slow or wedged daemon
/// paid [`PROBE_TIMEOUT`](crate::core::memory_reachable::PROBE_TIMEOUT) twice.
/// The value is the only thing the repair needs from the launch, so a dedicated
/// entry point carries it rather than a parameter every other caller would
/// default (#7715).
/// What: identical to [`prepare_session_with_repo_url_and_exe`] except
/// `memory_reachable` replaces the live probe. `None` keeps the probe, which is
/// what a resume — which never prepared — still passes.
/// Test: `repair_reuses_the_reachability_the_launch_resolved`.
pub fn prepare_session_for_repair(
    fw: &FrameworkPaths,
    project_dir: &Path,
    repo_url: Option<&str>,
    hook_exe: Option<&Path>,
    memory_reachable: Option<bool>,
) -> Result<PrepReport, PrepError> {
    let native = crate::core::output_style::claude_supports_native_output_style();
    prepare_session_inner(
        fw,
        project_dir,
        None,
        native,
        repo_url,
        None,
        HostInputs {
            home: dirs::home_dir().as_deref(),
            hook_exe,
            memory_reachable,
        },
    )
}

/// Prepare a session whose managed id is already known (#4832).
///
/// Why: the compiled prompt is now per-SESSION
/// (`.trusty-mpm/sessions/<id>/INSTRUCTIONS-COMPILED.md`), and the two callers
/// that provision a managed session — `inproject::create_session_worktree` and
/// the daemon's in-project `prepare_inproject_session` — hold that id before
/// they call here. Without it, preparation would write into the unmanaged
/// `local` bucket while the spawn (which does know the id) refreshed the real
/// per-session file, leaving a stale copy no writer ever updates again — the
/// exact defect shape #4832 removes.
/// What: [`prepare_session_with_repo_url`] with the managed session id threaded
/// down to [`crate::core::harness_root::session_scope`]. Callers with no
/// session identity keep using the id-less entry points, which resolve the
/// scope from `TM_MANAGED_SESSION_ID` or fall back to
/// [`crate::core::harness_root::UNMANAGED_SESSION_SCOPE`].
/// Test: `prepare_session_for_managed_writes_the_per_session_compiled_prompt`.
pub fn prepare_session_for_managed(
    fw: &FrameworkPaths,
    project_dir: &Path,
    repo_url: Option<&str>,
    session_id: &str,
) -> Result<PrepReport, PrepError> {
    let native = crate::core::output_style::claude_supports_native_output_style();
    prepare_session_inner(
        fw,
        project_dir,
        None,
        native,
        repo_url,
        Some(session_id),
        HostInputs::with_home(dirs::home_dir().as_deref()),
    )
}

/// The deploy layout for a session whose harness is spawned with
/// [`SETTING_SOURCES_FLAG`](crate::core::model_inject::SETTING_SOURCES_FLAG).
///
/// Why (issue #4203): `--setting-sources project,local` makes Claude Code read
/// ONLY the project and local tiers; the `user` tier (`$HOME/.claude`) is
/// excluded deliberately (#1269). Deploying such a session's roster through
/// `FrameworkPaths::default()` therefore writes it somewhere that session will
/// never look — and nothing reports it, because the deploy genuinely succeeds
/// and the load silently finds nothing. Naming the correct layout ONCE, here,
/// is what lets every isolated caller share it instead of each re-deriving it
/// (and three of them getting it wrong independently).
/// What: `FrameworkPaths::for_managed_workspace(project_dir)` — the deploy
/// DESTINATION becomes `<project_dir>/.claude/{agents,skills}`, which the
/// `project` and `local` tiers read, while every framework SOURCE path still
/// resolves from the home-relative install root (#1931).
/// Test: `isolated_layout_deploys_into_a_tier_the_spawn_reads`,
/// `isolated_layout_keeps_framework_source_at_the_install_root`.
pub(crate) fn isolated_framework_paths(project_dir: &Path) -> FrameworkPaths {
    FrameworkPaths::for_managed_workspace(project_dir)
}

/// Prepare a session whose harness will be spawned with
/// [`SETTING_SOURCES_FLAG`](crate::core::model_inject::SETTING_SOURCES_FLAG).
///
/// Why (issue #4203): `tm launch`, `tm connect`, and `tm meta launch` each
/// built their own `FrameworkPaths::default()` and each therefore deployed the
/// agent roster into the one tier their own spawn flag excludes — three
/// independent instances of the same defect, because each call site was free to
/// resolve the layout itself. This entry point REMOVES that degree of freedom
/// rather than checking it after the fact: an isolated caller supplies no
/// `FrameworkPaths` at all, so there is no wrong value left to pass.
/// Callers whose spawn does NOT carry the flag must keep using
/// [`prepare_session`] with their own `fw` — notably `tm session start`, which
/// spawns a bare `claude` (`commands/session/start.rs`) and so genuinely does
/// read the user tier; pointing it here would be a regression, not a fix.
/// What: resolves [`isolated_framework_paths`] for `project_dir` (the cwd the
/// harness is spawned in) and delegates to [`prepare_session_with_repo_url`],
/// which is exactly [`prepare_session`] when `repo_url` is `None`.
/// Test: `isolated_layout_deploys_into_a_tier_the_spawn_reads`;
/// `launch_paths_prepare_through_the_isolated_seam` (tm binary) binds the call
/// sites to it.
pub fn prepare_isolated_session(
    project_dir: &Path,
    repo_url: Option<&str>,
) -> Result<PrepReport, PrepError> {
    let fw = isolated_framework_paths(project_dir);
    prepare_session_with_repo_url(&fw, project_dir, repo_url)
}

/// Prepare a session, selecting an explicit output style (HR-4).
///
/// Why: `tm launch --style <id>` lets the operator override the configured
/// active output style for a single launch; the override must reach the
/// `outputStyle` settings key (for native-capable Claude Code) and the
/// prompt-injection seam (for older builds). The flag-less [`prepare_session`]
/// delegates here with `None`.
/// What: identical to [`prepare_session`] except the active output-style id is
/// resolved via [`crate::core::output_style::resolve_active_style`] with
/// `explicit_style` taking precedence over the `[style] active` config key and
/// the professional default. An unknown id is logged and falls back to the
/// default (DOC-17) rather than failing the launch.
/// Test: `prepare_session_writes_configured_style`,
/// `prepare_session_explicit_style_overrides_config`.
pub fn prepare_session_with_style(
    fw: &FrameworkPaths,
    project_dir: &Path,
    explicit_style: Option<&str>,
) -> Result<PrepReport, PrepError> {
    // Probe the live Claude Code version ONCE and thread the decision through the
    // stash write so the stashed prompt matches what the launcher will inject
    // (issue #1409). Real detection, fail-safe to injection.
    let native = crate::core::output_style::claude_supports_native_output_style();
    prepare_session_with_style_and_native(fw, project_dir, explicit_style, native)
}

/// Prepare a session with the `native_supported` output-style decision supplied
/// explicitly (no live `claude --version` probe).
///
/// Why: the stash (`last-instructions.md`) must equal the launch prompt
/// byte-for-byte, and that prompt depends on whether Claude Code supports native
/// output styles. Probing `claude --version` inside `prepare_session` couples the
/// stash invariant to the host, which broke `prepare_session_stash_reflects_override`
/// on CI (issue #1409). This seam pins the decision so tests can assert the
/// invariant deterministically under BOTH `native_supported = true` and `false`;
/// [`prepare_session_with_style`] supplies real detection in production.
/// What: identical to [`prepare_session_with_style`] except the stash is written
/// from
/// [`build_system_prompt_for_with_style_and_native`](crate::core::session_launch::build_system_prompt_for_with_style_and_native)
/// using the supplied flag,
/// so the stash always reflects the exact injected (or non-injected) launch prompt.
/// Test: `prepare_session_stash_reflects_override`.
pub fn prepare_session_with_style_and_native(
    fw: &FrameworkPaths,
    project_dir: &Path,
    explicit_style: Option<&str>,
    native_supported: bool,
) -> Result<PrepReport, PrepError> {
    prepare_session_with_home(
        fw,
        project_dir,
        explicit_style,
        native_supported,
        dirs::home_dir().as_deref(),
    )
}

/// [`prepare_session_with_style_and_native`] with the USER-GLOBAL home supplied.
///
/// Why (#5544): `prepare_session` writes two files that belong to the user, not
/// to the project — `~/.claude.json` and `~/.claude/settings.json`. A test
/// driving the real pipeline therefore reached the developer's own home, and the
/// only way to stop it was repointing the process's `$HOME` — a global write
/// every sibling test in the same binary observes for its duration, which is the
/// flake class #5544 tracks. The home cannot come off `FrameworkPaths`:
/// `for_managed_project` relocates every `.claude/` path on it onto the
/// workspace, so deriving it there sent both writes into the operator's repo.
/// Passing it is the only shape that is correct under every root AND redirectable.
/// What: identical to [`prepare_session_with_style_and_native`], which is this
/// function with `dirs::home_dir()`. An absent or relative `home` declines the
/// two user-global writes rather than guessing a location.
/// Test: `prepare_session_does_not_seed_the_workspace_on_the_managed_path`,
/// `global_hook_cleanup_reaches_the_real_home_under_an_overridden_root`.
pub fn prepare_session_with_home(
    fw: &FrameworkPaths,
    project_dir: &Path,
    explicit_style: Option<&str>,
    native_supported: bool,
    home: Option<&Path>,
) -> Result<PrepReport, PrepError> {
    prepare_session_inner(
        fw,
        project_dir,
        explicit_style,
        native_supported,
        None,
        None,
        HostInputs::with_home(home),
    )
}

/// [`prepare_session_with_home`] with the trusty-memory verdict pinned (#7685).
///
/// Why: auto memory is a FALLBACK — the project-tier `autoMemoryEnabled: false`
/// key is written only when trusty-memory answered (owner ruling 2026-09-12).
/// Production resolves that by probing the host, which a test cannot point
/// anywhere, so both branches would otherwise be untestable and the one that
/// matters — leaving auto memory ON beside a dead daemon — would ship unproven.
/// What: identical to [`prepare_session_with_home`] except `memory_reachable`
/// replaces the live probe. Production callers keep using the probe-resolving
/// entry points.
/// Test: `prepare_session_disables_auto_memory_when_trusty_memory_is_reachable`,
/// `prepare_session_restores_auto_memory_when_trusty_memory_is_down`.
pub fn prepare_session_with_memory_reachable(
    fw: &FrameworkPaths,
    project_dir: &Path,
    home: Option<&Path>,
    memory_reachable: bool,
) -> Result<PrepReport, PrepError> {
    prepare_session_inner(
        fw,
        project_dir,
        None,
        false,
        None,
        None,
        HostInputs {
            home,
            hook_exe: None,
            memory_reachable: Some(memory_reachable),
        },
    )
}
