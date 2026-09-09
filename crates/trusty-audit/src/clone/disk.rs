//! The trees a clone leaves on disk, and what is done with them (#5669).
//!
//! Why: its own file for the reason `super::super::cli::render::cloned` has
//! one — `super` sits at the 500-SLOC production cap, and #5669's shared
//! measuring function is what pushed it over. This is the cohesive piece to
//! lift out: every item here answers one question, what is on disk for one
//! repository and whether it stays there. `super` keeps the loop, the report,
//! and the names.
//!
//! What: [`measure_tree`], the one measurement every disk figure and every
//! budget decision comes from; [`verify_checkout`], which decides whether a
//! staged tree is a checkout at all; and [`finish_one`], which promotes it or
//! removes it through [`discard_at`].
//! Test: `super::clone_tests`.

use std::path::Path;

use super::{CloneState, watchdog};

/// The one measurement every disk figure and every budget decision comes from.
///
/// Why: a tree whose own ROOT cannot be opened used to measure 0 bytes, and a
/// confident 0 for a checkout of any size is the fail-open the budget cannot
/// survive — the ceiling is never reached, so it is never enforced. That is a
/// different fact from an entry BELOW the root being unreadable, which is
/// ordinary: `git` creates and removes temporary pack files throughout a fetch,
/// so a walk racing one sees an entry vanish. The two used to collapse into the
/// same `(0, false)` (#5669).
/// What: `Err` when the root cannot be opened, so the caller must decide rather
/// than silently count nothing. `Ok((bytes, false))` when the root opened but
/// something below it did not, which makes `bytes` a floor — and a floor over a
/// ceiling still crosses it. The root is opened exactly ONCE and the walk reads
/// that handle, so there is no window between a guard and the walk it guards.
/// Never follows symlinks.
/// Test: `super::clone_tests::an_unreadable_subtree_marks_the_size_incomplete`,
/// `super::clone_tests::an_unreadable_root_is_a_measurement_failure_not_a_zero`.
///
/// # Errors
///
/// One line naming the path and the OS error, ready to become a gap.
pub(super) fn measure_tree(path: &Path) -> Result<(u64, bool), String> {
    match std::fs::read_dir(path) {
        Ok(entries) => Ok(walk(entries)),
        Err(source) => Err(format!(
            "{} could not be measured: {source}",
            path.display()
        )),
    }
}

/// Bytes occupied by a directory tree BELOW the root, and whether the walk saw
/// all of it.
///
/// The floor semantics [`measure_tree`] documents, applied to a subdirectory:
/// an unreadable one contributes 0 and clears the flag rather than failing the
/// whole measurement (#5215 review).
pub(super) fn dir_size(path: &Path) -> (u64, bool) {
    match std::fs::read_dir(path) {
        Ok(entries) => walk(entries),
        Err(_) => (0, false),
    }
}

/// Sum one already-opened directory handle, recursing into its subdirectories.
pub(super) fn walk(entries: std::fs::ReadDir) -> (u64, bool) {
    let mut total = 0u64;
    let mut complete = true;
    for entry in entries {
        let Ok(entry) = entry else {
            complete = false;
            continue;
        };
        let child = entry.path();
        if child.is_symlink() {
            continue;
        }
        if child.is_dir() {
            let (bytes, ok) = dir_size(&child);
            total = total.saturating_add(bytes);
            complete &= ok;
        } else {
            match std::fs::symlink_metadata(&child) {
                Ok(meta) => total = total.saturating_add(meta.len()),
                Err(_) => complete = false,
            }
        }
    }
    (total, complete)
}

/// Is this staged tree a checkout the sweep can actually read?
///
/// Why: `gh` exiting zero is not proof of a usable repository. Cloning a
/// COMMITLESS repository exits zero and leaves a directory
/// holding only `.git`, and `gh repo clone` forwards that status — reported as
/// `Cloned`, the audit claims coverage of a repository nothing ever read
/// (#5215 review). This is the check the `#[ignore]`d live test was making and
/// the production path was not.
/// What: `.git` must be a real directory, and `HEAD` must resolve — either to a
/// detached SHA, or to a ref that exists loose or in `packed-refs`. An
/// unresolvable `HEAD` is exactly the commitless case.
/// Test: `super::clone_tests::a_commitless_clone_is_not_a_usable_checkout`,
/// `super::clone_tests::a_checkout_with_a_packed_head_ref_is_usable`.
pub(super) fn verify_checkout(tree: &Path) -> Result<(), String> {
    let git = tree.join(".git");
    if !git.is_dir() {
        return Err("the clone left no .git directory".to_string());
    }
    let head = match std::fs::read_to_string(git.join("HEAD")) {
        Ok(text) => text.trim().to_string(),
        Err(source) => return Err(format!("the clone left no readable .git/HEAD: {source}")),
    };
    let Some(reference) = head.strip_prefix("ref:").map(str::trim) else {
        // A detached HEAD is a raw SHA, which means there is a commit.
        return Ok(());
    };
    if git.join(reference).exists() {
        return Ok(());
    }
    let packed = std::fs::read_to_string(git.join("packed-refs")).unwrap_or_default();
    if packed.lines().any(|line| line.ends_with(reference)) {
        return Ok(());
    }
    Err(format!(
        "the repository has no commits — HEAD points at {reference}, which does not exist"
    ))
}

/// What one acquisition settled to, before it becomes a [`ClonedRepo`].
#[derive(Debug)]
pub(super) struct Finished {
    /// What happened to the repository.
    pub(super) state: CloneState,
    /// Bytes this repository leaves on disk — the checkout when it was
    /// promoted, and what a failed removal left behind when it was not.
    pub(super) bytes: u64,
    /// Whether [`Finished::bytes`] is a total or a floor.
    pub(super) bytes_complete: bool,
    /// Why a tree that should have been removed still exists — see
    /// [`ClonedRepo::staging_residue`].
    pub(super) residue: Option<String>,
}

/// Remove a tree this run must not leave behind, saying so when it cannot.
///
/// Why: the removal used to be `let _ = std::fs::remove_dir_all(staged)`, which
/// makes "nothing survives under staging" a claim the code does not enforce —
/// a read-only parent directory or a file held open leaves the whole partial
/// tree on disk, unreported, uncounted against the ceiling that just stopped it,
/// and ready to be resumed as a corrupt checkout by the next run (#5669).
/// What: removes the tree; an already-absent tree is success. On a real failure
/// it MEASURES what survived, so the bytes count against the budget instead of
/// being reported as zero, and returns the sentence the report prints as its own
/// gap line.
///
/// A tree that cannot be REMOVED and cannot be MEASURED either is a third case,
/// and it is reported as unknown rather than as a floor of zero: "at least 0
/// bytes" reads as a measured figure to a recipient, and the number it hides can
/// be gigabytes. The gap line names the measurement failure, and
/// [`Finished::bytes_complete`] is cleared, which is what turns every later
/// budget decision in the run into the floor it has become (#5669).
/// Test: `super::clone_tests::a_staged_tree_that_cannot_be_removed_is_its_own_gap`,
/// `super::clone_tests::an_unmeasurable_residue_is_reported_unknown_not_zero`.
pub(super) fn discard_at(path: &Path, state: CloneState) -> Finished {
    let source = match std::fs::remove_dir_all(path) {
        Ok(()) => {
            return Finished {
                state,
                bytes: 0,
                bytes_complete: true,
                residue: None,
            };
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Finished {
                state,
                bytes: 0,
                bytes_complete: true,
                residue: None,
            };
        }
        Err(source) => source,
    };
    // #5669: the removal failed, so whatever is there is still occupying disk.
    // Whether that is a FIGURE at all is what this match keeps: an unopenable
    // root used to collapse into `(0, false)`, which the sentence below then
    // printed as "at least 0 bytes" — a measurement, of a tree nothing measured.
    let (bytes, bytes_complete, size) = match measure_tree(path) {
        Ok((bytes, true)) => (
            bytes,
            true,
            format!("{bytes} bytes are still on disk and count against the disk budget"),
        ),
        Ok((bytes, false)) => (
            bytes,
            false,
            format!("at least {bytes} bytes are still on disk and count against the disk budget"),
        ),
        Err(why) => (
            0,
            false,
            format!(
                "its size is unknown ({why}), so what it holds is not counted against the disk \
                 budget and every later budget decision in this run is a floor"
            ),
        ),
    };
    Finished {
        state,
        bytes,
        bytes_complete,
        residue: Some(format!(
            "the partial checkout at {} could not be removed: {source}; {size}",
            path.display()
        )),
    }
}

/// Turn one `gh` result into a state, moving the partial into place or removing it.
///
/// Why: this is the fail-open site. `gh repo clone` failing part-way leaves a
/// directory that LOOKS like a checkout, and a caller that reported success on
/// it would hand a half-fetched repository to the sweep, which would analyze it
/// and report on it as if it were whole. The rename is what makes "a directory
/// under `repos/` is a completed clone" true by construction rather than by
/// convention.
/// What: on a failure or a budget kill, removes the staged tree and returns
/// why — and when the removal itself fails, says so rather than swallowing it,
/// see [`discard_at`]. On completion, VERIFIES the tree before promoting it — a
/// zero exit that produced no usable checkout becomes [`CloneState::Empty`], and
/// nothing is promoted. Only a verified tree is renamed onto `dest`, and a
/// promoted tree that cannot then be MEASURED is removed again: an unmeasurable
/// checkout is one the ceiling can never be enforced against, which is the same
/// answer [`watchdog`] gives for the same reading (#5669).
/// Test: `super::clone_tests::a_failed_clone_leaves_nothing_behind`,
/// `super::clone_tests::a_successful_clone_is_renamed_into_place`,
/// `super::clone_tests::a_commitless_clone_is_not_a_usable_checkout`,
/// `super::clone_tests::a_budget_kill_removes_the_staged_tree`,
/// `super::clone_tests::a_staged_tree_that_cannot_be_removed_is_its_own_gap`.
pub(super) fn finish_one(dest: &Path, staged: &Path, outcome: watchdog::Outcome) -> Finished {
    match outcome {
        // #6001: the reason arrives as a string rather than a `GhError`, because
        // acquisition now has two mechanisms and only one of them is `gh`.
        watchdog::Outcome::Failed(reason) => {
            return discard_at(staged, CloneState::Failed(reason));
        }
        // #5669: the same removal a failure gets — a tree the budget stopped is
        // as unusable as one the remote refused, and leaving it would defeat
        // the ceiling it just crossed.
        watchdog::Outcome::OverBudget {
            staged_bytes,
            budget_bytes,
        } => {
            return discard_at(
                staged,
                CloneState::BudgetExceeded {
                    staged_bytes,
                    budget_bytes,
                },
            );
        }
        watchdog::Outcome::Completed => {}
    }
    // #5215: verify BEFORE the rename, so an unusable tree never occupies the
    // destination even briefly.
    if let Err(why) = verify_checkout(staged) {
        return discard_at(staged, CloneState::Empty(why));
    }
    if let Err(source) = std::fs::rename(staged, dest) {
        return discard_at(
            staged,
            CloneState::Failed(format!(
                "clone completed but could not be moved into place: {source}"
            )),
        );
    }
    match measure_tree(dest) {
        Ok((bytes, bytes_complete)) => Finished {
            state: CloneState::Cloned,
            bytes,
            bytes_complete,
            residue: None,
        },
        // #5669: a promoted tree that measures 0 for any size would spend the
        // whole budget invisibly, so it is discarded rather than counted.
        Err(why) => discard_at(
            dest,
            CloneState::Failed(format!(
                "{why} — a checkout of unknown size cannot be held to the disk budget"
            )),
        ),
    }
}
