//! [`PrepError`], the typed failure of session preparation.
//!
//! Why (#11821): `mod.rs` sat at the 500-SLOC cap, and the fail-closed
//! `install.toml` variant would push it over. The error type and its
//! fatal-or-not policy are one cohesive unit; this is a pure relocation,
//! re-exported from `super` so every `session_launch::PrepError` path stays.
//! What: the enum and [`PrepError::is_fatal`].
//! Test: `instruction_failure_is_fatal`, `deploy_and_io_failures_stay_non_fatal`.

use std::path::PathBuf;

/// A failure raised while preparing a session for launch.
///
/// Why: preparation performs agent deployment and filesystem I/O; callers need
/// a single typed error surface that names which stage failed.
/// What: variants for the agent-deploy stage and the instruction stage.
/// Test: not exercised by the happy-path tests; surfaced on invalid paths.
// #11443: new variants must not break downstream exhaustive matches.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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
    ///   * [`build_instructions`](crate::core::instruction_pipeline::build_instructions) failing to compose or write the merged
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
    /// No instructional content resolved, so the PM instructions cannot be
    /// composed at all (#9012). Fatal for the same reason
    /// [`Self::Instructions`] is; the message names `tm content install`.
    /// Test: `content_failure_is_fatal`.
    #[error("cannot compose the PM instructions: {source}")]
    Content {
        /// The content resolution or load failure.
        source: crate::core::content_source::AgentContentError,
    },
    /// #10459: an Architect launch on a Claude Code below v2.1.295, or one
    /// whose version cannot be read. Fatal: refused before any write.
    #[error("{0}")]
    ClaudeVersion(#[from] crate::core::architect_claude_version::ArchitectVersionError),
    /// #10612: an Architect's project hooks could not be written, so it would
    /// run without the exec-form pm-guard. Fatal: the launch is refused.
    /// Test: `an_architect_launch_whose_hook_write_fails_is_refused_10612`.
    #[error("refusing the Architect launch: pm-guard hooks not written to {path}: {source}")]
    ArchitectHooks {
        /// The project settings file the hooks belong in.
        path: PathBuf,
        /// The write failure.
        source: Box<PrepError>,
    },
    /// #11821: `install.toml` exists but is malformed or unreadable, so the
    /// config dir is unknown. Fatal: refused before any write, never `$HOME`.
    /// Test: `isolated_prep_refuses_a_bad_install_toml_11821`.
    #[error("refusing to prepare the session: {0}")]
    InstallPaths(#[from] trusty_common::install_paths::InstallPathsError),
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
        // #9012: no content means no instructions — the same condition.
        matches!(
            self,
            Self::Instructions { .. }
                | Self::Content { .. }
                | Self::ClaudeVersion(_)
                | Self::ArchitectHooks { .. }
                | Self::InstallPaths(_) // #11821: unknown config dir refuses
        )
    }
}
