//! Per-release feature highlights and the once-per-release `tm doctor` notice
//! (#11003).
//!
//! Why: the changelog runs to thousands of lines and `tm doctor` checks health
//! only, so nothing told a user what a release added or which command to try.
//! What: [`RELEASES`] embeds one curated highlights file per release (3–5 lines
//! per feature, the last naming the command to try). [`for_version`] picks the
//! newest file released at or before the installed version, so a patch release
//! with no file of its own still shows the latest highlights. [`SeenStore`]
//! records the last highlights release `tm doctor` announced, and
//! [`doctor_notice`] decides whether to announce one now.
//! Test: `whats_new_tests.rs`; the binary path in
//! `tests/tm_whats_new.rs`.

use std::path::{Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};

/// Every curated highlights file, `(release version, markdown)`.
///
/// Embedded rather than read at runtime so the text always matches the
/// installed binary. Add a row per new file under `src/assets/whats-new/`;
/// `every_highlights_file_is_registered` fails on a file with no row.
pub const RELEASES: &[(&str, &str)] =
    &[("1.14.14", include_str!("../assets/whats-new/1.14.14.md"))];

/// File name of the last-seen store under the framework root.
pub const STORE_FILE: &str = "whats-new.json";

/// The store format this build reads and writes (ADR-0077 D3).
pub const SCHEMA_VERSION: u32 = 1;

/// The command a notice points at.
pub const COMMAND: &str = "tm whats-new";

/// One release's curated highlights.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Highlights {
    /// The release the file was written for.
    pub version: Version,
    /// The markdown, as embedded.
    pub text: &'static str,
}

/// One `## ` section of a highlights file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feature<'a> {
    /// The heading text, without `## `.
    pub title: &'a str,
    /// The non-blank lines under the heading, in order.
    pub lines: Vec<&'a str>,
}

/// Every embedded highlights file whose version parses, oldest first.
///
/// Test: `every_release_row_parses_and_is_well_formed`.
pub fn embedded() -> Vec<Highlights> {
    let mut all: Vec<Highlights> = RELEASES
        .iter()
        .filter_map(|(v, text)| {
            Version::parse(v)
                .ok()
                .map(|version| Highlights { version, text })
        })
        .collect();
    all.sort_by(|a, b| a.version.cmp(&b.version));
    all
}

/// The newest embedded highlights released at or before `installed`.
///
/// Test: `for_version_picks_the_newest_release_not_after_the_installed_one`.
pub fn for_version(installed: &Version) -> Option<Highlights> {
    embedded()
        .into_iter()
        .rev()
        .find(|h| h.version <= *installed)
}

/// The `## ` sections of `text`, each with its non-blank body lines.
///
/// Test: `features_splits_on_level_two_headings`.
pub fn features(text: &str) -> Vec<Feature<'_>> {
    let mut out: Vec<Feature<'_>> = Vec::new();
    for line in text.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            out.push(Feature {
                title: title.trim(),
                lines: Vec::new(),
            });
        } else if let Some(current) = out.last_mut()
            && !line.trim().is_empty()
        {
            current.lines.push(line);
        }
    }
    out
}

/// What `tm whats-new` prints for `installed` (the crate version string).
///
/// What: the matching file verbatim; a lead line naming the older release when
/// only an older file applies; a one-line "none" message otherwise.
/// Test: `render_prints_the_exact_release`,
/// `render_names_the_older_release_it_falls_back_to`,
/// `render_says_so_when_no_release_applies`.
pub fn render(installed: &str) -> String {
    let found = Version::parse(installed).ok().and_then(|v| for_version(&v));
    match found {
        Some(h) if h.version.to_string() == installed => h.text.to_owned(),
        Some(h) => format!(
            "No highlights were curated for {installed}; showing {}.\n\n{}",
            h.version, h.text
        ),
        None => format!("No curated highlights ship with trusty-mpm {installed}.\n"),
    }
}

/// The one-line pointer `tm doctor` prints for `highlights`.
pub fn notice_line(highlights: &Highlights) -> String {
    let n = features(highlights.text).len();
    let noun = if n == 1 { "highlight" } else { "highlights" };
    format!(
        "New in trusty-mpm {}: {n} {noun} — run `{COMMAND}`",
        highlights.version
    )
}

/// A last-seen store failure. Never fatal to `tm doctor`.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The framework root could not be resolved, so there is no store path.
    #[error("the store location is unresolved: {0}")]
    Unresolved(String),
    /// The store exists but could not be read.
    #[error("could not read {path}: {source}")]
    Read {
        /// The store path.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// The store is not valid JSON of the expected shape.
    #[error("{path} is not a whats-new store: {source}")]
    Parse {
        /// The store path.
        path: PathBuf,
        /// The decode error.
        source: serde_json::Error,
    },
    /// The store was written by a build with another format.
    #[error("{path} has schema_version {found}; this tm reads {SCHEMA_VERSION}")]
    Schema {
        /// The store path.
        path: PathBuf,
        /// The schema version found.
        found: u32,
    },
    /// The atomic write failed; the previous file, if any, is unchanged.
    #[error("could not write {path}: {source}")]
    Write {
        /// The store path.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
}

#[derive(Serialize, Deserialize)]
struct StoreDoc {
    schema_version: u32,
    last_seen_version: String,
}

/// The last highlights release `tm doctor` announced, one JSON file.
///
/// Only `tm` writes it (ADR-0077 D3), through [`SeenStore::mark_seen`].
#[derive(Debug, Clone)]
pub struct SeenStore {
    path: PathBuf,
}

impl SeenStore {
    /// The store at `<root>/whats-new.json`.
    pub fn in_root(root: &Path) -> Self {
        Self {
            path: root.join(STORE_FILE),
        }
    }

    /// The store's file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The recorded version, or `None` when the file does not exist.
    ///
    /// # Errors
    /// [`StoreError::Read`], [`StoreError::Parse`] or [`StoreError::Schema`].
    ///
    /// Test: `mark_seen_round_trips_through_last_seen`,
    /// `last_seen_refuses_another_schema_version`.
    pub fn last_seen(&self) -> Result<Option<String>, StoreError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Read {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let doc: StoreDoc = serde_json::from_slice(&bytes).map_err(|source| StoreError::Parse {
            path: self.path.clone(),
            source,
        })?;
        if doc.schema_version != SCHEMA_VERSION {
            return Err(StoreError::Schema {
                path: self.path.clone(),
                found: doc.schema_version,
            });
        }
        Ok(Some(doc.last_seen_version))
    }

    /// Record `version` as seen, atomically (temp file, then rename).
    ///
    /// # Errors
    /// [`StoreError::Write`]; the previous file is left as it was.
    ///
    /// Test: `mark_seen_round_trips_through_last_seen`.
    pub fn mark_seen(&self, version: &Version) -> Result<(), StoreError> {
        let doc = StoreDoc {
            schema_version: SCHEMA_VERSION,
            last_seen_version: version.to_string(),
        };
        let write_err = |source| StoreError::Write {
            path: self.path.clone(),
            source,
        };
        let mut bytes = serde_json::to_vec_pretty(&doc).map_err(|e| write_err(e.into()))?;
        bytes.push(b'\n');
        trusty_common::atomic_file::write_atomic(&self.path, &bytes).map_err(write_err)
    }
}

/// What `tm doctor` does about the notice this run.
#[derive(Debug)]
pub struct NoticeOutcome {
    /// The line to print, when a release has not been announced yet.
    pub line: Option<String>,
    /// A store failure to report. When set, nothing was recorded, so the
    /// notice shows again next run (fail-open: shown, never silently seen).
    pub warning: Option<StoreError>,
}

/// Decide the notice for `current` and record it as seen when shown.
///
/// Why: the notice must show once per release, a store failure must never
/// fail doctor, and a failed write must not count as seen.
/// What: no `current` → nothing. A store whose recorded version equals
/// `current` → nothing. Otherwise the line is returned; a readable store is
/// then written with `current`, and a read or write failure becomes the
/// warning without a write (an unreadable file is never overwritten).
/// Test: `doctor_notice_shows_once_then_stays_quiet`,
/// `doctor_notice_shows_again_when_the_write_fails`,
/// `doctor_notice_does_not_overwrite_an_unreadable_store`,
/// `doctor_notice_reports_an_unresolved_store`.
pub fn doctor_notice(
    store: Result<SeenStore, StoreError>,
    current: Option<&Highlights>,
) -> NoticeOutcome {
    let Some(current) = current else {
        return NoticeOutcome {
            line: None,
            warning: None,
        };
    };
    let last = store.and_then(|s| s.last_seen().map(|last| (s, last)));
    let warning = match last {
        Ok((_, Some(seen))) if seen == current.version.to_string() => {
            return NoticeOutcome {
                line: None,
                warning: None,
            };
        }
        Ok((store, _)) => store.mark_seen(&current.version).err(),
        Err(e) => Some(e),
    };
    NoticeOutcome {
        line: Some(notice_line(current)),
        warning,
    }
}

#[cfg(test)]
#[path = "whats_new_tests.rs"]
mod tests;
