//! The `raw_cargo` row of `tm doctor` (#8692, plan Q2a).
//!
//! Why: the build lease refuses a second leased build in a directory a lease
//! owns, but it cannot refuse a raw `cargo` it never sees (a human terminal, a
//! wrapper script). A raw build in the shared `CARGO_TARGET_DIR` can replace
//! a binary (`CARGO_BIN_EXE_tm`) a running test uses (#9223), so that case is
//! reported here instead.
//! What: no lease builds in a shared directory (a slot replaces it, and an
//! unleased run there is refused), so cargo's build lock held there means a
//! build outside the lease. The row checks this project's shared directory and
//! a shared ambient `CARGO_TARGET_DIR` with `cargo_lock_held` (bounded, and an
//! unreadable lock reads as held). WARN naming each busy directory; OK
//! otherwise.
//! Test: the `#[cfg(test)]` suite below.

use std::path::{Path, PathBuf};

use trusty_mpm::core::build_lease::stale_guard::cargo_lock_held;
use trusty_mpm::core::build_lease::target_dir::{SharedDirs, resolve_pool};
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

/// This check's name.
const CHECK: &str = "raw_cargo";

/// The row for `dirs`, the shared target directories, with `held` reading
/// cargo's build lock in one.
///
/// Test: `a_held_cargo_lock_in_a_shared_dir_warns_8692`,
/// `a_free_or_absent_shared_dir_is_ok_8692`.
pub(crate) fn raw_cargo_check(dirs: &[PathBuf], held: impl Fn(&Path) -> bool) -> DoctorCheck {
    if dirs.is_empty() {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Ok,
            "no shared CARGO_TARGET_DIR applies to this project",
        );
    }
    let names = |dirs: &[&PathBuf]| {
        dirs.iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let busy: Vec<&PathBuf> = dirs.iter().filter(|d| held(d)).collect();
    if busy.is_empty() {
        let all: Vec<&PathBuf> = dirs.iter().collect();
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Ok,
            format!("no cargo outside the lease is compiling in {}", names(&all)),
        );
    }
    DoctorCheck::new(
        CHECK,
        CheckStatus::Warn,
        format!(
            "a cargo outside `tm build-lease` holds the build lock (or it is unreadable) in the \
             shared CARGO_TARGET_DIR {}; no lease builds there, so a test running binaries \
             from it can see them replaced (#9223). The lease cannot refuse a build it never \
             sees: run builds through `tm build-lease` (#8692)",
            names(&busy)
        ),
    )
}

/// This project's shared target directory, and the ambient one when shared.
///
/// What: the repo's shared directory from [`resolve_pool`], plus a
/// `CARGO_TARGET_DIR` the session inherits when [`SharedDirs`] counts it as
/// shared and it is not under the slot pool (a slot a lease may hold).
fn shared_dirs(project_dir: Option<&Path>, home: &Path) -> Vec<PathBuf> {
    let builders = MpmConfig::load_default().builders;
    let mut dirs: Vec<PathBuf> = project_dir
        .and_then(|dir| resolve_pool(&builders, home, dir).ok())
        .and_then(|(_pool, shared)| shared)
        .into_iter()
        .collect();
    let pool_root = builders.effective_slot_pool_root(home);
    let ambient = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
    if let Some(dir) = ambient
        && SharedDirs::resolve(&builders, home).contains(&dir)
        && !dir.starts_with(&pool_root)
        && !dirs.contains(&dir)
    {
        dirs.push(dir);
    }
    dirs
}

/// The live row for `project_dir`.
pub(crate) fn raw_cargo_row(project_dir: Option<&Path>) -> DoctorCheck {
    let Some(home) = dirs::home_dir() else {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Unknown,
            "no home directory, so the shared CARGO_TARGET_DIR cannot be resolved",
        );
    };
    raw_cargo_check(&shared_dirs(project_dir, &home), cargo_lock_held)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn a_held_cargo_lock_in_a_shared_dir_warns_8692() {
        let tmp = tempfile::tempdir().expect("tmp");
        let shared = tmp.path().join("shared");
        std::fs::create_dir_all(shared.join("debug")).expect("profile");
        let lock = std::fs::File::create(shared.join("debug/.cargo-lock")).expect("lock");
        // SAFETY: valid descriptor; stands in for a raw cargo compiling there.
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
        let check = raw_cargo_check(std::slice::from_ref(&shared), cargo_lock_held);
        assert_eq!(check.name, "raw_cargo");
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(
            check.message.contains(&shared.display().to_string())
                && check.message.contains("#9223"),
            "{}",
            check.message
        );
    }

    #[test]
    fn a_free_or_absent_shared_dir_is_ok_8692() {
        let tmp = tempfile::tempdir().expect("tmp");
        let shared = tmp.path().join("shared");
        std::fs::create_dir_all(shared.join("debug")).expect("profile");
        std::fs::File::create(shared.join("debug/.cargo-lock")).expect("an unheld lock");
        let absent = tmp.path().join("absent");
        let check = raw_cargo_check(&[shared, absent], cargo_lock_held);
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        let none = raw_cargo_check(&[], |_| true);
        assert_eq!(none.status, CheckStatus::Ok, "{}", none.message);
    }
}
