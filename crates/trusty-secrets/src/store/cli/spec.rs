//! [`CliSpec`]: the static facts a CLI-backed backend hands the runner.
//!
//! Why: the runner is shared, but the program name, the timeout, the
//! operator hints, and the stderr phrases that mean "no such item" or
//! "signed out" are vendor facts. One `const` per backend keeps vendor text
//! out of the runner and every value out of the spec.
//! What: [`CliSpec`], `&'static` data plus a timeout, built in `const`
//! context with [`CliSpec::new`], [`CliSpec::with_hints`] and
//! [`CliSpec::with_markers`].
//! Test: `classify_marker_table`, `runner_missing_program_is_cli_not_installed`.

use std::time::Duration;

/// Static facts about one vendor CLI.
///
/// Why: see the module docs.
/// What: markers are matched ASCII case-insensitively against stderr, and a
/// locked marker wins over a missing one. The hints appear verbatim in
/// [`crate::SecretsError::CliNotInstalled`] and
/// [`crate::SecretsError::BackendLocked`], so they must be fixed text.
/// Test: `classify_marker_table`.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct CliSpec {
    /// Backend id, e.g. `onepassword`.
    pub backend: &'static str,
    /// The program, a bare name resolved on `PATH`, or an absolute path.
    pub program: &'static str,
    /// How long one run may take before its process group is killed.
    pub timeout: Duration,
    /// Install instruction for [`crate::SecretsError::CliNotInstalled`].
    pub install_hint: &'static str,
    /// Unlock instruction for [`crate::SecretsError::BackendLocked`].
    pub locked_hint: &'static str,
    /// stderr phrases that mean the item does not exist.
    pub missing_markers: &'static [&'static str],
    /// stderr phrases that mean the CLI is locked or signed out.
    pub locked_markers: &'static [&'static str],
}

impl CliSpec {
    /// A spec with empty hints and no markers: every failure reads as
    /// [`super::Verdict::Other`].
    pub const fn new(backend: &'static str, program: &'static str, timeout: Duration) -> Self {
        Self {
            backend,
            program,
            timeout,
            install_hint: "",
            locked_hint: "",
            missing_markers: &[],
            locked_markers: &[],
        }
    }

    /// Set the install and unlock instructions.
    #[must_use]
    pub const fn with_hints(mut self, install: &'static str, locked: &'static str) -> Self {
        self.install_hint = install;
        self.locked_hint = locked;
        self
    }

    /// Set the stderr markers for a missing item and a locked CLI.
    #[must_use]
    pub const fn with_markers(
        mut self,
        missing: &'static [&'static str],
        locked: &'static [&'static str],
    ) -> Self {
        self.missing_markers = missing;
        self.locked_markers = locked;
        self
    }
}
