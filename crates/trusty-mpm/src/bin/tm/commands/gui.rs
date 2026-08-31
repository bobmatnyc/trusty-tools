//! `tm gui` — launch the separately installed Tauri desktop binary.
//!
//! Why: split out of `main.rs` (#6483) — adding the `--single-pane` TUI
//! routing pushed that file past the 500-SLOC cap, and these three helpers are
//! a self-contained subcommand handler that never belonged in the bootstrap.
//! What: [`launch_gui`] resolves and spawns `trusty-mpm-gui`; the two helpers
//! it calls stay here with it.
//! Test: `tests.rs::gui_not_found_error_has_install_hint`,
//! `tests.rs::gui_binary_resolution_falls_back_to_bare_name`.

/// Launch the Tauri desktop GUI by shelling out to the `trusty-mpm-gui` binary.
///
/// Why: the GUI lives in the separate, publish=false `trusty-mpm-gui` crate
/// (it owns Tauri's `build.rs` + `tauri.conf.json`, which cannot be published
/// cleanly to crates.io). Declaring it as an optional Cargo dependency blocks
/// `cargo publish` for trusty-mpm, so `tm gui` instead launches a separately
/// installed `trusty-mpm-gui` binary — matching the Single-Install convention.
/// What: resolves the `trusty-mpm-gui` executable next to the running `tm`
/// binary (via `current_exe().parent()`), falling back to a bare `trusty-mpm-gui`
/// name so the OS resolves it on `PATH`. Spawns it and waits for it to exit,
/// returning an actionable error if the binary is not installed.
/// Test: the not-found → install-hint mapping is covered by `tests.rs`
/// (`gui_not_found_error_has_install_hint`), which exercises `gui_status_to_result`
/// directly with a synthetic `NotFound` error.
pub(crate) fn launch_gui() -> anyhow::Result<()> {
    let program = resolve_gui_binary();
    gui_status_to_result(std::process::Command::new(&program).status())
}

/// Map the outcome of spawning `trusty-mpm-gui` to a CLI-friendly result.
///
/// Why: factoring the result mapping out of `launch_gui` keeps the actionable
/// "not installed" hint unit-testable without actually spawning a GUI process.
/// What: success → `Ok`; non-zero exit → error with the status; `NotFound`
/// spawn error → the install hint; any other spawn error → a context error.
/// Test: `tests.rs::gui_not_found_error_has_install_hint`.
pub(crate) fn gui_status_to_result(
    status: std::io::Result<std::process::ExitStatus>,
) -> anyhow::Result<()> {
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => anyhow::bail!("trusty-mpm-gui exited with status: {status}"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => anyhow::bail!(
            "trusty-mpm-gui is not installed.\n\
             Install it with: cargo install trusty-mpm-gui\n\
             (the desktop GUI ships as a separate Tauri crate; `tm gui` launches it)"
        ),
        Err(err) => Err(anyhow::Error::new(err).context("failed to launch trusty-mpm-gui")),
    }
}

/// Resolve the path to the `trusty-mpm-gui` executable.
///
/// Why: a `cargo install`-based deployment lands every trusty-* binary in the
/// same directory (`~/.cargo/bin`), so the sibling-of-`tm` lookup is the most
/// reliable. We fall back to the bare binary name so a `PATH`-installed GUI is
/// still found when `current_exe()` is unavailable or the sibling is missing.
/// What: returns `<dir-of-current-exe>/trusty-mpm-gui` when that file exists,
/// otherwise the bare `trusty-mpm-gui` name (resolved by the OS via `PATH`).
/// Test: indirectly exercised by `launch_gui`'s missing-binary test; the
/// sibling-exists branch is environment-dependent and not unit-tested.
pub(crate) fn resolve_gui_binary() -> std::path::PathBuf {
    const GUI_BIN: &str = "trusty-mpm-gui";
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        // Include the platform executable suffix (`.exe` on Windows; "" on
        // macOS/Linux) so the sibling lookup finds the GUI binary on every OS.
        let sibling = dir.join(format!("{GUI_BIN}{}", std::env::consts::EXE_SUFFIX));
        if sibling.is_file() {
            return sibling;
        }
    }
    std::path::PathBuf::from(GUI_BIN)
}
