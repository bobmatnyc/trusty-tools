//! The embedded Chrome extension and its publication into `state/extension` (`assets.py`).
//!
//! Why: Chrome loads the unpacked extension from one stable directory and
//! the worker reloads when the broker reports another `version_name` stamp
//! (#11363), so the Rust broker must publish the same bytes to the same place
//! and stamp them exactly as Python does; a profile then sees no change.
//! What: [`EMBEDDED`] carries `crates/trusty-browser/extension/` byte for byte
//! (copied from the Python 0.7.0 tree). [`signature`] is `assets.signature`,
//! [`stamp`] its first 16 hex characters, [`extension_path`] the locked
//! publish (one-time migration of the old symlinked layout, files written by
//! temp file and rename, `manifest.json` last, stale files pruned, every
//! `assets/<hash>` turned into a `../extension` symlink).
//! Test: `embedded_stamp_matches_python`, `fresh_publish_matches_python_tree`,
//! `republish_migrates_the_symlinked_layout_like_python` in `tests/install.rs`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::pyjson::Json;

/// The manifest key the worker compares with the broker's stamp.
pub const STAMP: &str = "version_name";
/// The extension id Chrome derives from the manifest `key`.
pub const EXTENSION_ID: &str = "jabfhcolknbbdajilgpficniedhcfahc";

macro_rules! embed {
    ($($p:literal),* $(,)?) => {
        [$(($p, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/extension/", $p)) as &[u8])),*]
    };
}

/// Every extension file as `(relative path, bytes)`.
pub const EMBEDDED: [(&str, &[u8]); 15] = embed![
    "codes.js",
    "icons/trusty-browser-128.png",
    "icons/trusty-browser-16.png",
    "icons/trusty-browser-32.png",
    "icons/trusty-browser-48.png",
    "icons/trusty-browser.webp",
    "indicator.js",
    "indicator_content.js",
    "manifest.json",
    "pages.js",
    "pages_read.js",
    "setup.css",
    "setup.html",
    "setup.js",
    "worker.js",
];

/// Asset files keyed by path; `Path` order is component order, as Python sorts.
pub type Files = BTreeMap<PathBuf, Vec<u8>>;

/// [`EMBEDDED`] as [`Files`].
pub fn embedded() -> Files {
    EMBEDDED
        .iter()
        .map(|(p, b)| (PathBuf::from(p), b.to_vec()))
        .collect()
}

fn other(msg: String) -> io::Error {
    io::Error::other(msg)
}

/// `assets.signature`: sha256 over `path \0 sha256(bytes)` in sorted path order.
pub fn signature(files: &Files) -> String {
    let mut h = Sha256::new();
    for (rel, data) in files {
        h.update(rel.to_string_lossy().as_bytes());
        h.update(b"\0");
        h.update(Sha256::digest(data));
    }
    hex::encode(h.finalize())
}

/// `assets.stamp`: the 16-character signature prefix written into the manifest.
pub fn stamp(files: &Files) -> String {
    signature(files)[..16].to_owned()
}

/// Read a directory as [`Files`]; a symlink is refused as Python's `signature` does.
pub fn read_dir(dir: &Path) -> io::Result<Files> {
    let mut out = Files::new();
    walk(dir, Path::new(""), &mut |rel, path, meta| {
        if meta.file_type().is_symlink() {
            return Err(other(
                "Unexpected symlink inside extension assets".to_owned(),
            ));
        }
        if meta.is_file() {
            out.insert(rel.to_path_buf(), fs::read(path)?);
        }
        Ok(())
    })?;
    Ok(out)
}

type Visit<'a> = dyn FnMut(&Path, &Path, &fs::Metadata) -> io::Result<()> + 'a;

/// Depth-first `lstat` walk that never follows a symlink.
fn walk(root: &Path, rel: &Path, visit: &mut Visit<'_>) -> io::Result<()> {
    for entry in fs::read_dir(root.join(rel))? {
        let entry = entry?;
        let rel = rel.join(entry.file_name());
        let meta = entry.path().symlink_metadata()?;
        visit(&rel, &entry.path(), &meta)?;
        if meta.is_dir() {
            walk(root, &rel, visit)?;
        }
    }
    Ok(())
}

/// `assets.published`: the stamp after `+` in the published manifest, if any.
pub fn published(state: &Path) -> Option<String> {
    let raw = fs::read(state.join("extension").join("manifest.json")).ok()?;
    let manifest = Json::parse(&raw).ok()?;
    let value = manifest.get(STAMP)?.as_str()?;
    value.rsplit_once('+').map(|(_, s)| s.to_owned())
}

fn hash_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `enrollment.private_directory`: parents default mode, the leaf 0700, then owner-only.
fn private_directory(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    if crate::private::private_dir(&fs::symlink_metadata(path)?) {
        Ok(())
    } else {
        Err(other(
            "state must be a private directory owned by the current user".to_owned(),
        ))
    }
}

/// The published manifest: `version_name = "<version>+<stamp>"`, `indent=2` + newline.
fn stamped_manifest(raw: &[u8], stamp: &str) -> io::Result<Vec<u8>> {
    let mut body = Json::parse(raw).map_err(|e| other(e.to_string()))?;
    let version = body
        .get("version")
        .and_then(Json::as_str)
        .ok_or_else(|| other("extension manifest has no version".to_owned()))?
        .to_owned();
    body.insert(STAMP, Json::from(format!("{version}+{stamp}").as_str()));
    let text = body
        .dumps_indent2_ascii()
        .map_err(|e| other(e.to_string()))?;
    Ok(format!("{text}\n").into_bytes())
}

/// `write_file`: default-mode parents, a 0600 temp file in the same directory, rename.
fn write_file(target: &Path, data: &[u8]) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| other("asset path has no parent".to_owned()))?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(data)?;
    tmp.persist(target).map_err(|e| e.error)?;
    Ok(())
}

/// `assets.update`: make `target` hold exactly `files`, `manifest.json` last.
fn update(files: &Files, target: &Path) -> io::Result<()> {
    let manifest = Path::new("manifest.json");
    let stamp = stamp(files);
    let wanted: Vec<&PathBuf> = files
        .keys()
        .filter(|p| p.file_name().is_some_and(|n| n != ".DS_Store"))
        .collect();
    let order = wanted
        .iter()
        .filter(|p| p.as_path() != manifest)
        .chain(wanted.iter().filter(|p| p.as_path() == manifest));
    for rel in order {
        let raw = &files[*rel];
        let data = if rel.as_path() == manifest {
            stamped_manifest(raw, &stamp)?
        } else {
            raw.clone()
        };
        let dest = target.join(rel);
        let same = match fs::symlink_metadata(&dest) {
            Ok(m) if m.is_file() => fs::read(&dest)? == data,
            _ => false,
        };
        if !same {
            write_file(&dest, &data)?;
        }
    }
    let mut present = Vec::new();
    walk(target, Path::new(""), &mut |rel, _, meta| {
        present.push((rel.to_path_buf(), meta.file_type()));
        Ok(())
    })?;
    present.sort_by(|a, b| b.0.cmp(&a.0));
    for (rel, kind) in present {
        let path = target.join(&rel);
        if kind.is_symlink() || (kind.is_file() && !wanted.contains(&&rel)) {
            fs::remove_file(&path)?;
        } else if kind.is_dir() && fs::read_dir(&path)?.next().is_none() {
            fs::remove_dir(&path)?;
        }
    }
    Ok(())
}

/// `assets.migrate`: every real `assets/<hash>` directory becomes a `../extension` symlink.
fn migrate(assets: &Path) -> io::Result<()> {
    for entry in fs::read_dir(assets)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if !hash_name(&name.to_string_lossy()) || path.symlink_metadata()?.is_symlink() {
            continue;
        }
        if !path.is_dir() {
            return Err(other(format!(
                "Refusing to replace an unrelated path: {}",
                path.display()
            )));
        }
        let scratch = tempfile::Builder::new().prefix("tmp").tempdir_in(assets)?;
        let link = scratch.path().join("link");
        std::os::unix::fs::symlink(Path::new("..").join("extension"), &link)?;
        fs::rename(&path, scratch.path().join("old"))?;
        fs::rename(&link, &path)?;
    }
    Ok(())
}

/// Publish the embedded extension into `state/extension` and return that path.
pub fn extension_path(state: &Path) -> io::Result<PathBuf> {
    publish(state, &embedded())
}

/// `assets.extension_path` for any source [`Files`].
///
/// Why: Chrome stores the resolved path of an unpacked extension, so the
/// directory is updated in place and old hash paths are symlinked to it (#11363).
/// What: under `assets/install.lock`, refuses a foreign symlink in `assets/`
/// or at `extension`, replaces an old `extension -> assets/<hash>` link by a
/// real directory (built in a scratch directory, then renamed), updates in
/// place otherwise, then [`migrate`]s.
/// Test: `fresh_publish_matches_python_tree`, `republish_migrates_the_symlinked_layout_like_python`,
/// `publish_refuses_an_unrelated_extension_path`.
pub fn publish(state: &Path, files: &Files) -> io::Result<PathBuf> {
    private_directory(state)?;
    let assets = state.join("assets");
    private_directory(&assets)?;
    let target = state.join("extension");
    let lock = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(assets.join("install.lock"))?;
    // SAFETY: flock on an fd we own; released when `lock` drops.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let up = Path::new("..").join("extension");
    for entry in fs::read_dir(&assets)? {
        let path = entry?.path();
        let hashed = path
            .file_name()
            .is_some_and(|n| hash_name(&n.to_string_lossy()));
        if hashed && path.symlink_metadata()?.is_symlink() && fs::read_link(&path)? != up {
            return Err(other(format!("Unexpected symlink at {}", path.display())));
        }
    }
    let unrelated = || {
        other(format!(
            "Refusing to replace an unrelated path: {}",
            target.display()
        ))
    };
    match fs::symlink_metadata(&target) {
        Ok(m) if m.is_symlink() => {
            let pointed = fs::read_link(&target)?;
            let in_assets = pointed.parent() == Some(Path::new("assets"));
            let hashed = pointed
                .file_name()
                .is_some_and(|n| hash_name(&n.to_string_lossy()));
            if !in_assets || !hashed {
                return Err(unrelated());
            }
            // One-time migration from the symlinked layout: a real directory.
            fs::remove_file(&target)?;
        }
        Ok(m) if !m.is_dir() => return Err(unrelated()),
        _ => {}
    }
    if target.exists() {
        update(files, &target)?;
    } else {
        let scratch = tempfile::Builder::new().prefix("tmp").tempdir_in(state)?;
        let fresh = scratch.path().join("extension");
        fs::DirBuilder::new().mode(0o700).create(&fresh)?;
        update(files, &fresh)?;
        fs::rename(&fresh, &target)?;
    }
    migrate(&assets)?;
    Ok(target)
}
