//! The `deps_budget` row of `tm doctor` (#10212).
//!
//! Why: an 844,490-entry shared `debug/deps` stalled syspolicyd on the Mac,
//! and nothing reported the growth beforehand. This row names each shared
//! target and builder slot whose `debug/deps` has reached the warn line, so an
//! operator rotates it before the host stalls.
//! What: one local row, no daemon. The shared targets are the
//! `<owner>/<repo>` directories under `~/.trusty-tools/cargo-target`; the slots
//! come from `evict::list_pool_slots`. Each `debug/deps` gets one bounded count
//! (`deps_budget::count_deps`, never a full readdir). Warn when any count
//! reaches the line; Unknown when a dir or a root cannot be listed; else Ok. A
//! missing `debug/deps` is listed as missing, never as 0 entries.
//! Test: the `#[cfg(test)]` suite below.

use std::path::{Path, PathBuf};

use trusty_mpm::core::build_lease::deps_budget::{DEPS_WARN_ENTRIES, DepsCount, count_deps};
use trusty_mpm::core::build_lease::evict::list_pool_slots;
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

/// This check's name.
const CHECK: &str = "deps_budget";

/// The `<owner>/<repo>` target directories under the shared root.
///
/// What: two directory levels, the layout `default_cargo_target_dir_at`
/// writes. A missing root is no targets; an owner that cannot be listed is
/// an error, so a hidden target is never reported as healthy.
fn shared_targets(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let owners = match std::fs::read_dir(root) {
        Ok(owners) => owners,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut targets = Vec::new();
    for owner in owners {
        let owner = owner?.path();
        if !owner.is_dir() {
            continue;
        }
        for repo in std::fs::read_dir(&owner)? {
            let repo = repo?.path();
            if repo.is_dir() {
                targets.push(repo);
            }
        }
    }
    targets.sort();
    Ok(targets)
}

/// One count rendered for the row.
fn verdict(count: &DepsCount) -> String {
    match count {
        DepsCount::Under(n) => format!("ok, {} entries", thousands(*n)),
        DepsCount::AtLeast(n) => format!("WARN, >= {} entries", thousands(*n)),
        DepsCount::Missing => "missing (nothing built in this profile)".into(),
        DepsCount::Unreadable(err) => format!("unreadable ({err})"),
        _ => "unrecognised count".into(),
    }
}

/// `1234567` as `1,234,567`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The row for the targets under `shared_root` and the slots under `pool_root`.
///
/// Why: the shared target is where unleased builds write, and a slot is
/// seeded from it, so both are measured (#10212 plan, Part 3).
/// What: `<target>/debug/deps` counted against `line` for every target and
/// slot. Warn outranks Unknown, which outranks Ok, so one oversized dir is
/// never hidden behind an unreadable sibling.
/// Test: `deps_budget_warns_over_the_line`, `deps_budget_is_ok_under_the_line`,
/// `a_missing_deps_dir_is_reported_missing_not_zero`,
/// `an_unreadable_deps_dir_is_unknown`, `an_unlistable_root_is_unknown`.
pub(crate) fn deps_budget_check(shared_root: &Path, pool_root: &Path, line: usize) -> DoctorCheck {
    let mut parts = Vec::new();
    let mut status = CheckStatus::Ok;
    let mut targets = Vec::new();
    match shared_targets(shared_root) {
        Ok(found) => targets.extend(found),
        Err(err) => {
            status = CheckStatus::Unknown;
            parts.push(format!(
                "{} cannot be listed ({err})",
                shared_root.display()
            ));
        }
    }
    match list_pool_slots(pool_root) {
        Ok(slots) => targets.extend(slots.into_iter().map(|s| s.path)),
        Err(err) => {
            status = CheckStatus::Unknown;
            parts.push(format!("{} cannot be listed ({err})", pool_root.display()));
        }
    }
    let mut over = false;
    for target in &targets {
        let deps = target.join("debug").join("deps");
        let count = count_deps(&deps, line);
        match count {
            DepsCount::AtLeast(_) => over = true,
            DepsCount::Under(_) | DepsCount::Missing => {}
            // Unreadable, or a variant this row does not know: never Ok.
            _ => status = CheckStatus::Unknown,
        }
        parts.push(format!("{}: {}", deps.display(), verdict(&count)));
    }
    if targets.is_empty() && status == CheckStatus::Ok {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Ok,
            format!(
                "no shared target under {} and no builder slot under {} yet",
                shared_root.display(),
                pool_root.display()
            ),
        );
    }
    let body = format!(
        "warn line {} entries in debug/deps. {}.",
        thousands(line),
        parts.join("; ")
    );
    if over {
        DoctorCheck::new(
            CHECK,
            CheckStatus::Warn,
            format!(
                "{body} A deps dir this large stalls syspolicyd on every exec. With no build \
                 running in it, rename that target's `debug/` aside (cargo rebuilds it cold) \
                 and delete the old tree in the background (#10212)."
            ),
        )
    } else {
        DoctorCheck::new(CHECK, status, body)
    }
}

/// The live row against this machine's shared root and slot pool.
pub(crate) fn deps_budget_row() -> DoctorCheck {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let shared_root = home
        .join(trusty_common::crate_config::TRUSTY_TOOLS_DIR)
        .join(trusty_mpm::core::build_env::CARGO_TARGET_SUBDIR);
    let pool_root = MpmConfig::load_default()
        .builders
        .effective_slot_pool_root(&home);
    deps_budget_check(&shared_root, &pool_root, DEPS_WARN_ENTRIES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A shared root and a pool root, each with one target holding `n` deps entries.
    fn roots(shared_n: usize, slot_n: usize) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let shared = tmp.path().join("cargo-target");
        let pool = tmp.path().join("pool");
        for (deps, n) in [
            (shared.join("o/r/debug/deps"), shared_n),
            (pool.join("o/r/slot-0/debug/deps"), slot_n),
        ] {
            std::fs::create_dir_all(&deps).expect("deps");
            for i in 0..n {
                std::fs::write(deps.join(format!("f{i}")), b"").expect("file");
            }
        }
        (tmp, shared, pool)
    }

    #[test]
    fn deps_budget_warns_over_the_line() {
        let (_tmp, shared, pool) = roots(6, 2);
        let check = deps_budget_check(&shared, &pool, 5);
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        for part in [
            "o/r/debug/deps: WARN, >= 5 entries",
            "slot-0/debug/deps: ok, 2 entries",
        ] {
            assert!(check.message.contains(part), "{part}: {}", check.message);
        }
    }

    #[test]
    fn deps_budget_is_ok_under_the_line() {
        let (_tmp, shared, pool) = roots(4, 1);
        let check = deps_budget_check(&shared, &pool, 5);
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        for part in [
            "o/r/debug/deps: ok, 4 entries",
            "slot-0/debug/deps: ok, 1 entries",
        ] {
            assert!(check.message.contains(part), "{part}: {}", check.message);
        }
    }

    #[test]
    fn counts_are_grouped_by_thousands() {
        for (n, want) in [
            (5, "5"),
            (1_000, "1,000"),
            (150_000, "150,000"),
            (844_490, "844,490"),
        ] {
            assert_eq!(thousands(n), want);
        }
    }

    /// Fail-Open Check: a target with no `debug/deps` is named missing, not 0.
    #[test]
    fn a_missing_deps_dir_is_reported_missing_not_zero() {
        let (_tmp, shared, pool) = roots(1, 1);
        std::fs::create_dir_all(pool.join("o/r/slot-1")).expect("bare slot");
        let check = deps_budget_check(&shared, &pool, 5);
        assert!(
            check.message.contains("slot-1/debug/deps: missing"),
            "{}",
            check.message
        );
        assert!(!check.message.contains("0 entries"), "{}", check.message);
    }

    /// Fail-Open Check: a deps dir that cannot be listed is never Ok.
    #[test]
    fn an_unreadable_deps_dir_is_unknown() {
        let (_tmp, shared, pool) = roots(1, 1);
        let deps = shared.join("o/r/debug/deps");
        std::fs::set_permissions(&deps, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let check = deps_budget_check(&shared, &pool, 5);
        std::fs::set_permissions(&deps, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
        assert!(
            check.message.contains("o/r/debug/deps: unreadable"),
            "{}",
            check.message
        );
    }

    /// Fail-Open Check: a root that cannot be listed hides its targets, so it is Unknown.
    #[test]
    fn an_unlistable_root_is_unknown() {
        for root in ["cargo-target", "pool"] {
            let (tmp, shared, pool) = roots(1, 1);
            let locked = tmp.path().join(root);
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
                .expect("chmod");
            let check = deps_budget_check(&shared, &pool, 5);
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            assert_eq!(
                check.status,
                CheckStatus::Unknown,
                "{root}: {}",
                check.message
            );
            assert!(
                check.message.contains("cannot be listed"),
                "{root}: {}",
                check.message
            );
        }
    }
}
