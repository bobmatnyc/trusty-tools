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
        let mut bad_key = ok.clone();
        bad_key.instance_env = vec![(name.into(), None)];
        assert!(bad_key.validate().is_err(), "instance name {name:?}");
    }
    let mut dup = ok.clone();
    dup.instance_env = vec![("K".into(), None), ("K".into(), Some("v".into()))];
    assert!(dup.validate().is_err(), "a duplicate instance name");
    let mut nul_value = ok.clone();
    nul_value.instance_env = vec![("K".into(), Some("a\0b".into()))];
    assert!(nul_value.validate().is_err(), "a NUL instance value");
}

// #11098: without a default key a sandbox daemon reads as production.
#[test]
fn descriptor_pins_the_data_dir_override_as_its_default_instance_key() {
    let name = crate::data_dir::DATA_DIR_OVERRIDE_ENV;
    assert_eq!(
        plain_descriptor().instance_env,
        vec![(name.to_string(), std::env::var_os(name))]
    );
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
    desc.instance_env = Vec::new();
    let env = start::daemon_env(&desc, caller);
    let names: Vec<&str> = env.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["HOME", "TRUSTY_SANDBOX", "TRUSTY_DATA_DIR"]);
    assert_eq!(env[0].1, "v-HOME");

    // #11098: the instance values replace the caller's, and `None` unsets.
    desc.instance_env = vec![
        ("TRUSTY_DATA_DIR".into(), Some("/inst".into())),
        ("TRUSTY_SANDBOX".into(), None),
        ("TC_KEY".into(), Some("k".into())),
    ];
    let env = start::daemon_env(&desc, caller);
    let want: Vec<(String, std::ffi::OsString)> = [
        ("HOME", "v-HOME"),
        ("TRUSTY_DATA_DIR", "/inst"),
        ("TC_KEY", "k"),
    ]
    .iter()
    .map(|(n, v)| (n.to_string(), (*v).into()))
    .collect();
    assert_eq!(env, want);
}

#[test]
fn environ_match_needs_every_instance_value_and_unset_equals_unset() {
    let environ = b"HOME=/h\0TRUSTY_DATA_DIR_OVERRIDE=/a\0EMPTY=\0";
    let want = |pairs: &[(&str, Option<&str>)]| -> Vec<(String, Option<std::ffi::OsString>)> {
        pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.map(Into::into)))
            .collect()
    };
    let o = "TRUSTY_DATA_DIR_OVERRIDE";
    assert!(liveness::environ_matches(
        environ,
        &want(&[(o, Some("/a"))])
    ));
    assert!(liveness::environ_matches(environ, &[]), "empty matches all");
    // A sibling: another override, or none where this instance has one.
    assert!(!liveness::environ_matches(
        environ,
        &want(&[(o, Some("/b"))])
    ));
    assert!(!liveness::environ_matches(environ, &want(&[(o, None)])));
    assert!(!liveness::environ_matches(
        b"HOME=/h\0",
        &want(&[(o, Some("/a"))])
    ));
    assert!(liveness::environ_matches(b"HOME=/h\0", &want(&[(o, None)])));
    // Empty is a value, distinct from unset; a name prefix is not the name.
    assert!(liveness::environ_matches(
        environ,
        &want(&[("EMPTY", Some(""))])
    ));
    assert!(!liveness::environ_matches(
        environ,
        &want(&[("EMPTY", None)])
    ));
    assert!(liveness::environ_matches(environ, &want(&[("HOM", None)])));
    // Every value must hold, not one of them.
    let both = want(&[("HOME", Some("/h")), (o, Some("/b"))]);
    assert!(!liveness::environ_matches(environ, &both));
}

// #11098: a read error other than "vanished" or "another uid's entry" once
// read a live daemon as not running.
#[test]
fn read_error_is_skipped_only_for_a_vanished_or_foreign_entry() {
    let me = std::process::id();
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    let os = std::io::Error::from_raw_os_error;
    assert!(liveness::skippable(&os(libc::ENOENT), me, euid));
    assert!(liveness::skippable(&os(libc::ESRCH), me, euid));
    for errno in [libc::EMFILE, libc::ENFILE, libc::ENOMEM, libc::EIO] {
        assert!(!liveness::skippable(&os(errno), me, euid), "errno {errno}");
    }
    // EACCES on this uid's own entry is not hidepid at work: fail closed.
    assert!(!liveness::skippable(&os(libc::EACCES), me, euid));
    // On another uid's entry (pid 1 is root's) it is.
    if euid != 0 {
        assert!(liveness::skippable(&os(libc::EACCES), 1, euid));
    }
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
