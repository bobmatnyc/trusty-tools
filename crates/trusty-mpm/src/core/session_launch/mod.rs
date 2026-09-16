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

// #6649: the launch lines an operator sees when an asset tier is not clean.
mod asset_notices;
// #7685: the public `prepare_session*` entry points. Split out of this file,
// which was at the 500-SLOC production cap — see `entry.rs`'s own doc. The
// re-exports below keep every `session_launch::prepare_session*` path intact.
mod entry;
// `isolated_framework_paths` has no caller outside `entry` itself; the tests
// that pin the layout reach it as `super::entry::isolated_framework_paths`.
pub use entry::{
    prepare_isolated_session, prepare_session, prepare_session_for_managed,
    prepare_session_for_repair, prepare_session_with_home, prepare_session_with_memory_reachable,
    prepare_session_with_repo_url, prepare_session_with_repo_url_and_exe,
    prepare_session_with_style, prepare_session_with_style_and_native,
};
// #6887: the bulk-read diversion `PreToolUse` groups.
mod divert_hooks;
// #7849: the read-only diff between a project's settings file and the hook
// groups its CURRENT config asks for, shared by `deploy_validate` and
// `tm doctor`.
mod hook_group_diff;
/// The ONE way a non-terminal launch path surfaces a `PrepReport`'s findings.
///
/// Why: five spawn paths wrote the same two loops by hand, and a sixth would
/// have surfaced only half of them. See [`asset_notices::log_prep_findings`].
/// Test: covered by `asset_notices`' own tests (this is a plain re-export).
pub use asset_notices::{PrepScope, log_prep_findings};
pub(crate) use hook_group_diff::{ProjectHookGroupGaps, project_hook_group_gaps};
mod palace_alias;
// #7780: preserve a malformed project settings.json before a writer rewrites it.
// #7789: `pub(crate)` because the managed-tier writers in `core::standalone`
// share the rule; the module stays here because `PrepError` does.
pub(crate) mod malformed_backup;
// #4448: the ONE wiring of the shadowing-agent quarantine, shared by
// `prepare_session_inner` and `sync_session_assets`.
mod project_hooks;
// #7688: the `Stop`/`SubagentStop` prompt-feedback capture groups.
mod prompt_feedback_hooks;
#[cfg(test)]
#[path = "prompt_feedback_hooks_tests.rs"]
mod prompt_feedback_hooks_tests;
mod quarantine_shadows;
// #7490: the project-tier hook merge every resume and relaunch path was
// missing, plus the read-only gap predicate `tm doctor` reports from.
mod resume_hooks;
pub use resume_hooks::{ensure_project_hooks, missing_lifecycle_hook_events};
// #7849: `deploy_validate` repairs a toggle-driven hook gap through this exact
// writer, and drives it from a hermetic `FrameworkPaths` so the repair never
// reads the operator's `$HOME`.
pub(crate) use resume_hooks::ensure_project_hooks_with;
mod search_index;
mod settings;
mod skills;
// #7617: the launch-time `statusLine` guarantee, both tiers.
mod statusline;
pub use statusline::{ensure_status_line, ensure_status_line_in};
mod sync_assets;
// #7688: the four `--append-system-prompt` composers, split out of this module
// when the addendum pushed it over the 500-SLOC production cap. Re-exported so
// every existing `session_launch::build_system_prompt_for*` call site is
// unchanged.
mod system_prompt;
pub use system_prompt::{
    build_system_prompt_for, build_system_prompt_for_with_roster,
    build_system_prompt_for_with_style, build_system_prompt_for_with_style_and_native,
};
#[cfg(test)]
mod tests;
mod workstream_label;
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
// #4181 — successor to the #3926 suite: the approval is deleted, so this file
// now pins that NO name is pre-approved and that a stale one is stripped.
#[cfg(test)]
#[path = "tests_launch_trust_3926.rs"]
mod tests_launch_trust_3926;
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
// #7751: real-path coverage for the stack-profile `skillOverrides` write.
#[cfg(test)]
#[path = "tests_skill_overrides_7751.rs"]
mod tests_skill_overrides_7751;

// #7780: the malformed-settings copy-aside, at every writer's call site and
// once per `prepare_session`.
#[cfg(test)]
#[path = "tests_malformed_settings_7780.rs"]
mod tests_malformed_settings_7780;

// #7762: the cross-process settings lock, at the call sites that race.
#[cfg(test)]
#[path = "tests_settings_lock_7762.rs"]
mod tests_settings_lock_7762;

use std::path::{Path, PathBuf};

use crate::core::agent_deployer::{DeployResult, deploy_agents_filtered, retract_framework_agents};
use crate::core::instruction_pipeline::{PipelineInput, PipelineOutput, build_instructions};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deployer::DeployStats;
use settings::{
    deploy_output_style,
    preseed_workspace_trust_home,
    remove_global_trusty_memory_hooks,
    write_auto_memory_enabled,
    write_output_style,
    // #7688: the launch path is the only caller that turns the capture on, so
    // the plain `write_project_hooks` is not reached from this module.
    // write_enabled_plugins is not imported here: it is already brought into
    // scope by the pub(crate) re-export below (#7678).
    write_project_hooks_with_prompt_feedback,
};

/// Re-export of the project-tier `enabledPlugins` writer (#7678).
///
/// Why: `mod settings` above is private, so `tm doctor --fix`'s repair
/// ([`crate::core::doctor_repair_scope`]) could not reach the writer
/// `prepare_session` uses without this re-export — and a second implementation
/// of the same write is exactly what the repair must not be.
/// What: re-exports [`settings::write_enabled_plugins`] and its trust-injected
/// seam [`settings::write_enabled_plugins_with_trust`] — plain re-exports, no
/// logic of their own.
/// Test: `write_enabled_plugins_denies_a_non_opted_plugin`,
/// `session_scope_repair_applies_the_default_deny_map`.
pub(crate) use settings::{write_enabled_plugins, write_enabled_plugins_with_trust};

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

/// Re-export of the auto-memory settings key and the one-key settings merge
/// (#7685), so `tm doctor`'s `auto_memory` check and its `--fix` repair read and
/// write exactly what this launch path writes.
pub(crate) use settings::{AUTO_MEMORY_KEY, merge_settings_key};

/// Re-export of the trusty-search project-index registration entry point and
/// the trusty-memory palace-slug derivation.
///
/// Why: `mod search_index` / `mod settings` above are private, so a
/// `pub(crate)` item inside either is unreachable from outside `session_launch`
/// without a re-export at this (public) module boundary — mirroring the
/// `OUTPUT_STYLE` re-export just above. // #4181: both used to feed the
/// `.mcp.json` injectors this module owned; ADR-0042 deleted those, and
/// `core::mcp_session_env` now consumes the same two values to build the
/// `TRUSTY_INDEX` / `TRUSTY_MEMORY_PALACE` variables the spawn exports.
/// What: re-exports [`search_index::register_project_index`] and
/// [`settings::resolve_palace_slug`] — plain re-exports, no logic of their own.
/// Test: `tests_search_index.rs` and `resolve_palace_slug_*` in `tests.rs`.
pub(crate) use search_index::register_project_index;
pub(crate) use settings::resolve_palace_slug;

/// Re-export of the #1939 palace-alias healing so it can run per launch from
/// `core::managed_config::ensure_managed_config_dir_with_root`.
///
/// Why (#4181): `maybe_register_palace_alias` had exactly one call site — inside
/// `settings::inject_trusty_memory_mcp`, immediately before the write. ADR-0042
/// deletes that injector, and deleting it as written would stop the claude-mpm
/// split-brain healing with no test failing to say so. The call is rehomed onto
/// `ensure_managed_config_dir`, which every spawn, resume and in-place relaunch
/// already reaches.
/// What: re-exports [`palace_alias::maybe_register_palace_alias`] — a plain
/// re-export, no logic of its own.
/// Test: `palace_alias`'s own unit tests, plus
/// `ensure_managed_config_dir_heals_a_bare_repo_palace_alias`, which fails if
/// the rehomed call site is deleted.
pub(crate) use palace_alias::maybe_register_palace_alias;

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
    /// Operator-facing asset-hygiene lines for this launch (#6649).
    ///
    /// Why (owner ruling 2026-09-02): the launch already quarantines shadowing
    /// agents and could already see project-tier skill strays, and every one of
    /// those findings went to `tracing` — which a terminal session does not
    /// show. This is the field that makes them visible. It is SEPARATE from
    /// [`Self::roster_errors`] because these are not provisioning failures: the
    /// launch succeeded, and what is reported is the state of the operator's
    /// own working tree.
    /// What: at most three lines from
    /// [`asset_notices::launch_asset_notices`] — agents quarantined, skills
    /// stray, duplicates — EMPTY when all three are clean, because this runs on
    /// every launch and a clean project must add nothing to the terminal.
    /// Callers MUST surface each line: the CLI paths print to stdout, the
    /// daemon and client paths log at `warn`.
    /// Test: `prepare_session_reports_a_quarantined_agent_as_a_launch_notice`,
    /// `prepare_session_on_a_clean_project_reports_no_asset_notice`.
    pub asset_notices: Vec<String>,
    /// Whether trusty-memory answered this launch's reachability probe (#7685).
    ///
    /// Why: the runtime adapter needs the same answer to decide
    /// `CLAUDE_CODE_DISABLE_AUTO_MEMORY`, and it runs after preparation in the
    /// same request. Returning the resolved value is what lets it reuse this
    /// one rather than paying a second `PROBE_TIMEOUT` against a daemon that is
    /// already known to be hung.
    /// What: the value this preparation wrote `autoMemoryEnabled` against —
    /// `true` means auto memory was turned off, `false` means it was restored.
    /// Test: `prepare_session_reports_the_resolved_reachability`.
    pub memory_reachable: bool,
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
    /// No stable installed `tm`/`trusty-mpm` binary could be resolved, so the
    /// project's hooks were left untouched (#7244).
    ///
    /// Non-fatal, per #2149's rule that a preparation hiccup must not stop a
    /// launch: the session still starts, without tm's project-tier hooks. What
    /// it must NOT do is write hooks pointing at a build artifact, which is the
    /// alternative this variant exists to replace.
    /// Test: `write_project_hooks_writes_nothing_when_the_exe_cannot_be_resolved`.
    #[error("could not resolve a stable hook binary: {source}")]
    HookExe {
        /// Which resolution step refused, and the path it refused.
        #[source]
        source: crate::core::standalone::hooks::StableHookExeError,
    },
    /// The pre-rewrite snapshot of the project's `settings.json` could not be
    /// taken, so the hooks writer left the file alone (#7244, round 3).
    ///
    /// Why its own variant rather than [`Self::Io`]: the operator's next move
    /// differs. A generic I/O failure is usually about the path; this one says
    /// the CURRENT file survived and only the update was skipped, which is the
    /// benign half of fail-closed and reads as alarming without that framing.
    /// Non-fatal, for the same reason [`Self::HookExe`] is: the session starts
    /// with the hooks already on disk.
    /// Test: `write_project_hooks_aborts_when_the_snapshot_fails`.
    #[error("could not snapshot {path} before rewriting it: {source}")]
    HookSnapshot {
        /// The settings file whose rewrite was abandoned.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// The project's `.claude/settings.json` could not be preserved, so the
    /// writer that was about to replace it left it alone (#7780).
    ///
    /// Why its own variant rather than [`Self::Io`]: like [`Self::HookSnapshot`]
    /// this reports the benign half of fail-closed — the file on disk survived
    /// and only tm's update was skipped — and a bare I/O error reads as if the
    /// launch had lost the operator's settings, which is the opposite of what
    /// happened. Raised for a file that cannot be READ at all as well as for a
    /// preserved copy that cannot be written; neither leaves anything downstream
    /// able to put the original bytes back. Non-fatal, for the reason
    /// [`Self::HookExe`] is: the session still starts.
    /// Test: `malformed_backup::tests::refuses_when_the_copy_cannot_be_written`,
    /// `merge_settings_refuses_when_the_copy_cannot_be_written`.
    #[error("refused to rewrite {path}: its current contents could not be preserved: {source}")]
    SettingsBackup {
        /// The settings file whose rewrite was abandoned.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// The cross-process lock guarding a `settings.json` could not be acquired,
    /// so the writer read nothing and wrote nothing (#7762).
    ///
    /// Why its own variant rather than [`Self::Io`]: this is the fail-closed arm
    /// of [`crate::core::settings_lock`], and the remedy is specific — a stale
    /// sidecar, a permissions problem on `.claude/`, or another `tm` holding the
    /// file for longer than expected. The alternative it replaces is a writer
    /// that proceeds unlocked and silently drops a concurrent writer's keys.
    /// Non-fatal, for the reason [`Self::HookExe`] is: the session still starts,
    /// with whatever is already on disk.
    /// Test: `merge_settings_refuses_when_the_lock_cannot_be_acquired`.
    #[error("could not acquire the settings lock for {path}: {source}")]
    SettingsLock {
        /// The settings file whose update was abandoned.
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

/// Shared body for every `prepare_session*` entry point.
///
/// Why: the public entry points differ only in how they resolve `explicit_style`,
/// `native_supported`, and (issue #1605) the cloned-from `repo_url`. Funnelling
/// them through one private body keeps the long preparation sequence in a single
/// place so the variants cannot drift. `repo_url` is the only new degree of
/// freedom: it is threaded to [`maybe_register_palace_alias`] as the
/// authoritative remote and is `None` for the flag-less / style-only entry
/// points (which then fall back to the workspace's own git origin remote).
/// What: identical to the documented [`prepare_session_with_style_and_native`]
/// behaviour, plus it passes `repo_url` to [`maybe_register_palace_alias`].
/// Issue #2149: an agent-deploy or skill-deploy failure is captured into
/// [`PrepReport::roster_errors`] rather than short-circuiting the function via
/// `?` — every step after the roster deploy (CLAUDE.md/instructions, the
/// trusty-mpm output-style write, MCP injection, hooks) still runs
/// unconditionally, so a session ALWAYS launches carrying its trusty-mpm
/// identity even when the roster itself is empty or broken. This function now
/// only returns `Err` for genuinely fatal failures (the instruction pipeline,
/// or the inspection-stash IO).
/// Test: covered by every `prepare_session_*` test plus
/// `prepare_session_continues_after_agent_deploy_failure`,
/// `prepare_session_continues_after_skill_deploy_failure`.
/// The ambient host inputs [`prepare_session_inner`] would otherwise read from
/// the process itself.
///
/// Why: every field is something production reads from the environment and
/// tests must pin — `home` since #5544, `hook_exe` since #7244,
/// `memory_reachable` since #7685. Grouping them keeps
/// [`prepare_session_inner`]'s arity where clippy wants it and puts the
/// injections side by side, so the next one has an obvious home.
/// What: `home` is the USER-GLOBAL home (not derivable from `FrameworkPaths` —
/// the managed constructors relocate every `.claude/` path on it onto the
/// workspace); `hook_exe` is the stable binary the project-tier hooks writer
/// should bake, `None` meaning "resolve the running one"; `memory_reachable`
/// pins whether trusty-memory answered, `None` meaning "probe the host".
/// Test: `repair_closes_gaps_on_incomplete_workspace`,
/// `prepare_session_disables_auto_memory_when_trusty_memory_is_reachable`,
/// `prepare_session_restores_auto_memory_when_trusty_memory_is_down`, plus
/// every `prepare_session_*` test.
#[derive(Clone, Copy, Default)]
pub(super) struct HostInputs<'a> {
    pub(super) home: Option<&'a Path>,
    pub(super) hook_exe: Option<&'a Path>,
    pub(super) memory_reachable: Option<bool>,
}

impl<'a> HostInputs<'a> {
    /// The production shape: a real home, the running binary for hooks, and a
    /// live reachability probe.
    pub(super) fn with_home(home: Option<&'a Path>) -> Self {
        Self {
            home,
            hook_exe: None,
            memory_reachable: None,
        }
    }
}

pub(super) fn prepare_session_inner(
    fw: &FrameworkPaths,
    project_dir: &Path,
    explicit_style: Option<&str>,
    native_supported: bool,
    repo_url: Option<&str>,
    session_id: Option<&str>,
    host: HostInputs<'_>,
) -> Result<PrepReport, PrepError> {
    let home = host.home;
    // #7806: opens this launch's stack-detection scope. Every manifest
    // resolution below — and the five-plus in the call sites it reaches — then
    // shares ONE nested walk, while a daemon-hosted launch still re-detects a
    // tree that changed since the last one.
    crate::core::manifest::invalidate_stack_detection(project_dir);
    // #7685: resolved ONCE per launch, here, and returned on the `PrepReport` so
    // the runtime adapter reuses this answer instead of dialling the daemon a
    // second time.
    let memory_reachable =
        crate::core::memory_reachable::resolve_memory_reachable(host.memory_reachable);
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
    // #7727: `fw.skill_deploy_dir()` is the skills tier the agent bodies name.
    let deploy = match deploy_agents_filtered(
        &plan.agent_source,
        &fw.agent_deploy_dir(),
        &fw.skill_deploy_dir(),
        |name| plan.agent_selected(name),
    ) {
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
    // #6649: the report is KEPT so the launch line can name what moved — the
    // `warn!` below goes to a log no terminal session is showing the operator.
    let mut quarantine_report = None;
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
            quarantine_report = Some(report);
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

    let skill_deploy = skills::deploy_session_skills(
        fw,
        &plan,
        project_dir,
        &deploy.declared_skills,
        &mut roster_errors,
    );

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
        // #7673 review: the injected home, not `dirs::home_dir()` inside the
        // guard — the same seam every other host input rides on (#5544).
        home: home.map(Path::to_path_buf),
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

    // #7673: one WARN per launch naming every `CLAUDE.md` ABOVE this project —
    // Claude Code prepends each to every turn and the session cannot see where
    // the text came from. `project_dir` may be a subdirectory (`session start`
    // in place); the scan resolves the nearest project boundary itself. A scan
    // that fails is announced, never silent.
    let managed_config = crate::core::trusty_tools_config::managed_claude_config_dir();
    crate::core::ancestor_claude_md::warn_ancestors(project_dir, home, managed_config.as_deref());

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

    // #7685: Claude Code auto-memory (`MEMORY.md`) is a FALLBACK — trusty-memory
    // is the memory, and auto memory stays on only when trusty-memory is not
    // available (owner ruling 2026-09-12). The write is TWO-WAY: `true` is
    // written as deliberately as `false`, because only an explicit `true` can
    // revert a `false` an earlier healthy-trusty-memory launch left behind.
    // Non-fatal for the same reason the write above is: a failure costs the
    // project the setting, not its session.
    if let Err(err) = write_auto_memory_enabled(project_dir, !memory_reachable) {
        tracing::warn!("failed to write Claude Code `{AUTO_MEMORY_KEY}`: {err}");
    } else if !memory_reachable {
        tracing::info!(
            "trusty-memory is not reachable — wrote `{AUTO_MEMORY_KEY}: true` to restore \
             Claude Code auto-memory as this project's fallback (#7685)"
        );
    }

    // #7422: write the project-tier `enabledPlugins` allowlist beside the output
    // style. Default-deny: every plugin the managed config dir knows about is
    // written `false` unless this project's `[session] plugins` names it.
    // Non-fatal — a failure costs the project its plugin scoping, not its
    // session, and the unscoped state is exactly the pre-#7422 behaviour.
    if let Err(err) = write_enabled_plugins(
        project_dir,
        crate::core::trusty_tools_config::managed_claude_config_dir().as_deref(),
    ) {
        tracing::warn!("failed to write the project plugin allowlist: {err}");
    }

    // #7751: turn off the skill families this project's detected stack cannot
    // use (owner ruling 2026-09-13), merged into the same project-tier file. An
    // unknown stack writes nothing, and so does a detection a scan bound cut
    // short (#7781); a malformed file is warned about and left alone. Non-fatal
    // — a failure costs listing tokens, not the session.
    match crate::core::skill_overrides::write_skill_overrides(project_dir) {
        Ok(crate::core::skill_overrides::SkillOverridesOutcome::Written(added)) => {
            tracing::info!(
                "turned off {} skill(s) this stack cannot use: {added:?}",
                added.len()
            );
        }
        Ok(_) => {}
        Err(err) => tracing::warn!("failed to write stack-profile skillOverrides: {err}"),
    }

    // Inject `tm statusline` into the project's `.claude/settings.json` so
    // Claude Code shows live context in its status bar. Only sets the key when
    // absent (never clobbers the user's existing statusLine). Non-fatal.
    // #7617: and into the user tier on the same call — provisioning owns both,
    // so the `💸` segment is core setup rather than a project's to arrange.
    if let Err(err) = ensure_status_line(project_dir) {
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
    let hooks_written = match write_project_hooks_with_prompt_feedback(
        project_dir,
        // #7244: `None` resolves the running installed binary. When nothing
        // stable resolves this now returns an error and writes NOTHING, rather
        // than wiring every hook to a build artifact. A test pins a stable path
        // through `HostInputs::hook_exe` so it asserts the write, not the host.
        host.hook_exe,
        config.hooks.prompt_context,
        plan.divert_enabled,
        // #7688: register the `Stop`/`SubagentStop` capture only where the flag
        // is on, so a project with the feature off spawns no process per turn.
        crate::core::prompt_self_improvement::enabled_for(project_dir),
    ) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!("failed to write trusty-mpm project hooks: {err}");
            false
        }
    };

    // #4181 (ADR-0042): tm no longer writes MCP config into the workspace. The
    // four framework builtins and every `tm mcp add` server are declared once in
    // `<CLAUDE_CONFIG_DIR>/.claude.json`'s user-scope `mcpServers` map, seeded by
    // `mcp_config::seed_builtin_servers` and read by the relocated spawn's
    // `--setting-sources user,project,local`. What remains here is the one piece
    // that cannot be shared across projects: the trusty-search index must exist
    // before `TRUSTY_INDEX` can point at it. `core::mcp_session_env` derives that
    // variable (and `TRUSTY_MEMORY_PALACE`) for the spawn.
    crate::core::provisioning_stage::emit(
        crate::core::provisioning_stage::ProvisioningStage::ConfiguringMcp,
    );

    // Find-or-create the project's trusty-search index (issues #1373, #1908) so
    // the `TRUSTY_INDEX` the spawn exports resolves. Gated by the manifest's
    // `[mcp] trusty_search` toggle, the same gate `mcp_session_env` applies when
    // it builds the variable. Best-effort: an unreachable daemon yields no index
    // and the session searches unpinned rather than failing to launch.
    if plan.inject_trusty_search {
        let _ = register_project_index(project_dir);
    } else {
        tracing::debug!("manifest disables the trusty-search project index");
    }

    // Heal the claude-mpm palace split-brain before the session starts (#1939):
    // when the derived `owner-repo` palace does not exist but the BARE repo-name
    // one does, register an alias so the exported `TRUSTY_MEMORY_PALACE` resolves
    // to the existing store. // #4181: this used to run inside the deleted memory
    // injector; `ensure_managed_config_dir` carries the daemon/interactive paths,
    // and this call carries the standalone `prepare_session` path. Best-effort
    // and side-effect-only; never fails the launch.
    if plan.inject_trusty_memory {
        maybe_register_palace_alias(project_dir, repo_url);
    }

    // Pre-seed per-directory trust for this workspace in `~/.claude.json`
    // (issue #1269) so the interactive tmux Claude session does not stall on the
    // "Do you trust this folder?" dialog and the injected task prompt is
    // received. tm owns this workspace path, so marking it trusted is safe.
    //
    // // #4181: no `enabledMcpjsonServers` is written, and any left by a prior
    // version is stripped — see `settings::preseed_workspace_trust`. Approving a
    // name is what lets a repo's `.mcp.json` entry displace the operator's own
    // user-scope declaration, so removing the approval removes the
    // name-squatting exploit #3918→#3950 kept re-opening rather than defusing it
    // once more. Non-fatal: a failure only means the operator sees the dialog.
    if let Err(err) = preseed_workspace_trust_home(home, project_dir) {
        tracing::warn!("failed to pre-seed workspace trust: {err}");
    }

    // Remove the now-redundant global `trusty-memory` hook entries so they no
    // longer fire for every Claude Code session (including claude-mpm). The
    // project hooks above scope them to trusty-mpm sessions. Non-fatal.
    if let Err(err) = remove_global_trusty_memory_hooks(home) {
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
        // #6286: the derived socket, else a path nothing serves — catch-up set,
        // else the daemon's actual discovered bound address, never a
        // hardcoded port.
        let memory_socket = trusty_common::memory_rpc::resolve_memory_socket_or_unreachable();
        let opts = crate::core::catchup::CatchupOptions {
            project_dir: project_dir.to_path_buf(),
            memory_socket,
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
    // It is deliberately the last step that WRITES, so a refusal is not also a
    // half-provisioned workspace: everything the session needs is already on
    // disk by the time this can fail. #6649 puts exactly one step after it —
    // `launch_asset_notices`, which only READS the tiers this launch just wrote.
    // Nothing that writes, and nothing that depends on the write succeeding, may
    // go below this line.
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
    // #7514: record the fold against THIS preparation's framework root.
    crate::core::instruction_pipeline::write_compiled_prompt_recording_in(
        &fw.root,
        &compiled,
        &resolved_prompt,
    )
    .map_err(|source| PrepError::Instructions {
        path: compiled.clone(),
        source,
    })?;

    // #6649: computed LAST so the skill tier it reads is the one this launch
    // leaves behind, not the one it inherited.
    let asset_notices =
        asset_notices::launch_asset_notices(fw, project_dir, quarantine_report.as_ref());

    Ok(PrepReport {
        deploy,
        skill_deploy,
        instructions,
        stash,
        compiled_prompt: compiled,
        output_style,
        hooks_written,
        catchup_context,
        roster_errors,
        asset_notices,
        memory_reachable,
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
