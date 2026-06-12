//! `tctl version` — emit tctl's own version + embedded stack_version + contract floor.
//!
//! Why: `version` is the capability-discovery entry point (DOC-1 D3b / DOC-5
//! §4.2). It is fully implemented in Phase 0 so the CLI surface is immediately
//! discoverable even before the backend is wired.
//!
//! What: Calls `output::print_version` with the crate version (from
//! `CARGO_PKG_VERSION`) and a Phase-0 placeholder `stack_version`. When
//! `--json` is set, emits the DOC-5 capability-discovery object
//! `{"tool","tool_version","stack_version","contract_floor","contract_target"}`.
//!
//! Test: Run `tctl version` and assert stdout contains `"tctl v"`. Run
//! `tctl version --json` and parse the JSON; assert `"tool"` == `"trusty-controller"`.

use crate::output;

/// The Phase-0 placeholder stack version.
///
/// Why: The full `stack_version` (DOC-2 §1 `YYYY.MM-N` label) requires the
/// manifest to be loaded and the BOM tuple to be reconciled against the
/// installed members. Phase 0 uses a constant placeholder; the real value is
/// introduced in the Phase-1 manifest/dispatch work.
///
/// What: A constant `&str` used by `run` and surfaced in the `--json` output.
///
/// Test: The integration test for `tctl version --json` asserts
/// `stack_version == PHASE0_STACK_VERSION`.
pub const PHASE0_STACK_VERSION: &str = "0.0.0-scaffold";

/// Handle the `tctl version` subcommand.
///
/// Why: Dispatches to the renderer so `main.rs` stays as a thin shim.
///
/// What: Calls `output::print_version(json, PHASE0_STACK_VERSION)` and returns
/// exit code `0`.
///
/// Test: Call `run(false)` and `run(true)` in a test; both should not panic
/// and the underlying `print_version` is tested in `output::tests`.
pub fn run(json: bool) {
    output::print_version(json, PHASE0_STACK_VERSION);
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `PHASE0_STACK_VERSION` is non-empty (sanity check).
    #[test]
    fn stack_version_non_empty() {
        assert!(!PHASE0_STACK_VERSION.is_empty());
    }

    /// `run` does not panic for either output mode.
    #[test]
    fn run_does_not_panic() {
        // We can't easily assert on stdout in a unit test, but we can
        // confirm the function returns normally.
        run(false);
        run(true);
    }
}
