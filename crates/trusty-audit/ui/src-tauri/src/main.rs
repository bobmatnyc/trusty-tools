//! The auditor client's desktop shell (Tauri 2) — phase 1 of epic #5477.
//!
//! Why: the recipient of an audit handoff package is an outsider on their own
//! machine, and DOC-68 §11 settles what this window may be: "a view over
//! `Session::execute`, never a second place a capability can live." So this
//! file is the same kind of shim `crates/trusty-audit/src/main.rs` is for the
//! CLI — window wiring and an invoke handler, with nothing else to diverge
//! from what the library does. The capability set is
//! `trusty_audit::session::Command`, `crate::cli` matches it exhaustively, and
//! a variant without a CLI arm fails to compile; adding a capability here
//! instead would route around that.
//!
//! What: one Tauri command — [`guided::guided`] — over
//! `Session::execute(Command::Guided)`, and one window. Phase 1 adds no
//! `Command` variant.
//!
//! What phase 1 is not: repository selection, tool installation, the run view
//! and the return package are later phases and still run from the
//! `trusty-audit` command line. Bundling, signing and notarisation are #5484
//! and #5481, which is why `tauri.conf.json` sets `bundle.active: false`.
//!
//! Test: `super::guided::view_tests` for the mapping; launching the app for
//! the window itself, which Tauri's event loop does not make unit-testable.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod guided;

use guided::guided;

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![guided])
        .run(tauri::generate_context!())
        .expect("the Tauri runtime failed to start");
}
