//! The `slot_pool_budget` row of `tm doctor` (#8451).
//!
//! Why: the builder slot pool reached 2.6 TB with nothing reporting it until
//! the worktree guard refused. This row puts the pool's slot count and its
//! volume's usage beside the eviction threshold the daemon sweeps at, so an
//! operator sees the budget before the guard trips.
//! What: one local row — no daemon needed — from the pool directory listing
//! (`evict::list_pool_slots`, no byte walk: a terabyte pool cannot be summed
//! inside a doctor run) and one `statvfs` of its volume. Ok under the
//! threshold; Warn at or over it, where the sweep should be evicting; Unknown
//! when the pool cannot be listed or the volume cannot be measured.
//! Test: the `#[cfg(test)]` suite below.

use std::path::Path;

use trusty_mpm::core::build_lease::config::BuildLeaseConfig;
use trusty_mpm::core::build_lease::evict::{
    EVICT_PCT_KEY, PoolSlot, effective_evict_pct, list_pool_slots,
};
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::disk_usage_guard::{MeasuredMount, active_threshold_at, fmt_pct, measure};
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

/// This check's name.
const CHECK: &str = "slot_pool_budget";

/// The row from a listing, a measurement and the two thresholds.
///
/// Test: `an_absent_pool_is_ok`, `an_unlistable_pool_is_unknown`,
/// `an_unmeasurable_volume_is_unknown`, `under_the_threshold_is_ok`,
/// `at_the_threshold_warns`.
pub(crate) fn budget_check(
    pool: &Path,
    slots: std::io::Result<Vec<PoolSlot>>,
    measured: Option<&MeasuredMount>,
    threshold: u8,
    guard: u8,
) -> DoctorCheck {
    let slots = match slots {
        Ok(slots) => slots,
        Err(err) => {
            return DoctorCheck::new(
                CHECK,
                CheckStatus::Unknown,
                format!("the slot pool {} cannot be listed: {err}", pool.display()),
            );
        }
    };
    if !pool.is_dir() {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Ok,
            format!("no builder slot pool yet at {}", pool.display()),
        );
    }
    let Some(m) = measured else {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Unknown,
            format!(
                "the volume holding {} cannot be measured, so the eviction budget cannot be \
                 checked",
                pool.display()
            ),
        );
    };
    let mut repos: Vec<&Path> = slots.iter().filter_map(|s| s.path.parent()).collect();
    repos.sort();
    repos.dedup();
    let raw = m.bytes.map_or_else(String::new, |b| {
        format!(
            " ({:.0} GiB used, {:.0} GiB free)",
            b.used_bytes as f64 / GIB,
            b.available_bytes as f64 / GIB
        )
    });
    let body = format!(
        "{} slot dir(s) in {} repo(s) under {}. Volume {} at {}{raw}; the daemon evicts idle \
         slot dirs oldest-first at {threshold}% (`{EVICT_PCT_KEY}`), below the {guard}% \
         worktree guard.",
        slots.len(),
        repos.len(),
        pool.display(),
        m.mount_point,
        fmt_pct(&m.usage_pct),
    );
    if m.usage_pct < f32::from(threshold) {
        DoctorCheck::new(CHECK, CheckStatus::Ok, body)
    } else {
        DoctorCheck::new(
            CHECK,
            CheckStatus::Warn,
            format!(
                "{body} Eviction is due: if this persists, every slot is in use or the sweep \
                 is off (`TRUSTY_MPM_SLOT_POOL_EVICT`)."
            ),
        )
    }
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// The live row against this machine's pool and config.
pub(crate) fn slot_pool_budget_row() -> DoctorCheck {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
    let pool = MpmConfig::load_default()
        .builders
        .effective_slot_pool_root(&home);
    let guard = active_threshold_at(&home).threshold_pct;
    let threshold =
        effective_evict_pct(BuildLeaseConfig::load_default().slot_pool_evict_pct, guard);
    let measured = pool.is_dir().then(|| measure(&pool)).flatten();
    budget_check(
        &pool,
        list_pool_slots(&pool),
        measured.as_ref(),
        threshold,
        guard,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mount(pct: f32) -> MeasuredMount {
        MeasuredMount {
            mount_point: "/vol".into(),
            usage_pct: pct,
            bytes: None,
        }
    }

    fn pool_with_two_repos() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pool = tmp.path().join("pool");
        for slot in ["o/r/slot-0", "o/r/slot-1", "p/q/slot-0"] {
            std::fs::create_dir_all(pool.join(slot)).expect("slot");
        }
        (tmp, pool)
    }

    #[test]
    fn an_absent_pool_is_ok() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pool = tmp.path().join("absent");
        let check = budget_check(&pool, list_pool_slots(&pool), None, 85, 90);
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        assert!(check.message.contains("no builder slot pool"));
    }

    /// Fail-Open Check: a pool that cannot be listed is not healthy.
    #[test]
    fn an_unlistable_pool_is_unknown() {
        let (_tmp, pool) = pool_with_two_repos();
        std::fs::set_permissions(&pool, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let listed = list_pool_slots(&pool);
        std::fs::set_permissions(&pool, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let check = budget_check(&pool, listed, Some(&mount(50.0)), 85, 90);
        assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
    }

    /// Fail-Open Check: an unmeasured volume is not healthy.
    #[test]
    fn an_unmeasurable_volume_is_unknown() {
        let (_tmp, pool) = pool_with_two_repos();
        let check = budget_check(&pool, list_pool_slots(&pool), None, 85, 90);
        assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
    }

    #[test]
    fn under_the_threshold_is_ok() {
        let (_tmp, pool) = pool_with_two_repos();
        let check = budget_check(&pool, list_pool_slots(&pool), Some(&mount(84.9)), 85, 90);
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        for part in [
            "3 slot dir(s) in 2 repo(s)",
            "84.9%",
            "85%",
            EVICT_PCT_KEY,
            "90%",
        ] {
            assert!(check.message.contains(part), "{part}: {}", check.message);
        }
    }

    #[test]
    fn at_the_threshold_warns() {
        let (_tmp, pool) = pool_with_two_repos();
        let check = budget_check(&pool, list_pool_slots(&pool), Some(&mount(85.0)), 85, 90);
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(check.message.contains("Eviction is due"));
    }
}
