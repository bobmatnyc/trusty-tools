//! `tm doctor --rotate-deps` — rotate each oversized cargo `debug/` aside (#10212).
//!
//! Why: builds outside `tm build-lease` grow the shared target with nothing to
//! bound it, and only an explicit operator act may rotate the shared dir
//! (Architect ruling Q3a). The behaviour lives in
//! [`trusty_mpm::core::build_lease::deps_rotate::rotate_all`]; this file is
//! path resolution and printing.
//! What: [`rotate_deps_locally`] resolves the shared root and the slot pool the
//! `deps_budget` row reads, runs the pass at the row's warn line, and prints one
//! line per target. A dry run unless `--yes`.
//! Test: `deps_rotate_tests.rs` covers the rotation and the refusal; `tests`
//! below pins the printed outcome.

use std::path::Path;

use trusty_mpm::core::build_lease::deps_budget::DEPS_WARN_ENTRIES;
use trusty_mpm::core::build_lease::deps_rotate::{Rotation, rotate_all};
use trusty_mpm::core::config::MpmConfig;

/// `--rotate-deps` action — preview, or apply, the rotation.
///
/// Test: `a_dry_run_moves_nothing_and_names_the_apply_command`.
pub(crate) fn rotate_deps_locally(yes: bool) {
    let Some(home) = dirs::home_dir() else {
        eprintln!("  could not resolve the home directory — nothing was checked");
        return;
    };
    let shared_root = home
        .join(trusty_common::crate_config::TRUSTY_TOOLS_DIR)
        .join(trusty_mpm::core::build_env::CARGO_TARGET_SUBDIR);
    let pool_root = MpmConfig::load_default()
        .builders
        .effective_slot_pool_root(&home);
    print!(
        "{}",
        report(&shared_root, &pool_root, DEPS_WARN_ENTRIES, yes)
    );
}

/// The text one pass prints.
fn report(shared_root: &Path, pool_root: &Path, line: usize, yes: bool) -> String {
    let mode = if yes {
        "applying"
    } else {
        "dry run — nothing will be moved"
    };
    let mut out =
        format!("\nrotate-deps: cargo debug/ dirs at or over {line} entries ({mode}) (#10212)\n");
    let rows = rotate_all(shared_root, pool_root, line, yes);
    for (target, rotation) in &rows {
        let mark = if rotation.is_failure() { "FAIL" } else { "-" };
        out.push_str(&format!("  {mark} {}\n", rotation.describe(target)));
    }
    if !yes && rows.iter().any(|(_, r)| *r == Rotation::WouldRotate) {
        out.push_str("  apply with: tm doctor --rotate-deps --yes\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dry_run_moves_nothing_and_names_the_apply_command() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let shared = tmp.path().join("cargo-target");
        let deps = shared.join("o/r/debug/deps");
        std::fs::create_dir_all(&deps).expect("deps");
        for i in 0..3 {
            std::fs::write(deps.join(format!("f{i}")), b"").expect("file");
        }
        let text = report(&shared, &tmp.path().join("pool"), 2, false);
        assert!(text.contains("would rename it aside"), "{text}");
        assert!(text.contains("tm doctor --rotate-deps --yes"), "{text}");
        assert!(deps.join("f0").exists(), "a dry run moves nothing");
    }
}
