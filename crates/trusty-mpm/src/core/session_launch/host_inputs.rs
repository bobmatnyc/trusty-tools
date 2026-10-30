//! The host inputs the shared session-prep body takes (split out of `mod.rs`
//! for the 500-SLOC cap, #10459).

use std::path::Path;

/// The ambient host inputs [`super::prepare_session_inner`] would otherwise read from
/// the process itself.
///
/// Why: every field is something production reads from the environment and
/// tests must pin — `home` since #5544, `hook_exe` since #7244. Grouping them keeps
/// [`super::prepare_session_inner`]'s arity where clippy wants it and puts the
/// injections side by side, so the next one has an obvious home.
/// What: `home` is the USER-GLOBAL home (not derivable from `FrameworkPaths` —
/// the managed constructors relocate every `.claude/` path on it onto the
/// workspace); `hook_exe` is the stable binary the project-tier hooks writer
/// should bake, `None` meaning "resolve the running one"; `palace_registry` (#8311) is the trusty-memory registry the palace alias is
/// written to, `None` meaning "the one the trusty-memory daemon reads";
/// `claude_config_dir` (#9846) relocates the user tier off `home`, as
/// `CLAUDE_CONFIG_DIR` does for Claude Code, `None` meaning `<home>/.claude`.
/// Test: `repair_closes_gaps_on_incomplete_workspace`,
/// `session_prep_writes_the_palace_alias_under_the_named_registry`, plus
/// every `prepare_session_*` test.
#[derive(Clone, Copy, Default)]
pub(crate) struct HostInputs<'a> {
    pub(super) home: Option<&'a Path>,
    pub(super) hook_exe: Option<&'a Path>,
    pub(super) palace_registry: Option<&'a Path>,
    pub(super) claude_config_dir: Option<&'a Path>,
}

impl<'a> HostInputs<'a> {
    /// The production shape: a real home, the running binary for hooks, and
    /// the daemon's own palace registry.
    pub(super) fn with_home(home: Option<&'a Path>) -> Self {
        Self {
            home,
            ..Self::default()
        }
    }
}
