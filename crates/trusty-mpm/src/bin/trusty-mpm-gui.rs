//! trusty-mpm GUI shim — delegates to the Tauri app in `trusty-mpm-gui`.
//!
//! Why: the Tauri build system requires `build.rs` + `tauri.conf.json` in the
//! crate root, so the GUI logic stays in its own `trusty-mpm-gui` crate. This
//! thin shim exposes the GUI as a `[[bin]]` target of the unified `trusty-mpm`
//! crate so `cargo build -p trusty-mpm --features gui --bin trusty-mpm-gui`
//! produces a working binary without a separate install step.
//! What: suppresses the console window on Windows release builds, then calls
//! `trusty_mpm_gui::run()`.
//! Test: `cargo build -p trusty-mpm --features gui --bin trusty-mpm-gui` must
//! compile; the window opens and `invoke('check_health')` returns a boolean.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    trusty_mpm_gui::run();
}
