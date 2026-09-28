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
//!
//! #8794 critic round 3: the invalidation MOVES each matched
//! `.fingerprint/<package>-<hash>` directory into a per-handover trash tree
//! under [`INVALIDATED_DIR`] — one `rename` per directory, never a walk of its
//! files — because it runs under the daemon's claim mutex inside the hook's
//! 2-second budget. [`SlotPool::purge_invalidated`] deletes the trash off that
//! path; [`SlotPool::invalidated_trees`] lists every tree still there, so a
//! delete that was interrupted is picked up by the slot's next handover.
//! Test: the `#[cfg(test)]` suite below.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{
    SEED_MARKER, SlotPool, SlotPoolError, body_reads_seeded, hold_idle_build_locks,
    marker_reads_seeded, seed_failure_path, subdirectories, unique_nanos,
};

/// The marker line naming the holder the slot was last handed to.
const SERVED_PREFIX: &str = "served: ";

/// The slot subdirectory invalidated fingerprints are moved into (#8794).
///
/// Why: one level of nesting (`<slot>/.trusty-invalidated/<run>/<entry>`) keeps
/// the moved directories out of the depth-2 scans the claim path runs — the
/// cargo-lock probe and the profile walk each see one child per trash tree,
/// not one per moved package.
const INVALIDATED_DIR: &str = ".trusty-invalidated";

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
    /// Otherwise resolves the checkout's path packages, takes every cargo lock
    /// in the slot, moves the `.fingerprint/<package>-<hash>` directories of
    /// those packages in every profile into a fresh trash tree under
    /// [`INVALIDATED_DIR`], and rewrites the marker to name `holder`. The claim
    /// path's cost is one `rename` per matched directory, not per file; the
    /// caller deletes the trash off that path with [`Self::purge_invalidated`].
    ///
    /// # Errors
    ///
    /// [`SlotPoolError::ActiveBuild`] when a cargo build holds a lock in the
    /// slot; [`SlotPoolError::HandOver`] when the slot is not seeded, the
    /// checkout's path packages cannot be resolved or are none (#8794), or a
    /// fingerprint cannot be moved or the marker rewritten. The caller hands
    /// out no slot on any error, and the marker keeps its previous holder.
    ///
    /// Test: `a_slot_handed_to_a_new_holder_invalidates_the_repo_packages`,
    /// `a_slot_holding_a_live_build_is_not_handed_over`,
    /// `an_unresolvable_package_set_hands_over_nothing`,
    /// `a_fingerprint_that_cannot_be_moved_hands_over_nothing`,
    /// `a_marker_that_cannot_be_rewritten_hands_over_nothing`.
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
        // #8794: an unknown package set would invalidate nothing and still
        // record the new holder — a slot serving the old holder's builds.
        let packages = path_packages(self.checkout.as_deref()).map_err(fail)?;
        // Held until the marker names the new holder, so a build that starts
        // meanwhile waits rather than reading a half-invalidated slot.
        let _locks = hold_idle_build_locks(&path).map_err(|lock| SlotPoolError::ActiveBuild {
            path: path.clone(),
            lock,
        })?;
        let moved = invalidate_fingerprints(&path, &packages).map_err(fail)?;
        tracing::info!(slot = %path.display(), %served, moved, "builder slot handed to a new holder");
        rewrite_served(&path, &body, &served).map_err(fail)?;
        Ok(path)
    }

    /// The trash trees [`Self::hand_over`] has left in slot `index` (#8794).
    ///
    /// Why: the delete runs off the claim path, so a daemon that stops mid-delete
    /// leaves a tree behind. Listing every tree, not only the newest, is what
    /// removes such a leftover at the slot's next handover instead of letting it
    /// pile up. Bounded: one `read_dir` of a directory holding one entry per
    /// undeleted handover.
    /// What: the real (non-symlink) directories under `<slot>/`[`INVALIDATED_DIR`].
    /// The caller lists them under the claim mutex, where no handover is still
    /// filling one.
    /// Test: `a_leftover_trash_tree_is_purged_at_the_next_handover`.
    #[must_use]
    pub fn invalidated_trees(&self, index: u32) -> Vec<PathBuf> {
        let trash = self.slot_path(index).join(INVALIDATED_DIR);
        // #8794: a symlinked trash directory is never listed, so a purge
        // deletes nothing outside the slot.
        if !is_real_dir(&trash) {
            return Vec::new();
        }
        subdirectories(&trash)
    }

    /// Delete trash trees listed by [`Self::invalidated_trees`]; returns how many
    /// are gone.
    ///
    /// Why (#8794): this is the unbounded half of a handover — about 9,000 files
    /// on the real pool — so it runs on a blocking task after the claim answers.
    /// What: `remove_dir_all` per tree. A tree already gone counts as removed,
    /// because two purges of one slot can overlap; any other failure is logged
    /// and the tree stays listed for the next handover to retry.
    /// Test: `a_leftover_trash_tree_is_purged_at_the_next_handover`.
    pub fn purge_invalidated(trees: &[PathBuf]) -> usize {
        trees
            .iter()
            .filter(|tree| match std::fs::remove_dir_all(tree) {
                Ok(()) => true,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
                Err(err) => {
                    tracing::warn!(tree = %tree.display(), "could not purge invalidated builder fingerprints: {err}");
                    false
                }
            })
            .count()
    }
}

/// The names of the path (non-registry) packages in `checkout`'s `Cargo.lock`.
///
/// Why: a path package's lock entry carries no `source`, and only path
/// packages are judged fresh by source mtime.
/// What: reads the nearest `Cargo.lock` at or above `checkout`. #8794: no
/// checkout, no lock, a lock that does not parse, and a lock naming no path
/// package are each an `Err` — the handover must not proceed on an empty set.
/// Test: `an_unresolvable_package_set_hands_over_nothing`.
fn path_packages(checkout: Option<&Path>) -> Result<BTreeSet<String>, String> {
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
    let Some(checkout) = checkout else {
        return Err("no checkout is recorded, so the repo's own packages are unknown".to_string());
    };
    let Some((lock_path, body)) = checkout.ancestors().find_map(|dir| {
        let lock = dir.join("Cargo.lock");
        std::fs::read_to_string(&lock).ok().map(|body| (lock, body))
    }) else {
        return Err(format!("no Cargo.lock at or above {}", checkout.display()));
    };
    let lock = toml::from_str::<Lock>(&body)
        .map_err(|err| format!("could not parse {}: {err}", lock_path.display()))?;
    let packages: BTreeSet<String> = lock
        .package
        .into_iter()
        .filter(|p| p.source.is_none())
        .map(|p| p.name)
        .collect();
    if packages.is_empty() {
        return Err(format!("{} names no path package", lock_path.display()));
    }
    Ok(packages)
}

/// Move every `.fingerprint/<package>-<hash>` of `packages` under `slot` into a
/// fresh trash tree (#8794).
///
/// What: looks in each profile directory (`<slot>/<profile>` and
/// `<slot>/<triple>/<profile>`), skipping [`INVALIDATED_DIR`] and any
/// `.fingerprint` that is not a real directory. Each match is `rename`d into
/// `<slot>/`[`INVALIDATED_DIR`]`/<pid>.<nanos>/<profile-ordinal>.<entry>`, which
/// is created on the first match. A missing fingerprint makes cargo rebuild the
/// unit and every unit depending on it. Returns how many were moved. Any
/// unreadable `.fingerprint` or failed move is an `Err`: the slot is then not
/// handed out, since a partly invalidated slot must not read as clean.
fn invalidate_fingerprints(slot: &Path, packages: &BTreeSet<String>) -> Result<usize, String> {
    let trash =
        slot.join(INVALIDATED_DIR)
            .join(format!("{}.{}", std::process::id(), unique_nanos()));
    let mut moved = 0;
    let mut profiles = Vec::new();
    for child in subdirectories(slot) {
        if child.file_name() == Some(std::ffi::OsStr::new(INVALIDATED_DIR)) {
            continue;
        }
        profiles.extend(subdirectories(&child));
        profiles.push(child);
    }
    for (ordinal, profile) in profiles.iter().enumerate() {
        let fingerprints = profile.join(".fingerprint");
        // #8794: never follow a symlinked `.fingerprint` out of the slot.
        if !is_real_dir(&fingerprints) {
            continue;
        }
        let entries = std::fs::read_dir(&fingerprints)
            .map_err(|err| format!("could not read {}: {err}", fingerprints.display()))?;
        for entry in entries {
            let entry =
                entry.map_err(|err| format!("could not read {}: {err}", fingerprints.display()))?;
            let name = entry.file_name();
            let Some(package) = name.to_str().and_then(unit_package) else {
                continue;
            };
            if !packages.contains(package) {
                continue;
            }
            if moved == 0 {
                create_trash(&trash)?;
            }
            let aside = trash.join(format!("{ordinal}.{}", name.to_string_lossy()));
            std::fs::rename(entry.path(), &aside)
                .map_err(|err| format!("could not move {} aside: {err}", entry.path().display()))?;
            moved += 1;
        }
    }
    Ok(moved)
}

/// Whether `path` is a directory itself, not a symlink to one.
fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Create this handover's trash tree inside a real [`INVALIDATED_DIR`] (#8794).
///
/// Why: a symlinked trash directory would move the fingerprints, and the purge
/// after them, outside the slot. `create_dir` (not `_all`) of the tree itself
/// also refuses a name another run already holds.
/// Test: `a_symlinked_trash_directory_is_not_used`.
fn create_trash(trash: &Path) -> Result<(), String> {
    let parent = trash.parent().unwrap_or(trash);
    match std::fs::create_dir(parent) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && is_real_dir(parent) => {}
        Err(err) => return Err(format!("could not create {}: {err}", parent.display())),
    }
    std::fs::create_dir(trash).map_err(|err| format!("could not create {}: {err}", trash.display()))
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

    /// A pool under `root` with no checkout recorded.
    fn bare_pool(root: &Path) -> SlotPool {
        let identity = GithubPath {
            owner: "acme".to_string(),
            repo: "widgets".to_string(),
        };
        SlotPool::new(root.join("pool"), identity, 4)
    }

    /// A pool under `root` whose checkout's `Cargo.lock` names one path package
    /// (`widgets-core`) and one registry package (`serde`).
    fn pool_with_checkout(root: &Path) -> SlotPool {
        let checkout = super::super::test_support::write_checkout(&root.join("checkout"));
        bare_pool(root).with_checkout(checkout)
    }

    /// Seed slot 0, hand it to A, and plant A's build; returns the slot and
    /// A's repo-package fingerprint.
    fn slot_held_by_a(pool: &SlotPool) -> (PathBuf, PathBuf) {
        pool.seed(0, None).expect("a seeded slot 0");
        let slot = pool.hand_over(0, "toolu_A").expect("A is handed slot 0");
        let (local, _) = plant_fingerprints(&slot);
        (slot, local)
    }

    /// The `served:` line of slot `slot`'s marker.
    fn served_line(slot: &Path) -> String {
        std::fs::read_to_string(slot.join(SEED_MARKER))
            .expect("marker")
            .lines()
            .find(|line| line.starts_with(SERVED_PREFIX))
            .unwrap_or_default()
            .to_string()
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
        // #8794 critic round 3: the claim path moved the build aside; the
        // delete is left for the purge.
        assert_eq!(pool.invalidated_trees(0).len(), 1, "one trash tree");
    }

    /// #8794 critic round 3, HIGH: no checkout, no `Cargo.lock`, a lock that
    /// does not parse, and a lock naming no path package each leave the package
    /// set empty. That is an error, not a handover that invalidates nothing.
    /// At c7cf951a4 each arm handed the slot to B with A's build still in it.
    #[test]
    fn an_unresolvable_package_set_hands_over_nothing() {
        let registry_only = "version = 4\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        for (arm, lock) in [
            ("no checkout", None),
            ("no Cargo.lock", Some(None)),
            ("unparsable Cargo.lock", Some(Some("[[package]\n"))),
            ("no path package", Some(Some(registry_only))),
        ] {
            let root = tempfile::tempdir().expect("tempdir");
            let (slot, local) = slot_held_by_a(&pool_with_checkout(root.path()));
            let before = served_line(&slot);
            let pool = match lock {
                None => bare_pool(root.path()),
                Some(body) => {
                    let checkout = root.path().join("other-checkout");
                    std::fs::create_dir_all(&checkout).expect("checkout");
                    if let Some(body) = body {
                        std::fs::write(checkout.join("Cargo.lock"), body).expect("lock");
                    }
                    bare_pool(root.path()).with_checkout(checkout)
                }
            };

            let err = pool
                .hand_over(0, "toolu_B")
                .expect_err(&format!("{arm}: an unknown package set hands over nothing"));
            assert!(
                matches!(err, SlotPoolError::HandOver { .. }),
                "{arm}: {err}"
            );
            assert!(local.is_dir(), "{arm}: A's build is untouched");
            assert_eq!(served_line(&slot), before, "{arm}: the marker keeps A");
        }
    }

    /// #8794 critic round 3: a fingerprint that cannot be moved aside is a
    /// HandOver error, and the marker keeps the previous holder.
    #[test]
    fn a_fingerprint_that_cannot_be_moved_hands_over_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().expect("tempdir");
        let pool = pool_with_checkout(root.path());
        let (slot, local) = slot_held_by_a(&pool);
        let before = served_line(&slot);
        let fingerprints = local.parent().expect("parent").to_path_buf();
        let perms = std::fs::metadata(&fingerprints)
            .expect("meta")
            .permissions();
        std::fs::set_permissions(&fingerprints, std::fs::Permissions::from_mode(0o555))
            .expect("read-only .fingerprint");

        let result = pool.hand_over(0, "toolu_B");
        std::fs::set_permissions(&fingerprints, perms).expect("restore perms");

        let err = result.expect_err("an unmovable fingerprint hands over nothing");
        assert!(matches!(err, SlotPoolError::HandOver { .. }), "{err}");
        assert!(local.is_dir(), "A's build is still in place");
        assert_eq!(served_line(&slot), before, "the marker keeps A");
    }

    /// #8794 critic round 3: a marker that cannot be rewritten is a HandOver
    /// error, and the marker keeps the previous holder.
    #[test]
    fn a_marker_that_cannot_be_rewritten_hands_over_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().expect("tempdir");
        let pool = pool_with_checkout(root.path());
        let (slot, _) = slot_held_by_a(&pool);
        let before = served_line(&slot);
        // The trash parent exists, so only the marker's draft needs the slot
        // directory to be writable.
        std::fs::create_dir_all(slot.join(INVALIDATED_DIR)).expect("trash parent");
        let perms = std::fs::metadata(&slot).expect("meta").permissions();
        std::fs::set_permissions(&slot, std::fs::Permissions::from_mode(0o555))
            .expect("read-only slot");

        let result = pool.hand_over(0, "toolu_B");
        std::fs::set_permissions(&slot, perms).expect("restore perms");

        let err = result.expect_err("an unwritable marker hands over nothing");
        assert!(matches!(err, SlotPoolError::HandOver { .. }), "{err}");
        assert!(err.to_string().contains("record the holder"), "{err}");
        assert_eq!(served_line(&slot), before, "the marker keeps A");
    }

    /// #8794 critic round 3: a trash tree an interrupted purge left behind is
    /// listed at the slot's next handover and deleted with the new one.
    #[test]
    fn a_leftover_trash_tree_is_purged_at_the_next_handover() {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = pool_with_checkout(root.path());
        let (slot, local) = slot_held_by_a(&pool);
        let leftover = slot
            .join(INVALIDATED_DIR)
            .join("1.1")
            .join("0.widgets-core-0123456789abcdef");
        std::fs::create_dir_all(&leftover).expect("an interrupted purge's tree");
        std::fs::write(leftover.join("lib.json"), b"{}").expect("a file in it");

        pool.hand_over(0, "toolu_B").expect("B is handed slot 0");
        assert!(!local.exists(), "A's build is out of .fingerprint");
        let trees = pool.invalidated_trees(0);
        assert_eq!(
            trees.len(),
            2,
            "the leftover and this handover's: {trees:?}"
        );

        assert_eq!(SlotPool::purge_invalidated(&trees), 2);
        assert!(pool.invalidated_trees(0).is_empty(), "nothing piles up");
        assert_eq!(
            SlotPool::purge_invalidated(&trees),
            2,
            "a tree another purge already removed counts as gone"
        );
    }

    /// #8794 critic round 3: a `.fingerprint` that is a symlink is not followed
    /// out of the slot.
    #[test]
    fn a_symlinked_fingerprint_directory_is_not_followed() {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = pool_with_checkout(root.path());
        let (slot, _) = pool.seed(0, None).expect("a seeded slot 0");
        let outside = root.path().join("outside");
        let foreign = outside.join("widgets-core-0123456789abcdef");
        std::fs::create_dir_all(&foreign).expect("a directory outside the slot");
        std::fs::create_dir_all(slot.join("debug")).expect("profile");
        std::os::unix::fs::symlink(&outside, slot.join("debug").join(".fingerprint"))
            .expect("symlink");

        pool.hand_over(0, "toolu_B").expect("B is handed slot 0");
        assert!(foreign.is_dir(), "nothing outside the slot is moved");
    }

    /// #8794 critic round 3: a symlinked trash directory is neither filled nor
    /// listed for a purge, so nothing outside the slot is moved or deleted.
    #[test]
    fn a_symlinked_trash_directory_is_not_used() {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = pool_with_checkout(root.path());
        let (slot, local) = slot_held_by_a(&pool);
        let outside = root.path().join("outside");
        let foreign = outside.join("1.1");
        std::fs::create_dir_all(&foreign).expect("a directory outside the slot");
        std::os::unix::fs::symlink(&outside, slot.join(INVALIDATED_DIR)).expect("symlink");

        assert!(pool.invalidated_trees(0).is_empty(), "nothing to purge");
        let err = pool
            .hand_over(0, "toolu_B")
            .expect_err("no trash tree outside the slot");
        assert!(matches!(err, SlotPoolError::HandOver { .. }), "{err}");
        assert!(local.is_dir(), "A's build stays in place");
        assert!(foreign.is_dir(), "nothing outside the slot is touched");
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
