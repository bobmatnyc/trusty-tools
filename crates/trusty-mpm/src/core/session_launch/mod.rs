//! Pre-launch preparation for a Claude Code session.
//!
//! Why: every trusty-mpm session is launched as `claude` (the Claude Code CLI),
//! never `claude-mpm`. The "trusty-mpm" behaviour is supplied entirely through
//! the custom instructions Claude Code reads at startup — the deployed agents in
//! `~/.claude/agents/` and the project `CLAUDE.md`. Both the CLI (`tm session
//! start`) and the shared client (`DaemonClient::launch_session`, used by the
//! TUI's `/connect`) must perform the identical preparation; centralizing it
//! here keeps the two launch paths from drifting.
//! What: [`prepare_session`] deploys composed agents to `~/.claude/agents/` and
//! runs the instruction merge pipeline, writing/merging the project `CLAUDE.md`
//! and stashing the merged result under `<project>/.trusty-mpm/`. It returns a
//! [`PrepReport`] describing what happened so callers can report it.
//! Test: `prepare_session_writes_claude_md_and_stash` and
//! `prepare_session_is_idempotent` in this module's tests.

mod custom_mcp;
mod native_mcp;
mod palace_alias;
// #4448: the ONE wiring of the shadowing-agent quarantine, shared by
// `prepare_session_inner` and `sync_session_assets`.
mod project_hooks;
mod quarantine_shadows;
mod search_index;
mod settings;
mod sync_assets;
#[cfg(test)]
mod tests;
mod workstream_label;
// Injector-specific unit tests live in a dedicated `_tests.rs` file (1500-SLOC
// test cap) rather than inline in `native_mcp.rs` (500-SLOC production cap).
#[cfg(test)]
#[path = "native_mcp_tests.rs"]
mod native_mcp_tests;
// Mirrors the `native_mcp_tests.rs` split (issue #2739 follow-up).
#[cfg(test)]
#[path = "custom_mcp_tests.rs"]
mod custom_mcp_tests;
mod worktree_sync;
// Split out of `tests.rs` to keep it under the 1500-SLOC test-file cap
// (issue #2149 roster-deploy-failure-continues coverage) — mirrors the
// `doctor_output_style.rs` / `doctor_fs_checks.rs` split pattern.
#[cfg(test)]
#[path = "tests_roster.rs"]
mod tests_roster;
// Split out of `tests.rs` to keep it under the 1500-SLOC test-file cap
// (issue #3427 scaffolding-gitignore wiring coverage) — mirrors the
// `tests_roster.rs` split pattern above.
#[cfg(test)]
#[path = "tests_scaffold_gitignore.rs"]
mod tests_scaffold_gitignore;
// Split out of `tests.rs` to keep it under the 1500-SLOC test-file cap
// (issue #2914 ephemeral-index-leak regression coverage) — mirrors the
// `tests_roster.rs` split pattern above.
#[cfg(test)]
#[path = "tests_search_index.rs"]
mod tests_search_index;
// Issue #3918 follow-up (code-critic BLOCK, name-squatting exploit) — e2e
// coverage driving the REAL `prepare_session_inner` pipeline end to end,
// mirroring the `tests_roster.rs` split pattern above.
#[cfg(test)]
#[path = "tests_mcp_trust_seed_e2e.rs"]
mod tests_mcp_trust_seed_e2e;
// Issue #3926 — e2e coverage for the `tm launch` MCP-trust-preseed
// name-squatting fix, mirroring the `tests_mcp_trust_seed_e2e` split pattern
// immediately above.
#[cfg(test)]
#[path = "tests_launch_trust_3926.rs"]
mod tests_launch_trust_3926;
// Issue #3934 — e2e coverage for the manifest-toggle name-squatting fix
// (the `[mcp]` toggle disabling trusty-memory/trusty-search's force-overwrite
// injector reopened the #3926/#3918 exploit class), mirroring the
// `tests_launch_trust_3926.rs` split pattern immediately above.
#[cfg(test)]
#[path = "tests_manifest_toggle_trust_3934.rs"]
mod tests_manifest_toggle_trust_3934;
// Issue #4072 — concurrency coverage for the shared `~/.claude.json`
// read-modify-write cycle both trust seeders perform. Its own file because
// `settings.rs` (485/500 production SLOC) and `tests.rs` (1482/1500 test
// SLOC) are both effectively at their caps; mirrors the split pattern above.
#[cfg(test)]
#[path = "tests_claude_json_concurrency_4072.rs"]
mod tests_claude_json_concurrency_4072;
// Issue #4448 — call-site coverage for the shadowing-agent quarantine. Its own
// file for the same reason as the split above, and because these tests exist
// specifically to fail when the MOVE-authorising call site is deleted,
// reordered, or re-aimed at the operator's own `~/.claude/agents`.
#[cfg(test)]
#[path = "tests_quarantine_4448.rs"]
mod tests_quarantine_4448;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::core::agent_deployer::{DeployResult, deploy_agents_filtered, retract_framework_agents};
use crate::core::agent_skill_codeploy::{co_deploy_skill_set, log_declared_skills};
use crate::core::instruction_pipeline::{PipelineInput, PipelineOutput, build_instructions};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deployer::DeployStats;
use crate::core::skill_tiers::{
    deploy_all_skill_tiers, list_project_custom_stems, list_source_stems,
};
use custom_mcp::inject_custom_trusty_mcps;
use native_mcp::inject_native_trusty_mcps;
use settings::{
    deploy_output_style, preseed_workspace_trust_home, remove_global_trusty_memory_hooks,
    write_output_style, write_project_hooks, write_status_line,
};

/// Re-export of the project-tier output-style/statusLine resolution primitives
/// for reuse by `core::standalone::settings_defaults` (issue #2214).
///
/// Why: `mod settings` above is private, so a `pub(crate)` item inside it is
/// still unreachable from outside `session_launch` without a re-export at this
/// (public) module boundary. `core::standalone::settings_defaults::ensure_settings_defaults`
/// seeds `outputStyle`/`statusLine` into the tm-owned `CLAUDE_CONFIG_DIR`
/// settings.json using these exact same values, rather than duplicating the
/// default-id constant or the absolute-binary-path resolution logic.
/// What: re-exports [`settings::OUTPUT_STYLE`],
/// [`settings::resolve_statusline_command`], and
/// [`settings::is_stale_statusline_command`] (so `settings_defaults` can
/// self-heal a stale/ephemeral `statusLine` entry — #2229) under the
/// `session_launch::` path.
/// Test: covered indirectly by
/// `core::standalone::settings_defaults` tests (this is a plain re-export, no
/// logic of its own).
pub(crate) use settings::{OUTPUT_STYLE, is_stale_statusline_command, resolve_statusline_command};

/// Re-export of the four framework-builtin MCP-server force-overwrite
/// injectors, and the trusty-search index derivation they depend on, for
/// reuse by `runtime::claude_code::prepare_managed_config` (issue #3950
/// residual-gap fix).
///
/// Why: `mod settings`/`mod search_index` above are private, so a
/// `pub(crate)` item inside either is still unreachable from outside
/// `session_launch` without a re-export at this (public) module boundary —
/// mirroring the `OUTPUT_STYLE` re-export just above.
/// `runtime::claude_code::prepare_managed_config` (the tmux daemon-spawn
/// adapter behind `RuntimeAdapter::spawn`/`spawn_resume`) is a genuinely
/// disjoint call chain from wherever `prepare_session_with_repo_url` last
/// ran the injectors for a given workspace — `spawn_resume` in particular
/// reaches it with NO corresponding `prepare_session*` call anywhere in its
/// own request chain (see `daemon::managed_routes::lifecycle::resume_managed`'s
/// doc). Before #3950's follow-up fix, that call site derived
/// `enabledMcpjsonServers` membership from a hardcoded `true` (the two
/// unconditional builtins) or a bare manifest-toggle read (the two
/// conditional builtins) — exactly the "toggle/attempt ≠ write success"
/// defect this issue closes everywhere else, left open on the headless
/// resume path with no same-run evidence at all. Exposing the injectors
/// themselves (rather than only the higher-level `prepare_session_inner`
/// pipeline, which also redeploys agents/skills/CLAUDE.md — unwanted,
/// heavier work for a mere resume) lets that call site run the SAME
/// idempotent read-merge-write ([`settings::inject_mcp_server`]'s own doc:
/// "skip the write when the entry already matches") and derive real,
/// this-run pin results exactly like `session_launch::prepare_session_inner`
/// and `standalone::load::load_alias` do.
/// What: re-exports [`settings::inject_trusty_mpm_mcp`],
/// [`settings::inject_trusty_review_mcp`], [`settings::inject_trusty_memory_mcp`],
/// [`search_index::inject_trusty_search_mcp`], and
/// [`search_index::register_project_index`] (the find-or-create index
/// lookup `inject_trusty_search_mcp`'s `index_id` pin depends on).
/// Test: covered by each injector's own unit tests in `tests_mcp_trust_seed_e2e.rs`
/// / `native_mcp_tests.rs` / `tests.rs` (this is a plain re-export, no logic
/// of its own); the new call site is covered by
/// `runtime::claude_code::tests::prepare_managed_config_excludes_builtins_when_mcp_json_write_fails`.
pub(crate) use search_index::{inject_trusty_search_mcp, register_project_index};
pub(crate) use settings::{
    inject_trusty_memory_mcp, inject_trusty_mpm_mcp, inject_trusty_review_mcp,
};

/// Re-export of the resume-time worktree/upstream sync primitives (issue
/// #2647) for reuse by `daemon::managed_routes::lifecycle::resume_managed`.
///
/// Why: `mod worktree_sync` above is private; this re-export is the public
/// boundary the resume path calls through, mirroring the `settings`
/// re-export just above.
/// What: re-exports [`worktree_sync::resume_self_heal`] — the single
/// call-site entry point wrapping [`worktree_sync::sync_worktree_with_upstream`]
/// and [`worktree_sync::self_heal_claude_md`] with logging.
/// Test: covered by `worktree_sync`'s own unit tests (this is a plain
/// re-export, no logic of its own).
pub use worktree_sync::resume_self_heal;

/// Re-export of the asset re-sync entry point (issue #2444) for the daemon's
/// `sync-assets` route and `tm doctor`/`tm sessions ls` staleness surfaces.
///
/// Why: `mod sync_assets` above is private; this is the public boundary
/// external callers go through, mirroring the `worktree_sync` re-export above.
/// What: re-exports [`sync_assets::sync_session_assets`],
/// [`sync_assets::SyncAssetsError`], and [`sync_assets::SyncAssetsReport`].
/// Test: covered by `sync_assets`'s own unit tests (this is a plain
/// re-export, no logic of its own).
pub use sync_assets::{SyncAssetsError, SyncAssetsReport, sync_session_assets};

/// Re-export of the launch-time `ws/<session-name>` workstream-label ensure
/// (issue #3726) for the daemon's `spawn_managed_*` call sites.
///
/// Why: `mod workstream_label` above is private; this is the public boundary
/// the spawn paths call through, mirroring the `sync_assets`/`worktree_sync`
/// re-exports above.
/// What: re-exports [`workstream_label::ensure_workstream_label`], its
/// [`workstream_label::LabelOutcome`] result type, and the detached
/// [`workstream_label::spawn_workstream_label_ensure`] wrapper the daemon
/// spawn paths actually call (fire-and-forget, never on the launch critical
/// path).
/// Test: covered by `workstream_label`'s own unit tests (this is a plain
/// re-export, no logic of its own).
pub use workstream_label::{LabelOutcome, ensure_workstream_label, spawn_workstream_label_ensure};

/// Outcome of the pre-launch preparation for one session.
///
/// Why: callers (CLI, client) report agent-deploy counts and CLAUDE.md status
/// to the operator; bundling them avoids returning a loose tuple.
/// What: the agent [`DeployResult`], the instruction [`PipelineOutput`], and the
/// path the merged instructions were stashed to.
/// Test: asserted by `prepare_session_writes_claude_md_and_stash`.
#[derive(Debug)]
pub struct PrepReport {
    /// Result of deploying composed agents to `~/.claude/agents/`.
    pub deploy: DeployResult,
    /// Result of deploying skill files to `~/.claude/skills/`.
    pub skill_deploy: DeployStats,
    /// Result of the instruction merge pipeline.
    pub instructions: PipelineOutput,
    /// Path the merged instructions were stashed to for inspection.
    pub stash: PathBuf,
    /// Path the PROJECT-LOCAL compiled prompt was written to (#4752).
    ///
    /// Why: exposing it makes the launch-ordering guarantee assertable — a
    /// returned `PrepReport` means this file exists and holds the exact text
    /// the session is about to receive, because the write is fatal and happens
    /// before this value is constructed.
    /// What: `<project_dir>/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md`, as
    /// resolved by [`crate::core::instruction_pipeline::compiled_prompt_path`].
    /// NOT the global `~/.trusty-mpm/framework/` path — that shared location was
    /// the collision #4752 removed, since every project would have overwritten
    /// the same file.
    ///
    /// It holds the same text as [`Self::stash`] AT RETURN, but is not
    /// permanently byte-identical to it, so do not treat the two as
    /// interchangeable: the stash write degrades to a warning (so `stash` may be
    /// absent or stale), and for managed spawns
    /// `runtime::claude_code::build_prompt_file` later refreshes this file with a
    /// `None`-style composition that can differ from the `effective_style` text
    /// written here.
    /// Test: `prepare_session_writes_the_compiled_prompt_before_returning`,
    /// `prepare_session_fails_when_the_compiled_prompt_cannot_be_written`.
    pub compiled_prompt: PathBuf,
    /// Path the `trusty-mpm` output style was deployed to, if it succeeded.
    ///
    /// `None` when the deploy write failed; the session still launches in
    /// that case, just with the operator's default style.
    pub output_style: Option<PathBuf>,
    /// Whether the `trusty-memory` hook block was written to the project's
    /// `.claude/settings.json`.
    ///
    /// `false` when writing the project hooks failed; the session still
    /// launches, it just won't fire the trusty-memory hooks.
    pub hooks_written: bool,
    /// Whether the `trusty-mpm` MCP server entry was ACTUALLY force-written
    /// to `.mcp.json` this run (issue #3950 — fifth instance of the
    /// name-approval/content-pinning vulnerability class).
    ///
    /// `false` when [`inject_trusty_mpm_mcp`] failed (disk full, permission
    /// error, transient I/O fault) — the session still launches, but this
    /// name must NOT enter `enabledMcpjsonServers` for it (see
    /// `trusted_mcp_names`'s computation below), since its content was never
    /// re-pinned to the framework-controlled command this run.
    /// Test: `prepare_session_excludes_all_builtins_from_trust_when_mcp_json_write_fails`.
    pub trusty_mpm_injected: bool,
    /// Whether the `trusty-review` MCP server entry was ACTUALLY
    /// force-written to `.mcp.json` this run — see [`Self::trusty_mpm_injected`].
    /// Test: `prepare_session_excludes_all_builtins_from_trust_when_mcp_json_write_fails`.
    pub trusty_review_injected: bool,
    /// Whether the `trusty-memory` MCP server entry was ACTUALLY
    /// force-written to `.mcp.json` this run.
    ///
    /// `false` when the manifest toggle disabled the injector, OR the toggle
    /// was on but [`inject_trusty_memory_mcp`] failed — either way this name
    /// must NOT enter `enabledMcpjsonServers` (issue #3950).
    /// Test: `prepare_session_excludes_all_builtins_from_trust_when_mcp_json_write_fails`.
    pub trusty_memory_injected: bool,
    /// Whether the `trusty-search` MCP server entry was ACTUALLY
    /// force-written to `.mcp.json` this run — see
    /// [`Self::trusty_memory_injected`].
    /// Test: `prepare_session_excludes_all_builtins_from_trust_when_mcp_json_write_fails`.
    pub trusty_search_injected: bool,
    /// Incremental catch-up context to inject as seed context for this session.
    ///
    /// Populated when `config.catchup.auto` is true; `None` otherwise or when
    /// the catch-up runtime fails (fail-open — never blocks session launch).
    /// The caller (session start path) should print this after the normal
    /// launch summary so the operator sees it in the terminal.
    ///
    // CUTOVER BRIDGE — remove post-migration (#1762)
    pub catchup_context: Option<String>,
    /// Non-fatal errors raised while deploying the agent/skill roster.
    ///
    /// Why (issue #2149): a roster-deploy failure (e.g. a corrupt agent
    /// manifest) must NEVER abort the rest of preparation — the identity
    /// carrier (the trusty-mpm output style + `outputStyle` settings key)
    /// still has to be written so the session self-identifies as trusty-mpm
    /// even with an empty/broken agent roster. This field is how that failure
    /// is surfaced instead of being silently swallowed: one formatted message
    /// per failed stage (`"agent deploy failed: …"` / `"skill deploy failed:
    /// …"`). Empty when both deploys succeeded.
    /// What: callers (CLI, daemon provisioner) MUST log this at error level
    /// and MAY surface it in `tm doctor` / the operator-facing launch summary.
    /// Test: `prepare_session_continues_after_agent_deploy_failure`,
    /// `prepare_session_continues_after_skill_deploy_failure`.
    pub roster_errors: Vec<String>,
}

/// A failure raised while preparing a session for launch.
///
/// Why: preparation performs agent deployment and filesystem I/O; callers need
/// a single typed error surface that names which stage failed.
/// What: variants for the agent-deploy stage and the instruction stage.
/// Test: not exercised by the happy-path tests; surfaced on invalid paths.
#[derive(Debug, thiserror::Error)]
pub enum PrepError {
    /// Deploying composed agents to `~/.claude/agents/` failed.
    #[error("agent deploy failed: {0}")]
    Deploy(String),
    /// Deploying skill files to `~/.claude/skills/` failed.
    #[error("skill deploy failed: {0}")]
    SkillDeploy(String),
    /// A filesystem operation on the inspection stash failed.
    #[error("io error for {path}: {source}")]
    Io {
        /// The path the failed operation targeted.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// The session's instructions could not be established — the ONE fatal
    /// preparation condition (#4752, owner ruling 2026-08-04).
    ///
    /// Covers BOTH instruction failures, because they are the same condition
    /// reaching two sites, not two error classes:
    ///   * [`build_instructions`] failing to compose or write the merged
    ///     instructions, and
    ///   * the compiled prompt write failing.
    ///
    /// Why it is its own variant rather than another [`Self::Io`]: #2149
    /// deliberately made preparation failures non-fatal so a roster- or
    /// skill-deploy hiccup could not stop a session launching, and every
    /// production caller still logs-and-continues on those. This one is ruled
    /// fatal — the session depends on its instructions, so a session that
    /// cannot get them must not start. [`PrepError::is_fatal`] is that
    /// discriminator; a blanket "all prep errors are fatal" would have reversed
    /// #2149 wholesale and regressed exactly what it protected.
    ///
    /// Renamed from `CompiledPrompt` (round 4): the variant no longer describes
    /// only the compiled-prompt write.
    /// Test: `instruction_failure_is_fatal`,
    /// `deploy_and_io_failures_stay_non_fatal`,
    /// `prepare_session_refuses_when_the_instructions_cannot_be_built`.
    #[error(
        "{}",
        crate::core::instruction_pipeline::instructions_failure_message(path, source)
    )]
    Instructions {
        /// The instruction path the failed operation targeted.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
}

impl PrepError {
    /// Whether this failure must abort the launch rather than be logged.
    ///
    /// Why (#4752): the seven spawning call sites treat preparation as
    /// best-effort (#2149). Exactly one CONDITION is ruled fatal — the session's
    /// instructions could not be established — and this is the single place that
    /// decides it, so a future variant does not silently inherit either policy.
    /// What: `true` only for [`Self::Instructions`], whichever of the two
    /// instruction sites raised it.
    /// Test: `instruction_failure_is_fatal`,
    /// `deploy_and_io_failures_stay_non_fatal`.
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Instructions { .. })
    }
}

/// Prepare a project directory for a fresh Claude Code session launch.
///
/// Why: launching `claude` is only correct if its custom instructions are in
/// place first — the composed agents must be deployed and the project
/// `CLAUDE.md` merged. This is the "custom instructions" step that makes a plain
/// `claude` process behave as a trusty-mpm session; both the CLI and the client
/// call this before sending `claude` into the tmux pane.
/// What: deploys composed agents from the framework agent source to
/// `~/.claude/agents/`, runs [`build_instructions`] for `project_dir` (which
/// loads or creates the project `CLAUDE.md`), writes the launch prompt — the
/// exact override-resolved AND output-style-injected text produced by
/// [`build_system_prompt_for_with_style`] — to
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
///   * [`build_instructions`] composes the merged instructions; and
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
/// `prepare_session_refuses_when_the_instructions_cannot_be_built`,
/// `compiled_write_failure_does_not_skip_the_mcp_injectors`.
pub fn prepare_session(fw: &FrameworkPaths, project_dir: &Path) -> Result<PrepReport, PrepError> {
    prepare_session_with_style(fw, project_dir, None)
}

/// Prepare a session, threading the cloned-from `repo_url` for palace pinning.
///
/// Why (issue #1605): a managed session cloned from `repo_url` lives under a
/// throwaway `<owner>/<repo>/<session-id>/` workspace whose basename is the
/// session-id. Without the originating `repo_url`, the trusty-memory MCP
/// injection would derive (and pin) the WRONG palace from that directory
/// basename. The provisioner knows the `repo_url` it cloned, so threading it
/// here lets the injector pin `env.TRUSTY_MEMORY_PALACE` to the project's
/// canonical `owner-repo` slug. The flag-less [`prepare_session`] delegates with
/// `None`, which falls back to the workspace's own `git remote get-url origin`.
/// What: identical to [`prepare_session`] except the optional `repo_url` (from
/// `LaunchParams`/`SessionRecord`) is threaded down to the trusty-memory MCP
/// injector for palace-slug derivation. Real native-style detection is applied.
/// Test: `prepare_session_repo_url_pins_palace` in this module's tests.
pub fn prepare_session_with_repo_url(
    fw: &FrameworkPaths,
    project_dir: &Path,
    repo_url: Option<&str>,
) -> Result<PrepReport, PrepError> {
    let native = crate::core::output_style::claude_supports_native_output_style();
    prepare_session_inner(fw, project_dir, None, native, repo_url, None)
}

/// Prepare a session whose managed id is already known (#4832).
///
/// Why: the compiled prompt is now per-SESSION
/// (`.trusty-mpm/sessions/<id>/INSTRUCTIONS-COMPILED.md`), and the two callers
/// that provision a managed session — `WorkspaceProvisioner::provision_in` and
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
    prepare_session_inner(fw, project_dir, None, native, repo_url, Some(session_id))
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

/// Defensively (re)write the `tm statusline` config for an EXISTING session
/// workspace, without re-running the full preparation pipeline.
///
/// Why (issue #1913): sessions spawned via the pre-fix in-project worktree path
/// never ran [`prepare_session_with_repo_url`] at all, so their on-disk
/// `.claude/settings.json` may be permanently missing the `statusLine` key, and
/// nothing else in the launch path ever backfills it. Re-running the FULL prep
/// pipeline (agent/skill redeploy, CLAUDE.md merge, MCP injection) on every
/// resume is riskier than necessary here — those steps are not all confirmed
/// idempotent under a resumed (not freshly-provisioned) workspace — so this
/// exposes ONLY the one step `write_status_line` itself documents as safe to
/// call unconditionally (it never clobbers a genuine user customization). The
/// resume path calls this defensively so a session stuck in the pre-#1913
/// broken state self-heals the next time it is resumed, without the broader
/// blast radius of a full re-prep. As of #1914, the same call ALSO upgrades a
/// stale bare `tm`/`trusty-mpm statusline` command (the pre-#1914 default,
/// which silently fails to render under a minimal `PATH`) to the resolved
/// absolute path — the two self-heal concerns share this one entry point
/// rather than growing a second, duplicate resume hook.
/// What: thin `pub` wrapper over `settings::write_status_line` (`pub(super)`,
/// so not directly reachable from `crate::daemon::managed_routes`). Delegates
/// verbatim — no additional logic.
/// Test: the underlying idempotency and path-resolution guarantees are covered
/// by `write_status_line_injects_when_absent` / `write_status_line_skips_when_already_set`
/// / `write_status_line_preserves_user_config` / `write_status_line_heals_stale_tm_default`
/// / `write_status_line_heals_stale_trusty_mpm_default` in this module's test
/// file; `resume_managed_backfills_missing_status_line` in
/// `tests/session_manager_mvp.rs` covers the `resume_managed` call site.
pub fn ensure_status_line(project_dir: &Path) -> Result<(), PrepError> {
    settings::write_status_line(project_dir)
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
/// from [`build_system_prompt_for_with_style_and_native`] using the supplied flag,
/// so the stash always reflects the exact injected (or non-injected) launch prompt.
/// Test: `prepare_session_stash_reflects_override`.
pub fn prepare_session_with_style_and_native(
    fw: &FrameworkPaths,
    project_dir: &Path,
    explicit_style: Option<&str>,
    native_supported: bool,
) -> Result<PrepReport, PrepError> {
    prepare_session_inner(
        fw,
        project_dir,
        explicit_style,
        native_supported,
        None,
        None,
    )
}

/// Shared body for every `prepare_session*` entry point.
///
/// Why: the public entry points differ only in how they resolve `explicit_style`,
/// `native_supported`, and (issue #1605) the cloned-from `repo_url`. Funnelling
/// them through one private body keeps the long preparation sequence in a single
/// place so the variants cannot drift. `repo_url` is the only new degree of
/// freedom: it is threaded to the trusty-memory MCP injector for palace pinning
/// and is `None` for the flag-less / style-only entry points (which then fall
/// back to the workspace's own git origin remote).
/// What: identical to the documented [`prepare_session_with_style_and_native`]
/// behaviour, plus it passes `repo_url` to [`inject_trusty_memory_mcp`].
/// Issue #2149: an agent-deploy or skill-deploy failure is captured into
/// [`PrepReport::roster_errors`] rather than short-circuiting the function via
/// `?` — every step after the roster deploy (CLAUDE.md/instructions, the
/// trusty-mpm output-style write, MCP injection, hooks) still runs
/// unconditionally, so a session ALWAYS launches carrying its trusty-mpm
/// identity even when the roster itself is empty or broken. This function now
/// only returns `Err` for genuinely fatal failures (the instruction pipeline,
/// or the inspection-stash IO).
/// Test: covered by every `prepare_session_*` test plus
/// `prepare_session_repo_url_pins_palace`,
/// `prepare_session_continues_after_agent_deploy_failure`,
/// `prepare_session_continues_after_skill_deploy_failure`.
fn prepare_session_inner(
    fw: &FrameworkPaths,
    project_dir: &Path,
    explicit_style: Option<&str>,
    native_supported: bool,
    repo_url: Option<&str>,
    session_id: Option<&str>,
) -> Result<PrepReport, PrepError> {
    // Load the user config ONCE and thread it through both the manifest
    // resolution / catalog-root path AND the style resolution path below. Reading
    // `config.toml` a second time mid-function (the old `MpmConfig::load` just
    // before style resolution) was a redundant filesystem read for the same data.
    let config = crate::core::config::MpmConfig::load(&fw.root);

    // Resolve the effective harness manifest (HR-2 / DOC-17) and materialize the
    // provisioning plan it implies. The NORMATIVE precedence is
    // project override > user config > catalog manifest > compiled-in default;
    // the compiled-in default reproduces today's provisioning exactly, so an
    // absent manifest is a zero-regression no-op. The plan tells us WHICH agents
    // and skills to deploy, from WHICH source (bundled vs synced catalog), which
    // MCP servers to inject, and the manifest's default output style. The
    // existing deploy machinery still does the deployment.
    //
    // The catalog root comes from the ONE shared helper (`catalog_root_for`) the
    // `tm catalog` CLI also uses, so the manifest path the resolver reads is the
    // same `<framework>/catalog` checkout `CatalogSync` populates — honouring the
    // `[manifest]` config / `TRUSTY_MPM_CATALOG_*` env catalog-source overrides.
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    let manifest_sources =
        crate::core::manifest::ManifestSources::resolve(project_dir, &catalog_root);
    let manifest = crate::core::manifest::resolve_manifest(&manifest_sources);
    let plan = crate::core::manifest::HarnessPlan::from_manifest(&manifest, fw, &catalog_root);

    // Non-fatal roster-deploy errors accumulate here (issue #2149) instead of
    // aborting the function early — every step below this point (output-style
    // write, MCP injection, hooks) MUST still run so a session always carries
    // its trusty-mpm identity even when the agent/skill roster fails to
    // deploy. Errors are surfaced loudly via `tracing::error!` at the point of
    // failure AND collected here so callers (the daemon provisioner, `tm
    // doctor`) can report the gap instead of it being silently swallowed by a
    // best-effort `warn` at the call site.
    let mut roster_errors: Vec<String> = Vec::new();

    // Deploy composed agents into the tm-managed `CLAUDE_CONFIG_DIR` tier
    // (`fw.agent_deploy_dir()`), which a managed session's harness reads at
    // startup and re-scans mid-session. Issue #4409: this used to target the
    // workspace's own `.claude/agents/`, giving every project a mutable copy of
    // the bundled roster that outranked — and silently shadowed — the canonical
    // one; the workspace tier is retracted just below. The manifest's agent-set
    // selection (include/exclude) still restricts WHICH source agents deploy;
    // the default manifest selects all of them. Announce the stage BEFORE the
    // step so a slow deploy is visibly "in flight" rather than only surfacing
    // after the fact (issue #1904); a no-op outside a daemon `spawn_managed`
    // scope (see `provisioning_stage`).
    crate::core::provisioning_stage::emit(
        crate::core::provisioning_stage::ProvisioningStage::DeployingAgents,
    );
    let deploy = match deploy_agents_filtered(&plan.agent_source, &fw.agent_deploy_dir(), |name| {
        plan.agent_selected(name)
    }) {
        Ok(result) => result,
        Err(err) => {
            // LOUD: an empty agent roster means the launched session has
            // nothing to delegate to. This must never be a quiet `warn` — it
            // is the exact failure mode that shipped issue #2149 (a session
            // with no roster AND no trusty-mpm identity).
            tracing::error!(
                project_dir = %project_dir.display(),
                "agent deploy FAILED — session will launch WITHOUT the tm/mpm agent \
                 roster: {err}. Identity/output-style provisioning continues regardless."
            );
            roster_errors.push(format!("agent deploy failed: {err}"));
            DeployResult::default()
        }
    };

    // Issue #4409, the other half of the flip: retract the bundled agents an
    // OLDER binary deployed into this workspace's `.claude/agents/`. The
    // project tier outranks the config-dir tier in the harness's agent
    // resolution, so a stale copy left behind would shadow the canonical roster
    // forever — and nothing refreshes it any more. Only manifest-tracked,
    // framework-owned files are removed; hand-placed and user-owned files are
    // untouched. Non-fatal, like every other roster step here.
    //
    // The target is `project_dir`'s OWN `.claude/agents`, spelled out rather
    // than taken from `fw.claude_agents_dir()`. Those are the same directory
    // for a managed `fw` (that is exactly what `for_managed_workspace` sets),
    // but `fw` is a home-tier `FrameworkPaths::default()` on the non-git
    // `tm session start` and TUI `/connect` paths — where
    // `fw.claude_agents_dir()` is the operator's `~/.claude/agents`, and
    // retracting it would delete the roster out of a Claude Code install that
    // has nothing to do with trusty-mpm. Retraction is a WORKSPACE operation;
    // binding it to the workspace path makes that structural.
    // Test: `prepare_session_never_retracts_the_operator_home_agents_tier`.
    match retract_framework_agents(&project_dir.join(".claude").join("agents")) {
        Ok(retracted) if !retracted.removed.is_empty() => {
            tracing::info!(
                project_dir = %project_dir.display(),
                count = retracted.removed.len(),
                "retracted per-workspace bundled agents; the roster now lives in the \
                 tm-managed config dir (issue #4409)"
            );
        }
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(
                project_dir = %project_dir.display(),
                "workspace bundled-agent retraction failed: {err}. The stale project-tier \
                 copies may shadow the canonical roster until this is resolved."
            );
            roster_errors.push(format!("agent retraction failed: {err}"));
        }
    }

    // #4448: retraction's blind spot. It can only delete what its ownership
    // ledger names, so a copy written before that ledger existed survives it,
    // outranks the canonical user tier, and is never refreshed again. This
    // sweep reaches those — but only when all four gates agree the file is tm's
    // (see `trusty_agents_common::agents::quarantine`): it never touches a
    // git-tracked file, a claude-mpm artifact, an operator-owned ledger entry,
    // or anything hand-authored, and it never deletes. Runs AFTER retraction:
    // both target the same directory and retraction is the ledger-proven half.
    // Non-fatal, like every other roster step here.
    // Test: `prepare_session_quarantines_a_shadowing_workspace_agent`,
    // `prepare_session_never_quarantines_the_operator_home_agents_tier`.
    match quarantine_shadows::quarantine_workspace_shadows(fw, project_dir) {
        Ok(report) => {
            if let Some(summary) = quarantine_shadows::summarize(&report) {
                tracing::warn!(project_dir = %project_dir.display(), "{summary}");
            }
            if !report.failed.is_empty() {
                roster_errors.push(format!(
                    "agent quarantine could not move {} shadowing file(s)",
                    report.failed.len()
                ));
            }
        }
        Err(err) => {
            tracing::warn!(
                project_dir = %project_dir.display(),
                "shadowing-agent quarantine refused to run: {err}. Untracked project-tier \
                 copies may still shadow the canonical roster."
            );
            roster_errors.push(format!("agent quarantine refused: {err}"));
        }
    }

    // Self-heal the skill *source* directory before deploying from it
    // (#1917): `plan.skill_source` falls back to `fw.skill_source_dir()`,
    // which was previously populated ONLY by a separate, explicit
    // `tm install` run — a stale or missing directory (e.g. after the
    // mpm-*→tm-* rename, #1905, or on a machine that never ran `tm install`
    // under the current binary) made `deploy_skills_filtered` below silently
    // deploy zero skills, with no error surfaced anywhere. Refreshing here
    // removes that dependency on a prior manual install; it is a no-op when
    // the `agents/skills` git submodule is in play (see
    // `skill_source::ensure_skill_source_fresh`). Non-fatal: a refresh
    // failure falls back to whatever is already on disk, matching pre-#1917
    // behaviour rather than blocking the session.
    if let Err(err) = crate::core::skill_source::ensure_skill_source_fresh(fw) {
        tracing::warn!("failed to refresh skill source directory: {err}");
    }

    // Surface pre-deploy skill staleness (issue #2876). Reading the deployed
    // manifest BEFORE the refresh below tells us which bundled skills this
    // workspace was serving STALE — a long-lived worktree that never
    // re-provisioned keeps the old skill text (e.g. an outdated attribution
    // footer) until this very deploy self-heals it. The deploy below fixes it;
    // the warn makes the drift auditable so an operator can see a workspace was
    // running behind the installed binary's assets. Non-fatal and read-only.
    let stale =
        crate::core::skill_staleness::stale_skills(&plan.skill_source, &fw.claude_skills_dir());
    if !stale.is_empty() {
        tracing::warn!(
            project_dir = %project_dir.display(),
            stale_skills = %stale.join(", "),
            "deployed skills were stale relative to bundled assets — refreshing now \
             (run `tm doctor` to audit; long-lived worktrees drift until re-provisioned)"
        );
    }

    // DOC-42 (issue #2889): fold every deployed agent's declared `skills:`
    // into the bundled-tier `select` predicate below, so a skill the harness
    // manifest would otherwise exclude still deploys when an agent depends on
    // it (§SPEC-AGENTSKILLS-02 co-deploy guarantee). Resolving the stem sets
    // ONCE here (rather than inside `deploy_all_skill_tiers`) lets the same
    // sets drive both the co-deploy `select` override AND the resolution
    // logging below, without a second directory scan.
    let bundled_stems = list_source_stems(&plan.skill_source).unwrap_or_default();
    let user_stems = list_source_stems(&fw.user_skill_source_dir()).unwrap_or_default();
    let project_stems = list_project_custom_stems(&fw.claude_skills_dir()).unwrap_or_default();
    let co_deploy_skills = co_deploy_skill_set(&deploy.declared_skills);
    log_declared_skills(
        &deploy.declared_skills,
        &project_stems,
        &user_stems,
        &bundled_stems,
    );

    // Deploy skill files — Claude Code reads `~/.claude/skills/` at startup.
    // Skills carry no inheritance, so this is a manifest-tracked content copy.
    // Three tiers merge here with precedence project-custom > user-custom >
    // bundled (#2816): the manifest's skill-set selection restricts WHICH
    // BUNDLED source skills deploy — OR'd with `co_deploy_skills` (DOC-42) so
    // an agent-declared dependency still deploys even when the manifest
    // wouldn't otherwise select it; the user-custom tier
    // (`~/.trusty-mpm/skills/`) is deployed in full and overrides a same-named
    // bundled skill; a skill the user hand-placed in the project's
    // `.claude/skills/` (absent from the deploy manifest) outranks both and is
    // never overwritten. See `core::skill_tiers`.
    crate::core::provisioning_stage::emit(
        crate::core::provisioning_stage::ProvisioningStage::DeployingSkills,
    );
    let skill_deploy = match deploy_all_skill_tiers(
        &plan.skill_source,
        &fw.user_skill_source_dir(),
        &fw.claude_skills_dir(),
        |name| plan.skill_selected(name) || co_deploy_skills.contains(name),
    ) {
        Ok(result) => result.stats,
        Err(err) => {
            tracing::error!(
                project_dir = %project_dir.display(),
                "skill deploy FAILED — session will launch WITHOUT the tm/mpm skill \
                 set: {err}. Identity/output-style provisioning continues regardless."
            );
            roster_errors.push(format!("skill deploy failed: {err}"));
            DeployStats::default()
        }
    };

    // Compose the effective launch instructions (framework + delegation
    // authority + project CLAUDE.md); this loads or creates the project
    // CLAUDE.md so Claude Code picks it up automatically.
    crate::core::provisioning_stage::emit(
        crate::core::provisioning_stage::ProvisioningStage::BuildingInstructions,
    );
    let input = PipelineInput {
        // #4588: the roster is resolved from the project by the one shared
        // resolver (project tier + the tm-managed `CLAUDE_CONFIG_DIR` tier
        // #4409 deploys into + the operator's generic `~/.claude/agents`), so
        // the count printed at session start is the roster the PM receives.
        project_dir: project_dir.to_path_buf(),
        claude_md_path: project_dir.join("CLAUDE.md"),
    };
    // #4752 (owner ruling, round 4): a session DEPENDS on its instructions, so
    // failing to build them refuses the launch. This used to return the
    // non-fatal `PrepError::Instructions(PipelineError)`, which every caller
    // logged-and-continued past — starting a session whose instructions were
    // never established, the one case the ordering contract could not promise.
    // It is the SAME fatal condition as the compiled write below, not a second
    // error class, so it maps onto the same variant.
    let instructions = build_instructions(&input).map_err(|e| match e {
        crate::core::instruction_pipeline::PipelineError::Io { path, source } => {
            PrepError::Instructions { path, source }
        }
    })?;

    // Resolve the EFFECTIVE output style, folding the manifest's default in as
    // the lowest precedence below the existing HR-4 sources. Precedence:
    // explicit `--style` flag > `[style] active` config key > manifest `[style]
    // active` > professional default. We compute this as a single `Option<&str>`
    // and pass it as the `explicit` argument to both the prompt-injection seam
    // and the settings.json resolver — when neither the flag nor the config sets
    // a style, the manifest's value applies; otherwise the higher source wins
    // exactly as before (zero regression for the flag/config paths). `config` was
    // loaded ONCE at the top of this function.
    let effective_style: Option<String> = explicit_style
        .map(str::to_owned)
        .or_else(|| config.style.active.clone())
        .or_else(|| plan.style.clone());

    // Stash the EXACT text the launch path passes to
    // `claude --append-system-prompt-file` — including the HR-4 output-style
    // injection — so `tm session instructions` shows what was actually used,
    // including any project-level overrides under `<project>/.trusty-mpm/`.
    //
    // This MUST go through the same `build_system_prompt_for_with_style` seam the
    // launcher uses (issue #1409): that seam resolves the override-layered prompt
    // via `resolve_pm_prompt` AND applies the output-style version-fallback
    // injection. Writing the bare `resolve_pm_prompt` text here (pre-injection)
    // while the launcher injects later made the stash diverge from reality
    // whenever `claude` was absent/old (native unsupported → injection fires),
    // breaking the stash/launch invariant in a host-dependent way. Routing both
    // through the single seam keeps them identical regardless of Claude version
    // (issue #381 / the #382 concern).
    let resolved_prompt = build_system_prompt_for_with_style_and_native(
        project_dir,
        effective_style.as_deref(),
        native_supported,
    );
    // #4752: these two writes DEGRADE TO A WARNING; they must never short-circuit
    // this function. An unwritable `.trusty-mpm/` (disk full, bad perms) used to
    // return `PrepError::Io` HERE, and because `Io` is non-fatal every caller
    // launched the session anyway — having skipped the fatal compiled write
    // below, so the session ran with instructions that were never recorded. That
    // is exactly what the ordering contract forbids.
    //
    // The stash is an inspection copy (`tm session instructions`), not the text
    // the session runs on, so losing it is not itself a reason to refuse a
    // launch. What matters is that it cannot take the fatal write down with it.
    // See the ordering contract on `prepare_session`.
    // Test: `stash_write_failure_does_not_skip_the_fatal_instruction_write`.
    // #4832: the harness ROOT, not `project_dir` — a worktree must never grow
    // its own `.trusty-mpm/`.
    let stash_dir = crate::core::harness_root::harness_dir(project_dir);
    let stash = stash_dir.join("last-instructions.md");
    match std::fs::create_dir_all(&stash_dir)
        .and_then(|()| std::fs::write(&stash, &resolved_prompt))
    {
        Ok(()) => {}
        Err(e) => tracing::warn!(
            "could not refresh the instruction stash at {} (non-fatal): {e}",
            stash.display()
        ),
    }

    // Resolve the active output style for settings.json using the same
    // EFFECTIVE style computed above (HR-4 sources + the HR-2 manifest default).
    // An unknown id is logged and falls back to the professional default rather
    // than failing the launch (DOC-17). The resolved id is written into
    // `.claude/settings.json` so a native-capable Claude Code (>= 1.0.83) applies
    // it directly; older builds pick it up via prompt injection at the
    // `build_system_prompt_for` seam.
    let active_style_id = match crate::core::output_style::resolve_active_style(
        &config,
        effective_style.as_deref(),
    ) {
        Ok(style) => style.id,
        Err(err) => {
            tracing::warn!(
                %err,
                "falling back to the default output style for settings.json"
            );
            crate::core::bundle::DEFAULT_OUTPUT_STYLE_ID
        }
    };

    // Set the Claude Code output style so the launched session's status bar
    // reads `style:<active_style_id>`. A failure here is non-fatal: the session
    // still launches, it just shows the operator's default style.
    if let Err(err) = write_output_style(project_dir, Some(active_style_id)) {
        tracing::warn!("failed to set trusty-mpm output style: {err}");
    }

    // Inject `tm statusline` into the project's `.claude/settings.json` so
    // Claude Code shows live context in its status bar. Only sets the key when
    // absent (never clobbers the user's existing statusLine). Non-fatal.
    if let Err(err) = write_status_line(project_dir) {
        tracing::warn!("failed to write statusLine config: {err}");
    }

    // Write the project-tier trusty-mpm-owned hooks: the `trusty-memory`
    // block, the PM-enforcement guard, and (issue #2003) the lifecycle triad
    // (circuit breaker / audit log / dashboard) — folded in here because the
    // daemon's managed launch excludes the user tier where that triad would
    // otherwise be provisioned. Non-fatal: the session still launches, it
    // just won't record memory or lifecycle events via the hooks.
    //
    // #5034: `[hooks] prompt_context = false` suppresses the per-prompt
    // `trusty-memory prompt-context` injection (and strips one a prior launch
    // wrote). Default `true` — every other hook is written either way.
    let hooks_written = match write_project_hooks(project_dir, config.hooks.prompt_context) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!("failed to write trusty-mpm project hooks: {err}");
            false
        }
    };

    // Inject the `trusty-memory` MCP server into the project's `.mcp.json` so
    // the launched `claude` process can reach the memory tools (`memory_recall`,
    // `memory_note`, …). Gated by the manifest's `[mcp] trusty_memory` toggle
    // (default on). Non-fatal: the session still launches, it just lacks the
    // memory tools.
    //
    // `trusty_memory_injected` (issue #3950 — fifth instance of the
    // name-approval/content-pinning vulnerability class): tracks whether the
    // entry was ACTUALLY force-written this run, not merely whether the
    // toggle was on. A toggle that is on but a write that fails (disk full,
    // permission error, transient I/O fault) must NOT count as "pinned" —
    // see this variable's use in `trusted_mcp_names` below.
    crate::core::provisioning_stage::emit(
        crate::core::provisioning_stage::ProvisioningStage::ConfiguringMcp,
    );

    // #4181: keep `.mcp.json` out of git's index BEFORE any injector writes to
    // it — see `native_mcp::exclude_mcp_json_from_git` for why, and why a
    // failure here must not stop the injectors below.
    native_mcp::exclude_mcp_json_from_git(project_dir);

    let mut trusty_memory_injected = false;
    if plan.inject_trusty_memory {
        // Pin the project's palace via `env.TRUSTY_MEMORY_PALACE` (issue #1605).
        // `repo_url` (the cloned-from URL, threaded from LaunchParams) is the
        // authoritative identity for repo_url-cloned sessions; the injector
        // falls back to the workspace's own git origin remote when it is `None`,
        // and to the bare stub when no slug can be derived (fail-open).
        match inject_trusty_memory_mcp(project_dir, repo_url) {
            Ok(()) => trusty_memory_injected = true,
            Err(err) => tracing::warn!("failed to inject trusty-memory MCP server: {err}"),
        }
    } else {
        tracing::debug!("manifest disables trusty-memory MCP injection");
    }

    // Register + pin the project's trusty-search index (issue #1373). Derive
    // the project's canonical index id (git-root basename, via the shared
    // `trusty_common::derive_index_id`), best-effort find-or-create it in the
    // running daemon, then inject the `trusty-search` MCP stub PINNED to that id
    // (`serve --index <id>`). Pinning makes a bare `search`/`grep` resolve to
    // the session's OWN project index instead of letting the LLM guess (and
    // routinely pick the wrong `claude-mpm` index). The daemon-unreachable case
    // is handled inside `register_project_index` (logged, non-fatal) and still
    // returns the id so the stub is pinned; a `None` id (empty derivation) falls
    // back to the unpinned stub. Either way the session launches.
    // Gated by the manifest's `[mcp] trusty_search` toggle (default on).
    // `trusty_search_injected` mirrors `trusty_memory_injected` above (#3950).
    let mut trusty_search_injected = false;
    if plan.inject_trusty_search {
        let pinned_index = register_project_index(project_dir);
        match inject_trusty_search_mcp(project_dir, pinned_index.as_deref()) {
            Ok(()) => trusty_search_injected = true,
            Err(err) => tracing::warn!("failed to inject trusty-search MCP server: {err}"),
        }
    } else {
        tracing::debug!("manifest disables trusty-search MCP injection");
    }

    // Force-write the canonical `trusty-mpm` / `trusty-review` MCP server
    // entries (issue #3918 follow-up, code-critic BLOCK — name-squatting
    // exploit). Both names are unconditionally pre-approved in a managed
    // session's `enabledMcpjsonServers` (they are framework builtins — see
    // `mcp_config::BUILTIN_MANAGED_MCP_SERVERS`), but that approval is
    // NAME-based and CONTENT-BLIND: Claude Code still reads whatever command
    // sits under the name in this file. Without an injector here, a hostile
    // clone could squat either pre-approved name with an attacker-controlled
    // command (no human present to decline it), and — independently — a
    // freshly-cloned project with no committed entry got no WORKING
    // connection to its own trusty-mpm/trusty-review servers, so #3918's
    // actual symptom was not fixed in the general case. Unconditional (no
    // manifest toggle, unlike the optional trusty-memory/trusty-search
    // integrations above) and MUST run before the trust pre-seed below —
    // same ordering reasoning as the native/custom injectors. Non-fatal.
    //
    // `trusty_mpm_injected`/`trusty_review_injected` (issue #3950): these two
    // have no manifest escape hatch, but the WRITE itself can still fail —
    // before this fix, either name was unconditionally added to
    // `enabledMcpjsonServers` regardless of whether this force-overwrite
    // actually succeeded, reopening the identical vulnerability for the
    // "unconditional" pair. See `trusted_mcp_names` below.
    let trusty_mpm_injected = match inject_trusty_mpm_mcp(project_dir) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!("failed to inject trusty-mpm MCP server: {err}");
            false
        }
    };
    let trusty_review_injected = match inject_trusty_review_mcp(project_dir) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!("failed to inject trusty-review MCP server: {err}");
            false
        }
    };

    // Inject NATIVE trusty MCP servers registered via `tm mcp add` (e.g.
    // slack-mcp, gworkspace-mcp, trusty-analyze) into the workspace `.mcp.json`
    // (issue #2739). Fleet sessions launch `claude --setting-sources
    // project,local`, which never reads the managed `.claude.json` `mcpServers`
    // map where `tm mcp` writes — so without this bridge those servers are
    // invisible to managed sessions. Scope is the native-trusty allowlist ONLY
    // (third-party/HTTP managed servers are handled by the custom-server bridge
    // just below); the memory and search injectors above already own their own
    // pinned entries and are excluded from the allowlist. MUST run BEFORE the
    // trust pre-seed below so the injected names flow into
    // `enabledMcpjsonServers` and skip the "new MCP servers found" dialog.
    // Non-fatal: a failure only means those extra tools are absent from this
    // session.
    if let Err(err) = inject_native_trusty_mcps(project_dir) {
        tracing::warn!("failed to inject native trusty MCP servers: {err}");
    }

    // Inject CUSTOM (non-native, non-builtin) MCP servers — issue #2739
    // follow-up — from BOTH the user-scope managed registry (any `tm mcp add`
    // entry not on the native allowlist) and the resolved project-scope
    // manifest `[mcp.custom]` table (`plan.custom_mcp_servers`), project
    // overriding user on a name collision. See `session_launch::custom_mcp`
    // for the full scope/precedence/security model. MUST also run before the
    // trust pre-seed below, same reasoning as the native injector. Non-fatal:
    // a failure yields an empty project-scope-names set, which only means
    // this run's project-scope servers (if any bridged before the failure)
    // are conservatively left OUT of trust pre-approval rather than in it.
    let project_scope_mcp_names =
        match inject_custom_trusty_mcps(project_dir, &plan.custom_mcp_servers) {
            Ok(names) => names,
            Err(err) => {
                tracing::warn!("failed to bridge custom MCP servers: {err}");
                BTreeSet::new()
            }
        };

    // Pre-seed per-directory trust for this workspace in `~/.claude.json`
    // (issue #1269) so the interactive tmux Claude session does not stall on the
    // "Do you trust this folder?" dialog and the injected task prompt is
    // received. tm owns this workspace path, so marking it trusted is safe.
    //
    // `enabledMcpjsonServers` (issue #3926 security fix): computed here, NEVER
    // by reading the workspace's own `.mcp.json` (that file is git-tracked and
    // travels WITH a cloned repo — see `mcp_config::mcp_server_names`'s
    // SECURITY doc for the vulnerability this closes). `launch_trusted_mcp_names`
    // returns each of the framework builtin four ONLY when its own
    // `trusty_*_injected` bool is true THIS run — i.e. its force-overwrite
    // injector actually SUCCEEDED writing the framework-controlled command,
    // not merely that a manifest toggle was on (issue #3950 — fifth
    // instance: a toggle on but a write that fails, e.g. disk full,
    // permission error, or transient I/O fault, must NOT leave a spoofed or
    // stale `.mcp.json` entry pre-approved; issue #3934's earlier fix closed
    // the toggle-off case but still passed the toggle value directly,
    // ignoring the injector's own `Result`) — UNION the operator's own
    // `tm mcp add` registry (also force-overwritten above, by the
    // native/custom injectors, whenever a registry name matches) —
    // provenance the operator or the framework controls, mirroring
    // `mcp_config::managed_mcp_server_names`'s already-fixed derivation for
    // the daemon-managed path (#3918/#3924). `project_scope_mcp_names` is
    // then subtracted (issue #2739's existing defense-in-depth, preserved
    // unchanged): a project-scope `[mcp.custom]` entry ships with the cloned
    // repo itself, so it still surfaces Claude Code's "new MCP servers
    // found" consent dialog even after `tm project trust` — see
    // `session_launch::custom_mcp`'s "Consent gate" docs. A name that
    // reaches neither source — e.g. one a cloned repo merely committed to
    // `.mcp.json` directly, a conditional builtin whose injector was
    // disabled, or ANY of the four builtins whose force-overwrite write
    // failed this run — now correctly falls through to that same dialog
    // instead of being silently pre-approved. Non-fatal: a trust-seed
    // failure only means the operator may see the dialog.
    let trusted_mcp_names: BTreeSet<String> = crate::core::mcp_config::launch_trusted_mcp_names(
        trusty_mpm_injected,
        trusty_review_injected,
        trusty_memory_injected,
        trusty_search_injected,
    )
    .into_iter()
    .filter(|name| !project_scope_mcp_names.contains(name))
    .collect();
    if let Err(err) = preseed_workspace_trust_home(project_dir, &trusted_mcp_names) {
        tracing::warn!("failed to pre-seed workspace trust: {err}");
    }

    // Remove the now-redundant global `trusty-memory` hook entries so they no
    // longer fire for every Claude Code session (including claude-mpm). The
    // project hooks above scope them to trusty-mpm sessions. Non-fatal.
    if let Err(err) = remove_global_trusty_memory_hooks() {
        tracing::warn!("failed to remove global trusty-memory hooks: {err}");
    }

    // Deploy the bundled output-style definition so Claude Code can resolve the
    // `trusty-mpm` name written into `.claude/settings.json` above. Non-fatal:
    // a missing style file just falls back to the operator's default. Uses
    // `fw.claude_home_dir()` (issue #1860) rather than `dirs::home_dir()`
    // directly so isolated `FrameworkPaths::under(tempdir)` callers (tests)
    // stay confined to the temp dir instead of leaking into the real `$HOME`.
    //
    // NOTE (#4203): "home tier" describes only the callers that pass a
    // home-rooted `fw` — `tm session start`, `tm run`/standalone, `tm repair`.
    // For an `isolated_framework_paths` caller (`tm launch`, `tm connect`,
    // `tm meta launch`) and for the daemon's `for_managed_workspace` spawn
    // (#1931), `fw.claude_home_dir()` IS `project_dir`, so this call and the
    // project-tier one below write the same bytes to the same place — harmless
    // and idempotent, but it means those commands do NOT refresh
    // `$HOME/.claude/output-styles/`. Refreshing the real home tier is owned by
    // `tm install` and `tm catalog apply`, which still resolve
    // `FrameworkPaths::default()`.
    let output_style = match deploy_output_style(&fw.claude_home_dir()) {
        Ok(path) => Some(path),
        Err(err) => {
            tracing::warn!("failed to deploy trusty-mpm output style file: {err}");
            None
        }
    };

    // Issue #2125 item 2: ALSO deploy the bundled output styles under the
    // PROJECT tier (`<project_dir>/.claude/output-styles/`). The daemon
    // managed-spawn path launches `claude --setting-sources project,local`,
    // which excludes the `user` tier the deploy above lands in — without a
    // project-tier copy, the `outputStyle` id `write_output_style` writes into
    // `<project_dir>/.claude/settings.json` cannot resolve under that flag, so
    // Claude Code silently falls back to its own default (never applying the
    // PM delegation persona). Non-fatal: a failure here only leaves that one
    // carrier degraded — the home-tier deploy and the standalone `tm run`
    // driver (which passes no `--setting-sources` flag) are unaffected.
    if let Err(err) = deploy_output_style(project_dir) {
        tracing::warn!("failed to deploy project-tier trusty-mpm output style: {err}");
    }

    // Issue #3427: ensure the harness-scaffolding paths this deploy just wrote
    // (or may write in a future session) are gitignored in `project_dir`, so
    // they never enter this project's git history — the precondition for the
    // "would be overwritten by merge" collision this issue reports. A no-op
    // when `project_dir` is not a git working tree, and idempotent otherwise
    // (see `scaffold_gitignore` module docs). Non-fatal: a write failure only
    // means the operator keeps doing this manually, it never blocks launch.
    // This only prevents FUTURE commits — a project that already committed
    // these paths needs the `scaffold_tracking` doctor check's remediation,
    // not this step.
    if let Err(err) = crate::core::scaffold_gitignore::ensure_scaffold_gitignored(project_dir) {
        tracing::warn!("failed to update .gitignore for harness scaffolding: {err}");
    }

    // DOC-28 cutover bridge — auto-inject catch-up as seed context (#1762).
    // Fail-open: if catch-up fails for any reason (daemon not running, no git
    // repo, runtime error), the session still launches; catchup_context is None.
    // CUTOVER BRIDGE — remove post-migration (#1762)
    let catchup_context = if config.catchup.auto {
        // Discovery-first (issue #2030): resolves TRUSTY_MEMORY_URL when set,
        // else the daemon's actual discovered bound address, never a
        // hardcoded port.
        let memory_url = trusty_common::mcp::memory_rpc::resolve_memory_base_url_or_unreachable();
        let opts = crate::core::catchup::CatchupOptions {
            project_dir: project_dir.to_path_buf(),
            memory_url,
            include_git: config.catchup.include_git,
            include_palace: config.catchup.include_palace,
            git_limit: config.catchup.git_limit,
            drawer_limit: config.catchup.drawer_limit,
            // Auto-inject always uses the watermark (incremental).
            full: false,
        };
        // Auto-inject advances the watermark so subsequent sessions are incremental.
        let ctx = crate::core::catchup::run_catchup_blocking(opts, true);
        if ctx.is_empty() { None } else { Some(ctx) }
    } else {
        None
    };

    // #4752: refresh the PROJECT-LOCAL compiled prompt
    // (`<project_dir>/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md`) with the
    // text this session will actually receive (`resolved_prompt`, the same
    // string stashed to `.trusty-mpm/last-instructions.md` above).
    //
    // It is deliberately the LAST step, so a refusal is not also a
    // half-provisioned workspace: everything the session needs is already on
    // disk by the time this can fail. Nothing below this line may depend on the
    // write succeeding — keep it last.
    // #4832 migration, best-effort: retire the pre-#4832 per-project compiled
    // prompt so an upgraded install is not left with a file nothing refreshes.
    // Run against BOTH the directory handed in (which is the worktree on a
    // managed session — every worktree of an upgraded project holds one) and
    // the harness root. A failure here is logged, never fatal: the migration is
    // housekeeping and must not cost anyone a launch.
    for legacy_base in [
        project_dir,
        &crate::core::harness_root::harness_root(project_dir),
    ] {
        match crate::core::instruction_pipeline::remove_legacy_compiled_prompt(legacy_base) {
            Ok(true) => tracing::info!(
                base = %legacy_base.display(),
                "removed the pre-#4832 per-project compiled prompt"
            ),
            Ok(false) => {}
            Err(e) => tracing::warn!(
                base = %legacy_base.display(),
                "could not remove the pre-#4832 compiled prompt (non-fatal): {e}"
            ),
        }
    }

    let scope = crate::core::harness_root::session_scope(session_id);
    let compiled = crate::core::instruction_pipeline::compiled_prompt_path(project_dir, &scope);
    crate::core::instruction_pipeline::write_compiled_prompt_to(&compiled, &resolved_prompt)
        .map_err(|source| PrepError::Instructions {
            path: compiled.clone(),
            source,
        })?;

    Ok(PrepReport {
        deploy,
        skill_deploy,
        instructions,
        stash,
        compiled_prompt: compiled,
        output_style,
        hooks_written,
        trusty_mpm_injected,
        trusty_review_injected,
        trusty_memory_injected,
        trusty_search_injected,
        catchup_context,
        roster_errors,
    })
}

/// Build the project-agnostic `--append-system-prompt` text (no overrides).
///
/// Why: every `claude` session launched by trusty-mpm must be a configured PM
/// instance. trusty-mpm owns its PM instructions: they are assembled IN MEMORY
/// from the compile-time bundled sections and passed to
/// `claude --append-system-prompt-file`. Nothing is read back from an installed
/// `INSTRUCTIONS.md` to produce them (#4752 retired that path — see the note
/// below). This variant is kept for callers that do not know the project
/// directory (e.g. tests); prefer [`build_system_prompt_for`] at launch sites so
/// project-level overrides apply.
/// What: returns [`crate::core::instruction_pipeline::assemble_system_prompt`],
/// trimmed. `Option` is retained for API compatibility and is always `Some`.
///
/// #4752: this used to read `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md`
/// and regenerate it on disk when missing. That file is retired — nothing writes
/// it, `tm install` deletes a stale copy, and the compiled prompt is now
/// per-project — so the round-trip could only ever have returned either bundled
/// content it already had in memory, or a stale leftover. Composing directly
/// removes the last dependency on the retired path and the home directory.
/// Test: `build_system_prompt_includes_trusty_block`.
pub fn build_system_prompt() -> Option<String> {
    let composed = crate::core::instruction_pipeline::assemble_system_prompt();
    let trimmed = composed.trim_end();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

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
/// What: delegates to
/// [`crate::core::instruction_overrides::resolve_pm_prompt`], which layers the
/// override files onto the bundled PM prompt and always appends the
/// non-overridable `BASE_PM` floor last, then applies HR-4 output-style
/// version-fallback injection via [`build_system_prompt_for_with_style`] with no
/// explicit style override.
/// Test: `build_system_prompt_for_applies_project_override`.
pub fn build_system_prompt_for(project_dir: &Path) -> String {
    build_system_prompt_for_with_style(project_dir, None)
}

/// Build the launch prompt for `project_dir`, applying overrides AND HR-4
/// output-style version-fallback injection with an explicit style override.
///
/// Why: on Claude Code builds older than `1.0.83` the native `outputStyle`
/// settings key is ignored, so the active output style only takes effect if its
/// content is folded into the `--append-system-prompt` text. This is the single
/// launch seam where that injection happens, so both the CLI (`tm launch`) and
/// the client (`/connect`) get identical behaviour. The `--style` flag flows in
/// as `explicit_style`.
/// What: resolves the override-layered PM prompt via
/// [`crate::core::instruction_overrides::resolve_pm_prompt`], then calls
/// [`crate::core::output_style::apply_output_style_to_prompt`], which is a no-op
/// on native-capable Claude Code and prepends the active style otherwise. The
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

/// Build the launch prompt with the `native_supported` output-style decision
/// supplied explicitly (no live `claude --version` probe).
///
/// Why: the public [`build_system_prompt_for_with_style`] probes
/// `claude --version`, so any test (or caller) that exercises the launch prompt
/// is silently coupled to whether `claude` is installed on the host — the
/// host-dependence that broke `prepare_session_stash_reflects_override` on CI
/// (issue #1409). This seam pins the decision so the stash/launch invariant can
/// be asserted deterministically under BOTH `native_supported = true` (no
/// injection) and `false` (injection fires). Production code keeps real
/// detection via the wrapper above.
/// What: resolves the override-layered PM prompt via
/// [`crate::core::instruction_overrides::resolve_pm_prompt`], then applies the
/// injection decision with the caller-supplied flag via
/// [`crate::core::output_style::apply_output_style_to_prompt_with_native`].
/// Test: `prepare_session_stash_reflects_override`.
pub fn build_system_prompt_for_with_style_and_native(
    project_dir: &Path,
    explicit_style: Option<&str>,
    native_supported: bool,
) -> String {
    let prompt = crate::core::instruction_overrides::resolve_pm_prompt(project_dir);
    crate::core::output_style::apply_output_style_to_prompt_with_native(
        project_dir,
        explicit_style,
        prompt,
        native_supported,
    )
}
