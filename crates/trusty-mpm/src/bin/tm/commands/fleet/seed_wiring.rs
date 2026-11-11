//! The wiring a kept core fleet script must carry for this release (#11058).
//!
//! Why: `tm fleet init` keeps an edited `scripts/fleet-poll.py` and refreshes
//! its unedited siblings. A kept poller that never calls `owner_answer_alerts`
//! then runs beside a `fleet_poll_alerts.py` that provides it, so owner_answer
//! alerts never fire and the only output was the generic "custom copy kept"
//! line.
//! What: [`REQUIRED`] lists, per core script, the symbol it must name for the
//! seeded siblings of this release to work. [`kept_copy_warnings`] turns the
//! missing ones into `WARNING` steps for `init`; [`doctor_row`] reports the
//! same finding as the `fleet_seed_wiring` row of `tm doctor`. Neither writes
//! anything: the keep/replace decision stays in `seed::deploy_file`.
//! Test: `an_edited_poller_without_owner_answer_wiring_is_warned_11058`,
//! `an_edited_poller_with_owner_answer_wiring_is_not_warned_11058`,
//! `every_required_wiring_is_in_its_shipped_seed_11058`,
//! `doctor_names_a_kept_poller_without_owner_answer_wiring_11058`,
//! `doctor_warns_when_a_kept_script_cannot_be_read_11058`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

use super::super::Step;

/// This check's `tm doctor` row name.
pub(crate) const CHECK: &str = "fleet_seed_wiring";

/// One symbol a core fleet script must name.
pub(crate) struct Wiring {
    /// The script, below the Architect directory.
    pub(crate) dest: &'static str,
    /// The text the script must contain.
    pub(crate) symbol: &'static str,
    /// What is lost without it, for the report.
    pub(crate) capability: &'static str,
    /// The seeded sibling that provides or emits it.
    pub(crate) sibling: &'static str,
}

/// Every symbol this release's seeded siblings need from a core script.
///
/// Why: the one list a release edits when it adds a sibling capability.
/// What: each `symbol` must appear in the shipped `dest` and its bare form in
/// the shipped `sibling`; `every_required_wiring_is_in_its_shipped_seed_11058`
/// fails when the seed assets and this list disagree.
/// Test: `every_required_wiring_is_in_its_shipped_seed_11058`.
pub(crate) const REQUIRED: &[Wiring] = &[
    Wiring {
        dest: "scripts/fleet-poll.py",
        symbol: "owner_answer_alerts",
        capability: "owner_answer alerts (dashboard answers not yet relayed)",
        sibling: "scripts/fleet_poll_alerts.py",
    },
    Wiring {
        dest: "scripts/fleet-classify.py",
        symbol: "\"owner_answer\"",
        capability: "the owner_answer alert kind and its re-emit TTL",
        sibling: "scripts/fleet_poll_alerts.py",
    },
];

/// Whether `haystack` contains `needle` as a byte run.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

/// What a copy of `dest` lacking `w.symbol` loses, and the two remedies.
fn lack_detail(dest: &str, w: &Wiring) -> String {
    format!(
        "{dest} lacks {}: it never names `{}`, which this release's {} relies on; \
         replace it with the shipped seed (move it aside and re-run `tm fleet init`) \
         or merge `{}` from the seed into it",
        w.capability, w.symbol, w.sibling, w.symbol
    )
}

/// One finding per [`REQUIRED`] symbol `bytes` (a copy of `dest`) lacks.
fn missing(dest: &str, bytes: &[u8]) -> Vec<String> {
    REQUIRED
        .iter()
        .filter(|w| w.dest == dest && !contains(bytes, w.symbol.as_bytes()))
        .map(|w| lack_detail(dest, w))
        .collect()
}

/// The `WARNING` steps for a kept custom copy of `dest` holding `bytes`.
///
/// Why: #11058, a kept core script silently disabled a sibling's feature.
/// What: one `Skipped("WARNING …")` per missing symbol, the shape
/// `records_era_steps` uses; empty when the copy carries the wiring or `dest`
/// is not a core script.
/// Test: `an_edited_poller_without_owner_answer_wiring_is_warned_11058`,
/// `an_edited_poller_with_owner_answer_wiring_is_not_warned_11058`.
pub(crate) fn kept_copy_warnings(dest: &str, bytes: &[u8]) -> Vec<Step> {
    missing(dest, bytes)
        .into_iter()
        .map(|d| Step::Skipped(format!("WARNING {d}")))
        .collect()
}

/// The findings for every core script below each Architect directory.
///
/// What: an absent script is no finding (`init` writes it); a script that
/// cannot be read is a finding, never a pass (#11058 Fail-Open).
/// Test: `doctor_warns_when_a_kept_script_cannot_be_read_11058`.
pub(crate) fn findings(dirs: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    for dir in dirs {
        let mut dests: Vec<&str> = REQUIRED.iter().map(|w| w.dest).collect();
        dests.sort_unstable();
        dests.dedup();
        for dest in dests {
            let path = dir.join(dest);
            match std::fs::read(&path) {
                Ok(bytes) => out.extend(
                    missing(dest, &bytes)
                        .into_iter()
                        .map(|d| format!("{}: {d}", dir.display())),
                ),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => out.push(format!(
                    "cannot read {}: {err}; its wiring is unchecked",
                    path.display()
                )),
            }
        }
    }
    out
}

/// The `fleet_seed_wiring` row for the Architects recorded under `home`.
///
/// Why: #11058, `tm doctor` must name a kept core script that lacks this
/// release's wiring, not only the next `tm fleet init`.
/// What: reads `~/.trusty-mpm/config.toml` strictly; no file or no recorded
/// Architect is `Ok`; a config that does not parse is `Unknown`; any
/// [`findings`] entry is `Warn`, joined.
/// Test: `doctor_names_a_kept_poller_without_owner_answer_wiring_11058`,
/// `doctor_warns_when_a_kept_script_cannot_be_read_11058`.
pub(crate) fn doctor_row(home: &Path) -> DoctorCheck {
    let path = super::super::user_config_path(home);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return DoctorCheck::new(CHECK, CheckStatus::Ok, "no Architect recorded");
        }
        Err(err) => {
            return DoctorCheck::new(
                CHECK,
                CheckStatus::Unknown,
                format!("cannot read {}: {err}", path.display()),
            );
        }
    };
    let config = match super::super::config::parse_user_config(&raw, &path) {
        Ok((_, config)) => config,
        Err(err) => return DoctorCheck::new(CHECK, CheckStatus::Unknown, format!("{err:#}")),
    };
    let dirs = super::super::config::architects(&config);
    if dirs.is_empty() {
        return DoctorCheck::new(CHECK, CheckStatus::Ok, "no Architect recorded");
    }
    let found = findings(&dirs);
    if found.is_empty() {
        DoctorCheck::new(
            CHECK,
            CheckStatus::Ok,
            "every core fleet script carries this release's wiring",
        )
    } else {
        DoctorCheck::new(CHECK, CheckStatus::Warn, found.join("; "))
    }
}
