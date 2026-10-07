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
/// same socket, so it must not unlock [`INDEX_DIR_ENV`] for the shared
/// server; a different file, or a directory that does not exist, is not it.
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
    assert!(!super::same_socket(&link.join("other.sock"), &socket));
    assert!(!super::same_socket(
        &tmp.path().join("secrets.sock"),
        &socket
    ));
    let absent = tmp.path().join("absent/secrets.sock");
    assert!(!super::same_socket(&absent, &socket));
}
