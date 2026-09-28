//! Tests for [`super`]: the relocation, the lazy default server, and the
//! reaper's refusal to act on anything it did not verify (#6542).

use super::*;

/// The resolved tmux binary, or `None` (and a skip note) when the host has
/// no working tmux.
fn working_tmux() -> Option<String> {
    let bin = crate::core::tmux::resolve_tmux_binary_or_bare();
    let version = crate::core::spawn_disclaim::disclaimed_output(&bin, &["-V".into()]);
    if version.is_ok_and(|out| out.status.success()) {
        Some(bin)
    } else {
        eprintln!("tmux not available; skipping");
        None
    }
}

/// Run `args` against the server behind `socket`; whether tmux exited 0.
fn tmux_at(bin: &str, socket: &Path, args: &[&str]) -> bool {
    let mut argv = vec!["-S".to_string(), socket.to_string_lossy().into_owned()];
    argv.extend(args.iter().map(|a| (*a).to_string()));
    crate::core::spawn_disclaim::disclaimed_output(bin, &argv).is_ok_and(|out| out.status.success())
}

/// Kills the server behind `socket` on drop, panic included.
struct ServerGuard {
    bin: String,
    socket: PathBuf,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = tmux_at(&self.bin, &self.socket, &["kill-server"]);
    }
}

/// A private scratch directory under `/tmp`, short enough for a socket path.
fn scratch_dir(prefix: &str) -> PathBuf {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(RELOCATION_ROOT)
        .expect("scratch dir")
        .keep()
}

#[test]
fn a_relocated_directory_names_its_owner() {
    assert_eq!(owner_pid("4242"), Some(4242));
    assert_eq!(owner_pid(""), None);
    assert_eq!(owner_pid("42x"), None);
    assert_eq!(owner_pid("+42"), None);
    assert_eq!(owner_pid("tm-tmux-4242"), None);
    assert_eq!(owner_pid("u502"), None);
}

/// The lib test binary's constructor relocated it (`test_support`).
#[test]
fn this_test_binary_runs_on_a_relocated_tmux_server() {
    let dir = relocated_dir().expect("the lib test_support constructor relocates tmux");
    let parent = Path::new(RELOCATION_ROOT).join(format!("{DIR_PREFIX}{}", effective_uid()));
    assert_eq!(dir, parent.join(std::process::id().to_string()));
    assert!(
        checked(&parent, Kind::PrivateDir).is_some(),
        "parent is 0700"
    );
    assert_eq!(
        std::env::var_os("TMUX_TMPDIR").as_deref(),
        Some(dir.as_os_str())
    );
    assert!(
        std::env::var_os("TMUX").is_none(),
        "the host's $TMUX must not reach a test"
    );
    assert!(dir.is_dir(), "{} must exist while tests run", dir.display());
}

/// No constructor starts a server; the first in-process resolution does, with
/// `exit-empty off` from [`ensure_default_server`].
#[test]
fn the_first_resolution_starts_a_config_free_default_server() {
    let Some(bin) = working_tmux() else {
        return;
    };
    let dir = relocated_dir().expect("relocated");
    let socket = dir
        .join(format!("tmux-{}", effective_uid()))
        .join("default");
    assert!(
        checked(&socket, Kind::Socket).is_some(),
        "{} must be the lazily started default server",
        socket.display()
    );
    let out = crate::core::spawn_disclaim::disclaimed_output(
        &bin,
        &[
            "-S".to_string(),
            socket.to_string_lossy().into_owned(),
            "show-options".to_string(),
            "-gv".to_string(),
            "exit-empty".to_string(),
        ],
    )
    .expect("show-options");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "off");
}

/// A server started under a relocated directory dies with it.
#[test]
fn reaping_a_directory_kills_its_servers_and_removes_it() {
    let Some(bin) = working_tmux() else {
        return;
    };
    let dir = scratch_dir("tm-tmux-reap-");
    let socket_dir = dir.join("tmux-test");
    std::fs::create_dir(&socket_dir).expect("socket dir");
    let socket = socket_dir.join("s");
    let _guard = ServerGuard {
        bin: bin.clone(),
        socket: socket.clone(),
    };
    let new_session = ["new-session", "-d", "-s", "reap", "sleep 300"];
    assert!(tmux_at(&bin, &socket, &new_session));
    assert!(tmux_at(&bin, &socket, &["has-session", "-t", "=reap"]));

    reap_dir(&dir);

    assert!(
        !tmux_at(&bin, &socket, &["has-session", "-t", "=reap"]),
        "server must be dead"
    );
    assert!(!dir.exists(), "{} must be removed", dir.display());
}

/// The #6542 critic HIGH: a planted symlink must never steer the reaper to a
/// server outside the directory it reaps, and a non-socket entry is left alone.
///
/// Before the fix `reap_dir` listed a symlinked subdirectory through the link,
/// sent `kill-server` through a symlinked socket, and `remove_dir_all`-ed the
/// regular file — so another local user could make a test run kill the
/// operator's real tmux server.
#[test]
fn reaping_never_follows_a_planted_symlink_or_touches_a_non_socket() {
    let Some(bin) = working_tmux() else {
        return;
    };
    // The victim: a live server and a file outside the reaped directory.
    let victim = scratch_dir("tm-tmux-victim-");
    let victim_sockets = victim.join("tmux-v");
    std::fs::create_dir(&victim_sockets).expect("victim socket dir");
    let victim_socket = victim_sockets.join("s");
    let _victim_guard = ServerGuard {
        bin: bin.clone(),
        socket: victim_socket.clone(),
    };
    let new_session = ["new-session", "-d", "-s", "victim", "sleep 300"];
    assert!(tmux_at(&bin, &victim_socket, &new_session));
    let victim_file = victim.join("outside");
    std::fs::write(&victim_file, "keep").expect("victim file");

    // The plant: every entry the old reaper followed or deleted.
    let planted = scratch_dir("tm-tmux-plant-");
    let sub = planted.join("tmux-p");
    std::fs::create_dir(&sub).expect("planted socket dir");
    std::os::unix::fs::symlink(&victim_sockets, planted.join("linked-dir")).expect("link");
    std::os::unix::fs::symlink(&victim_socket, sub.join("linked-socket")).expect("link");
    std::os::unix::fs::symlink(&victim_file, sub.join("linked-file")).expect("link");
    let plain = sub.join("plain");
    std::fs::write(&plain, "plain").expect("non-socket entry");

    reap_dir(&planted);

    let alive = tmux_at(&bin, &victim_socket, &["has-session", "-t", "=victim"]);
    let plain_kept = std::fs::read_to_string(&plain).ok();
    let victim_kept = std::fs::read_to_string(&victim_file).ok();
    let _ = std::fs::remove_dir_all(&planted);
    let _ = std::fs::remove_dir_all(&victim);
    assert!(alive, "a server reached through a symlink must survive");
    assert_eq!(
        plain_kept.as_deref(),
        Some("plain"),
        "non-socket left alone"
    );
    assert_eq!(
        victim_kept.as_deref(),
        Some("keep"),
        "link target untouched"
    );
}

/// The uid, type and identity checks, including another uid's entry, which
/// no unprivileged test can plant on disk.
#[test]
fn only_an_unchanged_owned_socket_or_private_dir_is_reapable() {
    let uid = effective_uid();
    let dir = tempfile::Builder::new()
        .prefix("tm-tmux-kind-")
        .tempdir_in(RELOCATION_ROOT)
        .expect("scratch dir");
    let socket = dir.path().join("s");
    let first = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    let meta = std::fs::symlink_metadata(&socket).expect("socket meta");
    assert!(reapable(&meta, uid, Kind::Socket));
    assert!(
        !reapable(&meta, uid.wrapping_add(1), Kind::Socket),
        "other uid"
    );
    assert!(
        !reapable(&meta, uid, Kind::PrivateDir),
        "a socket is no dir"
    );

    let dir_meta = std::fs::symlink_metadata(dir.path()).expect("dir meta");
    assert!(reapable(&dir_meta, uid, Kind::PrivateDir));
    assert!(!reapable(&dir_meta, uid.wrapping_add(1), Kind::PrivateDir));
    let open = dir.path().join("open");
    std::fs::create_dir(&open).expect("open dir");
    std::fs::set_permissions(&open, std::os::unix::fs::PermissionsExt::from_mode(0o777))
        .expect("chmod");
    assert!(
        checked(&open, Kind::PrivateDir).is_none(),
        "others can write"
    );

    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&socket, &link).expect("link");
    assert!(checked(&link, Kind::Socket).is_none(), "never followed");

    // A socket swapped in at the same path is a different identity. The first
    // stays linked under another name, so its inode cannot be reused.
    let seen = checked(&socket, Kind::Socket).expect("owned socket");
    let other = dir.path().join("s2");
    let _second = std::os::unix::net::UnixListener::bind(&other).expect("bind");
    std::fs::rename(&socket, dir.path().join("s-old")).expect("move aside");
    std::fs::rename(&other, &socket).expect("swap in");
    assert_ne!(checked(&socket, Kind::Socket), Some(seen), "swapped socket");
    drop(first);
}
