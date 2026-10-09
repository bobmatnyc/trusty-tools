//! Reading the host file and a project's route file, refusing symlinks.
//!
//! Why: a symlink lets a file outside the reviewed tree stand in for the
//! host ceiling or a project's routes (#8454 S2 plan §4; the Architect's
//! review extends the host check to its parent directory).
//! What: [`read_host`] and [`read_project`] check each named path with
//! `symlink_metadata`, open the file, confirm the opened file is the one
//! checked, and read at most [`MAX_FILE_BYTES`]. Every fault is a named
//! error; nothing falls back to a default.
//! Test: `symlinked_file_refused`, `host_config_parent_dir_symlink_denies_all`,
//! `host_missing_denies_all`, `oversized_files_are_refused`.

use std::fs::{File, Metadata};
use std::io::{ErrorKind, Read as _};
use std::path::Path;

use crate::gchat::routes::{CONFIG_DIR, ROUTES_FILE};
use crate::policy::host::HostError;
use crate::policy::project_file::ProjectFileError;

/// The largest host or project file read (256 KiB, S2b plan §2).
pub const MAX_FILE_BYTES: u64 = 256 * 1024;

/// Why a file read failed, before it is named as a host or project fault.
#[derive(Debug)]
enum ReadFault {
    Missing,
    NotRegular {
        what: &'static str,
        kind: &'static str,
    },
    Io(ErrorKind),
    TooLarge,
}

/// Read `config.yaml` as text, refusing a symlinked file or parent dir.
///
/// Why: the host file is the root of trust; a fault here denies all.
/// What: the parent directory must be a real directory and the file a real
/// file (no symlink at either), under the size cap, and UTF-8.
/// Test: `host_missing_denies_all`, `host_config_parent_dir_symlink_denies_all`,
/// `symlinked_file_refused`.
pub fn read_host(path: &Path) -> Result<String, HostError> {
    let parent = path.parent().ok_or(HostError::Missing)?;
    let bytes = (|| {
        // #8454 Architect review: a symlinked ~/.trusty-tools/trusty-mpm/
        // would let another tree supply the ceiling.
        check_dir(parent, "its parent directory")?;
        read_regular(path, "config.yaml")
    })()
    .map_err(|f| match f {
        ReadFault::Missing => HostError::Missing,
        ReadFault::NotRegular { what, kind } => HostError::NotRegular { what, kind },
        ReadFault::Io(kind) => HostError::Read {
            reason: kind.to_string(),
        },
        ReadFault::TooLarge => HostError::TooLarge {
            limit: MAX_FILE_BYTES,
        },
    })?;
    String::from_utf8(bytes).map_err(|_| HostError::NotUtf8)
}

/// A project's route file, as read.
#[derive(Debug)]
pub enum ProjectRead {
    /// No `.trusty-channels/routes.toml`: zero routes, status `Missing`.
    Missing,
    /// The bytes read, before the load gate.
    Bytes(Vec<u8>),
}

/// Read `<dir>/.trusty-channels/routes.toml`, refusing symlinks.
///
/// Why: S2 plan §4: gchat checks only the final component; S2b adds the
/// `.trusty-channels` directory.
/// What: a missing directory or file is [`ProjectRead::Missing`]; a
/// symlinked or non-directory `.trusty-channels`, a symlinked or non-regular
/// file, an I/O error or an oversized file is refused.
/// Test: `symlinked_file_refused`, `oversized_files_are_refused`.
pub fn read_project(dir: &Path) -> Result<ProjectRead, ProjectFileError> {
    let config = dir.join(CONFIG_DIR);
    let result = check_dir(&config, ".trusty-channels")
        .and_then(|()| read_regular(&config.join(ROUTES_FILE), "routes.toml"));
    match result {
        Ok(bytes) => Ok(ProjectRead::Bytes(bytes)),
        Err(ReadFault::Missing) => Ok(ProjectRead::Missing),
        Err(ReadFault::NotRegular { what, kind }) => {
            Err(ProjectFileError::NotRegular { what, kind })
        }
        Err(ReadFault::Io(kind)) => Err(ProjectFileError::Read {
            reason: kind.to_string(),
        }),
        Err(ReadFault::TooLarge) => Err(ProjectFileError::TooLarge {
            limit: MAX_FILE_BYTES,
        }),
    }
}

fn lstat(path: &Path) -> Result<Metadata, ReadFault> {
    std::fs::symlink_metadata(path).map_err(|e| match e.kind() {
        ErrorKind::NotFound => ReadFault::Missing,
        kind => ReadFault::Io(kind),
    })
}

fn check_dir(path: &Path, what: &'static str) -> Result<(), ReadFault> {
    if !lstat(path)?.file_type().is_dir() {
        return Err(ReadFault::NotRegular {
            what,
            kind: "directory",
        });
    }
    Ok(())
}

/// Read a regular, non-symlink file of at most [`MAX_FILE_BYTES`].
fn read_regular(path: &Path, what: &'static str) -> Result<Vec<u8>, ReadFault> {
    let not_regular = ReadFault::NotRegular { what, kind: "file" };
    let checked = lstat(path)?;
    if !checked.file_type().is_file() {
        return Err(not_regular);
    }
    let mut file = File::open(path).map_err(|e| match e.kind() {
        ErrorKind::NotFound => ReadFault::Missing,
        kind => ReadFault::Io(kind),
    })?;
    let opened = file.metadata().map_err(|e| ReadFault::Io(e.kind()))?;
    // #8454: the path was swapped for another file between the check and
    // the open; the load gate would refuse foreign bytes too.
    if !same_file(&checked, &opened) {
        return Err(not_regular);
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ReadFault::Io(e.kind()))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ReadFault::TooLarge);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn same_file(a: &Metadata, b: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    a.dev() == b.dev() && a.ino() == b.ino() && b.file_type().is_file()
}

#[cfg(not(unix))]
fn same_file(_: &Metadata, b: &Metadata) -> bool {
    b.file_type().is_file()
}
