//! Which assistant instance serves a project directory (#11649).
//!
//! Why: an assistant instance (personal, CTO) works for one project directory,
//! so a session started inside that directory must reach that instance and no
//! other. A wrong or silent default here would put one instance's knowledge
//! and instructions into another's work.
//! What: the optional `project_dir` key of an instance's `config.toml`
//! ([`super::home::AssistantHomeConfig`]). [`canonical_project_dir`] validates
//! a value (absolute, an existing directory, canonicalised);
//! [`set_project_dir`] writes it; [`project_claims`] lists every home's claim;
//! [`instance_for_project`] resolves a working directory to exactly one
//! instance. Every failure is a typed [`AssistantError`]: no arm falls back to
//! a default instance.
//! Test: `super::tests::project_tests` — the whole module.

use std::path::{Path, PathBuf};

use super::error::AssistantError;
use super::home::{AssistantHome, AssistantHomeConfig};
use super::instance::AssistantInstanceId;

/// The `config.toml` key naming an instance's project directory.
/// Test: `super::tests::project_tests::set_project_dir_writes_the_canonical_path`.
pub const PROJECT_DIR_KEY: &str = "project_dir";

/// One home's declared project directory, as written in its `config.toml`.
///
/// Why: resolution and health both need every claim; the declared value is
/// kept verbatim so health can report a stale one by the path the user wrote.
/// Test: `super::tests::project_tests::claims_list_every_declared_project_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectClaim {
    /// The instance making the claim.
    pub id: AssistantInstanceId,
    /// The `project_dir` value as declared.
    pub declared: PathBuf,
}

impl ProjectClaim {
    /// The claim's canonical directory, or `None` when the declared path is no
    /// longer an absolute, existing directory. Such a claim matches nothing;
    /// [`super::health`] reports it.
    /// Test: `super::tests::project_tests::a_stale_claim_matches_nothing`.
    pub fn canonical(&self) -> Option<PathBuf> {
        canonical_project_dir(&self.declared).ok()
    }
}

/// Validate a project directory and return its canonical form.
///
/// Why: a relative path, a `..` hop or a symlink would let two spellings of
/// one directory pass as two claims, or one claim reach outside its tree.
/// Canonicalising on write and on read compares real directories only.
/// What: [`AssistantError::InvalidProjectDir`] unless `raw` is absolute and
/// names an existing directory; otherwise the canonical path (symlinks and
/// `..` resolved).
/// Test: `super::tests::project_tests::project_dir_must_be_absolute_and_existing`.
pub fn canonical_project_dir(raw: &Path) -> Result<PathBuf, AssistantError> {
    let invalid = |reason: String| AssistantError::InvalidProjectDir {
        path: raw.to_path_buf(),
        reason,
    };
    if !raw.is_absolute() {
        return Err(invalid("it is not an absolute path".into()));
    }
    let meta = std::fs::metadata(raw).map_err(|e| invalid(format!("it cannot be read ({e})")))?;
    if !meta.is_dir() {
        return Err(invalid("it is not a directory".into()));
    }
    raw.canonicalize()
        .map_err(|e| invalid(format!("it cannot be resolved ({e})")))
}

/// Read and parse a home's `config.toml`; `Ok(None)` when the file is absent.
///
/// Why: a malformed config might hold a claim, so resolution must stop on it
/// rather than skip it and pick another instance.
/// Test: `super::tests::project_tests::a_malformed_config_stops_resolution`.
pub fn read_home_config(
    home: &AssistantHome,
) -> Result<Option<AssistantHomeConfig>, AssistantError> {
    let path = home.config_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(AssistantError::InvalidConfig {
                path,
                reason: format!("it could not be read ({e})"),
            });
        }
    };
    toml::from_str(&raw)
        .map(Some)
        .map_err(|e| AssistantError::InvalidConfig {
            path,
            // The parser's first line names the position, never the values.
            reason: e
                .to_string()
                .lines()
                .next()
                .unwrap_or("invalid TOML")
                .into(),
        })
}

/// Every `project_dir` declared by a home under `root`, sorted by instance id.
///
/// What: walks the directories directly under `root` whose names are valid
/// instance ids; a home without `config.toml`, or without the key, makes no
/// claim. An absent `root` yields no claims. A config that cannot be read or
/// parsed is [`AssistantError::InvalidConfig`].
/// Test: `super::tests::project_tests::claims_list_every_declared_project_dir`.
pub fn project_claims(root: &Path) -> Result<Vec<ProjectClaim>, AssistantError> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(AssistantError::Io {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    let mut claims = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let Ok(id) = AssistantInstanceId::new(entry.file_name().to_string_lossy()) else {
            continue;
        };
        let home = AssistantHome::under(root, id.clone());
        if let Some(declared) = read_home_config(&home)?.and_then(|c| c.project_dir) {
            claims.push(ProjectClaim { id, declared });
        }
    }
    claims.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(claims)
}

/// Pairs of instances claiming the same canonical directory.
///
/// What: `(dir, first, second)` per duplicate, ids in sorted order.
/// Test: `super::tests::project_tests::two_homes_claiming_one_dir_is_a_named_conflict`.
pub fn duplicate_claims(
    claims: &[ProjectClaim],
) -> Vec<(PathBuf, AssistantInstanceId, AssistantInstanceId)> {
    let resolved: Vec<(PathBuf, &AssistantInstanceId)> = claims
        .iter()
        .filter_map(|c| c.canonical().map(|dir| (dir, &c.id)))
        .collect();
    let mut out = Vec::new();
    for (i, (dir, first)) in resolved.iter().enumerate() {
        for (other, second) in &resolved[i + 1..] {
            if dir == other {
                out.push((dir.clone(), (*first).clone(), (*second).clone()));
            }
        }
    }
    out
}

/// The one instance whose project directory holds `cwd`.
///
/// Why: see this module's doc comment — resolution either names exactly one
/// instance or says why it cannot.
/// What: canonicalises `cwd` (so a `..` hop or a symlink that leaves a project
/// directory resolves outside it) and picks the claim whose canonical
/// directory is the deepest ancestor of it. Errors:
/// [`AssistantError::InvalidProjectDir`] for a relative or unresolvable `cwd`,
/// [`AssistantError::NoInstanceForProject`] when no claim holds it,
/// [`AssistantError::ProjectDirConflict`] naming both instances when two claim
/// the matching directory, and [`AssistantError::InvalidConfig`] for any home
/// whose config cannot be parsed.
/// Test: `super::tests::project_tests::resolves_a_cwd_at_or_under_a_project_dir`,
/// `super::tests::project_tests::two_homes_claiming_one_dir_is_a_named_conflict`,
/// `super::tests::project_tests::dotdot_and_symlink_escapes_are_rejected`,
/// `super::tests::project_tests::no_match_is_an_error_not_a_default`,
/// `super::tests::project_tests::a_malformed_config_stops_resolution`.
pub fn instance_for_project(
    root: &Path,
    cwd: &Path,
) -> Result<AssistantInstanceId, AssistantError> {
    let cwd = canonical_project_dir(cwd)?;
    let mut matches: Vec<(PathBuf, AssistantInstanceId)> = project_claims(root)?
        .into_iter()
        .filter_map(|c| c.canonical().map(|dir| (dir, c.id)))
        .filter(|(dir, _)| cwd.starts_with(dir))
        .collect();
    let Some(depth) = matches.iter().map(|(d, _)| d.components().count()).max() else {
        return Err(AssistantError::NoInstanceForProject { cwd });
    };
    matches.retain(|(d, _)| d.components().count() == depth);
    let (dir, first) = matches.remove(0);
    if let Some((_, second)) = matches.into_iter().next() {
        return Err(AssistantError::ProjectDirConflict {
            dir,
            first: first.to_string(),
            second: second.to_string(),
        });
    }
    Ok(first)
}

/// Validate `raw` and write it as `home`'s `project_dir`.
///
/// Why: the write is where a bad path is cheapest to refuse, and a second home
/// claiming the same directory would make resolution ambiguous.
/// What: [`canonical_project_dir`], then refuses with
/// [`AssistantError::ProjectDirConflict`] when another home under `root`
/// already claims the canonical directory, then sets the key in `config.toml`
/// (creating the file with `id` when absent) and keeps every other key.
/// Returns the canonical path written.
/// Test: `super::tests::project_tests::set_project_dir_writes_the_canonical_path`,
/// `super::tests::project_tests::set_project_dir_refuses_a_second_claim`.
pub fn set_project_dir(
    root: &Path,
    home: &AssistantHome,
    raw: &Path,
) -> Result<PathBuf, AssistantError> {
    let dir = canonical_project_dir(raw)?;
    ensure_unclaimed(root, home.id(), &dir)?;
    let path = home.config_path();
    let existing = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => home.seed_config_body(),
        Err(source) => return Err(AssistantError::Io { path, source }),
    };
    let mut doc = parse_document(&path, &existing)?;
    doc[PROJECT_DIR_KEY] = toml_edit::value(dir_str(&dir)?);
    write_config(&path, &doc)?;
    Ok(dir)
}

/// Refuse when an instance other than `id` under `root` claims `dir`.
pub(super) fn ensure_unclaimed(
    root: &Path,
    id: &AssistantInstanceId,
    dir: &Path,
) -> Result<(), AssistantError> {
    for claim in project_claims(root)? {
        if &claim.id != id && claim.canonical().as_deref() == Some(dir) {
            return Err(AssistantError::ProjectDirConflict {
                dir: dir.to_path_buf(),
                first: claim.id.to_string(),
                second: id.to_string(),
            });
        }
    }
    Ok(())
}

/// A canonical directory as a TOML string; a non-UTF-8 path cannot be stored.
pub(super) fn dir_str(dir: &Path) -> Result<&str, AssistantError> {
    dir.to_str()
        .ok_or_else(|| AssistantError::InvalidProjectDir {
            path: dir.to_path_buf(),
            reason: "it is not valid UTF-8, so config.toml cannot hold it".into(),
        })
}

/// Parse `raw` as an editable TOML document.
pub(super) fn parse_document(
    path: &Path,
    raw: &str,
) -> Result<toml_edit::DocumentMut, AssistantError> {
    raw.parse::<toml_edit::DocumentMut>()
        .map_err(|e| AssistantError::InvalidConfig {
            path: path.to_path_buf(),
            reason: e
                .to_string()
                .lines()
                .next()
                .unwrap_or("invalid TOML")
                .into(),
        })
}

/// Write `doc` to `path` atomically.
pub(super) fn write_config(
    path: &Path,
    doc: &toml_edit::DocumentMut,
) -> Result<(), AssistantError> {
    crate::state_writer::atomic_write(path, doc.to_string().as_bytes()).map_err(|e| {
        AssistantError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(e.to_string()),
        }
    })
}
