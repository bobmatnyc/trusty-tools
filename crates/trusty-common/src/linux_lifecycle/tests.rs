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
