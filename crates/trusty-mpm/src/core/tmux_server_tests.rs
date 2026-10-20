//! Tests for the #8928 step 4 workspace tmux server.

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use super::*;

fn scratch() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    (dir, root)
}

#[test]
fn a_workspace_name_is_stable_and_distinct() {
    let (_dir, root) = scratch();
    let a = root.join("one/trusty-tools");
    let b = root.join("two/trusty-tools");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    assert_eq!(workspace_server_name(&a), workspace_server_name(&a));
    assert_ne!(workspace_server_name(&a), workspace_server_name(&b));
    // Another spelling of the same directory names the same server.
    let link = root.join("link");
    std::os::unix::fs::symlink(&a, &link).expect("symlink");
    assert_eq!(workspace_server_name(&link), workspace_server_name(&a));
    assert_eq!(
        workspace_server_name(&a.join("../trusty-tools")),
        workspace_server_name(&a)
    );
}

#[test]
fn a_workspace_name_is_a_safe_socket_name() {
    for path in ["/w/Trusty Tools.v2", "/w/x", "/", "/w/ünïcode", "/w/----"] {
        let name = workspace_server_name(Path::new(path));
        assert!(name.starts_with(WORKSPACE_SERVER_PREFIX), "{name}");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{path} -> {name}"
        );
        assert!(name.len() <= 6 + 24 + 1 + 12, "{name}");
        assert!(!name.contains("--"), "no empty slug segment: {name}");
    }
    assert_eq!(
        workspace_server_name(Path::new("/w/Trusty Tools.v2"))
            .rsplit_once('-')
            .map(|(slug, _)| slug.to_owned()),
        Some("tm-ws-trusty-tools-v2".to_owned())
    );
}

#[test]
fn the_socket_dir_follows_the_tmp_symlink() {
    let tmp = std::fs::canonicalize("/tmp").expect("canonical /tmp");
    assert_eq!(socket_dir(None, 502), tmp.join("tmux-502"));
    assert_eq!(socket_dir(Some(Path::new("")), 502), tmp.join("tmux-502"));
    let (_dir, root) = scratch();
    let link = root.join("tmpdir-link");
    std::os::unix::fs::symlink(&root, &link).expect("symlink");
    assert_eq!(socket_dir(Some(&link), 7), root.join("tmux-7"));
    assert_eq!(
        server_socket_path(&root.join("tmux-7"), "tm-ws-x-1"),
        root.join("tmux-7/tm-ws-x-1")
    );
}

#[test]
fn server_args_select_the_named_server() {
    assert_eq!(
        server_args("tm-ws-a-1"),
        ["-L".to_owned(), "tm-ws-a-1".to_owned()]
    );
}

#[test]
fn live_servers_are_listed_and_dead_ones_skipped() {
    let (_dir, root) = scratch();
    assert_eq!(
        live_workspace_servers(&root.join("absent")).expect("absent"),
        Vec::<String>::new()
    );
    let _live_b = UnixListener::bind(root.join("tm-ws-b-2")).expect("bind b");
    let _live_a = UnixListener::bind(root.join("tm-ws-a-1")).expect("bind a");
    drop(UnixListener::bind(root.join("tm-ws-dead-3")).expect("bind dead"));
    std::fs::write(root.join("tm-ws-file-4"), "x").expect("write file");
    let _default = UnixListener::bind(root.join("default")).expect("bind default");
    assert_eq!(
        live_workspace_servers(&root).expect("readable"),
        vec!["tm-ws-a-1".to_owned(), "tm-ws-b-2".to_owned()]
    );
}
