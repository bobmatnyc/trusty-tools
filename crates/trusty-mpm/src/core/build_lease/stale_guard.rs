//! Keep a slot directory from serving one worktree's build to another (#8261).
//!
//! Why: cargo hashes a path package's identity RELATIVE to its workspace root
//! and records dep-info paths package-relative, so two worktrees of one
//! repository map to the SAME unit names in a target directory, and cargo
//! decides freshness by comparing mtimes. When a slot's previous holder built
//! worktree A and the next holder builds worktree B, B's sources are often OLDER
//! than A's outputs — cargo then reports every path crate "Fresh" and the test
//! binary contains none of B's changes (observed on a pooled slot, 2026-09-24).
//! A silently stale verdict is worse than a slow one.
//!
//! What: each slot directory carries [`LAST_CHECKOUT_MARKER`], naming the
//! checkout root that last built in it. [`invalidate_if_checkout_changed`]
//! compares it with the incoming holder's checkout; on a change — or when the
//! marker is missing, since a slot from the retired dispatch-time pool has an
//! unknown history — it deletes the `.fingerprint` entries of the checkout's
//! own workspace packages, so cargo rebuilds exactly those crates and their
//! dependents while every registry dependency stays warm. When the workspace
//! package list cannot be read, it deletes EVERY fingerprint: a cold build is
//! the price of certainty. The slot's flock is held throughout, so no build
//! is running in the directory while it is edited.
//! Test: the `#[cfg(test)]` suite below.

use std::path::{Path, PathBuf};

/// The marker file inside a slot directory naming its last checkout.
pub const LAST_CHECKOUT_MARKER: &str = ".trusty-slot-last-checkout";

/// How far below the slot directory `.fingerprint` directories are searched:
/// `<profile>/.fingerprint` and `<triple>/<profile>/.fingerprint`.
const FINGERPRINT_DEPTH: usize = 3;

/// What [`invalidate_if_checkout_changed`] did.
///
/// Test: every test below.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Invalidation {
    /// The slot last built this same checkout; nothing was touched.
    SameCheckout,
    /// The slot last built another checkout (or an unknown one); this many
    /// fingerprint entries were removed.
    Cleared {
        /// The previous checkout, or `None` when the marker was missing.
        previous: Option<String>,
        /// Fingerprint entries removed.
        removed: usize,
        /// Whether every fingerprint went because the package list was unreadable.
        all: bool,
    },
}

/// The root of the git checkout containing `cwd`, or `cwd` itself.
///
/// What: the nearest ancestor holding a `.git` entry — a directory in a main
/// checkout, a file in a linked worktree.
/// Test: `the_checkout_root_is_found_from_a_subdirectory`.
#[must_use]
pub fn checkout_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// Invalidate `slot_dir`'s workspace fingerprints if its last checkout differs.
///
/// What: see the module doc. `packages` lists the checkout's own workspace
/// package names; it is only called when a clear is needed.
///
/// # Errors
///
/// A description when the marker could not be written; the fingerprints have
/// already been cleared by then.
///
/// Test: `a_different_checkout_clears_only_workspace_fingerprints`,
/// `the_same_checkout_touches_nothing`, `a_missing_marker_clears`,
/// `an_unreadable_package_list_clears_every_fingerprint`.
pub fn invalidate_if_checkout_changed(
    slot_dir: &Path,
    checkout: &Path,
    packages: impl FnOnce() -> Result<Vec<String>, String>,
) -> Result<Invalidation, String> {
    let marker = slot_dir.join(LAST_CHECKOUT_MARKER);
    let current = checkout.display().to_string();
    let previous = std::fs::read_to_string(&marker)
        .ok()
        .map(|s| s.trim().to_string());
    if previous.as_deref() == Some(current.as_str()) {
        return Ok(Invalidation::SameCheckout);
    }
    let names = packages();
    let all = names.is_err();
    let names = names.unwrap_or_default();
    let mut removed = 0;
    for dir in fingerprint_dirs(slot_dir) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if (all || names.iter().any(|pkg| is_unit_of(&name, pkg)))
                && std::fs::remove_dir_all(entry.path()).is_ok()
            {
                removed += 1;
            }
        }
    }
    std::fs::write(&marker, &current)
        .map_err(|err| format!("could not write {}: {err}", marker.display()))?;
    Ok(Invalidation::Cleared {
        previous,
        removed,
        all,
    })
}

/// Whether a fingerprint entry `<pkg>-<16 hex>` belongs to package `pkg`.
fn is_unit_of(entry: &str, pkg: &str) -> bool {
    entry
        .strip_prefix(pkg)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|hash| hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Every `.fingerprint` directory within [`FINGERPRINT_DEPTH`] of `root`.
fn fingerprint_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut frontier = vec![root.to_path_buf()];
    for _ in 0..FINGERPRINT_DEPTH {
        let mut next = Vec::new();
        for dir in frontier {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                if entry.file_name() == ".fingerprint" {
                    out.push(entry.path());
                } else {
                    next.push(entry.path());
                }
            }
        }
        frontier = next;
    }
    out
}

/// The checkout's own workspace package names, via `cargo metadata --no-deps`.
///
/// # Errors
///
/// A description when cargo could not be run or its output not parsed.
///
/// Test: `workspace_packages_lists_this_workspace`.
pub fn workspace_packages(checkout: &Path) -> Result<Vec<String>, String> {
    let out = std::process::Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .current_dir(checkout)
        .output()
        .map_err(|err| format!("could not run cargo metadata: {err}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|err| format!("cargo metadata output did not parse: {err}"))?;
    Ok(meta["packages"]
        .as_array()
        .map(|pkgs| {
            pkgs.iter()
                .filter_map(|p| p["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slot directory with fingerprints for one workspace crate and one dependency.
    fn slot() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for unit in [
            "debug/.fingerprint/trusty-mpm-0123456789abcdef",
            "debug/.fingerprint/trusty-mpm-fedcba9876543210",
            "debug/.fingerprint/trusty-mpm-extra-0123456789abcdef",
            "debug/.fingerprint/serde-0123456789abcdef",
            "aarch64-apple-darwin/debug/.fingerprint/trusty-mpm-00000000000000aa",
        ] {
            std::fs::create_dir_all(dir.path().join(unit)).expect("mkdir");
        }
        dir
    }

    fn exists(root: &Path, rel: &str) -> bool {
        root.join(rel).exists()
    }

    #[test]
    fn a_different_checkout_clears_only_workspace_fingerprints() {
        let slot = slot();
        std::fs::write(slot.path().join(LAST_CHECKOUT_MARKER), "/wt/a").expect("marker");
        let got = invalidate_if_checkout_changed(slot.path(), Path::new("/wt/b"), || {
            Ok(vec!["trusty-mpm".into()])
        })
        .expect("cleared");
        assert_eq!(
            got,
            Invalidation::Cleared {
                previous: Some("/wt/a".into()),
                removed: 3,
                all: false
            }
        );
        let root = slot.path();
        assert!(!exists(
            root,
            "debug/.fingerprint/trusty-mpm-0123456789abcdef"
        ));
        assert!(!exists(
            root,
            "aarch64-apple-darwin/debug/.fingerprint/trusty-mpm-00000000000000aa"
        ));
        assert!(
            exists(root, "debug/.fingerprint/serde-0123456789abcdef"),
            "deps stay warm"
        );
        assert!(
            exists(root, "debug/.fingerprint/trusty-mpm-extra-0123456789abcdef"),
            "a different package sharing a prefix is not this one"
        );
        let marker = std::fs::read_to_string(root.join(LAST_CHECKOUT_MARKER)).expect("marker");
        assert_eq!(marker, "/wt/b");
    }

    #[test]
    fn the_same_checkout_touches_nothing() {
        let slot = slot();
        std::fs::write(slot.path().join(LAST_CHECKOUT_MARKER), "/wt/a").expect("marker");
        let got = invalidate_if_checkout_changed(slot.path(), Path::new("/wt/a"), || {
            panic!("the package list is not needed for the same checkout")
        })
        .expect("ok");
        assert_eq!(got, Invalidation::SameCheckout);
        assert!(exists(
            slot.path(),
            "debug/.fingerprint/trusty-mpm-0123456789abcdef"
        ));
    }

    #[test]
    fn a_missing_marker_clears() {
        let slot = slot();
        let got = invalidate_if_checkout_changed(slot.path(), Path::new("/wt/a"), || {
            Ok(vec!["trusty-mpm".into()])
        })
        .expect("cleared");
        assert!(
            matches!(
                got,
                Invalidation::Cleared {
                    previous: None,
                    removed: 3,
                    ..
                }
            ),
            "{got:?}"
        );
    }

    #[test]
    fn an_unreadable_package_list_clears_every_fingerprint() {
        let slot = slot();
        let got = invalidate_if_checkout_changed(slot.path(), Path::new("/wt/a"), || {
            Err("cargo metadata failed".into())
        })
        .expect("cleared");
        assert!(
            matches!(
                got,
                Invalidation::Cleared {
                    removed: 5,
                    all: true,
                    ..
                }
            ),
            "{got:?}"
        );
        assert!(!exists(
            slot.path(),
            "debug/.fingerprint/serde-0123456789abcdef"
        ));
    }

    #[test]
    fn workspace_packages_lists_this_workspace() {
        let names = workspace_packages(Path::new(env!("CARGO_MANIFEST_DIR"))).expect("metadata");
        assert!(names.iter().any(|n| n == "trusty-mpm"), "{names:?}");
        assert!(
            !names.iter().any(|n| n == "serde"),
            "--no-deps lists no registry crate"
        );
    }

    #[test]
    fn the_checkout_root_is_found_from_a_subdirectory() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".git"), "gitdir: /elsewhere").expect("linked .git");
        let sub = dir.path().join("crates/x");
        std::fs::create_dir_all(&sub).expect("mkdir");
        assert_eq!(checkout_root(&sub), dir.path());
    }
}
