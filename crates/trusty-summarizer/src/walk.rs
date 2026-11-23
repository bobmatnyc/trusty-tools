//! Expands the command-line paths into the batch's file list.
//!
//! What: a file is taken as given. A directory is walked recursively in name
//! order, skipping entries whose name starts with `.` and not following
//! symlinked directories (no loops). A path that does not exist, or a
//! directory that cannot be listed, becomes an error entry for that path, so
//! the batch still runs.

use std::path::{Path, PathBuf};

use crate::error::Error;

/// One batch input: a file to process, or a path that failed to expand.
pub type Input = Result<PathBuf, (PathBuf, Error)>;

/// Expands `roots` into files, in a deterministic order.
/// Test: `walk_recurses_and_skips_hidden`
pub fn expand(roots: &[PathBuf]) -> Vec<Input> {
    let mut out = Vec::new();
    for root in roots {
        match std::fs::metadata(root) {
            Ok(m) if m.is_dir() => walk_dir(root, &mut out),
            Ok(_) => out.push(Ok(root.clone())),
            Err(e) => out.push(Err((root.clone(), Error::io(root, e)))),
        }
    }
    out
}

/// [`expand`] with relative roots resolved against `base` (the caller's
/// working directory, for a warm server whose own differs). Paths in the
/// result, and in error entries, are spelled as the caller gave them, so
/// records match the in-process path exactly.
/// Test: `expand_in_spells_paths_as_given`
pub fn expand_in(base: Option<&Path>, roots: &[PathBuf]) -> Vec<Input> {
    let Some(base) = base else {
        return expand(roots);
    };
    let mut out = Vec::new();
    for root in roots {
        let abs = base.join(root);
        for item in expand(std::slice::from_ref(&abs)) {
            out.push(match item {
                Ok(p) => Ok(respell(&p, &abs, root)),
                Err((p, e)) => Err((respell(&p, &abs, root), respell_error(e, &abs, root))),
            });
        }
    }
    out
}

/// `p`, under `abs`, spelled under `shown` instead.
pub(crate) fn respell(p: &Path, abs: &Path, shown: &Path) -> PathBuf {
    match p.strip_prefix(abs) {
        Ok(rest) if rest.as_os_str().is_empty() => shown.to_path_buf(),
        Ok(rest) => shown.join(rest),
        Err(_) => p.to_path_buf(),
    }
}

/// An I/O error's path respelled as [`respell`] does; other errors as-is.
pub(crate) fn respell_error(e: Error, abs: &Path, shown: &Path) -> Error {
    match e {
        Error::Io { path, source } => Error::Io {
            path: respell(&path, abs, shown),
            source,
        },
        other => other,
    }
}

fn walk_dir(dir: &Path, out: &mut Vec<Input>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            out.push(Err((dir.to_path_buf(), Error::io(dir, e))));
            return;
        }
    };
    let mut children: Vec<(PathBuf, bool)> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                out.push(Err((dir.to_path_buf(), Error::io(dir, e))));
                continue;
            }
        };
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        // `file_type` does not follow symlinks; a symlink to a file is
        // still processed through `metadata`, a symlink to a directory is not.
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        let is_link = entry.file_type().is_ok_and(|t| t.is_symlink());
        if is_link && std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        children.push((path, is_dir));
    }
    children.sort();
    for (path, is_dir) in children {
        if is_dir {
            walk_dir(&path, out);
        } else {
            out.push(Ok(path));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_in_spells_paths_as_given() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("d")).unwrap();
        std::fs::write(tmp.path().join("d/a.txt"), "x").unwrap();
        let shown = |items: Vec<Input>| -> Vec<String> {
            items
                .into_iter()
                .map(|i| match i {
                    Ok(p) => p.display().to_string(),
                    Err((p, e)) => format!("ERR {} {e}", p.display()),
                })
                .collect()
        };
        let roots = [
            PathBuf::from("./d"),
            PathBuf::from("d/a.txt"),
            "nope".into(),
        ];
        let got = shown(expand_in(Some(tmp.path()), &roots));
        assert_eq!(got[..2], ["./d/a.txt".to_string(), "d/a.txt".into()]);
        assert!(
            got[2].starts_with("ERR nope cannot read nope:"),
            "{}",
            got[2]
        );
    }

    #[test]
    fn walk_recurses_and_skips_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("b/c")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        for f in ["a.md", "b/c/d.txt", "b/z.rs", ".hidden", ".git/config"] {
            std::fs::write(root.join(f), "x").unwrap();
        }
        let missing = root.join("missing");
        let got: Vec<_> = expand(&[root.to_path_buf(), missing.clone()])
            .into_iter()
            .map(|i| match i {
                Ok(p) => p.strip_prefix(root).unwrap().display().to_string(),
                Err((p, e)) => format!("ERR {} {}", p.display(), e.kind()),
            })
            .collect();
        assert_eq!(
            got,
            [
                "a.md".to_string(),
                "b/c/d.txt".into(),
                "b/z.rs".into(),
                format!("ERR {} io", missing.display()),
            ]
        );
    }
}
