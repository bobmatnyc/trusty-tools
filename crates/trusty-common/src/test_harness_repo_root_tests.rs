//! Table test for [`super::resolve_repo_root`] (#9298): source precedence, an
//! invalid explicit root, no root anywhere, and walk-up from a nested dir.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::{REPO_ROOT_ENV, resolve_repo_root};

/// A workspace root at `dir`: a `Cargo.toml` whose `[workspace]` line is
/// indented, so the test also pins the trimmed-line match.
fn workspace(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir.join("crates/member/src/nested")).expect("mkdir");
    std::fs::write(
        dir.join("Cargo.toml"),
        "  [workspace]\nmembers = [\"crates/*\"]\n",
    )
    .expect("write root manifest");
    // A member manifest that mentions `workspace` without the table header.
    std::fs::write(
        dir.join("crates/member/Cargo.toml"),
        "[package]\nname = \"member\"\n[dependencies]\nx = { workspace = true }\n",
    )
    .expect("write member manifest");
    dir.canonicalize().expect("canonical root")
}

/// Expected outcome of one row.
enum Want {
    Root(PathBuf),
    Err(ErrorKind),
}

/// One table row: (name, explicit, manifest dir, cwd, expected).
type Row<'a> = (
    &'a str,
    Option<&'a Path>,
    Option<&'a Path>,
    Option<&'a Path>,
    Want,
);

#[test]
fn resolve_repo_root_table() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a = workspace(&tmp.path().join("a"));
    let b = workspace(&tmp.path().join("b"));
    let a_member = a.join("crates/member");
    let b_nested = b.join("crates/member/src/nested");
    let lonely = tmp.path().join("lonely/deeper");
    std::fs::create_dir_all(&lonely).expect("mkdir lonely");
    let missing = tmp.path().join("does-not-exist");

    let rows: Vec<Row<'_>> = vec![
        (
            "explicit outranks manifest dir and cwd",
            Some(&b),
            Some(&a_member),
            Some(&a_member),
            Want::Root(b.clone()),
        ),
        (
            "explicit is not walked up, and an invalid one never falls through",
            Some(&a_member),
            Some(&a_member),
            Some(&a_member),
            Want::Err(ErrorKind::InvalidInput),
        ),
        (
            "an empty explicit value is invalid",
            Some(Path::new("")),
            Some(&a_member),
            None,
            Want::Err(ErrorKind::InvalidInput),
        ),
        (
            "manifest dir outranks cwd",
            None,
            Some(&a_member),
            Some(&b_nested),
            Want::Root(a.clone()),
        ),
        (
            "cwd walks up from a nested dir when the manifest dir is unset",
            None,
            None,
            Some(&b_nested),
            Want::Root(b.clone()),
        ),
        (
            "a manifest dir that does not exist falls through to cwd",
            None,
            Some(&missing),
            Some(&b_nested),
            Want::Root(b.clone()),
        ),
        (
            "a manifest dir outside any workspace falls through to cwd",
            None,
            Some(&lonely),
            Some(&a_member),
            Want::Root(a.clone()),
        ),
        (
            "no workspace anywhere is NotFound",
            None,
            Some(&lonely),
            Some(&lonely),
            Want::Err(ErrorKind::NotFound),
        ),
        (
            "no sources at all is NotFound",
            None,
            None,
            None,
            Want::Err(ErrorKind::NotFound),
        ),
    ];

    for (name, explicit, manifest_dir, cwd, want) in rows {
        let got = resolve_repo_root(explicit, manifest_dir, cwd);
        match (want, got) {
            (Want::Root(want), Ok(got)) => assert_eq!(got, want, "{name}"),
            (Want::Err(kind), Err(err)) => {
                assert_eq!(err.kind(), kind, "{name}: {err}");
                assert!(
                    err.to_string().contains(REPO_ROOT_ENV),
                    "{name}: the error must name {REPO_ROOT_ENV}: {err}"
                );
            }
            (Want::Root(want), Err(err)) => panic!("{name}: want {}, got {err}", want.display()),
            (Want::Err(kind), Ok(got)) => {
                panic!("{name}: want {kind:?}, got {}", got.display())
            }
        }
    }
}
