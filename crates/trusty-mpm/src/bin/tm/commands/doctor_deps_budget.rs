//! The `deps_budget` row of `tm doctor` (#10212).
//!
//! Why: an 844,490-entry shared `debug/deps` stalled syspolicyd on the Mac,
//! and nothing reported the growth beforehand. This row names each shared
//! target and builder slot whose `debug/deps` has reached the warn line, so an
//! operator rotates it before the host stalls.
//! What: one local row, no daemon. The shared targets are the
//! `<owner>/<repo>` directories under `~/.trusty-tools/cargo-target`; the slots
//! are `<pool>/<owner>/<repo>/slot-<n>`. Both walks are strict: a level that
//! cannot be listed makes the row Unknown (#10213 review). Each `debug/deps`
//! gets one bounded count (`deps_budget::count_deps`, never a full readdir). Warn when any count
//! reaches the line, or a rotated-aside tree's deleter died (#10212); Unknown when a dir or a root cannot be listed; else Ok. A
//! missing `debug/deps` is listed as missing, never as 0 entries.
//! Test: the `#[cfg(test)]` suite below.

use std::path::{Path, PathBuf};

use trusty_mpm::core::build_lease::deps_budget::{DEPS_WARN_ENTRIES, DepsCount, count_deps};
use trusty_mpm::core::build_lease::deps_rotate::stale_aside;
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

/// This check's name.
const CHECK: &str = "deps_budget";

/// The subdirectories of `dir`, recording every listing error.
///
/// Why: `evict::list_pool_slots` skips an owner or repo dir it cannot list, so
/// the slots under it vanish and the row would read Ok (#10213 review). This
/// walk is strict and leaves that function unchanged for its other callers.
/// What: a missing `dir` has no subdirectories and hides nothing; a `dir` that
/// cannot be opened, or an entry that cannot be read, is pushed to `errors`.
fn subdirs(dir: &Path, errors: &mut Vec<String>) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            errors.push(format!("{} cannot be listed ({err})", dir.display()));
            return Vec::new();
        }
    };
    let mut dirs = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) if entry.path().is_dir() => dirs.push(entry.path()),
            Ok(_) => {}
            Err(err) => errors.push(format!("{} cannot be listed ({err})", dir.display())),
        }
    }
    dirs.sort();
    dirs
}

/// Every `<root>/<owner>/<repo>` dir, and below it every `slot-<n>` dir when
/// `slots` is set; an unlistable level is an error and the walk goes on.
///
/// Test: `an_unlistable_pool_owner_is_unknown`,
/// `an_unlistable_shared_owner_keeps_the_other_targets`.
fn targets_under(root: &Path, slots: bool, errors: &mut Vec<String>) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    for owner in subdirs(root, errors) {
        for repo in subdirs(&owner, errors) {
            if !slots {
                targets.push(repo);
                continue;
            }
            let is_slot = |p: &PathBuf| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_prefix("slot-"))
                    .is_some_and(|i| i.parse::<u32>().is_ok())
            };
            targets.extend(subdirs(&repo, errors).into_iter().filter(is_slot));
        }
    }
    targets
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
/// `an_unreadable_deps_dir_is_unknown`, `an_unlistable_root_is_unknown`,
/// `an_unlistable_pool_owner_is_unknown`,
/// `a_leftover_aside_tree_warns_and_names_the_rotation`.
pub(crate) fn deps_budget_check(shared_root: &Path, pool_root: &Path, line: usize) -> DoctorCheck {
    let mut parts = Vec::new();
    let mut status = CheckStatus::Ok;
    let mut errors = Vec::new();
    let mut targets = targets_under(shared_root, false, &mut errors);
    targets.extend(targets_under(pool_root, true, &mut errors));
    if !errors.is_empty() {
        status = CheckStatus::Unknown;
        parts.extend(errors);
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
        // #10212: an aside tree whose deleter died is a delete that failed.
        for tree in stale_aside(target) {
            over = true;
            parts.push(format!(
                "{}: a rotated-aside profile not yet deleted",
                tree.display()
            ));
        }
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
                "{body} A deps dir this large stalls syspolicyd on every exec. \
                 `tm doctor --rotate-deps --yes` renames each such `debug/` aside while no \
                 build holds it (cargo rebuilds it cold) and deletes the old tree, \
                 including any rotated-aside tree left over (#10212)."
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

    /// Fail-Open Check: an unlistable pool owner hides its slots, so the row
    /// is Unknown, never Ok (#10213 review).
    #[test]
    fn an_unlistable_pool_owner_is_unknown() {
        let (tmp, shared, pool) = roots(1, 1);
        let owner = tmp.path().join("pool/o");
        std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let check = deps_budget_check(&shared, &pool, 5);
        std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
        assert!(
            check.message.contains("pool/o cannot be listed"),
            "{}",
            check.message
        );
    }

    /// One unlistable shared owner does not hide another owner's oversized
    /// target: Warn still wins over Unknown.
    #[test]
    fn an_unlistable_shared_owner_keeps_the_other_targets() {
        let (tmp, shared, pool) = roots(6, 1);
        let locked = tmp.path().join("cargo-target/locked");
        std::fs::create_dir_all(locked.join("repo")).expect("owner");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let check = deps_budget_check(&shared, &pool, 5);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        for part in [
            "locked cannot be listed",
            "o/r/debug/deps: WARN, >= 5 entries",
        ] {
            assert!(check.message.contains(part), "{part}: {}", check.message);
        }
    }

    /// #10212 Fail-Open Check: an aside tree whose deleter died is a failed
    /// delete, so the row warns and names it rather than reading Ok.
    #[test]
    fn a_leftover_aside_tree_warns_and_names_the_rotation() {
        let (_tmp, shared, pool) = roots(1, 1);
        let left = pool.join("o/r/slot-0/.slot-x.evicting.4294967294.1");
        std::fs::create_dir_all(left.join("deps")).expect("an aside tree");
        let check = deps_budget_check(&shared, &pool, 5);
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        for part in [
            ".slot-x.evicting.4294967294.1",
            "tm doctor --rotate-deps --yes",
        ] {
            assert!(check.message.contains(part), "{part}: {}", check.message);
        }
    }
}
