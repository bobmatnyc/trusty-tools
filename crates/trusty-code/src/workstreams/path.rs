//! Storage-path derivation for the workstream store (DOC-48 §3.1).
//!
//! Why: the spec pins the filename shape (`workstreams-{project_slug}-{hash}.json`)
//! to the daemon's OWN [`ProjectBinding`] — never a caller-supplied label —
//! so two daemons bound to the same project always agree on one file, and
//! two different projects (even same-named checkouts) never collide.
//! What: [`store_filename`] derives the filesystem-safe slug + a truncated
//! SHA-256 fingerprint of the canonical root from a [`ProjectBinding`];
//! [`store_path`] joins it under a data directory; [`default_data_dir`]
//! resolves `~/.trusty-code` (mirrors trusty-mpm's `~/.trusty-mpm`
//! precedent, `crates/trusty-mpm/src/core/paths.rs`).
//! Test: `path_tests`.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::binding::ProjectBinding;

/// Number of hex characters kept from the SHA-256 digest (§3.1: "truncated
/// to 8 chars").
const HASH_LEN: usize = 8;

/// Fixed filename used when the daemon is projectless (`ProjectBinding::None`).
///
/// Why: DOC-48 §2.3/§3.1 only specify the filename shape for a BOUND
/// project; a projectless daemon has no path to slug or hash. Rather than
/// fabricate a fake root, this ticket resolves the ambiguity with one fixed,
/// documented filename — a projectless daemon has at most one workstream
/// store regardless of which directory `tcode serve` happened to start in.
const PROJECTLESS_FILENAME: &str = "workstreams-projectless.json";

/// Resolve `~/.trusty-code`, creating it at mode `0700` and tightening it if a
/// previous run left it permissive.
///
/// Why: mirrors `trusty-mpm`'s `~/.trusty-mpm` precedent
/// (`crates/trusty-mpm/src/core/paths.rs`) rather than inventing a new
/// resolution convention. #5426 moved the resolution itself into
/// [`crate::paths::private_state`], which also owns the `0700` mode this
/// directory must carry — a workstream store sitting beside transcripts and
/// logs is private state, not an independent location. #6999: this function is
/// the ONE place every private-state writer resolves that root — the workstream
/// store, `serve`'s router, and `agent_loop::telemetry`'s own `default_data_dir`
/// all end here — so the mode guarantee is applied here rather than at each
/// writer's own `create_dir_all`, which would apply the process umask and leave
/// `0755`. Applying it anywhere else would be a second copy of the rule.
/// What: delegates to [`crate::paths::private_state::ensure_private_state_dir`]:
/// `dirs::home_dir()/.trusty-code`, created if missing and chmod'd to `0700` if
/// any group or other bit is set. On an I/O failure it falls back to the plain
/// [`crate::paths::private_state::private_state_dir`] path, logging at `warn` —
/// a harness that cannot create its state directory should still report what it
/// tried, not panic. Never panics.
/// Test: `path_tests::default_data_dir_is_dot_trusty_code`,
/// `path_tests::ensure_or_report_creates_and_tightens_a_permissive_dir`,
/// `tests/cli_e2e.rs::a_non_import_command_tightens_a_permissive_private_state_dir`,
/// `paths::private_state::private_state_tests::private_state_dir_matches_home_when_available`.
pub fn default_data_dir() -> PathBuf {
    // #6999: the README promised an existing permissive `~/.trusty-code` is
    // tightened on the next run; before this, only `tcode paths import` did it.
    // `private_state_dir` owns home resolution, including the no-home fallback.
    ensure_or_report(crate::paths::private_state::private_state_dir())
}

/// Create and tighten an already-resolved private-state root, or report why not.
///
/// Why: the hermetic core of [`default_data_dir`] (#6999), mirroring
/// [`crate::paths::private_state::private_state_dir_at`]'s reason for existing —
/// a test drives it with a temp directory, so no test creates, chmods, or
/// reports on the developer's real `~/.trusty-code`.
/// What: [`crate::paths::private_state::ensure_dir`], returning `dir` either
/// way. On failure — the path is a regular file, the home directory is
/// read-only, the filesystem is full — it logs at `warn` with the path and the
/// error and hands back the same path, so a harness that cannot create its state
/// directory still runs and still says what it tried. Never panics.
/// Test: `path_tests::ensure_or_report_creates_and_tightens_a_permissive_dir`,
/// `path_tests::ensure_or_report_falls_back_when_the_path_is_a_file`,
/// `tests/cli_e2e.rs::an_unusable_private_state_path_warns_instead_of_panicking`.
pub(crate) fn ensure_or_report(dir: PathBuf) -> PathBuf {
    if let Err(e) = crate::paths::private_state::ensure_dir(&dir) {
        tracing::warn!(
            path = %dir.display(),
            error = %e,
            "could not create the trusty-code private state directory; \
             continuing with the resolved path, which may not exist or may \
             be readable by other users on this machine"
        );
    }
    dir
}

/// Turn an arbitrary string into a filesystem-safe, lowercase, hyphenated
/// slug.
///
/// Why: a project's directory basename may contain characters unsafe for a
/// filename component (spaces, underscores, mixed case); the slug must be
/// stable and readable.
/// What: keeps ASCII alphanumerics (lowercased), collapses any run of other
/// characters into a single `-`, and trims leading/trailing `-`. An
/// all-non-alphanumeric input slugs to `"project"` rather than an empty
/// string.
/// Test: `path_tests::slugify_*`.
fn slugify(raw: &str) -> String {
    let mut slug = String::with_capacity(raw.len());
    let mut last_was_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "project".to_string()
    } else {
        slug
    }
}

/// Deterministic 8-hex-char fingerprint of a project root path.
///
/// Why: disambiguates multiple checkouts that share a directory basename
/// (§3.1: "to handle multiple checkouts of the same project").
/// What: SHA-256 of the path's platform string, truncated to
/// [`HASH_LEN`] lowercase hex characters. Deterministic for a given path,
/// unlike a per-process `RandomState`-seeded hasher.
/// Test: `path_tests::project_hash_is_deterministic`,
/// `path_tests::project_hash_differs_for_different_paths`.
fn project_hash(root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(root.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex.truncate(HASH_LEN);
    hex
}

/// Derive the workstream store's filename from a daemon's project binding
/// (DOC-48 §3.1).
///
/// Why: the ONE place the filename shape is assembled, so the store and any
/// future inspection tooling can never disagree on it.
/// What: for a bound project, `workstreams-{slug}-{hash}.json` where `slug`
/// comes from the root's basename and `hash` fingerprints the full root
/// path (§3.1). For [`ProjectBinding::None`], the fixed
/// [`PROJECTLESS_FILENAME`] (see its docs for why).
/// Test: `path_tests::store_filename_for_bound_project`,
/// `path_tests::store_filename_for_projectless`.
pub fn store_filename(binding: &ProjectBinding) -> String {
    match binding.root() {
        Some(root) => {
            let basename = root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| root.display().to_string());
            let slug = slugify(&basename);
            let hash = project_hash(root);
            format!("workstreams-{slug}-{hash}.json")
        }
        None => PROJECTLESS_FILENAME.to_string(),
    }
}

/// Join [`store_filename`] under a data directory to get the full store path.
///
/// Why: the single entry point `WorkstreamStore::load_for_binding` (and any
/// future daemon boot code) calls to resolve where the store file lives.
/// What: `data_dir.join(store_filename(binding))`.
/// Test: `path_tests::store_path_joins_data_dir_and_filename`.
pub fn store_path(data_dir: &Path, binding: &ProjectBinding) -> PathBuf {
    data_dir.join(store_filename(binding))
}

#[cfg(test)]
#[path = "path_tests.rs"]
mod path_tests;
