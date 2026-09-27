//! Working-directory index resolution, confirmed against the daemon's own
//! index list (#5264, #6864, #8229).
//!
//! Why: two callers resolve an index from a working directory — `serve`'s
//! startup pin (binary) and `search_health`'s cwd fallback (library). The
//! derived id is a bare basename, so two checkouts named alike collide on it;
//! only a `root_path` comparison tells them apart. `search_health` used the
//! bare id with no comparison and reported a different clone's index as this
//! project's, `healthy: true` (#8229). One shared implementation keeps the two
//! callers from disagreeing about which index a directory maps to.
//! What: the candidate derivation, the daemon-list parser, and the
//! confirmation verdict. No I/O beyond `canonicalize`; each caller fetches
//! `GET /indexes?details=true` with its own client.
//!
//! Test: `confirm_accepts_matching_root` and its siblings in the binary's
//! `serve_scope_tests.rs`; `cwd_fallback_resolves_same_basename_checkouts_to_their_own_indexes`
//! in the library.

use std::path::{Path, PathBuf};

/// The index id a working directory maps to, plus the root it was derived from.
///
/// The root is carried alongside the id because the id alone cannot be
/// verified — see [`confirm_candidate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CwdCandidate {
    pub index_id: String,
    pub project_root: PathBuf,
}

/// Derive the candidate index for a working directory.
///
/// Why (#5264): this is the tier that makes a bare `trusty-search serve` — the
/// registration `setup` writes — usable in a multi-project client, without
/// baking any one project into the config file.
/// What: routes through the trusty-common pair `resolve_project_root` (nearest
/// enclosing `.git`, else the directory itself) and `derive_index_id` (the path
/// basename, verbatim), so the id matches what trusty-mpm would register for
/// the same tree. Returns `None` when the derivation yields an empty id, which
/// `derive_index_id` documents for a filesystem-root path — an empty index id
/// addresses nothing and must never become a pin.
///
/// This deliberately does NOT reuse trusty-search's own `detect_project`, which
/// adds a `.trusty-search` marker-file tier and so can resolve a DIFFERENT root
/// than trusty-mpm would for the same tree. Pin derivation must match the
/// register-and-pin path (#1373), so it uses the trusty-common pair that path
/// uses. Consolidating the two would mean moving the marker tier into
/// trusty-common and changing what every existing caller resolves — a
/// behavioral change to a shared crate, not a refactor, so it is left alone.
/// Test: `cwd_candidate_uses_git_root`, `cwd_candidate_falls_back_to_basename`,
/// `cwd_candidate_none_for_root_path`.
pub fn derive_cwd_candidate(cwd: &Path) -> Option<CwdCandidate> {
    let project_root = trusty_common::resolve_project_root(cwd);
    let index_id = trusty_common::derive_index_id(&project_root);
    if index_id.is_empty() {
        return None;
    }
    Some(CwdCandidate {
        index_id,
        project_root,
    })
}

/// One index as the daemon reports it on `GET /indexes?details=true`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonIndex {
    pub id: String,
    /// `None` when the daemon could not render the root as UTF-8.
    pub root_path: Option<PathBuf>,
}

/// The verdict on whether a working-directory candidate may be pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmation {
    /// The daemon serves this id from this exact root — safe to pin.
    Confirmed,
    /// #6864: the derived id does not name this tree, but another registered
    /// index is rooted at it. That index is what the session pins.
    ServedByAnotherId { index_id: String },
    /// The daemon serves the id from a different project root, and no other
    /// registered index is rooted at this one.
    RootMismatch { serving_root: PathBuf },
    /// The daemon does not serve the id at all.
    NotServed,
    /// The daemon serves the id but reported no usable root to compare.
    RootUnknown,
}

/// Compare two roots, tolerating symlinks and platform path aliasing.
///
/// Why: on macOS the same directory is reachable as both `/tmp/x` and
/// `/private/tmp/x`, and a git worktree is routinely reached through a
/// symlink. A byte comparison would call those a mismatch and refuse a pin
/// that is in fact correct.
/// What: exact comparison first (cheap, and the only thing that works when a
/// root no longer exists on disk), then a `canonicalize` comparison. A
/// canonicalize failure on either side falls back to the already-failed exact
/// comparison, so a since-deleted root reads as a mismatch rather than a match.
fn same_root(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Decide whether the daemon's index list confirms a working-directory
/// candidate.
///
/// Why (#5264): the fail-open shape this guards against is a session that looks
/// healthy while serving another project's code. Matching on the derived id
/// alone would do exactly that, because `derive_index_id` is a bare path
/// basename: two checkouts named `api` collide. Comparing the daemon's
/// `root_path` against the root the id was derived from turns that collision
/// into a refusal.
/// What: finds the entry with a matching id, then requires its root to be the
/// same directory. Every other outcome is a distinct refusal reason so the
/// report can say which one happened.
///
/// #6864: refusing is right only when NOTHING serves this tree. The id
/// collision it guards against is the ordinary consequence of two checkouts of
/// one repository, and the second one is routinely registered under a distinct
/// id — `trusty-tools-checkout` beside `trusty-tools` on 2026-09-05 — so the
/// entries already in hand are scanned for a `root_path` that IS this tree
/// before the candidate is refused. That scan costs no request: `entries` is the
/// single `GET /indexes?details=true` the caller already made. The same scan
/// runs when the derived id is served from another root and when it is not
/// served at all, because the remedy is identical in both.
/// Test: `confirm_accepts_matching_root`, `confirm_rejects_colliding_basename`,
/// `confirm_rejects_unknown_index`, `confirm_rejects_null_root`,
/// `confirm_substitutes_the_index_rooted_at_the_cwd`,
/// `confirm_substitutes_when_the_derived_id_is_unserved`.
pub fn confirm_candidate(candidate: &CwdCandidate, entries: &[DaemonIndex]) -> Confirmation {
    let Some(entry) = entries.iter().find(|e| e.id == candidate.index_id) else {
        return match index_serving_root(entries, &candidate.project_root) {
            Some(index_id) => Confirmation::ServedByAnotherId { index_id },
            None => Confirmation::NotServed,
        };
    };
    let Some(root) = entry.root_path.as_ref() else {
        return Confirmation::RootUnknown;
    };
    if same_root(root, &candidate.project_root) {
        return Confirmation::Confirmed;
    }
    match index_serving_root(entries, &candidate.project_root) {
        Some(index_id) => Confirmation::ServedByAnotherId { index_id },
        None => Confirmation::RootMismatch {
            serving_root: root.clone(),
        },
    }
}

/// The id of the registered index whose root IS `root` (#6864).
///
/// Why: an index identifies a directory tree, so the tree — not the id derived
/// from its basename — is what decides which index answers about it. This is the
/// trusty-search half of the rule `trusty_common::search_index`'s
/// `index_id_serving_root` applies at registration time.
/// What: the first entry whose reported `root_path` is the same directory under
/// [`same_root`], which already tolerates symlinks and macOS's `/private`
/// aliasing. Entries with no reported root are skipped — an unconfirmable entry
/// is not a match.
/// Test: `confirm_substitutes_the_index_rooted_at_the_cwd`,
/// `confirm_substitutes_when_the_derived_id_is_unserved`.
fn index_serving_root(entries: &[DaemonIndex], root: &Path) -> Option<String> {
    entries
        .iter()
        .find(|e| e.root_path.as_deref().is_some_and(|r| same_root(r, root)))
        .map(|e| e.id.clone())
}

/// Read the daemon's index list into id/root pairs.
///
/// Why: `?details=true` carries `root_path` alongside each id (#661, added so
/// callers could derive the index from the current project directory), which is
/// the field [`confirm_candidate`] needs. The flat `GET /indexes` returns bare
/// ids and cannot confirm identity.
/// What: tolerates entries missing or misshaping `root_path` by carrying `None`
/// rather than dropping the entry, so a null root reports as unconfirmable
/// instead of as an unknown index. Entries with no string `id` are skipped.
/// Test: `parse_entries_reads_id_and_root`, `parse_entries_tolerates_null_root`.
pub fn parse_index_entries(body: &serde_json::Value) -> Vec<DaemonIndex> {
    body.get("indexes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let id = e.get("id")?.as_str()?.to_string();
                    let root_path = e
                        .get("root_path")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from);
                    Some(DaemonIndex { id, root_path })
                })
                .collect()
        })
        .unwrap_or_default()
}
