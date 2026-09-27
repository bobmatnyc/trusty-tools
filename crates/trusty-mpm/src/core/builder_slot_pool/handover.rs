//! Handing a seeded slot to its next holder (#8794).
//!
//! Why: two checkouts of one repo compile to the same artifact names, and cargo
//! judges a workspace crate fresh when its source files are no newer than the
//! last build's. A slot that built checkout A and is then handed to checkout B,
//! whose files are older, serves A's test binaries until `cargo clean -p`. The
//! daemon cannot see which worktree an isolated dispatch will run in at claim
//! time, so every change of holder counts as a worktree switch.
//! What: [`SlotPool::hand_over`] invalidates the cargo fingerprints of the
//! repo's own (path) packages, under the slot's cargo locks, whenever the
//! holder differs from the one the marker records, then records the new one.
//! Registry packages keep their fingerprints, so the dependency cache stays warm.
//! Test: the `#[cfg(test)]` suite below.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{
    SEED_MARKER, SlotPool, SlotPoolError, body_reads_seeded, hold_idle_build_locks,
    marker_reads_seeded, seed_failure_path, subdirectories, unique_nanos,
};

/// The marker line naming the holder the slot was last handed to.
const SERVED_PREFIX: &str = "served: ";

impl SlotPool {
    /// This pool, recording `checkout` as where its dispatches were issued from.
    ///
    /// Why: the checkout's `Cargo.lock` names the repo's own packages, which
    /// are the ones [`Self::hand_over`] must invalidate.
    #[must_use]
    pub fn with_checkout(mut self, checkout: PathBuf) -> Self {
        self.checkout = Some(checkout);
        self
    }

    /// Whether slot `index` carries a marker a successful seed wrote.
    #[must_use]
    pub fn is_seeded(&self, index: u32) -> bool {
        marker_reads_seeded(&self.slot_path(index))
    }

    /// The indexes of every `slot-<n>` directory this pool has on disk, ascending.
    ///
    /// Why: a seeded slot above the current cap is still a warm cache, so the
    /// claim looks at every slot that exists, not only `0..cap`.
    /// Test: `a_slot_handed_to_a_new_holder_invalidates_the_repo_packages`.
    #[must_use]
    pub fn existing_indexes(&self) -> Vec<u32> {
        let slot = self.slot_path(0);
        let Some(Ok(entries)) = slot.parent().map(std::fs::read_dir) else {
            return Vec::new();
        };
        let mut indexes: Vec<u32> = entries
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_prefix("slot-")?.parse().ok())
            .collect();
        indexes.sort_unstable();
        indexes
    }

    /// Why slot `index`'s last seed did not complete, when one is on record.
    ///
    /// Test: `a_refused_seed_is_recorded_for_the_next_admission`.
    #[must_use]
    pub fn last_seed_failure(&self, index: u32) -> Option<String> {
        let body = std::fs::read_to_string(seed_failure_path(&self.slot_path(index))).ok()?;
        body.lines()
            .find_map(|line| line.strip_prefix("error: "))
            .map(str::to_string)
    }

    /// Hand seeded slot `index` to `holder`, invalidating another holder's builds.
    ///
    /// Why: see the module doc. The check runs at the moment of the handover,
    /// against the slot's own marker and its cargo `flock`s, never a PID.
    /// What: when the marker already names `holder`, returns the path untouched.
    /// Otherwise takes every cargo lock in the slot, removes the
    /// `.fingerprint/<package>-<hash>` directories of the checkout's path
    /// packages in every profile, and rewrites the marker to name `holder`.
    ///
    /// # Errors
    ///
    /// [`SlotPoolError::ActiveBuild`] when a cargo build holds a lock in the
    /// slot; [`SlotPoolError::HandOver`] when the slot is not seeded or a
    /// fingerprint or the marker cannot be written. The caller hands out no
    /// slot on any error.
    ///
    /// Test: `a_slot_handed_to_a_new_holder_invalidates_the_repo_packages`,
    /// `a_slot_holding_a_live_build_is_not_handed_over`.
    pub fn hand_over(&self, index: u32, holder: &str) -> Result<PathBuf, SlotPoolError> {
        let path = self.slot_path(index);
        let fail = |detail: String| SlotPoolError::HandOver {
            path: path.clone(),
            detail,
        };
        let marker = path.join(SEED_MARKER);
        let body = std::fs::read_to_string(&marker)
            .map_err(|err| fail(format!("could not read {}: {err}", marker.display())))?;
        if !body_reads_seeded(&body) {
            return Err(fail("the slot is not seeded".to_string()));
        }
        let served = match &self.checkout {
            Some(checkout) => format!("{SERVED_PREFIX}{holder} from {}", checkout.display()),
            None => format!("{SERVED_PREFIX}{holder}"),
        };
        if body.lines().any(|line| line == served) {
            return Ok(path);
        }
        // Held until the marker names the new holder, so a build that starts
        // meanwhile waits rather than reading a half-invalidated slot.
        let _locks = hold_idle_build_locks(&path).map_err(|lock| SlotPoolError::ActiveBuild {
            path: path.clone(),
            lock,
        })?;
        let packages = self
            .checkout
            .as_deref()
            .map(path_packages)
            .unwrap_or_default();
        let removed = invalidate_fingerprints(&path, &packages).map_err(fail)?;
        tracing::info!(slot = %path.display(), %served, removed, "builder slot handed to a new holder");
        rewrite_served(&path, &body, &served).map_err(fail)?;
        Ok(path)
    }
}

/// The names of the path (non-registry) packages in `checkout`'s `Cargo.lock`.
///
/// Why: a path package's lock entry carries no `source`, and only path
/// packages are judged fresh by source mtime.
fn path_packages(checkout: &Path) -> BTreeSet<String> {
    #[derive(serde::Deserialize)]
    struct Lock {
        #[serde(default)]
        package: Vec<Package>,
    }
    #[derive(serde::Deserialize)]
    struct Package {
        name: String,
        #[serde(default)]
        source: Option<String>,
    }
    let Some(body) = checkout
        .ancestors()
        .find_map(|dir| std::fs::read_to_string(dir.join("Cargo.lock")).ok())
    else {
        return BTreeSet::new();
    };
    match toml::from_str::<Lock>(&body) {
        Ok(lock) => lock
            .package
            .into_iter()
            .filter(|p| p.source.is_none())
            .map(|p| p.name)
            .collect(),
        Err(err) => {
            tracing::warn!(checkout = %checkout.display(), "could not parse Cargo.lock: {err}");
            BTreeSet::new()
        }
    }
}

/// Remove every `.fingerprint/<package>-<hash>` of `packages` under `slot`.
///
/// What: looks in each profile directory (`<slot>/<profile>` and
/// `<slot>/<triple>/<profile>`). A missing fingerprint makes cargo rebuild the
/// unit and every unit depending on it. Returns how many were removed.
fn invalidate_fingerprints(slot: &Path, packages: &BTreeSet<String>) -> Result<usize, String> {
    let mut removed = 0;
    if packages.is_empty() {
        return Ok(removed);
    }
    let mut profiles = Vec::new();
    for child in subdirectories(slot) {
        profiles.extend(subdirectories(&child));
        profiles.push(child);
    }
    for fingerprints in profiles.iter().map(|p| p.join(".fingerprint")) {
        let Ok(entries) = std::fs::read_dir(&fingerprints) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(package) = name.to_str().and_then(unit_package) else {
                continue;
            };
            if !packages.contains(package) {
                continue;
            }
            std::fs::remove_dir_all(entry.path())
                .map_err(|err| format!("could not remove {}: {err}", entry.path().display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// The package a `.fingerprint` entry belongs to: `<package>-<16 hex digits>`.
fn unit_package(name: &str) -> Option<&str> {
    let (package, hash) = name.rsplit_once('-')?;
    (hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(package)
}

/// Rewrite the marker so it names `served` as the slot's holder, atomically.
fn rewrite_served(slot: &Path, body: &str, served: &str) -> Result<(), String> {
    let mut next: String = body
        .lines()
        .filter(|line| !line.starts_with(SERVED_PREFIX))
        .map(|line| format!("{line}\n"))
        .collect();
    next.push_str(served);
    next.push('\n');
    let marker = slot.join(SEED_MARKER);
    let draft = slot.join(format!(
        "{SEED_MARKER}.draft.{}.{}",
        std::process::id(),
        unique_nanos()
    ));
    std::fs::write(&draft, next)
        .and_then(|()| std::fs::rename(&draft, &marker))
        .map_err(|err| {
            drop(std::fs::remove_file(&draft));
            format!("could not record the holder in {}: {err}", marker.display())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::github_path::GithubPath;

    /// A pool under `root` whose checkout's `Cargo.lock` names one path package
    /// (`widgets-core`) and one registry package (`serde`).
    fn pool_with_checkout(root: &Path) -> SlotPool {
        let checkout = root.join("checkout");
        std::fs::create_dir_all(&checkout).expect("checkout");
        std::fs::write(
            checkout.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"widgets-core\"\nversion = \"0.1.0\"\n\n\
             [[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
        )
        .expect("Cargo.lock");
        let identity = GithubPath {
            owner: "acme".to_string(),
            repo: "widgets".to_string(),
        };
        SlotPool::new(root.join("pool"), identity).with_checkout(checkout)
    }

    /// Plant one fingerprint of each package, as a finished build leaves them.
    fn plant_fingerprints(slot: &Path) -> (PathBuf, PathBuf) {
        let fingerprints = slot.join("debug").join(".fingerprint");
        let local = fingerprints.join("widgets-core-0123456789abcdef");
        let registry = fingerprints.join("serde-fedcba9876543210");
        for dir in [&local, &registry] {
            std::fs::create_dir_all(dir).expect("fingerprint dir");
            std::fs::write(dir.join("lib.json"), b"{}").expect("fingerprint");
        }
        (local, registry)
    }

    /// #8794 item 2: slot claimed by A, then by B. B's claim invalidates the
    /// path package's fingerprint, keeps the registry one, and the marker names B.
    #[test]
    fn a_slot_handed_to_a_new_holder_invalidates_the_repo_packages() {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = pool_with_checkout(root.path());
        pool.seed(0, None).expect("a seeded slot 0");
        assert_eq!(pool.existing_indexes(), vec![0]);

        let slot = pool.hand_over(0, "toolu_A").expect("A is handed slot 0");
        let (local, registry) = plant_fingerprints(&slot);
        pool.hand_over(0, "toolu_A").expect("A again");
        assert!(local.is_dir(), "the same holder keeps its own build");

        pool.hand_over(0, "toolu_B").expect("B is handed slot 0");
        assert!(
            !local.exists(),
            "A's build of a repo package must not serve B"
        );
        assert!(registry.is_dir(), "the dependency cache stays warm");
        let marker = std::fs::read_to_string(slot.join(SEED_MARKER)).expect("marker");
        assert!(marker.contains("served: toolu_B from"), "{marker}");
        assert!(!marker.contains("toolu_A"), "{marker}");
        assert!(pool.is_seeded(0), "the rewrite keeps the slot seeded");
    }

    /// #8794: a slot a cargo build is using is not handed to anyone.
    #[test]
    fn a_slot_holding_a_live_build_is_not_handed_over() {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = pool_with_checkout(root.path());
        let (slot, _) = pool.seed(0, None).expect("a seeded slot 0");
        let (local, _) = plant_fingerprints(&slot);
        let lock = std::fs::File::create(slot.join("debug").join(".cargo-lock")).expect("lock");
        lock.lock().expect("hold the build lock");

        let err = pool
            .hand_over(0, "toolu_B")
            .expect_err("a live build keeps its slot");
        assert!(matches!(err, SlotPoolError::ActiveBuild { .. }), "{err}");
        assert!(local.is_dir(), "a refused handover touches nothing");
    }

    /// #8794: a seed refused for a live build is on record, so the admission
    /// that finds the slot still unseeded can say why.
    #[test]
    fn a_refused_seed_is_recorded_for_the_next_admission() {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = pool_with_checkout(root.path());
        let debug = pool.slot_path(0).join("debug");
        std::fs::create_dir_all(&debug).expect("a slot a build is using");
        let lock = std::fs::File::create(debug.join(".cargo-lock")).expect("lock");
        lock.lock().expect("hold the build lock");

        pool.seed(0, None)
            .expect_err("a live build refuses the seed");
        let why = pool.last_seed_failure(0).expect("the refusal is recorded");
        assert!(why.contains("cargo holds"), "{why}");
        assert!(!pool.is_seeded(0));
    }

    #[test]
    fn unit_package_reads_the_package_name() {
        assert_eq!(
            unit_package("trusty-mpm-0123456789abcdef"),
            Some("trusty-mpm")
        );
        assert_eq!(unit_package("trusty-mpm"), None);
        assert_eq!(unit_package("serde-12345"), None);
    }
}
