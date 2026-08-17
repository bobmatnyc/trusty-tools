//! Pin-file I/O — the writer, plus slug helpers layered on the shared resolver.
//!
//! Why: the pin file's SCHEMA and READ path moved to
//! `trusty_common::palace_resolve` (#5811) so every crate that asks "which
//! palace does this project use?" reads the same file the same way. trusty-memory
//! keeps the WRITER — it is the only crate that creates a pin — and the slug
//! helpers its own CLI and MCP surfaces call.
//! What: re-exports `ProjectPin`, `PIN_SCHEMA_VERSION`, `PIN_FILE_REL` and
//! `read_project_pin`; defines `write_project_pin`, `project_slug_from_basename`,
//! `project_slug_at`, `project_slug_at_readonly`, `pinned_slug_at`, `project_slug`.
//! Test: `pin_file_read_when_present`, `absent_pin_writes_computed_slug`,
//! `renamed_dir_with_pin_resolves_to_original_slug`,
//! `malformed_pin_does_not_fall_back_to_basename`.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::messaging::slugify_string;

use super::detection::{find_project_root, is_unsafe_pin_location, TRUSTY_TOOLS_DIR};

// #5811: schema and read path are shared. A second definition here would let
// the writer and the readers drift on the same file.
pub use trusty_common::palace_resolve::{ProjectPin, PIN_FILE_REL, PIN_SCHEMA_VERSION};

/// Read the palace pin from `.trusty-tools/trusty-memory.yaml` at `root`.
///
/// Why: kept as a trusty-memory entry point because the CLI (`link`, `doctor`)
/// and the startup scan call it; the logic itself is the shared one.
/// What: delegates to [`trusty_common::palace_resolve::read_project_pin`].
/// `Ok(None)` when the file is absent; `Err` when it exists but cannot be read
/// or parsed — never a silent fallthrough.
/// Test: `pin_file_read_when_present`, `read_project_pin_returns_none_when_absent`.
pub fn read_project_pin(root: &Path) -> Result<Option<ProjectPin>> {
    trusty_common::palace_resolve::read_project_pin(root).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Write a palace pin to `.trusty-tools/trusty-memory.yaml` at `root`.
///
/// Why: the lazy-write path in [`project_slug_at`] and the explicit
/// `trusty-memory link` backfill command both need to emit the same YAML
/// schema. A single writer keeps the format consistent.
/// What: creates `.trusty-tools/` if missing, serialises `pin`, and writes it
/// atomically (write to `<file>.tmp`, then rename). Returns the path written.
/// Test: `write_and_read_pin_round_trips`, `write_pin_omits_null_note`.
pub fn write_project_pin(root: &Path, pin: &ProjectPin) -> Result<PathBuf> {
    let dir = root.join(TRUSTY_TOOLS_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("create {}: {e}", dir.display()))?;
    let pin_path = root.join(PIN_FILE_REL);
    let tmp_path = pin_path.with_extension("yaml.tmp");
    let yaml = serde_yaml::to_string(pin).map_err(|e| anyhow::anyhow!("serialise pin: {e}"))?;
    let header = "# .trusty-tools/trusty-memory.yaml\n\
                  # This file pins the trusty-memory palace slug for this project.\n\
                  # Commit it so the linkage survives directory renames and drive reorgs.\n\
                  # Schema: https://github.com/bobmatnyc/trusty-tools (trusty-tools convention)\n\n";
    let content = format!("{header}{yaml}");
    std::fs::write(&tmp_path, &content)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &pin_path).map_err(|e| {
        anyhow::anyhow!(
            "rename {} → {}: {e}",
            tmp_path.display(),
            pin_path.display()
        )
    })?;
    Ok(pin_path)
}

/// Compute the palace slug purely from the directory basename.
///
/// Why: the resolution order needs the basename derivation without the
/// pin-file side effects, and both paths must be testable in isolation.
/// What: slugifies the last path component of `root`. `None` when it is empty
/// or slugifies to empty.
/// Test: `project_slug_at_returns_root_basename_slug`.
pub fn project_slug_from_basename(root: &Path) -> Option<String> {
    let basename = root.file_name()?.to_str()?;
    let slug = slugify_string(basename);
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
}

/// Read the pin at the project root above `start`, failing closed.
///
/// Why: `Ok(None)` (absent) and `Err` (present but untrustworthy) must reach
/// callers as different outcomes, because only the first may fall through to a
/// derived name. Returning `None` for both is the fail-open this replaced.
/// What: returns `Ok(None)` when there is no project root or no pin file,
/// `Ok(Some(slug))` for a valid non-empty pin, and `Err` when the pin exists
/// but cannot be read, cannot be parsed, or names an empty palace.
fn pin_outcome_at(start: &Path) -> Result<Option<String>> {
    let Some(root) = find_project_root(start) else {
        return Ok(None);
    };
    match read_project_pin(&root)? {
        Some(pin) if !pin.palace.trim().is_empty() => Ok(Some(pin.palace)),
        Some(_) => Err(anyhow::anyhow!(
            "palace pin {} has an empty `palace` field",
            root.join(PIN_FILE_REL).display()
        )),
        None => Ok(None),
    }
}

/// Derive a palace slug from the project root found at or above `start`.
///
/// Why: the interactive-command path (issue #88, Phase 1). Palace names must
/// match the canonical slug of the project they belong to, and that slug must
/// survive directory renames — the pin file is the stable anchor.
/// What: returns the pinned slug when one exists; otherwise computes the
/// basename slug and lazily writes the pin (best-effort, non-fatal) so future
/// calls take the pinned path. Returns `None` when no project root is found OR
/// when a pin exists but cannot be trusted (#5811 — deriving past a broken pin
/// is what sent writes to the wrong palace).
/// Test: `pin_file_read_when_present`, `absent_pin_writes_computed_slug`,
/// `renamed_dir_with_pin_resolves_to_original_slug`,
/// `malformed_pin_does_not_fall_back_to_basename`.
pub fn project_slug_at(start: &Path) -> Option<String> {
    let root = find_project_root(start)?;

    match pin_outcome_at(start) {
        Ok(Some(slug)) => return Some(slug),
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(
                path = %root.join(PIN_FILE_REL).display(),
                "palace pin exists but cannot be trusted ({e:#}); refusing to derive past it"
            );
            return None;
        }
    }

    // Compute from basename and lazily write the pin. `is_unsafe_pin_location`
    // (PR #492) keeps the write out of a temp/home/root directory, where it
    // would poison every subdirectory; the slug is still returned so memory
    // operations work.
    let slug = project_slug_from_basename(&root)?;
    if is_unsafe_pin_location(&root) {
        tracing::debug!(
            slug = %slug,
            root = %root.display(),
            "skipping lazy pin write: root is a system/home/temp dir"
        );
        return Some(slug);
    }
    let pin = ProjectPin::new(slug.clone());
    match write_project_pin(&root, &pin) {
        Ok(path) => tracing::debug!(
            slug = %slug,
            path = %path.display(),
            "wrote palace pin file (lazy init)"
        ),
        Err(e) => tracing::warn!(
            slug = %slug,
            root = %root.display(),
            "could not write palace pin file ({e:#}); slug will remain basename-derived"
        ),
    }
    Some(slug)
}

/// Return the *pinned* palace slug for the project at or above `start`, and
/// ONLY when a trustworthy pin file exists — never the basename fallback.
///
/// Why: the default-palace resolver consults the pin in isolation so the
/// basename fallback cannot shadow the git derivation that sits below it
/// (issue #1217). Since #5811 it also distinguishes an absent pin from a broken
/// one: both return `None`, but a broken one logs and is reported by
/// [`trusty_common::palace_resolve::resolve_palace`] as an error rather than a
/// fallthrough.
/// What: returns `Some(slug)` strictly when a valid non-empty pin exists.
/// Never writes, never panics.
/// Test: `pinned_slug_at_returns_pin_when_present`,
/// `pinned_slug_at_returns_none_without_pin`,
/// `malformed_pin_does_not_fall_back_to_basename`.
pub fn pinned_slug_at(start: &Path) -> Option<String> {
    match pin_outcome_at(start) {
        Ok(slug) => slug,
        Err(e) => {
            tracing::warn!("palace pin exists but cannot be trusted ({e:#}); ignoring");
            None
        }
    }
}

/// Derive a palace slug from the project root above `start`, WITHOUT the
/// lazy-write side effect.
///
/// Why: hooks and polled GUI endpoints run in read-only or short-lived contexts
/// where creating `.trusty-tools/trusty-memory.yaml` would be surprising. The
/// writing variant ([`project_slug_at`]) stays the right choice for interactive
/// commands.
/// What: returns the pinned slug when one exists, else the basename slug, and
/// never writes. Returns `None` when no project root is found OR when a pin
/// exists but cannot be trusted (#5811).
/// Test: `project_slug_at_readonly_reads_existing_pin`,
/// `project_slug_at_readonly_no_write_when_absent`,
/// `malformed_pin_does_not_fall_back_to_basename`,
/// `absent_pin_still_falls_back_to_basename`.
pub fn project_slug_at_readonly(start: &Path) -> Option<String> {
    let root = find_project_root(start)?;
    match pin_outcome_at(start) {
        Ok(Some(slug)) => Some(slug),
        Ok(None) => project_slug_from_basename(&root),
        Err(e) => {
            tracing::warn!(
                path = %root.join(PIN_FILE_REL).display(),
                "palace pin exists but cannot be trusted ({e:#}); refusing to derive past it"
            );
            None
        }
    }
}

/// Derive a palace slug for the current working directory.
///
/// Why: convenience wrapper for callers that want the "natural" project slug
/// (CLI commands, MCP handlers, tests running inside a repo).
/// What: reads `std::env::current_dir()` then delegates to [`project_slug_at`].
/// Test: `project_slug_finds_git_root`.
pub fn project_slug() -> Result<Option<String>> {
    let cwd = std::env::current_dir().map_err(|e| anyhow::anyhow!("read cwd: {e}"))?;
    Ok(project_slug_at(&cwd))
}
