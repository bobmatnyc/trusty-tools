//! A read-only census of orphaned index registrations, served over HTTP (#6371).
//!
//! Why: `indexes.toml` accumulates registrations whose `root_path` was deleted —
//! wiped `$TMPDIR` roots, removed worktrees, projects deleted from disk. The
//! boot reaper heals the ones it is allowed to touch, but a registration the
//! #767 allowlist excluded never reaches the in-memory registry, so it is
//! invisible to `GET /indexes` and to every console roster built from it
//! (#6363). An operator could see 60 of them keep `warm_boot_degraded` true and
//! had no way to LIST them short of opening `indexes.toml` by hand.
//!
//! What: `GET /registry/orphans` reads `indexes.toml` — the registry file, not
//! the live handles — classifies every row's root, and answers with the census.
//! It removes nothing. Removal stays `DELETE /indexes/{id}`, which is the one
//! deregistration path in this crate.
//!
//! The classification is three-valued on purpose. A root that is GONE and a
//! root that cannot be CHECKED are different facts, and collapsing them is how
//! a temporarily-unmounted volume gets listed for deletion. `exists()` answers
//! `false` for both, so [`classify_root`] routes the reap decision itself
//! through [`is_reapable_orphan`] — the crate's one definition of "safe to
//! reap" — and reports everything it declines as indeterminate rather than as
//! an orphan.
//!
//! Test: `classify_root_*` and `census_*` below.

use std::path::Path;

use serde::Serialize;

use crate::service::orphan_reaper::is_reapable_orphan;
use crate::service::persistence::PersistedIndex;

/// Why a root that is neither present nor a safe orphan could not be judged.
const REASON_EXTERNAL_VOLUME: &str =
    "the root is on an external volume; a mounted-but-unreadable volume is \
     indistinguishable from a deleted one without stat'ing it";

/// Why a missing root whose parent is also missing is not called an orphan.
const REASON_PARENT_MISSING: &str =
    "the root is missing and so is its parent directory, which is what an \
     unmounted volume looks like; the registration may become valid again";

/// What is known about one registration's `root_path`.
///
/// Why: the console offers to delete what this reports, so "gone" has to be a
/// stronger claim than "I looked and got `false`". The third arm is the whole
/// point — it is the answer for every root the daemon declines to judge, and
/// it renders as a listed-but-not-selected row rather than as a deletion
/// candidate.
/// What: `Present` — the root is on disk. `Orphaned` — [`is_reapable_orphan`]
/// says the root is gone and its parent survives it. `Indeterminate` — neither,
/// with the reason.
/// Test: `classify_root_reports_a_live_root_as_present`,
/// `classify_root_reports_a_deleted_root_as_orphaned`,
/// `classify_root_will_not_judge_a_root_whose_parent_is_also_gone`,
/// `classify_root_will_not_judge_an_external_volume`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootState {
    /// The root is on disk.
    Present,
    /// The root is gone and its removal is safe to act on.
    Orphaned,
    /// The root could not be judged; the string says why.
    Indeterminate(&'static str),
}

/// Classify one registration's root.
///
/// Why: see the module docs — the reap decision must have exactly one
/// definition, and `Orphaned` is decided by nothing but [`is_reapable_orphan`].
/// The two guards ahead of it are not a second copy of that predicate: they
/// separate the two cases it folds into a single `false`, so that a live root
/// and an unjudgeable one do not both read as "not an orphan" with no way to
/// tell which. The `/Volumes` prefix is checked BEFORE any syscall for the
/// reason `is_reapable_orphan` documents — a TCC-blocked external volume can
/// make `exists()` answer `false`, or hang.
/// What: `Indeterminate` for an external volume, `Present` when the root is on
/// disk, `Orphaned` when [`is_reapable_orphan`] agrees, `Indeterminate`
/// otherwise.
/// Test: the `classify_root_*` tests below.
pub fn classify_root(root: &Path) -> RootState {
    if root.starts_with("/Volumes") {
        return RootState::Indeterminate(REASON_EXTERNAL_VOLUME);
    }
    if root.exists() {
        return RootState::Present;
    }
    if is_reapable_orphan(root) {
        return RootState::Orphaned;
    }
    RootState::Indeterminate(REASON_PARENT_MISSING)
}

/// One registration whose root is gone.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OrphanRow {
    /// The registration id — what `DELETE /indexes/{id}` takes.
    pub id: String,
    /// The root that is no longer on disk, for the operator's confirm list.
    pub root_path: String,
    /// Whether the index stores its data beside the (now absent) root.
    pub colocated: bool,
    /// The canonical `owner/repo` this root belonged to, when recorded.
    pub repo_identity: Option<String>,
}

/// One registration whose root could not be judged.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UnjudgedRow {
    /// The registration id.
    pub id: String,
    /// The root that could not be judged.
    pub root_path: String,
    /// Why this daemon declined to call it gone.
    pub reason: String,
}

/// The census `GET /registry/orphans` answers with.
///
/// Why: `orphans` is the deletion candidate list and `indeterminate` is
/// deliberately a SEPARATE field rather than a flag on one list — a caller that
/// ignores the distinction and deletes everything it was handed then deletes
/// only what this daemon was willing to call gone.
/// What: the two lists, plus how many rows were live and how many the registry
/// held in total, so a caller can show "3 of 60" without summing anything.
/// Test: `census_separates_gone_from_unjudgeable`,
/// `census_counts_every_registry_row`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OrphanCensus {
    /// Registrations whose root is gone. Safe to offer for deletion.
    pub orphans: Vec<OrphanRow>,
    /// Registrations this daemon declined to judge. Never offer these.
    pub indeterminate: Vec<UnjudgedRow>,
    /// How many registrations have a live root.
    pub live_count: usize,
    /// How many registrations `indexes.toml` holds in total.
    pub total: usize,
}

/// Build the census from registry rows.
///
/// Why: separated from the handler so the classification is testable without a
/// daemon, an HTTP client, or the operator's real `indexes.toml`.
/// What: classifies each row's root and buckets it. Row order is the registry's.
/// Test: `census_separates_gone_from_unjudgeable`, `census_counts_every_registry_row`,
/// `census_of_an_empty_registry_is_empty`.
pub fn build_census(entries: &[PersistedIndex]) -> OrphanCensus {
    let mut census = OrphanCensus {
        orphans: Vec::new(),
        indeterminate: Vec::new(),
        live_count: 0,
        total: entries.len(),
    };
    for entry in entries {
        let root_path = entry.root_path.display().to_string();
        match classify_root(&entry.root_path) {
            RootState::Present => census.live_count += 1,
            RootState::Orphaned => census.orphans.push(OrphanRow {
                id: entry.id.clone(),
                root_path,
                colocated: entry.colocated,
                repo_identity: entry.repo_identity.clone(),
            }),
            RootState::Indeterminate(reason) => census.indeterminate.push(UnjudgedRow {
                id: entry.id.clone(),
                root_path,
                reason: reason.to_string(),
            }),
        }
    }
    census
}

/// `GET /registry/orphans` — list the registrations whose root is gone (#6371).
///
/// Why: this reads `indexes.toml` rather than `state.registry`, and that is the
/// entire reason the route exists. A registration the allowlist excluded at
/// warm boot is absent from the in-memory registry, so `GET /indexes` cannot
/// show it and neither can anything built from it (#6363) — yet it is exactly
/// the row an operator needs to see and remove.
/// What: reads the registry file on a blocking worker (a root on a network
/// mount can make a `stat` slow), builds the census, and answers with it. A
/// registry that cannot be READ is a `500` carrying the reason — never an empty
/// census, which would read as "nothing to clean up".
/// Test: `census_separates_gone_from_unjudgeable` covers the body this handler
/// serialises; the handler adds only the file read and the error mapping.
pub async fn registry_orphans_handler() -> axum::response::Response {
    use axum::response::IntoResponse as _;

    let built = tokio::task::spawn_blocking(|| -> anyhow::Result<OrphanCensus> {
        let path = crate::service::persistence::indexes_toml_path()?;
        let entries = crate::service::persistence::load_index_registry_at(&path)?;
        Ok(build_census(&entries))
    })
    .await;

    match built {
        Ok(Ok(census)) => axum::Json(census).into_response(),
        Ok(Err(e)) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({
                "error": format!("could not read the index registry: {e:#}")
            })),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({
                "error": format!("the registry census task failed: {e}")
            })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(id: &str, root: impl Into<PathBuf>) -> PersistedIndex {
        PersistedIndex::new(id.to_string(), root.into())
    }

    /// Why: a live root must never be offered for deletion.
    /// Test: this is the test.
    #[test]
    fn classify_root_reports_a_live_root_as_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(classify_root(tmp.path()), RootState::Present);
    }

    /// Why (#6371): the leak class this feature exists for — a wiped `$TMPDIR`
    /// root whose parent (`/private/var/folders/…/T`) is still there.
    /// Test: this is the test.
    #[test]
    fn classify_root_reports_a_deleted_root_as_orphaned() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let gone = tmp.path().join("wiped-root");
        assert_eq!(classify_root(&gone), RootState::Orphaned);
    }

    /// Why (#6371, the fail-open rule): a root whose PARENT is also gone is
    /// what an unmounted volume looks like. `exists()` answers `false` for it
    /// exactly as it does for a deleted directory, so classifying on `exists()`
    /// alone would list a volume's whole index roster for deletion the moment
    /// it was unplugged. Unknown is not stale.
    /// Test: this is the test.
    #[test]
    fn classify_root_will_not_judge_a_root_whose_parent_is_also_gone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let buried = tmp.path().join("gone-parent").join("gone-child");
        assert_eq!(
            classify_root(&buried),
            RootState::Indeterminate(REASON_PARENT_MISSING)
        );
    }

    /// Why (#723 / #873): a mounted-but-TCC-blocked external volume answers
    /// `false` to `exists()` for a perfectly live root, so `/Volumes` is never
    /// judged and never stat'ed.
    /// Test: this is the test.
    #[test]
    fn classify_root_will_not_judge_an_external_volume() {
        assert_eq!(
            classify_root(Path::new("/Volumes/Kemono/some/project")),
            RootState::Indeterminate(REASON_EXTERNAL_VOLUME)
        );
    }

    /// Why (#6371): the census's contract — the two lists must not mix, or a
    /// caller that deletes what it was handed deletes an unjudged root.
    /// Test: this is the test.
    #[test]
    fn census_separates_gone_from_unjudgeable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let census = build_census(&[
            entry("live", tmp.path()),
            entry("wiped", tmp.path().join("wiped-root")),
            entry("unplugged", "/Volumes/Kemono/project"),
        ]);

        assert_eq!(
            census
                .orphans
                .iter()
                .map(|o| o.id.as_str())
                .collect::<Vec<_>>(),
            vec!["wiped"],
            "only the deleted root is a deletion candidate: {census:?}"
        );
        assert_eq!(
            census
                .indeterminate
                .iter()
                .map(|u| u.id.as_str())
                .collect::<Vec<_>>(),
            vec!["unplugged"],
            "the external volume must be reported, not offered: {census:?}"
        );
        assert!(
            !census.indeterminate[0].reason.is_empty(),
            "an unjudged row must say why"
        );
    }

    /// Why: an operator reads "3 orphaned of 60 registered", so every row has
    /// to land in exactly one bucket and the total has to be the file's.
    /// Test: this is the test.
    #[test]
    fn census_counts_every_registry_row() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let census = build_census(&[
            entry("live", tmp.path()),
            entry("wiped-a", tmp.path().join("a")),
            entry("wiped-b", tmp.path().join("b")),
            entry("unplugged", "/Volumes/Kemono/project"),
        ]);
        assert_eq!(census.total, 4);
        assert_eq!(census.live_count, 1);
        assert_eq!(
            census.live_count + census.orphans.len() + census.indeterminate.len(),
            census.total,
            "every row must be counted exactly once: {census:?}"
        );
    }

    /// Why: an empty registry must answer an empty census, not an error.
    /// Test: this is the test.
    #[test]
    fn census_of_an_empty_registry_is_empty() {
        let census = build_census(&[]);
        assert_eq!(census.total, 0);
        assert!(census.orphans.is_empty());
        assert!(census.indeterminate.is_empty());
    }

    /// Why (#6371): the orphan row carries what the confirm list shows — the
    /// dead path, not only the id — so an operator can recognise what they are
    /// about to remove.
    /// Test: this is the test.
    #[test]
    fn an_orphan_row_carries_the_dead_root_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let gone = tmp.path().join("wiped-root");
        let census = build_census(&[entry("wiped", gone.clone())]);
        assert_eq!(census.orphans[0].root_path, gone.display().to_string());
    }
}
