//! Unit tests for where [`super::ServerSettings::from_args`] puts the
//! names-only index (#7524 M3). The environment is passed in as a closure,
//! so no test here calls `set_var`.
//!
//! Test: itself.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{INDEX_DIR_ENV, SOCKET_SUBPATH, ServerSettings};
use crate::store::INDEX_SUBDIR;

/// An environment that sets only [`INDEX_DIR_ENV`], to `value`.
fn index_env(value: &'static str) -> impl Fn(&str) -> Option<String> + Copy {
    move |name: &str| (name == INDEX_DIR_ENV).then(|| value.to_string())
}

/// `serve`, plus `--socket <socket>` when given, plus `extra`.
fn serve_args(socket: Option<&Path>, extra: &[&str]) -> Vec<OsString> {
    let mut args = vec![OsString::from("serve")];
    if let Some(socket) = socket {
        args.push("--socket".into());
        args.push(socket.into());
    }
    args.extend(extra.iter().map(OsString::from));
    args
}

/// Why: #7524 M3 — the on-demand client passes the caller's environment to
/// the server it spawns, and that server answers every client of the default
/// socket. One caller's `TRUSTY_SECRETS_INDEX_DIR` must not move the names
/// index for all of them, whether the socket is the default by omission, by
/// the `--socket` flag the client passes, or by another spelling of it.
/// Red when `from_args` takes the index from the environment on the default
/// socket.
/// Test: itself.
#[test]
fn settings_index_env_is_ignored_on_the_default_socket() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let env = index_env("/repo/checkout/index");
    let spellings = [
        home.join(SOCKET_SUBPATH),
        home.join(".trusty-tools/./trusty-secrets//secrets.sock"),
        home.join(".trusty-tools/trusty-secrets/../trusty-secrets/secrets.sock"),
    ];
    let sockets = std::iter::once(None).chain(spellings.iter().map(|s| Some(s.as_path())));
    for socket in sockets {
        let parsed = ServerSettings::from_args(serve_args(socket, &[]), env).unwrap();
        assert_eq!(parsed.index_root, home.join(INDEX_SUBDIR), "{socket:?}");
    }
}

/// Why: #7524 M3 — tests and sandboxes still move the index. Off the default
/// socket the environment does; the `--index-dir` flag does on any socket,
/// because argv is chosen by the spawning code, not inherited.
/// Test: itself.
#[test]
fn settings_index_override_survives_off_the_default_socket() {
    let env = index_env("/env/index");
    let sandbox = Path::new("/sandbox-7524/s.sock");
    let parsed = ServerSettings::from_args(serve_args(Some(sandbox), &[]), env).unwrap();
    assert_eq!(parsed.index_root, PathBuf::from("/env/index"));

    let Some(home) = dirs::home_dir() else {
        return;
    };
    let default_socket = home.join(SOCKET_SUBPATH);
    let args = serve_args(Some(&default_socket), &["--index-dir", "/flag/index"]);
    let parsed = ServerSettings::from_args(args, env).unwrap();
    assert_eq!(parsed.index_root, PathBuf::from("/flag/index"));
}

/// Why: #7524 M3 — a socket path through a symlinked directory names the
/// same directory, so it must not unlock [`INDEX_DIR_ENV`] for the shared
/// server. Any file name in that directory counts; another directory, or
/// one that does not exist, is not it.
/// Test: itself.
#[test]
fn settings_socket_alias_through_a_symlinked_dir_is_the_same_socket() {
    let tmp = tempfile::TempDir::new().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let socket = real.join("secrets.sock");
    assert!(super::same_socket(&link.join("secrets.sock"), &socket));
    assert!(super::same_socket(&real.join("x/../secrets.sock"), &socket));
    assert!(super::same_socket(&link.join("other.sock"), &socket));
    assert!(!super::same_socket(
        &tmp.path().join("secrets.sock"),
        &socket
    ));
    let absent = tmp.path().join("absent/secrets.sock");
    assert!(!super::same_socket(&absent, &socket));
}

/// Why: #7524 M3 review — APFS compares names case-insensitively, so a
/// server on `SECRETS.SOCK` in the default directory answers a client
/// dialling the default socket. Any socket in that directory is the default,
/// and so is a directory spelled in another case where the filesystem holds
/// it to be one directory.
/// What: the directory-case assertion runs only when a probe finds the
/// tempdir case-insensitive, so a case-sensitive Linux volume skips it.
/// Red when the file-name comparison is case-sensitive.
/// Test: itself.
#[test]
fn settings_index_env_is_ignored_for_a_case_variant_default_socket() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("Sockets");
    std::fs::create_dir(&dir).unwrap();
    let socket = dir.join("secrets.sock");
    assert!(super::same_socket(&dir.join("SECRETS.SOCK"), &socket));
    let folded = tmp.path().join("sockets");
    if std::fs::metadata(&folded).is_ok() {
        assert!(super::same_socket(&folded.join("Secrets.Sock"), &socket));
    }

    let Some(home) = dirs::home_dir() else {
        return;
    };
    let variant = home.join(".trusty-tools/trusty-secrets/SECRETS.SOCK");
    let env = index_env("/repo/checkout/index");
    let parsed = ServerSettings::from_args(serve_args(Some(&variant), &[]), env).unwrap();
    assert_eq!(parsed.index_root, home.join(INDEX_SUBDIR));
}

/// Why: #7524 M3 review — `--socket secrets.sock` run from the default
/// directory binds the default socket, but its parent is the empty path,
/// which `canonicalize` refuses, so the pre-fix check judged it another
/// socket.
/// What: the test reads the process working directory and never changes
/// it. The working directory stands in for the default directory in the
/// `same_socket` assertions; then a relative spelling of the real default
/// socket, climbing to `/` with `..`, goes through `from_args`.
/// Red when an empty parent does not resolve to the working directory.
/// Test: itself.
#[test]
fn settings_index_env_is_ignored_for_a_bare_relative_default_socket() {
    let cwd = std::env::current_dir().unwrap();
    let socket = cwd.join("secrets.sock");
    assert!(super::same_socket(Path::new("secrets.sock"), &socket));
    assert!(super::same_socket(Path::new("./secrets.sock"), &socket));

    let Some(home) = dirs::home_dir() else {
        return;
    };
    let Ok(below_root) = home.strip_prefix("/") else {
        return;
    };
    let mut relative: PathBuf = cwd.components().skip(1).map(|_| "..").collect();
    relative.push(below_root);
    relative.push(SOCKET_SUBPATH);
    let env = index_env("/repo/checkout/index");
    let parsed = ServerSettings::from_args(serve_args(Some(&relative), &[]), env).unwrap();
    assert_eq!(parsed.index_root, home.join(INDEX_SUBDIR));
}

/// Why: #7524 M3 delta review — the server's `create_dir_all` resolves a
/// `..` that follows a missing directory, so `nx/../sub` reaches `sub`
/// once `nx` exists. A lexical `..` over the missing `nx` judged it another
/// directory.
/// Red when a `..` among the missing names is applied lexically.
/// Test: itself.
#[test]
fn settings_dotdot_over_a_missing_dir_is_the_default_socket() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    let socket = tmp.path().join("sub/s.sock");
    let candidate = tmp.path().join("nx/../sub/s.sock");
    assert!(super::same_socket(&candidate, &socket));
}

/// Why: #7524 M3 delta review — APFS folds `ſ` (U+017F) to `s`, which
/// `to_lowercase` does not, so a missing name spelled with it could create
/// the default directory under another identity.
/// Red when a non-ASCII missing name is compared by `to_lowercase`.
/// Test: itself.
#[test]
fn settings_non_ascii_missing_name_is_the_default_socket() {
    let tmp = tempfile::TempDir::new().unwrap();
    let socket = tmp.path().join("secrets/s.sock");
    let candidate = tmp.path().join("\u{17f}ecrets/s.sock");
    assert!(super::same_socket(&candidate, &socket));
}
