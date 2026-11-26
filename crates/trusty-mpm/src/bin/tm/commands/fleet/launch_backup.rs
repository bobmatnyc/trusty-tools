//! The one-release undo copy of the files a launch deploy replaces (#11368).
//!
//! Why: #11368 — a release that breaks the Architect must come back from the
//! kept copy. Architect Q1: only files tm replaces are kept; an edited file
//! is never replaced, so never kept. Q3: one release under
//! `<dir>/.trusty-mpm/architect-bundle/<tm version>/`, older ones pruned.
//! What: [`keep`] writes each replaced file's bytes under `files/`, then the
//! `manifest.tsv` (`dest`, mode, the kept sha256, the sha256 tm writes),
//! each atomically, then prunes every other release. A release with no
//! manifest is incomplete and [`restore`] never reads it. [`restore`] puts a
//! kept file back only over the bytes this release wrote, or over nothing.
//! Test: `restore_brings_back_the_previous_release_bytes_11368`,
//! `a_failed_backup_aborts_and_writes_nothing_11368`,
//! `a_new_release_prunes_the_older_kept_copy_11368`,
//! `restore_never_overwrites_an_edited_file_11368`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use trusty_common::atomic_file::write_atomic;

use super::Step;
use super::seed::refresh::sha256_hex;

/// The backup root, below the Architect directory.
pub(crate) const ROOT: &str = ".trusty-mpm/architect-bundle";

/// The manifest file name inside one release's directory.
const MANIFEST: &str = "manifest.tsv";

/// The kept bytes' directory inside one release's directory.
const FILES: &str = "files";

/// One file the deploy is about to replace.
#[derive(Debug, Clone)]
pub(crate) struct Kept {
    /// Destination below the Architect directory.
    pub(crate) dest: String,
    /// The bytes on disk now, which the deploy replaces.
    pub(crate) bytes: Vec<u8>,
    /// Their permission bits.
    pub(crate) mode: u32,
    /// sha256 of the bytes the deploy writes.
    pub(crate) new_sha: String,
}

/// One manifest row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    mode: u32,
    old_sha: String,
    new_sha: String,
}

/// `<dir>/ROOT/<version>`; refuses a version that is not one path segment.
fn release_dir(dir: &Path, version: &str) -> anyhow::Result<PathBuf> {
    if version.is_empty() || version == "." || version == ".." || version.contains(['/', '\\']) {
        bail!("{version:?} is not a release version");
    }
    Ok(dir.join(ROOT).join(version))
}

/// Keep `kept` under `version`'s release, then prune every other release.
///
/// Why: Q3, Q8 — a backup that fails aborts the deploy before its first write.
/// What: nothing to keep writes and prunes nothing, so a relaunch on the
/// same release keeps the undo it has. A file already kept for this release
/// keeps its first bytes (the previous release's) and takes the new sha.
/// Test: `a_failed_backup_aborts_and_writes_nothing_11368`,
/// `a_new_release_prunes_the_older_kept_copy_11368`.
pub(crate) fn keep(dir: &Path, version: &str, kept: &[Kept]) -> anyhow::Result<Vec<Step>> {
    if kept.is_empty() {
        return Ok(Vec::new());
    }
    let root = release_dir(dir, version)?;
    let mut manifest = read_manifest(&root)?.unwrap_or_default();
    for k in kept {
        if let Some(entry) = manifest.get_mut(&k.dest) {
            entry.new_sha.clone_from(&k.new_sha);
            continue;
        }
        let path = root.join(FILES).join(&k.dest);
        write_atomic(&path, &k.bytes)
            .with_context(|| format!("backup: cannot keep {}", path.display()))?;
        manifest.insert(
            k.dest.clone(),
            Entry {
                mode: k.mode,
                old_sha: sha256_hex(&k.bytes),
                new_sha: k.new_sha.clone(),
            },
        );
    }
    let path = root.join(MANIFEST);
    write_atomic(&path, render_manifest(&manifest).as_bytes())
        .with_context(|| format!("backup: cannot write {}", path.display()))?;
    prune(dir, version)?;
    Ok(vec![Step::Changed(format!(
        "kept the {} replaced file(s) under {ROOT}/{version}/ for `tm architect restore`",
        kept.len()
    ))])
}

/// Remove every release directory under [`ROOT`] except `version`'s.
fn prune(dir: &Path, version: &str) -> anyhow::Result<()> {
    for (name, path) in releases_in(dir)? {
        if name != version && path.is_dir() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("backup: cannot prune {}", path.display()))?;
        }
    }
    Ok(())
}

/// Every entry under [`ROOT`], by name; none when it does not exist.
fn releases_in(dir: &Path) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let root = dir.join(ROOT);
    let read = match std::fs::read_dir(&root) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", root.display())),
    };
    let mut out = Vec::new();
    for entry in read {
        let entry = entry.with_context(|| format!("cannot read {}", root.display()))?;
        out.push((
            entry.file_name().to_string_lossy().into_owned(),
            entry.path(),
        ));
    }
    out.sort();
    Ok(out)
}

/// The manifest of the release at `root`; `None` when it has none.
fn read_manifest(root: &Path) -> anyhow::Result<Option<BTreeMap<String, Entry>>> {
    let path = root.join(MANIFEST);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let mut out = BTreeMap::new();
    for line in text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let cols: Vec<&str> = line.split('\t').collect();
        let [dest, mode, old_sha, new_sha] = cols.as_slice() else {
            bail!("{}: malformed row {line:?}", path.display());
        };
        let mode = u32::from_str_radix(mode, 8)
            .with_context(|| format!("{}: bad mode in {line:?}", path.display()))?;
        out.insert(
            (*dest).to_owned(),
            Entry {
                mode,
                old_sha: (*old_sha).to_owned(),
                new_sha: (*new_sha).to_owned(),
            },
        );
    }
    Ok(Some(out))
}

/// The manifest text: a header, then one `dest mode old new` row per file.
fn render_manifest(manifest: &BTreeMap<String, Entry>) -> String {
    let mut out = String::from("# dest\tmode\tkept_sha256\twritten_sha256 (#11368)\n");
    for (dest, e) in manifest {
        out.push_str(&format!(
            "{dest}\t{:o}\t{}\t{}\n",
            e.mode, e.old_sha, e.new_sha
        ));
    }
    out
}

/// What one `tm architect restore` did.
#[derive(Debug, Default)]
pub(crate) struct Restored {
    /// One entry per kept file.
    pub(crate) steps: Vec<Step>,
    /// The release restored from.
    pub(crate) version: String,
}

impl Restored {
    /// 0 every kept file is back, 1 a file was kept as edited or failed.
    pub(crate) fn exit(&self) -> i32 {
        let bad = self
            .steps
            .iter()
            .any(|s| matches!(s, Step::Failed(_) | Step::Skipped(_)));
        i32::from(bad)
    }
}

/// Put the kept copy of `version` (default: the one kept release) back in `dir`.
///
/// Why: #11368 AC "The previous release restores from the kept copy"; Q1
/// never overwrite an edited file. What: a file whose bytes are the ones this
/// release wrote, or that is absent, gets the kept bytes and mode; one that
/// already holds the kept bytes is unchanged; any other bytes are an edit,
/// kept and reported (exit 1). A kept file whose sha no longer matches its
/// manifest row fails. `Err`: no complete release, or more than one.
/// Test: `restore_brings_back_the_previous_release_bytes_11368`,
/// `restore_never_overwrites_an_edited_file_11368`.
pub(crate) fn restore(dir: &Path, version: Option<&str>) -> anyhow::Result<Restored> {
    let complete: Vec<String> = releases_in(dir)?
        .into_iter()
        .filter(|(_, p)| p.join(MANIFEST).is_file())
        .map(|(n, _)| n)
        .collect();
    let version = match (version, complete.as_slice()) {
        (Some(v), _) if complete.iter().any(|c| c == v) => v.to_owned(),
        (Some(v), _) => bail!(
            "no kept copy of release {v} under {}; kept: [{}]",
            dir.join(ROOT).display(),
            complete.join(", ")
        ),
        (None, [only]) => only.clone(),
        (None, []) => bail!("no kept copy under {}", dir.join(ROOT).display()),
        (None, many) => bail!(
            "more than one kept release ({}); pass --version",
            many.join(", ")
        ),
    };
    let root = release_dir(dir, &version)?;
    let manifest = read_manifest(&root)?.unwrap_or_default();
    let steps = manifest
        .iter()
        .map(|(dest, e)| restore_one(dir, &root, dest, e))
        .collect();
    Ok(Restored { steps, version })
}

/// Restore one manifest row; see [`restore`].
fn restore_one(dir: &Path, root: &Path, dest: &str, e: &Entry) -> Step {
    let kept_path = root.join(FILES).join(dest);
    let kept = match std::fs::read(&kept_path) {
        Ok(b) if sha256_hex(&b) == e.old_sha => b,
        Ok(_) => return Step::Failed(format!("{dest}: the kept copy does not match its sha256")),
        Err(err) => return Step::Failed(format!("{dest}: cannot read the kept copy: {err}")),
    };
    let path = dir.join(dest);
    match std::fs::read(&path) {
        Ok(live) if sha256_hex(&live) == e.old_sha => {
            return Step::Unchanged(format!("{dest} already holds the kept copy"));
        }
        Ok(live) if sha256_hex(&live) != e.new_sha => {
            return Step::Skipped(format!(
                "{dest}: edited since the deploy, so it is left as is; the kept copy is at \
                 {}",
                kept_path.display()
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Step::Failed(format!("{dest}: cannot read: {err}")),
    }
    match write_atomic(&path, &kept).and_then(|()| set_mode(&path, e.mode)) {
        Ok(()) => Step::Changed(format!("restored {dest}")),
        Err(err) => Step::Failed(format!("{dest}: cannot write: {err}")),
    }
}

/// The permission bits of `meta`.
#[cfg(unix)]
pub(crate) fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
}

/// No mode bits off Unix.
#[cfg(not(unix))]
pub(crate) fn mode_of(_meta: &std::fs::Metadata) -> u32 {
    0o644
}

/// Set `mode` on `path`.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// No mode bits to set off Unix.
#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}
