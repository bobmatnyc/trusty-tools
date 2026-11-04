//! The bundle walk (SPEC-OKF-01 §2.2.1, §2.11 file system rule 5).
//!
//! Why: `Bundle::open`, `regenerate_indexes` and `move_concept` all need the
//! same answer to "which files are concepts", and none may follow a link.
//! What: [`scan`] lists every `.md` file under the root except the reserved
//! `index.md` and `log.md`, skipping the root's `_sources/`, `_state/` and
//! dot directories. Each file is a concept ID or a read-only foreign path;
//! a symbolic link earns a warning and is not read.
//! Test: `c115_bundle_open_reads_foreign_files_and_writes_never_touch_them`,
//! `c53_put_across_an_in_root_symlink_is_path_escape`.

use std::path::Path;

use crate::OkfError;
use crate::bundle::paths::io_err;
use crate::format::container::is_reserved_file;
use crate::format::id::{ConceptRef, concept_ref_for_path};
use crate::format::validate::{Finding, FindingKind, Severity};

/// One `.md` file the walk found.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    /// Bundle-relative path, `/`-separated, `.md` included.
    pub rel: String,
    /// The concept ID, or the foreign path.
    pub r: ConceptRef,
}

/// The files and findings of one walk.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    /// Every concept and foreign file, sorted by path.
    pub files: Vec<Entry>,
    /// Foreign-path infos, symlink warnings, unreadable names.
    pub findings: Vec<Finding>,
}

/// Root-level directories that hold build machinery, not concepts (§2.2.1).
const MACHINERY: [&str; 2] = ["_sources", "_state"];

/// Walk the canonical `root`.
///
/// Test: `c115_bundle_open_reads_foreign_files_and_writes_never_touch_them`.
pub(crate) fn scan(root: &Path) -> Result<Scan, OkfError> {
    let mut out = Scan::default();
    walk(root, "", &mut out)?;
    out.files.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn walk(dir: &Path, prefix: &str, out: &mut Scan) -> Result<(), OkfError> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| io_err(dir, e))?
        .collect::<Result<_, _>>()
        .map_err(|e| io_err(dir, e))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            out.findings.push(Finding::new(
                Severity::Warning,
                FindingKind::UnreadableFile,
                path.to_string_lossy().into_owned(),
                "file name is not UTF-8; not read".to_string(),
                "SPEC-OKF-01 §2.11",
            ));
            continue;
        };
        let rel = format!("{prefix}{name}");
        let ft = entry.file_type().map_err(|e| io_err(&path, e))?;
        if ft.is_symlink() {
            out.findings.push(Finding::new(
                Severity::Warning,
                FindingKind::SymlinkSkipped,
                rel,
                "symbolic link in the bundle; not followed and not read".to_string(),
                "SPEC-OKF-01 §2.11",
            ));
        } else if ft.is_dir() {
            let skip =
                prefix.is_empty() && (MACHINERY.contains(&name.as_str()) || name.starts_with('.'));
            if !skip {
                walk(&path, &format!("{rel}/"), out)?;
            }
        } else if ft.is_file() && name.ends_with(".md") && !is_reserved_file(&name) {
            let (r, finding) = concept_ref_for_path(&rel);
            out.findings.extend(finding);
            out.files.push(Entry { rel, r });
        }
    }
    Ok(())
}
