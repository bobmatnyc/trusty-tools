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
    // See #11085 (#11123 deferred LOW): both reads of the variable happen under
    // ENV_LOCK, so a locked writer in another test cannot change it between them.
    let _env = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let proc = ProcessTable::system();
    assert!(liveness::skippable(&os(libc::ENOENT), &proc, me, euid));
    assert!(liveness::skippable(&os(libc::ESRCH), &proc, me, euid));
    for errno in [libc::EMFILE, libc::ENFILE, libc::ENOMEM, libc::EIO] {
        assert!(
            !liveness::skippable(&os(errno), &proc, me, euid),
            "errno {errno}"
        );
    }
    // EACCES on this uid's own entry is not hidepid at work: fail closed.
    assert!(!liveness::skippable(&os(libc::EACCES), &proc, me, euid));
    // On another uid's entry (pid 1 is root's) it is.
    if euid != 0 {
        assert!(liveness::skippable(&os(libc::EACCES), &proc, 1, euid));
    }
}

// #11085: every entry point without `_with_table` scans the real `/proc`.
#[test]
fn the_system_process_table_is_proc() {
    assert_eq!(ProcessTable::system().root(), std::path::Path::new("/proc"));
    assert_eq!(ProcessTable::default(), ProcessTable::system());
    let fake = ProcessTable::at("/tmp/fake-proc");
    assert_eq!(fake.root(), std::path::Path::new("/tmp/fake-proc"));
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
    let status = b"Name:\td\nState:\tS (sleeping)\nUid:\t1000\t1001\t1000\t1000\n";
    assert_eq!(liveness::parse_status(status), Some(('S', 1001)));
    let zombie = b"State:\tZ (zombie)\nUid:\t1\t1\t1\t1\n";
    assert_eq!(liveness::parse_status(zombie), Some(('Z', 1)));
    assert_eq!(liveness::parse_status(b"State:\tZ (zombie)\n"), None);
    // #11098: a non-UTF-8 `comm` in `Name:` is never decoded.
    let bad_name = b"Name:\tlc-\xff\xfe-bad\nState:\tS (sleeping)\nUid:\t7\t7\t7\t7\n";
    assert_eq!(liveness::parse_status(bad_name), Some(('S', 7)));
    // A non-UTF-8 `State:` value is unparsable, not a guess.
    assert_eq!(
        liveness::parse_status(b"State:\t\xff\nUid:\t7\t7\t7\t7\n"),
        None
    );
}

// #11098: an unparsable status body was skipped as "not a match", so a live
// daemon could read as not running.
#[test]
fn unparsable_status_fails_the_scan_instead_of_skipping_the_pid() {
    let bodies: [&[u8]; 3] = [b"", b"Name:\td\n", b"State:\tS (sleeping)\nUid:\tx\n"];
    for body in bodies {
        match liveness::status_admits(body, 42, 1000) {
            Err(LifecycleError::Io { context, .. }) => {
                assert_eq!(context, "/proc/42/status: unparsable");
            }
            other => panic!("unparsable status {body:?} must fail closed, got {other:?}"),
        }
    }
    let live = b"State:\tS (sleeping)\nUid:\t1000\t1000\t1000\t1000\n";
    assert!(matches!(liveness::status_admits(live, 42, 1000), Ok(true)));
    assert!(matches!(liveness::status_admits(live, 42, 1001), Ok(false)));
    let zombie = b"State:\tZ (zombie)\nUid:\t1000\t1000\t1000\t1000\n";
    assert!(matches!(
        liveness::status_admits(zombie, 42, 1000),
        Ok(false)
    ));
}

// ─── #11085: instance_path_args ─────────────────────────────────────────────

#[test]
fn descriptor_rejects_bad_instance_path_args() {
    let ok = plain_descriptor();
    assert!(
        ok.instance_path_args.is_empty(),
        "nothing opts in by default"
    );
    let with = |flag: &str, path: &str| {
        let mut desc = ok.clone();
        desc.instance_path_args = vec![(flag.into(), path.into())];
        desc.validate()
    };
    with("--data-dir", "/srv/data").expect("a flag and an absolute path");
    for flag in ["", "--data-dir=x", "--a\0b"] {
        assert!(with(flag, "/srv/data").is_err(), "flag {flag:?}");
    }
    assert!(with("--data-dir", "data").is_err(), "a relative path");
    assert!(with("--data-dir", "/srv/\0").is_err(), "a NUL in the path");
    let mut dup = ok.clone();
    dup.instance_path_args = vec![("-d".into(), "/a".into()), ("-d".into(), "/b".into())];
    assert!(dup.validate().is_err(), "a duplicate flag");
}

/// A `/proc/<pid>/cmdline` body for `argv`, with an `argv[0]`.
fn cmdline(argv: &[&str]) -> Vec<u8> {
    let mut out = b"/opt/bin/daemon\0".to_vec();
    for arg in argv {
        out.extend_from_slice(arg.as_bytes());
        out.push(0);
    }
    out
}

/// Two existing directories under a temp dir, `own` and `other`, as strings.
struct Dirs {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    own: String,
    other: String,
}

impl Dirs {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical tempdir");
        for name in ["own", "other"] {
            std::fs::create_dir(root.join(name)).expect("create dir");
        }
        let at = |name: &str| root.join(name).to_str().expect("utf-8").to_string();
        let (own, other) = (at("own"), at("other"));
        Self {
            _tmp: tmp,
            root,
            own,
            other,
        }
    }

    fn wanted(&self) -> Vec<(String, PathBuf)> {
        vec![("--data-dir".to_string(), PathBuf::from(&self.own))]
    }
}

// #11085: the AC6 residual — `--data-dir X` with the data-dir variable unset
// read as the default instance.
#[test]
fn path_flag_marks_a_sibling_only_when_every_value_names_another_path() {
    use instance::PathVerdict::{Other, Same, Undecided};
    let d = Dirs::new();
    let want = d.wanted();
    let verdict = |argv: &[&str]| instance::path_verdict(&cmdline(argv), &want);
    let other_eq = format!("--data-dir={}", d.other);
    assert_eq!(
        verdict(&["start", "--data-dir", &d.other]),
        Other,
        "`<flag> <value>`"
    );
    assert_eq!(verdict(&["start", &other_eq]), Other, "`<flag>=<value>`");
    assert_eq!(
        verdict(&["--data-dir", &d.other, "--data-dir", &d.other]),
        Other,
        "every value names another path"
    );
    // This instance, by text, by components and through a symlink.
    let own_eq = format!("--data-dir={}", d.own);
    let own_slash = format!("{}//", d.own);
    let alias = d.root.join("alias");
    std::os::unix::fs::symlink(&d.own, &alias).expect("symlink");
    let alias = alias.to_str().expect("utf-8").to_string();
    for own in [d.own.as_str(), &own_slash, &alias] {
        assert_eq!(verdict(&["start", "--data-dir", own]), Same, "{own}");
    }
    assert_eq!(verdict(&["start", &own_eq]), Same);
    // One value naming this instance beside one naming another decides nothing.
    assert_eq!(
        verdict(&["--data-dir", &d.other, "--data-dir", &d.own]),
        Undecided("--data-dir")
    );
    // No flag, a longer flag, or the flag after `--` constrains nothing.
    assert_eq!(verdict(&["start", "--foreground"]), Same);
    let longer = format!("--data-dir-x={}", d.other);
    assert_eq!(verdict(&["--data-dir-x", &d.other, &longer]), Same);
    assert_eq!(verdict(&["start", "--", "--data-dir", &d.other]), Same);
    // A descriptor that does not opt in sees no sibling.
    let argv = cmdline(&["start", "--data-dir", &d.other]);
    assert_eq!(instance::path_verdict(&argv, &[]), Same);
    // An existing directory is not one that does not exist.
    let missing_own = vec![("--data-dir".to_string(), d.root.join("missing"))];
    assert_eq!(instance::path_verdict(&argv, &missing_own), Other);
}

// #11085: a value the matcher cannot rule out is undecided — the scan counts
// it as this instance and refuses; a stop by pid refuses to signal it.
#[test]
fn path_flag_counts_every_undecidable_value_as_undecided() {
    use instance::PathVerdict::{Other, Undecided};
    let d = Dirs::new();
    let want = d.wanted();
    let verdict = |argv: &[&str]| instance::path_verdict(&cmdline(argv), &want);
    let undecided = Undecided("--data-dir");
    assert_eq!(
        verdict(&["start", "--data-dir"]),
        undecided,
        "a flag with no value"
    );
    // Both resolve from this process's cwd, which is not the daemon's.
    assert_eq!(verdict(&["--data-dir", "."]), undecided, "a relative value");
    assert_eq!(
        verdict(&["--data-dir=.."]),
        undecided,
        "a relative `=` value"
    );
    let missing = d.root.join("missing").to_str().expect("utf-8").to_string();
    assert_eq!(
        verdict(&["--data-dir", &missing]),
        undecided,
        "an unresolvable value"
    );
    // Any flag naming another path wins over an undecided one.
    let two = vec![
        ("--data-dir".to_string(), PathBuf::from(&d.own)),
        ("-d".to_string(), PathBuf::from(&d.own)),
    ];
    let argv = cmdline(&["--data-dir", ".", "-d", &d.other]);
    assert_eq!(instance::path_verdict(&argv, &two), Other);
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return; // root resolves through a mode-000 directory
    }
    let locked = d.root.join("locked");
    std::fs::create_dir_all(locked.join("inner")).expect("locked dir");
    let inner = locked.join("inner");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let behind_lock = instance::names_path(inner.as_os_str().as_encoded_bytes(), Path::new(&d.own));
    let wanted_locked = instance::names_path(d.other.as_bytes(), &inner);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert_eq!(
        behind_lock, None,
        "a value that cannot be resolved (EACCES)"
    );
    assert_eq!(
        wanted_locked, None,
        "an instance path that cannot be resolved (EACCES)"
    );
}
