//! `tm doctor` macOS TCC responsibility-taint check (issue #2997).
//!
//! Why: on macOS, an interactive `claude` launched inside a trusty-mpm-managed
//! tmux pane trips the TCC gate (App-Data / `RemovableVolumes`), and tccd walks
//! the pane's responsibility chain `claude → zsh → shared tmux server` and
//! attributes the prompt to the shared server. #3037 + the launcher-disclaim
//! extension route the pane's `claude` through `internal-spawn-disclaimed` so
//! it becomes its OWN responsible process — but that whole mechanism was
//! previously INVISIBLE to `tm doctor` (the diagnosis noted it had no TCC line
//! at all). This probe surfaces whether the disclaim is actually in force so
//! the taint class is diagnosable instead of silent.
//! What: [`check_tcc_taint`] gathers three facts — is this macOS, does the
//! private disclaim SPI resolve, and is the `TM_DISABLE_SPAWN_DISCLAIM` escape
//! hatch set — and folds them via the pure [`build_tcc_taint_check`]. Read-only
//! and instantaneous (no `log show`, no process walk): it reports the disclaim
//! wiring's status, not a live TCC event scan.
//! Test: `tcc_taint_ok_when_disclaim_active`, `tcc_taint_warn_when_disabled`,
//! `tcc_taint_warn_when_spi_absent`, `tcc_taint_ok_on_non_macos`.

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Probe whether managed panes spawn `claude` with TCC responsibility
/// disclaimed (issue #2997).
///
/// Why: see the module docs — makes the disclaim mechanism's state visible in
/// `tm doctor` so an operator seeing a "tmux"/"trusty-mpm would like to
/// access…" prompt can tell whether the attribution fix is active.
/// What: reads the three real inputs (macOS build, disclaim SPI availability
/// via [`crate::core::spawn_disclaim::disclaim_spi_available`], escape-hatch
/// state via [`crate::core::spawn_disclaim::disclaim_disabled_by_env`]) and
/// delegates the verdict to [`build_tcc_taint_check`].
/// Test: covered indirectly by the `build_tcc_taint_check` unit tests (the pure
/// verdict) — this wrapper only wires in the live inputs.
pub fn check_tcc_taint() -> DoctorCheck {
    let is_macos = cfg!(target_os = "macos");
    let spi_available = crate::core::spawn_disclaim::disclaim_spi_available();
    let disabled_by_env = crate::core::spawn_disclaim::disclaim_disabled_by_env();
    build_tcc_taint_check(is_macos, spi_available, disabled_by_env)
}

/// Fold the disclaim-state facts into a [`DoctorCheck`] (pure).
///
/// Why: keeping the verdict logic pure makes every branch — active, disabled,
/// SPI-absent, non-macOS — unit-testable without a real macOS or process env.
/// What: `Ok` on non-macOS (no TCC exists there). On macOS: `Warn` when the
/// escape hatch is set (disclaim off), `Warn` when the private SPI does not
/// resolve (can't disclaim on this OS), else `Ok` with the scope caveat that a
/// FIRST prompt may still appear — now attributed to Claude Code itself, which
/// the operator can grant once. Never `Fail`: a missing disclaim is a degraded
/// attribution state, not a broken stack.
/// Test: `tcc_taint_ok_when_disclaim_active`, `tcc_taint_warn_when_disabled`,
/// `tcc_taint_warn_when_spi_absent`, `tcc_taint_ok_on_non_macos`.
fn build_tcc_taint_check(
    is_macos: bool,
    spi_available: bool,
    disabled_by_env: bool,
) -> DoctorCheck {
    if !is_macos {
        return DoctorCheck::new(
            "tcc_taint",
            CheckStatus::Ok,
            "not applicable on this platform (macOS TCC only)",
        );
    }
    if disabled_by_env {
        return DoctorCheck::new(
            "tcc_taint",
            CheckStatus::Warn,
            "TM_DISABLE_SPAWN_DISCLAIM is set — managed panes launch `claude` WITHOUT the \
             TCC disclaim, so its data-access prompts are attributed to the shared tmux \
             server (issue #2997). Unset it to re-enable the disclaim.",
        );
    }
    if !spi_available {
        return DoctorCheck::new(
            "tcc_taint",
            CheckStatus::Warn,
            "the macOS responsibility-disclaim SPI did not resolve on this build — managed \
             panes cannot disclaim TCC responsibility, so `claude`'s data-access prompts may \
             be attributed to the shared tmux server (issue #2997).",
        );
    }
    DoctorCheck::new(
        "tcc_taint",
        CheckStatus::Ok,
        "managed panes spawn `claude` with TCC responsibility disclaimed (issue #2997 \
         attribution fix active). Note: a first-time data-access prompt may still appear — \
         now attributed to Claude Code itself, not tmux/trusty-mpm — and can be granted once.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcc_taint_ok_when_disclaim_active() {
        let c = build_tcc_taint_check(true, true, false);
        assert_eq!(c.status, CheckStatus::Ok);
        assert_eq!(c.name, "tcc_taint");
        assert!(c.message.contains("attribution fix active"));
    }

    #[test]
    fn tcc_taint_warn_when_disabled() {
        // Escape hatch set → disclaim off → warn, even though the SPI is present.
        let c = build_tcc_taint_check(true, true, true);
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.message.contains("TM_DISABLE_SPAWN_DISCLAIM"));
    }

    #[test]
    fn tcc_taint_warn_when_spi_absent() {
        let c = build_tcc_taint_check(true, false, false);
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.message.contains("SPI"));
    }

    #[test]
    fn tcc_taint_ok_on_non_macos() {
        // Off macOS there is no TCC — never warn regardless of the other inputs.
        let c = build_tcc_taint_check(false, false, true);
        assert_eq!(c.status, CheckStatus::Ok);
        assert!(c.message.contains("not applicable"));
    }

    #[test]
    fn tcc_taint_is_advisory_only() {
        // This probe must never Fail — a missing disclaim is degraded, not broken.
        for (macos, spi, disabled) in [
            (true, true, false),
            (true, true, true),
            (true, false, false),
            (true, false, true),
            (false, false, false),
        ] {
            assert_ne!(
                build_tcc_taint_check(macos, spi, disabled).status,
                CheckStatus::Fail
            );
        }
    }
}
