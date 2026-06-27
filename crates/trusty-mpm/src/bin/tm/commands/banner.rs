//! Handler for `tm banner` — standalone preview of the launch banner.
//!
//! Why: the robot-head banner only appears inside `tm launch` / `tm connect`,
//! which immediately spawn Claude. A dedicated preview subcommand lets operators
//! (and CI screenshot jobs) eyeball the colored ASCII art + welcome panel
//! without starting a session.
//! What: `run_banner_preview` delegates to
//! `formatters::banner::print_banner_preview`, which renders the robot art
//! (without the full-screen clear) + the rich info-box panel (without the
//! 1-second sleep).
//! Test: `cli_parses_banner`, `cli_parses_banner_reconnecting` in `tests.rs`.

/// Render and print the launch banner for interactive preview, then exit 0.
///
/// Why: decouples the preview entry point from the formatter so `main` stays
/// thin and the handler can evolve (e.g. add JSON output, width flags) without
/// touching the core formatter.
/// What: calls `formatters::banner::print_banner_preview(reconnecting)` which
/// renders the robot splash (no screen-clear) + welcome panel (no sleep).
/// Test: `cli_parses_banner`, `cli_parses_banner_reconnecting`.
pub(crate) fn run_banner_preview(reconnecting: bool) -> anyhow::Result<()> {
    crate::formatters::banner::print_banner_preview(reconnecting);
    Ok(())
}
