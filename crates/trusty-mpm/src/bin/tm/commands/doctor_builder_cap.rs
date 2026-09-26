//! The `builder_cap` row of `tm doctor` (#6892, rebuilt for #8261 increment two).
//!
//! Why: a refused or waiting build names its holders at the moment it waits;
//! an operator asking "what is holding my machine" has no build to hang that
//! on, so the machine's lease state gets its own row. Since option D the state
//! is LOCAL — flocks under `~/.trusty-mpm/build-slots/`, the process table, the
//! kernel's memory pressure — so the row reads it directly and needs no daemon.
//!
//! What: the lease holders (from the slot files), the census's foreign builds,
//! the memory-pressure level with its raw signals, the load, and the ceiling —
//! everything `tm build-lease` decides on, from the same `decide` call. FAIL
//! when the lease machinery itself is broken (a slot file that cannot be
//! locked, an unopenable `admission.lock`, an uncreatable slot directory), so a
//! degraded lease is never silent; WARN when a new build would wait or a
//! reading is degraded.
//! Test: the `#[cfg(test)]` suite below.

use trusty_mpm::core::build_lease::admission::{Decision, decide};
use trusty_mpm::core::build_lease::census::{LiveSampler, Sampler};
use trusty_mpm::core::build_lease::config::BuildLeaseConfig;
use trusty_mpm::core::build_lease::slots::{HolderRecord, SlotDir};
use trusty_mpm::core::builders::{BuildersConfig, resolve_max_concurrent};
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

/// This check's name.
const CHECK: &str = "builder_cap";

/// The row for one slot directory, from its holders and one decision.
///
/// Why: separated from the live read so a test can drive it with a real slot
/// directory and scripted readings.
/// What: FAIL naming each broken slot file or an unopenable `admission.lock`;
/// otherwise [`render_check`].
/// Test: `doctor_fails_on_a_broken_slot_file`,
/// `an_idle_machine_reports_ok_with_its_readings`.
pub(crate) fn slot_dir_check(
    slots: &SlotDir,
    config: (&BuildersConfig, &BuildLeaseConfig),
    ceiling: u32,
    sampler: &mut dyn Sampler,
) -> DoctorCheck {
    let mut broken: Vec<String> = slots
        .broken()
        .into_iter()
        .map(|(slot, err)| format!("slot-{slot}.lock ({err})"))
        .collect();
    if let Err(err) = slots.check_admission_lock() {
        broken.push(format!("admission.lock ({err})"));
    }
    if !broken.is_empty() {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Fail,
            format!(
                "the build-lease files in {} are broken: {}. A broken slot is skipped and an \
                 unopenable admission lock falls back to the census-bounded unleased path, so \
                 builds still run, but the cap is degraded until these are removed (#8261).",
                slots.path().display(),
                broken.join("; ")
            ),
        );
    }
    let holders = slots.holders();
    let readings = sampler.sample(&holders);
    let held = u32::try_from(holders.len()).unwrap_or(u32::MAX);
    let decision = decide(config.0, config.1, ceiling, held, &readings);
    render_check(
        &decision,
        &holders,
        &slots.path().display().to_string(),
        &config.0.deprecation_warnings(),
    )
}

/// Render the row from one decision and the holders it counted.
///
/// Test: `an_idle_machine_reports_ok_with_its_readings`.
fn render_check(
    decision: &Decision,
    holders: &[HolderRecord],
    slot_dir: &str,
    warnings: &[String],
) -> DoctorCheck {
    let held = if holders.is_empty() {
        "no build leases held".to_string()
    } else {
        format!(
            "{} build lease(s) held: {}",
            holders.len(),
            holders
                .iter()
                .map(HolderRecord::render)
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    let body = format!(
        "{held}. {} effective slot(s) of ceiling {} (`builders.max_concurrent` in \
         ~/.trusty-mpm/config.toml). Readings: {}. Slot files: {slot_dir}.",
        decision.n_effective, decision.ceiling, decision.readings
    );
    let mut concerns: Vec<String> = decision.withheld.clone();
    concerns.extend(decision.degraded.iter().cloned());
    concerns.extend(warnings.iter().cloned());
    if concerns.is_empty() {
        DoctorCheck::new(CHECK, CheckStatus::Ok, body)
    } else {
        DoctorCheck::new(
            CHECK,
            CheckStatus::Warn,
            format!(
                "{body} A new heavy build would wait or run degraded: {}",
                concerns.join("; ")
            ),
        )
    }
}

/// The live row.
///
/// What: an uncreatable slot directory FAILS; otherwise [`slot_dir_check`]
/// with this machine's readings.
pub(crate) fn builder_cap_row() -> DoctorCheck {
    let builders: BuildersConfig = MpmConfig::load_default().builders;
    let lease = BuildLeaseConfig::load_default();
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    match SlotDir::resolve(&home) {
        Ok(slots) => slot_dir_check(
            &slots,
            (&builders, &lease),
            resolve_max_concurrent(),
            &mut LiveSampler::new(&lease),
        ),
        Err((first, second)) => DoctorCheck::new(
            CHECK,
            CheckStatus::Fail,
            format!(
                "no build-slot directory could be created ({first}; fallback: {second}) — \
                 every `tm build-lease` runs census-bounded without a lease until this is \
                 fixed (#8261)."
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::memory_pressure::{
        MemoryPressure, PressureLevel, PressureSignal, PressureSource,
    };
    use trusty_mpm::core::build_lease::admission::Readings;
    use trusty_mpm::core::build_lease::census::Sampler;
    use trusty_mpm::core::build_probe::BuildGroup;

    struct Quiet;

    impl Sampler for Quiet {
        fn sample(&mut self, _holders: &[HolderRecord]) -> Readings {
            Readings::new(
                Ok(MemoryPressure::new(
                    PressureLevel::Normal,
                    Some(80.0),
                    PressureSource::MacosSysctl,
                    vec![PressureSignal::new(
                        "kern.memorystatus_vm_pressure_level",
                        "1 (normal)",
                    )],
                )),
                Ok(2.0),
                16,
                Ok(Vec::<BuildGroup>::new()),
                "test-host",
            )
        }
    }

    fn slots() -> (tempfile::TempDir, SlotDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let slots = SlotDir::at(tmp.path().join("slots")).expect("slot dir");
        (tmp, slots)
    }

    #[test]
    fn an_idle_machine_reports_ok_with_its_readings() {
        let (_tmp, slots) = slots();
        let mut guard = slots.try_acquire(0).expect("io").expect("free");
        guard
            .write_record(&HolderRecord::new(0, "cargo test -p x", "/repo"))
            .expect("record");
        let cfg = (BuildersConfig::default(), BuildLeaseConfig::default());
        let check = slot_dir_check(&slots, (&cfg.0, &cfg.1), 4, &mut Quiet);
        assert_eq!(check.name, "builder_cap");
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        for needle in [
            "1 build lease(s) held: slot 0: cargo test -p x",
            "kern.memorystatus_vm_pressure_level=1 (normal)",
            "ceiling 4",
        ] {
            assert!(
                check.message.contains(needle),
                "{needle}: {}",
                check.message
            );
        }
        drop(guard);
    }

    /// Critic round 1 (HIGH 1c): a broken slot file or admission lock FAILS.
    #[test]
    fn doctor_fails_on_a_broken_slot_file() {
        let cfg = (BuildersConfig::default(), BuildLeaseConfig::default());
        let (_tmp, broken_slot) = slots();
        std::fs::create_dir(broken_slot.path().join("slot-0.lock")).expect("mkdir");
        let check = slot_dir_check(&broken_slot, (&cfg.0, &cfg.1), 4, &mut Quiet);
        assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
        assert!(check.message.contains("slot-0.lock"), "{}", check.message);

        let (_tmp2, broken_admission) = slots();
        std::fs::create_dir(broken_admission.path().join("admission.lock")).expect("mkdir");
        let check = slot_dir_check(&broken_admission, (&cfg.0, &cfg.1), 4, &mut Quiet);
        assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
        assert!(
            check.message.contains("admission.lock"),
            "{}",
            check.message
        );
    }
}
