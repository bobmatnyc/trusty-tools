//! `trusty-installer version` — emit trusty-installer's version + embedded stack_version + contract floor.
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
//! Test: `version_human_line_names_tool_and_stack_version`,
//! `version_json_is_the_capability_discovery_object` (tests/version_output.rs).

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
/// Test: `version_json_is_the_capability_discovery_object`.
pub const PHASE0_STACK_VERSION: &str = "0.0.0-scaffold";

/// Handle the `trusty-installer version` subcommand.
///
/// Why: Dispatches to the renderer so `main.rs` stays as a thin shim.
///
/// What: Calls `output::print_version(json, PHASE0_STACK_VERSION)` and returns
/// exit code `0`.
///
/// Test: `version_human_line_names_tool_and_stack_version`,
/// `version_json_is_the_capability_discovery_object`.
pub fn run(json: bool) {
    output::print_version(json, PHASE0_STACK_VERSION);
}
