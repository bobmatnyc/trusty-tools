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
//!
//! Cost (#9045): the guard lists the slot root and nothing below it. Cargo's
//! layout puts `.cargo-lock` and `.fingerprint` at `<profile>/` and
//! `<triple>/<profile>/`, so both are probed by direct path. A slot's `deps`,
//! `build` and `incremental` grow without bound — one held 458,794 entries and
//! a readdir of it stalled `tm build-lease` for 30+ minutes.
//! Test: `stale_guard_tests.rs`.

use std::ffi::OsString;
use std::fs::FileType;
use std::io;
use std::path::{Path, PathBuf};

/// The marker file inside a slot directory naming its last checkout.
pub const LAST_CHECKOUT_MARKER: &str = ".trusty-slot-last-checkout";

/// The lock cargo holds on a profile directory for a build's life.
const CARGO_LOCK: &str = ".cargo-lock";

/// The per-profile directory of cargo's freshness records.
const FINGERPRINT_DIR: &str = ".fingerprint";

/// The filesystem calls the guard makes — a seam so tests can count them.
///
/// Why (#9045): the regression test must measure what the guard visits, not
/// wall time. What: [`RealFs`] is the only production implementation.
/// Test: `the_lock_guard_lists_only_the_slot_root`.
trait SlotFs {
    /// Names of the directories directly inside `dir` — one `read_dir`.
    fn child_dirs(&self, dir: &Path) -> io::Result<Vec<OsString>>;
    /// The type of `path`, following symlinks.
    fn file_type(&self, path: &Path) -> io::Result<FileType>;
    /// Try `flock(LOCK_EX | LOCK_NB)` on `lock`; `Ok(true)` when taken (and
    /// released at once), `Ok(false)` when another description holds it.
    fn try_lock(&self, lock: &Path) -> io::Result<bool>;
}

/// [`SlotFs`] over the real filesystem.
struct RealFs;

impl SlotFs for RealFs {
    fn child_dirs(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                out.push(entry.file_name());
            }
        }
        Ok(out)
    }

    fn file_type(&self, path: &Path) -> io::Result<FileType> {
        std::fs::metadata(path).map(|m| m.file_type())
    }

    fn try_lock(&self, lock: &Path) -> io::Result<bool> {
        use std::os::fd::AsRawFd;
        let file = std::fs::File::open(lock)?;
        // SAFETY: `file` owns a valid descriptor for both calls.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Ok(false);
        }
        // SAFETY: as above; releases the probe lock at once.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        Ok(true)
    }
}

/// Whether `err` means the path is not there (as opposed to unreadable).
fn is_absent(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

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
/// A description when a matching fingerprint entry could not be removed (the
/// marker is then left unchanged, and the caller must not build in the slot,
/// #8261 round 3), or when the marker could not be written.
///
/// Test: `a_different_checkout_clears_only_workspace_fingerprints`,
/// `the_same_checkout_touches_nothing`, `a_missing_marker_clears`,
/// `an_unreadable_package_list_clears_every_fingerprint`,
/// `a_failed_removal_is_an_error_and_keeps_the_marker`.
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
    for dir in fingerprint_dirs(&RealFs, slot_dir) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(all || names.iter().any(|pkg| is_unit_of(&name, pkg))) {
                continue;
            }
            // #8261 round 3: a fingerprint left behind can serve a stale
            // build, so a failed removal fails the whole invalidation.
            std::fs::remove_dir_all(entry.path()).map_err(|err| {
                format!(
                    "could not clear stale fingerprint {}: {err}; the slot is not used",
                    entry.path().display()
                )
            })?;
            removed += 1;
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

/// Whether a cargo build is running in `slot_dir` right now.
///
/// Why (#8261 round 3): a SIGKILLed `tm build-lease` frees its slot's flock at
/// once while the build it spawned keeps running in the slot's directory. Cargo
/// holds `flock(LOCK_EX)` on `<profile>/.cargo-lock` for a build's life, so that
/// lock — not the slot file — says whether the directory is still in use.
/// What: `true` when a `<profile>/.cargo-lock` or `<triple>/<profile>/.cargo-lock`
/// is locked by another open file description. Profiles are the slot root's
/// child directories holding a `.cargo-lock`; cargo locks the host profile on
/// every build, `--target` builds included. A slot that does not exist is free.
/// Fail-closed (#9045): a slot root that cannot be listed, or a lock file that
/// cannot be stat'ed or opened, counts as busy — its state is unknown.
/// Cost (#9045): one `read_dir` of the slot root and direct-path probes; no
/// directory below the root is listed.
/// Test: `a_held_cargo_lock_marks_the_directory_busy`,
/// `a_held_cross_target_lock_marks_the_directory_busy`,
/// `the_lock_guard_lists_only_the_slot_root`, `a_lock_inside_deps_is_never_probed`,
/// `an_unlistable_slot_counts_as_held`, `an_unstatable_cargo_lock_counts_as_held`,
/// `an_unopenable_cargo_lock_counts_as_held`, `a_missing_slot_reads_free`.
#[must_use]
pub fn cargo_lock_held(slot_dir: &Path) -> bool {
    cargo_lock_held_in(&RealFs, slot_dir)
}

/// [`cargo_lock_held`] over any [`SlotFs`].
fn cargo_lock_held_in(fs: &impl SlotFs, slot_dir: &Path) -> bool {
    // #9045: the slot root is the only directory listed; it holds profile,
    // triple and tool directories, never per-unit files.
    let children = match fs.child_dirs(slot_dir) {
        Ok(children) => children,
        Err(err) if is_absent(&err) => return false,
        Err(_) => return true,
    };
    let mut profiles = Vec::new();
    for child in &children {
        match probe_lock(fs, &slot_dir.join(child).join(CARGO_LOCK)) {
            LockProbe::Absent => {}
            LockProbe::Free => profiles.push(child),
            LockProbe::Busy => return true,
        }
    }
    // #9045: a `--target` layout reuses the host profile names, so each
    // `<triple>/<profile>/.cargo-lock` is a direct path, never a listing.
    children.iter().any(|child| {
        profiles.iter().any(|profile| {
            let lock = slot_dir.join(child).join(profile).join(CARGO_LOCK);
            probe_lock(fs, &lock) == LockProbe::Busy
        })
    })
}

/// The state of one `.cargo-lock` path.
#[derive(Debug, PartialEq, Eq)]
enum LockProbe {
    /// No lock file at the path (or a non-file, which no cargo locks).
    Absent,
    /// A lock file nobody holds.
    Free,
    /// Held by a live build, or its state could not be read.
    Busy,
}

/// Probe `lock` by direct path.
fn probe_lock(fs: &impl SlotFs, lock: &Path) -> LockProbe {
    match fs.file_type(lock) {
        Err(err) if is_absent(&err) => LockProbe::Absent,
        // #9045: EACCES or similar — unknown state, so never reported free.
        Err(_) => LockProbe::Busy,
        Ok(kind) if !kind.is_file() => LockProbe::Absent,
        Ok(_) => match fs.try_lock(lock) {
            Ok(true) => LockProbe::Free,
            Ok(false) | Err(_) => LockProbe::Busy,
        },
    }
}

/// Whether a fingerprint entry `<pkg>-<16 hex>` belongs to package `pkg`.
fn is_unit_of(entry: &str, pkg: &str) -> bool {
    entry
        .strip_prefix(pkg)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|hash| hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Every `<profile>/.fingerprint` and `<triple>/<profile>/.fingerprint` in `root`.
///
/// What: profiles are the root's child directories holding a `.fingerprint`;
/// the triple layouts reuse their names. An unlistable root yields nothing,
/// as before.
/// Cost (#9045): one `read_dir` of `root` and direct-path stats; no `deps`,
/// `build` or `incremental` listing.
/// Test: `fingerprint_search_lists_only_the_slot_root`.
fn fingerprint_dirs(fs: &impl SlotFs, root: &Path) -> Vec<PathBuf> {
    let Ok(children) = fs.child_dirs(root) else {
        return Vec::new();
    };
    let fingerprints = |dir: PathBuf| {
        let fp = dir.join(FINGERPRINT_DIR);
        fs.file_type(&fp).is_ok_and(|t| t.is_dir()).then_some(fp)
    };
    let profiles: Vec<&OsString> = children
        .iter()
        .filter(|child| fingerprints(root.join(child)).is_some())
        .collect();
    let mut out: Vec<PathBuf> = profiles
        .iter()
        .map(|profile| root.join(profile).join(FINGERPRINT_DIR))
        .collect();
    for child in &children {
        out.extend(
            profiles
                .iter()
                .filter_map(|profile| fingerprints(root.join(child).join(profile))),
        );
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
#[path = "stale_guard_tests.rs"]
mod tests;
