//! Unit tests for the pure parts of `linux_lifecycle`; the real-process tests
//! live in `tests/linux_lifecycle.rs`.

use super::process::strip_deleted;
use super::*;

#[test]
fn exe_name_strips_the_deleted_suffix() {
    assert_eq!(
        strip_deleted("/home/u/.cargo/bin/trusty-memory (deleted)"),
        "trusty-memory"
    );
    assert_eq!(strip_deleted("/usr/bin/trusty-search"), "trusty-search");
    // Only the kernel's trailing marker is stripped.
    assert_eq!(strip_deleted("/opt/a (deleted)/b"), "b");
}

#[test]
fn descriptor_rejects_a_binary_name_with_a_path_separator() {
    let make = |binary: &str, dir: &str| {
        DaemonDescriptor::with_install_dir(binary, dir, "/tmp/x.sock", StoppedSignal::SocketRefuses)
    };
    for bad in ["", ".", "..", "bin/daemon", "/abs/daemon"] {
        assert!(
            matches!(
                make(bad, "/opt/bin"),
                Err(LifecycleError::InvalidDescriptor { .. })
            ),
            "binary {bad:?} must be refused"
        );
    }
    assert!(matches!(
        make("daemon", "relative/bin"),
        Err(LifecycleError::InvalidDescriptor { .. })
    ));
    let ok = make("daemon", "/opt/bin").expect("valid descriptor");
    assert_eq!(ok.program(), PathBuf::from("/opt/bin/daemon"));

    // The fields are public, so every entry point re-validates.
    let mut edited = ok;
    edited.binary = "../daemon".into();
    assert!(edited.validate().is_err());
}

#[test]
fn descriptor_new_uses_the_canonical_bin_dir() {
    let desc = DaemonDescriptor::new("daemon", "/tmp/x.sock", StoppedSignal::SocketRefuses);
    match crate::bin_resolve::canonical_bin_dir() {
        Some(dir) => assert_eq!(desc.expect("descriptor").install_dir, dir),
        None => assert!(matches!(desc, Err(LifecycleError::NoInstallDir))),
    }
}

#[test]
fn lock_is_free_reads_an_absent_file_as_free_and_a_held_lock_as_held() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lock = dir.path().join("daemon.lock");
    assert!(lock_is_free(&lock).expect("absent lock"));

    let holder = std::fs::File::create(&lock).expect("create lock");
    holder.lock().expect("take lock");
    assert!(!lock_is_free(&lock).expect("held lock"));
    drop(holder);
    assert!(lock_is_free(&lock).expect("released lock"));
}

fn plain_descriptor() -> DaemonDescriptor {
    DaemonDescriptor::with_install_dir(
        "daemon",
        "/opt/bin",
        "/tmp/x.sock",
        StoppedSignal::SocketRefuses,
    )
    .expect("valid descriptor")
}

// #11098.
#[test]
fn descriptor_rejects_bad_spawn_and_liveness_fields() {
    let ok = plain_descriptor();
    assert_eq!(ok.working_dir, PathBuf::from("/"), "the default cwd is /");
    assert!(ok.env_passthrough.is_empty() && ok.liveness_args.is_empty());

    let mut relative = ok.clone();
    relative.working_dir = "data".into();
    assert!(relative.validate().is_err(), "a relative working_dir");
    let mut nul_arg = ok.clone();
    nul_arg.liveness_args = vec!["a\0b".into()];
    assert!(nul_arg.validate().is_err(), "a NUL in liveness_args");
    for name in ["", "A=B", "A\0"] {
        let mut bad_env = ok.clone();
        bad_env.env_passthrough = vec![name.into()];
        assert!(
            bad_env.validate().is_err(),
            "env name {name:?} must be refused"
        );
    }
}

// #11098: the shortcut "pass the caller's environment through" fails here.
#[test]
fn daemon_env_carries_the_base_and_the_allowlist_only() {
    let mut desc = plain_descriptor();
    desc.env_passthrough = vec!["TRUSTY_DATA_DIR".into()];
    let caller = |name: &str| {
        matches!(
            name,
            "HOME"
                | "TRUSTY_SANDBOX"
                | "TRUSTY_DATA_DIR"
                | "GITHUB_TOKEN"
                | "TRUSTY_EXIT_WITH_PARENT"
        )
        .then(|| std::ffi::OsString::from(format!("v-{name}")))
    };
    let env = start::daemon_env(&desc, caller);
    let names: Vec<&str> = env.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["HOME", "TRUSTY_SANDBOX", "TRUSTY_DATA_DIR"]);
    assert_eq!(env[0].1, "v-HOME");
}

#[test]
fn argv_match_needs_every_liveness_arg_as_a_whole_argument() {
    let cmdline = b"/bin/trusty-memory\0serve\0--foreground\0";
    let want = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    assert!(liveness::argv_matches(
        cmdline,
        &want(&["serve", "--foreground"])
    ));
    assert!(liveness::argv_matches(cmdline, &[]), "empty matches all");
    // A bridge of the same binary does not carry `--foreground`.
    let bridge = b"/bin/trusty-memory\0serve\0--stdio\0";
    assert!(!liveness::argv_matches(bridge, &want(&["--foreground"])));
    // Whole arguments only, and never argv[0].
    assert!(!liveness::argv_matches(cmdline, &want(&["--fore"])));
    assert!(!liveness::argv_matches(
        cmdline,
        &want(&["/bin/trusty-memory"])
    ));
}

#[test]
fn status_parse_reads_state_and_effective_uid() {
    let status = "Name:\td\nState:\tS (sleeping)\nUid:\t1000\t1001\t1000\t1000\n";
    assert_eq!(liveness::parse_status(status), Some(('S', 1001)));
    assert_eq!(liveness::parse_status("State:\tZ (zombie)\n"), None);
}
