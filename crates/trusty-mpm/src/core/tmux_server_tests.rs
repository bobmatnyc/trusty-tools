//! Tests for the #8928 step 4 workspace tmux server.

use std::os::unix::net::{UnixDatagram, UnixListener};
use std::path::{Path, PathBuf};

use super::*;

fn scratch() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    (dir, root)
}

/// A socket file nothing can stream-connect to. A dropped `UnixListener`
/// is not one under a parallel suite: macOS sets `FD_CLOEXEC` after
/// `socket()`, so a child another test forks in that window keeps the
/// listener alive (#10083). A datagram socket refuses a stream connect
/// whoever holds its fd.
fn dead_socket(path: &Path) {
    drop(UnixDatagram::bind(path).expect("bind dead socket"));
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

/// #10101: `-L <name>` only for a named server; the default-server argv is
/// exactly the command.
#[test]
fn server_argv_prefixes_only_a_named_server() {
    let command = || ["list-sessions".to_owned(), "-F".to_owned(), "x".to_owned()];
    assert_eq!(server_argv(None, command()), command().to_vec());
    assert_eq!(
        server_argv(Some("tm-ws-a-1"), command()),
        ["-L", "tm-ws-a-1", "list-sessions", "-F", "x"].map(String::from)
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
    dead_socket(&root.join("tm-ws-dead-3"));
    std::fs::write(root.join("tm-ws-file-4"), "x").expect("write file");
    let _default = UnixListener::bind(root.join("default")).expect("bind default");
    assert_eq!(
        live_workspace_servers(&root).expect("readable"),
        vec!["tm-ws-a-1".to_owned(), "tm-ws-b-2".to_owned()]
    );
}

#[test]
fn a_socket_probe_answers_and_refuses() {
    let (_dir, root) = scratch();
    let live = root.join("tm-ws-live-1");
    let _listener = UnixListener::bind(&live).expect("bind");
    assert_eq!(socket_answers(&live, PROBE_BUDGET), Some(true));
    let dead = root.join("tm-ws-dead-2");
    dead_socket(&dead);
    assert_eq!(socket_answers(&dead, PROBE_BUDGET), Some(false));
    assert_eq!(
        socket_answers(&root.join("absent"), PROBE_BUDGET),
        Some(false)
    );
}

/// Error arm (Fail-Open Check): while a probe of one server is in flight, a
/// second probe of that server answers `None` at once and never runs; another
/// server is probed as usual, and the first server is free once its probe
/// returns. Red on 2df3760494, whose every probe spawned a new thread.
#[test]
fn a_server_with_a_probe_in_flight_is_not_probed_again() {
    let (_dir, root) = scratch();
    let hung = root.join("tm-ws-hung-1");
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let started = std::time::Instant::now();
    let first = bounded_probe(&hung, std::time::Duration::from_millis(100), move || {
        let _ = wait.recv();
    });
    assert_eq!(first, None, "the hung probe expired");
    assert!(
        probe_in_flight(&hung),
        "the hung probe still holds its server"
    );

    let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&ran);
    let second = bounded_probe(&hung, PROBE_BUDGET, move || {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    assert_eq!(second, None);
    assert!(
        !ran.load(std::sync::atomic::Ordering::SeqCst),
        "a second probe of a server with one in flight ran"
    );
    assert!(started.elapsed() < PROBE_BUDGET, "the second probe waited");
    assert_eq!(
        bounded_probe(&root.join("tm-ws-other-2"), PROBE_BUDGET, || 7),
        Some(7)
    );

    release.send(()).expect("release the hung probe");
    let freed = std::time::Instant::now();
    while probe_in_flight(&hung) && freed.elapsed() < PROBE_BUDGET {
        std::thread::yield_now();
    }
    assert_eq!(bounded_probe(&hung, PROBE_BUDGET, || 1), Some(1));
}

#[test]
fn boot_time_is_parsed_from_proc_stat_and_sysctl() {
    assert_eq!(
        parse_proc_stat_btime("cpu  1 2 3\nbtime 1762500000\nprocesses 9\n"),
        Some(1_762_500_000)
    );
    assert_eq!(parse_proc_stat_btime("cpu 1\n"), None);
    assert_eq!(
        parse_kern_boottime("{ sec = 1762500000, usec = 12 } Fri Nov  7 08:00:00 2026"),
        Some(1_762_500_000)
    );
    assert_eq!(parse_kern_boottime("garbage"), None);
}

/// #10101: a stored socket in this process's socket directory names its
/// server; no socket is the default server.
#[test]
fn a_socket_in_the_socket_dir_names_its_server() {
    let dir = Path::new("/private/tmp/tmux-501");
    assert_eq!(server_of_socket(None, dir), Ok(None));
    assert_eq!(
        server_of_socket(Some(&dir.join("tm-ws-proj-abc")), dir),
        Ok(Some("tm-ws-proj-abc".to_owned()))
    );
}

/// #10101: a socket in another directory, or with no file name, is refused
/// rather than re-aimed at a same-named server here or the default server.
#[test]
fn a_socket_elsewhere_or_unnamed_is_refused() {
    let dir = Path::new("/private/tmp/tmux-501");
    let elsewhere = Path::new("/var/folders/x/tmux-501/tm-ws-proj-abc");
    let err = server_of_socket(Some(elsewhere), dir).expect_err("another dir is refused");
    assert!(
        err.contains("/var/folders/x/tmux-501/tm-ws-proj-abc"),
        "{err}"
    );
    assert!(server_of_socket(Some(Path::new("/")), dir).is_err());
}
