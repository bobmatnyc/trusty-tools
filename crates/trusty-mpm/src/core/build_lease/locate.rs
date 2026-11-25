//! Find the freshest built binary for one checkout: `tm build-lease where`
//! (#10835).
//!
//! Why: a leased build lands in whichever pool slot the lease granted, while
//! the shell's `CARGO_TARGET_DIR` names another directory. A QA run that
//! guessed between them ran a stale `debug/tm` (#10755).
//! What: [`candidate_dirs`] lists the target directories a build of this
//! checkout can have written to; [`freshest`] returns the existing artifact
//! with the newest mtime, or every directory it searched. Freshness is the
//! artifact's own mtime: cargo rewrites a binary when it relinks it, and the
//! lease's per-slot records (`slot-K.last`, `.trusty-slot-last-checkout`)
//! name the checkout that took a slot, not when its binary was linked. This
//! module reads only: no lease, no slot claim, no cargo.
//! Test: `locate_tests.rs`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::builder_slot_pool::staging::repo_slots;

/// One directory a build of this checkout may have written to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Candidate {
    /// Where the directory came from: `slot-<n>`, `CARGO_TARGET_DIR`,
    /// `shared target dir`, or `checkout target/`.
    pub source: String,
    /// The target directory (the parent of `debug/` and `release/`).
    pub dir: PathBuf,
}

/// The freshest artifact found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Located {
    /// Absolute path of the artifact.
    pub path: PathBuf,
    /// The candidate's [`Candidate::source`].
    pub source: String,
    /// The candidate's target directory.
    pub target_dir: PathBuf,
    /// The artifact's mtime, in seconds since the Unix epoch.
    pub mtime_unix: u64,
}

/// The target directories to search for `checkout`'s artifacts.
///
/// Why: the build may have run under a lease (a pool slot) or outside one
/// (the target directory in effect), so both are searched.
/// What: the target directory in effect first — `env_target` (relative
/// values resolve against `checkout`), else `<checkout>/target` — then
/// `shared` (the per-repo directory `.envrc` exports), then every SEEDED
/// `slot-<n>` under `pool_repo_dir` by index. An unseeded slot was never
/// granted to a build, so it is skipped. Duplicates are dropped.
/// Test: `candidates_put_the_target_in_effect_first_and_skip_unseeded_slots`.
#[must_use]
pub fn candidate_dirs(
    checkout: &Path,
    env_target: Option<&Path>,
    shared: Option<&Path>,
    pool_repo_dir: Option<&Path>,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut push = |source: String, dir: PathBuf| {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        let seen = out
            .iter()
            .any(|c| c.dir.canonicalize().unwrap_or_else(|_| c.dir.clone()) == key);
        if !seen {
            out.push(Candidate { source, dir });
        }
    };
    match env_target {
        Some(dir) => push("CARGO_TARGET_DIR".to_string(), checkout.join(dir)),
        None => push("checkout target/".to_string(), checkout.join("target")),
    }
    if let Some(dir) = shared {
        push("shared target dir".to_string(), dir.to_path_buf());
    }
    if let Some(repo_dir) = pool_repo_dir {
        for (index, seeded) in repo_slots(repo_dir) {
            if seeded {
                push(
                    format!("slot-{index}"),
                    repo_dir.join(format!("slot-{index}")),
                );
            }
        }
    }
    out
}

/// The newest existing `<dir>/<profile>/<bin>` across `candidates`.
///
/// What: `Ok` with the artifact whose mtime is newest; on a tie the earlier
/// candidate wins. Only regular files count (symlinks are followed).
///
/// # Errors
///
/// The artifact path probed in every candidate, in order, when none exists.
/// The caller names them; it never prints a guessed path.
///
/// Test: `the_newest_slot_artifact_wins`, `a_missing_binary_names_every_dir_searched`,
/// `the_profile_selects_the_artifact_directory`.
pub fn freshest(
    candidates: &[Candidate],
    bin: &str,
    profile: &str,
) -> Result<Located, Vec<PathBuf>> {
    let file = format!("{bin}{}", std::env::consts::EXE_SUFFIX);
    let mut best: Option<(SystemTime, Located)> = None;
    let mut searched = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let path = candidate.dir.join(profile).join(&file);
        let modified = std::fs::metadata(&path)
            .ok()
            .filter(std::fs::Metadata::is_file)
            .and_then(|m| m.modified().ok());
        searched.push(path.clone());
        let Some(modified) = modified else { continue };
        if best.as_ref().is_some_and(|(t, _)| *t >= modified) {
            continue;
        }
        let mtime_unix = modified
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        best = Some((
            modified,
            Located {
                path,
                source: candidate.source.clone(),
                target_dir: candidate.dir.clone(),
                mtime_unix,
            },
        ));
    }
    best.map(|(_, located)| located).ok_or(searched)
}

/// Is `bin` a bare binary name, not a path?
///
/// Why: the name is joined onto each target directory, so `../x` or `a/b`
/// would probe outside it. Test: `a_binary_name_with_a_path_is_rejected`.
#[must_use]
pub fn is_bare_name(bin: &str) -> bool {
    !bin.is_empty() && bin != "." && bin != ".." && !bin.contains(['/', '\\'])
}

#[cfg(test)]
#[path = "locate_tests.rs"]
mod tests;
