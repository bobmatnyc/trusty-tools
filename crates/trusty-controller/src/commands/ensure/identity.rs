//! Pure project-identity derivation for the `tctl ensure` project-setup stages.
//!
//! Why: the index-register and palace-create stages need two project-scoped
//! names: the trusty-search index id and the trusty-memory palace name. Both
//! must be derived deterministically so that re-running `ensure` targets the
//! same index / palace (idempotency). Critically, the palace name must equal the
//! slug trusty-memory's `validate_palace_name` derives from the same cwd
//! (`project_slug_at`: pin-file value, else slugified basename) — otherwise the
//! daemon rejects `POST /api/v1/palaces` with a 400. Keeping the derivation pure
//! (the pin-file read is the only I/O, and it is injected as parsed input in
//! tests) makes every branch unit-testable without a runtime or a real repo.
//!
//! What: [`index_id_for`] (trusty-search index id = directory basename, matching
//! `trusty-search`'s own `resolve_index_name` default), [`palace_slug`] (the pure
//! pin-or-basename slug core), and [`palace_name_for`] (the I/O edge that reads
//! the optional pin file then delegates to [`palace_slug`]). [`slugify`] mirrors
//! `trusty_memory::messaging::slugify_string` so the slugs agree byte-for-byte.
//!
//! Test: `tests` exercise `slugify`, `index_id_for`, and `palace_slug` across the
//! pin-present, basename, and empty cases; the daemon-parity slug cases mirror
//! the trusty-memory unit tests.

use std::path::Path;

use serde::Deserialize;

/// Relative path of the trusty-memory palace pin file within a project root.
///
/// Why: when present, the pin file is the authoritative palace slug (it survives
/// directory renames), so `ensure` must honour it exactly as the daemon does.
/// What: `".trusty-tools/trusty-memory.yaml"` — identical to
/// `trusty_memory::project_root::PIN_FILE_REL`.
/// Test: used by [`palace_name_for`]; the path join is trivial.
pub const PIN_FILE_REL: &str = ".trusty-tools/trusty-memory.yaml";

/// Minimal view of the trusty-memory palace pin file.
///
/// Why: `ensure` only needs the `palace` slug field; deserialising just that
/// (with `serde`'s ignore-unknown default) keeps `ensure` decoupled from the
/// full `ProjectPin` schema in trusty-memory while still reading the same file.
/// What: a single `palace: String` field; all other pin-file keys are ignored.
/// Test: `tests::palace_name_for_reads_pin` round-trips a written pin file.
#[derive(Debug, Deserialize)]
struct PinView {
    /// The pinned palace slug, stored verbatim (no re-slugification).
    palace: String,
}

/// Slugify a string the same way `trusty_memory::messaging::slugify_string` does.
///
/// Why: the palace name must match the daemon's derived slug byte-for-byte or
/// `validate_palace_name` rejects creation. Re-implementing the exact rule here
/// (lowercase, strip a trailing `.git`, map `_`/`-`/space/tab to a single `-`,
/// drop every other char, trim leading/trailing `-`) keeps trusty-controller
/// from taking a dependency on the whole trusty-memory crate while staying in
/// lockstep with its slug semantics.
/// What: applies the canonicalisation rules above and returns the slug.
/// Test: `tests::slugify_matches_memory_rules`.
pub fn slugify(input: &str) -> String {
    let lowered = input.trim().to_ascii_lowercase();
    let stripped = lowered.strip_suffix(".git").unwrap_or(&lowered);
    let mut out = String::with_capacity(stripped.len());
    let mut prev_hyphen = false;
    for c in stripped.chars() {
        let mapped = match c {
            'a'..='z' | '0'..='9' => Some(c),
            '_' | '-' | ' ' | '\t' => Some('-'),
            _ => None,
        };
        if let Some(c) = mapped {
            if c == '-' {
                if !prev_hyphen && !out.is_empty() {
                    out.push('-');
                    prev_hyphen = true;
                }
            } else {
                out.push(c);
                prev_hyphen = false;
            }
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Derive the trusty-search index id for a project directory.
///
/// Why: `ensure` registers the project under the same id `trusty-search index`
/// would use by default, so the two agree and re-registration is a no-op. That
/// default is the directory basename (`resolve_index_name`'s final fallback).
/// What: returns the final path component of `project_root` as a `String`.
/// Returns `None` when the path has no final component (filesystem root).
/// Test: `tests::index_id_is_basename`, `tests::index_id_root_is_none`.
pub fn index_id_for(project_root: &Path) -> Option<String> {
    project_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}

/// Derive the palace slug from an optional pin value and the project root.
///
/// Why: this is the pure decision core mirroring trusty-memory's
/// `project_slug_at` slug rule used by `validate_palace_name`: a committed pin
/// wins authoritatively; otherwise the slugified directory basename is used.
/// Keeping it pure (the pin value is passed in already-read) makes both branches
/// testable without filesystem I/O.
/// What: returns `Some(pin)` when `pin` is `Some` and non-empty after trimming;
/// else the slugified basename of `project_root`; else `None` (root-less path or
/// a basename that slugifies to empty).
/// Test: `tests::palace_slug_prefers_pin`, `tests::palace_slug_falls_back_to_basename`,
/// `tests::palace_slug_empty_pin_falls_through`.
pub fn palace_slug(pin: Option<&str>, project_root: &Path) -> Option<String> {
    if let Some(p) = pin {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    let basename = project_root.file_name()?.to_str()?;
    let slug = slugify(basename);
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
}

/// Read the optional pin file at `project_root` and derive the palace name.
///
/// Why: the I/O edge for [`palace_slug`] — keeps the filesystem read here so the
/// derivation core stays pure. A missing or unreadable pin file is non-fatal
/// (falls through to the basename slug), matching the daemon's tolerance.
/// What: reads `project_root/.trusty-tools/trusty-memory.yaml`; on a successful
/// parse with a non-empty `palace` field, uses it; otherwise (absent, unparsable,
/// or empty) falls back to [`palace_slug`] with `None`.
/// Test: `tests::palace_name_for_reads_pin`, `tests::palace_name_for_basename_when_no_pin`.
pub fn palace_name_for(project_root: &Path) -> Option<String> {
    let pin_path = project_root.join(PIN_FILE_REL);
    let pin_value = match std::fs::read_to_string(&pin_path) {
        Ok(s) => serde_yaml::from_str::<PinView>(&s)
            .ok()
            .map(|p| p.palace)
            .filter(|p| !p.trim().is_empty()),
        Err(_) => None,
    };
    palace_slug(pin_value.as_deref(), project_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Why: the slug must agree with trusty-memory's rule or palace creation is
    /// rejected; pin the representative cases from its own `slug_derivation_cases`.
    /// What: case folding, `_`/space → `-`, collapse, `.git` strip, trim.
    /// Test: This is the test.
    #[test]
    fn slugify_matches_memory_rules() {
        assert_eq!(slugify("Trusty-Tools"), "trusty-tools");
        assert_eq!(slugify("My_Cool Project"), "my-cool-project");
        assert_eq!(slugify("repo.git"), "repo");
        assert_eq!(slugify("  --weird__name--  "), "weird-name");
        assert_eq!(slugify("!!!"), "");
    }

    /// Why: the index id must be the bare directory basename so it matches
    /// `trusty-search index`'s default.
    /// What: `/Users/bob/Projects/trusty-tools` → `trusty-tools`.
    /// Test: This is the test.
    #[test]
    fn index_id_is_basename() {
        assert_eq!(
            index_id_for(Path::new("/Users/bob/Projects/trusty-tools")).as_deref(),
            Some("trusty-tools")
        );
    }

    /// Why: a root-less path has no basename; the id must be `None` so the caller
    /// surfaces an actionable error rather than registering an empty id.
    /// What: `/` → `None`.
    /// Test: This is the test.
    #[test]
    fn index_id_root_is_none() {
        assert_eq!(index_id_for(Path::new("/")), None);
    }

    /// Why: a committed pin is authoritative and wins over the basename (parity
    /// with the daemon's `project_slug_at` step a).
    /// What: pin `pinned-slug` beats basename `checkout-dir`.
    /// Test: This is the test.
    #[test]
    fn palace_slug_prefers_pin() {
        let root = PathBuf::from("/some/checkout-dir");
        assert_eq!(
            palace_slug(Some("pinned-slug"), &root).as_deref(),
            Some("pinned-slug")
        );
    }

    /// Why: without a pin, the slug is the slugified basename (parity with the
    /// daemon's `project_slug_at` step b — basename, NOT git owner/repo, which is
    /// what `validate_palace_name` checks).
    /// What: `/Users/bob/Projects/Trusty_Tools` → `trusty-tools`.
    /// Test: This is the test.
    #[test]
    fn palace_slug_falls_back_to_basename() {
        let root = PathBuf::from("/Users/bob/Projects/Trusty_Tools");
        assert_eq!(palace_slug(None, &root).as_deref(), Some("trusty-tools"));
    }

    /// Why: an empty/whitespace pin must be ignored so derivation falls through
    /// to the basename rather than producing an empty palace name.
    /// What: empty pin + basename `widget` → `widget`.
    /// Test: This is the test.
    #[test]
    fn palace_slug_empty_pin_falls_through() {
        let root = PathBuf::from("/x/widget");
        assert_eq!(palace_slug(Some("   "), &root).as_deref(), Some("widget"));
    }

    /// Why: the I/O edge must read a present pin file and use its `palace` value.
    /// What: writes a pin under a tempdir, asserts the read picks it up.
    /// Test: This is the test.
    #[test]
    fn palace_name_for_reads_pin() {
        let tmp = std::env::temp_dir().join(format!(
            "tctl-ensure-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let pin_dir = tmp.join(".trusty-tools");
        std::fs::create_dir_all(&pin_dir).expect("mkdir");
        std::fs::write(
            tmp.join(PIN_FILE_REL),
            "schema_version: 1\npalace: pinned-name\n",
        )
        .expect("write pin");
        assert_eq!(palace_name_for(&tmp).as_deref(), Some("pinned-name"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Why: when no pin file exists the I/O edge must fall back to the basename
    /// slug (the common first-run case).
    /// What: a tempdir with no pin file → slugified basename.
    /// Test: This is the test.
    #[test]
    fn palace_name_for_basename_when_no_pin() {
        let tmp = std::env::temp_dir().join(format!(
            "tctl-ensure-identity-nopin-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).expect("mkdir");
        let expected = slugify(tmp.file_name().unwrap().to_str().unwrap());
        assert_eq!(palace_name_for(&tmp).as_deref(), Some(expected.as_str()));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
