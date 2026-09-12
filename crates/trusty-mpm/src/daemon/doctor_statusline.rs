//! The `tm doctor` `statusline` check — can the `💸` segment render at all (#7617).
//!
//! Why: the savings segment has gone dark three times (#7209, #7245, #7617) and
//! each time the only evidence was its absence. Nothing anywhere answered "is
//! the status bar wired, are its inputs readable, and would it render?" — so
//! every investigation restarted from a screenshot. The owner's ruling is that
//! the segment is core setup guaranteed by the framework, and a guarantee with
//! no check is an intention.
//!
//! What: [`check_statusline`] probes the three things that have actually broken
//! — the `statusLine` command in the settings tiers (present, and pointing at a
//! binary that exists), the savings ledger and per-session statusline record
//! store (readable, not corrupt), and the render rule itself against a synthetic
//! fold. `Fail` only when the segment CANNOT render; `Warn` when it can but has
//! nothing to show yet. Read-only; the repair is `tm doctor --fix`'s
//! `repair_statusline` step.
//! Test: the `tests` module below.

use std::path::{Path, PathBuf};

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The check's name, asserted on by the tests and by `tm doctor --json`.
pub(crate) const NAME: &str = "statusline";

/// What one settings tier says about `statusLine`.
///
/// Why: "absent" and "present but pointing at a binary that is gone" need
/// different remediation, and #7262's build-tree damage is the second kind.
/// Test: `a_tier_with_no_entry_is_reported_missing`,
/// `a_tier_with_a_stale_command_is_reported_stale`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TierState {
    /// No `statusLine` key at all.
    Missing,
    /// Present, but the command's binary is ephemeral or no longer on disk.
    Stale,
    /// Present and pointing at a binary that exists.
    Wired,
}

/// Read one settings file's `statusLine` verdict.
///
/// What: [`TierState::Missing`] for an absent key, an unreadable file, or one
/// that is not a JSON object — none of those can carry a working entry.
/// Test: `a_tier_with_no_entry_is_reported_missing`,
/// `a_tier_with_a_stale_command_is_reported_stale`,
/// `a_tier_with_a_live_command_is_reported_wired`.
pub(crate) fn tier_state(settings_path: &Path) -> TierState {
    let Some(value) = super::doctor_hooks_hygiene::read_settings(settings_path) else {
        return TierState::Missing;
    };
    match value.get("statusLine") {
        None => TierState::Missing,
        Some(entry) if crate::core::session_launch::is_stale_statusline_command(entry) => {
            TierState::Stale
        }
        Some(_) => TierState::Wired,
    }
}

/// Probe whether the `💸` segment can render for this machine.
///
/// Why: see the module header.
/// What: gathers the settings tiers in play — the project's, and the user's
/// `~/.claude/settings.json` — plus whether the savings ledger and the
/// per-session statusline record store under `framework_root` are readable, and
/// folds them through [`build_statusline_check`].
/// Test: the fold's branches are covered directly; this wrapper only binds the
/// real probes to it.
pub(super) fn check_statusline(project_dir: Option<&Path>, framework_root: &Path) -> DoctorCheck {
    let mut tiers: Vec<(PathBuf, TierState)> = Vec::new();
    if let Some(project) = project_dir {
        let path = project.join(".claude").join("settings.json");
        tiers.push((path.clone(), tier_state(&path)));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let path = Path::new(&home).join(".claude").join("settings.json");
        tiers.push((path.clone(), tier_state(&path)));
    }
    build_statusline_check(
        &tiers,
        ledger_state(&crate::core::savings::savings_log_in(framework_root)),
        store_is_readable(&framework_root.join("usage")),
    )
}

/// Whether the ledger can be read, and whether it holds anything.
///
/// Why (Fail-Open Check): an unreadable ledger is NOT an empty one, and the
/// segment's empty state cannot tell them apart by design — it claims no
/// number either way. The doctor is where that distinction has to surface.
/// What: `Err` naming the reason when the file exists but cannot be read;
/// `Ok(row_count)` otherwise, with `0` for an absent file.
/// Test: `an_unreadable_ledger_fails`, `an_absent_ledger_warns`.
pub(crate) fn ledger_state(ledger: &Path) -> Result<usize, String> {
    match std::fs::read_to_string(ledger) {
        Ok(text) => Ok(text.lines().filter(|line| !line.trim().is_empty()).count()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(err.to_string()),
    }
}

/// Whether the per-session statusline record store can be listed.
///
/// What: `true` for a readable directory AND for an absent one — nothing has
/// written a session record yet on a fresh install, which is not a fault.
/// `false` only when the directory exists and cannot be listed.
/// Test: `an_unlistable_store_fails`.
pub(crate) fn store_is_readable(usage_dir: &Path) -> bool {
    !usage_dir.exists() || std::fs::read_dir(usage_dir).is_ok()
}

/// Fold the readings into a verdict (pure).
///
/// Why: every branch is then assertable without a settings file, a ledger or a
/// home directory, which is what keeps this check deterministic on CI.
/// What: `Fail` when NO tier carries a usable entry, when the ledger cannot be
/// read, when the record store cannot be listed, or when the render rule itself
/// declines a synthetic fold — those are the four ways the segment cannot
/// render. `Warn` when it can render but has no rows yet, or when some tier is
/// stale while another is wired. `Ok` otherwise, naming the row count.
/// Test: `a_wired_machine_with_rows_is_ok`, `no_tier_wired_fails`,
/// `a_stale_tier_beside_a_wired_one_warns`, `an_unreadable_ledger_fails`,
/// `an_absent_ledger_warns`, `an_unlistable_store_fails`.
pub(crate) fn build_statusline_check(
    tiers: &[(PathBuf, TierState)],
    ledger: Result<usize, String>,
    store_readable: bool,
) -> DoctorCheck {
    if !renders_for_a_synthetic_fold() {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Fail,
            "the 💸 segment's render rule produced no figure for a synthetic fold, \
             so it cannot render for a real one either. See issue #7617."
                .to_string(),
        );
    }
    if !store_readable {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Fail,
            "the per-session statusline record store under the framework root \
             cannot be listed, so the segment has no denominator to price \
             against. Remediation: check the permissions on \
             `~/.trusty-mpm/usage/`."
                .to_string(),
        );
    }
    let rows = match ledger {
        Err(reason) => {
            return DoctorCheck::new(
                NAME,
                CheckStatus::Fail,
                format!(
                    "the savings ledger exists but cannot be read ({reason}), so the \
                     💸 segment shows its empty state for every session and no \
                     reading distinguishes that from a genuine zero. Remediation: \
                     `tm repair savings-ledger`."
                ),
            );
        }
        Ok(rows) => rows,
    };

    let wired: Vec<&PathBuf> = tiers
        .iter()
        .filter(|(_, state)| *state == TierState::Wired)
        .map(|(path, _)| path)
        .collect();
    let stale: Vec<&PathBuf> = tiers
        .iter()
        .filter(|(_, state)| *state == TierState::Stale)
        .map(|(path, _)| path)
        .collect();

    if wired.is_empty() {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Fail,
            format!(
                "no settings tier carries a usable `statusLine` command ({} tier(s) \
                 checked, {} stale), so Claude Code renders no status bar at all and \
                 the 💸 segment can never appear. Remediation: `tm doctor --fix --yes`, \
                 or start a session — provisioning seeds both tiers (#7617).",
                tiers.len(),
                stale.len()
            ),
        );
    }
    if let Some(path) = stale.first() {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Warn,
            format!(
                "`statusLine` is wired in {} tier(s), but {} points at a binary that \
                 is ephemeral or no longer on disk — whichever tier Claude Code \
                 resolves first decides what renders. Remediation: \
                 `tm doctor --fix --yes` (#7617).",
                wired.len(),
                path.display()
            ),
        );
    }
    if rows == 0 {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Warn,
            "`statusLine` is wired and every input is readable, but the savings \
             ledger holds no rows yet, so the 💸 segment shows its empty state. \
             Start a session and re-run; `tm doctor`'s `instruction_compression` \
             check says whether this project's fold produces any."
                .to_string(),
        );
    }
    DoctorCheck::new(
        NAME,
        CheckStatus::Ok,
        format!(
            "`statusLine` is wired in {} settings tier(s), the savings ledger holds \
             {rows} row(s), and the 💸 render rule produces a figure",
            wired.len()
        ),
    )
}

/// Whether the render rule yields a figure for a fold that plainly has one.
///
/// Why (#7617 closure condition 2): "the segment renders for a synthetic
/// payload". The renderer itself lives in the `tm` binary, but the RULE that
/// decides whether a figure exists is [`crate::core::savings::SavingsTotal`]'s,
/// and that is what has to hold for anything to be drawn.
/// What: a 25%-saving fold must price to `Some`.
/// Test: `a_wired_machine_with_rows_is_ok` exercises the true arm; the false
/// arm is unreachable by construction and is a guard, not a branch to cover.
fn renders_for_a_synthetic_fold() -> bool {
    crate::core::savings::SavingsTotal {
        tokens_saved: 1_000,
        tokens_before: 4_000,
        cost_saved_usd: 0.01,
        rows: 1,
    }
    .percent_saved(None)
    .is_some()
}

#[cfg(test)]
#[path = "doctor_statusline_tests.rs"]
mod tests;
