//! Tests for [`super::write_executable`] (#10248).

use super::*;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[test]
fn a_stub_runs_and_is_mode_0755() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = dir.path().join("stub");
    write_executable(&stub, "#!/bin/sh\necho stub-ran \"$1\"\n").expect("write stub");
    let mode = std::fs::metadata(&stub).expect("stat").permissions().mode();
    assert_eq!(mode & 0o777, STUB_MODE);
    let out = Command::new(&stub).arg("x").output().expect("exec stub");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "stub-ran x\n");
}

#[test]
fn rewriting_a_stub_replaces_it_and_leaves_no_staging_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = dir.path().join("stub");
    write_executable(&stub, "#!/bin/sh\necho one\n").expect("first write");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    write_executable(&stub, b"#!/bin/sh\necho two\n").expect("second write");
    assert_eq!(
        std::fs::read_to_string(&stub).expect("read"),
        "#!/bin/sh\necho two\n"
    );
    let mode = std::fs::metadata(&stub).expect("stat").permissions().mode();
    assert_eq!(mode & 0o777, STUB_MODE);
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("stub")]);
}

#[test]
fn a_missing_parent_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stub = dir.path().join("absent").join("stub");
    let err = write_executable(&stub, "#!/bin/sh\n").expect_err("no parent dir");
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert!(!stub.exists());
}

/// The #10248 race, forced by load: four threads write and exec stubs while
/// four more spawn `/bin/true` without pause. Writing the stubs in-process
/// failed about one exec in seven with ETXTBSY on Linux; through
/// [`write_executable`] none may fail.
///
/// The body runs in a child process of this test binary, so its spawn load
/// cannot reach a sibling test, and no sibling test's write fd can reach it.
#[test]
fn stubs_exec_while_siblings_spawn_without_etxtbsy() {
    const CHILD_RUN: &str = "TRUSTY_COMMON_TEST_10248_STRESS";
    if std::env::var_os(CHILD_RUN).is_none() {
        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, rest)| rest);
        let name = format!("{module}::stubs_exec_while_siblings_spawn_without_etxtbsy");
        let out = Command::new(std::env::current_exe().expect("test binary"))
            .args([
                name.as_str(),
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(CHILD_RUN, "1")
            .output()
            .expect("re-run this test as a child process");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "child run failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let stop = Arc::new(AtomicBool::new(false));
    let spawners: Vec<_> = (0..4)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = Command::new("/bin/true").status();
                }
            })
        })
        .collect();
    let writers: Vec<_> = (0..4)
        .map(|t| {
            let stub = dir.path().join(format!("stub-{t}"));
            std::thread::spawn(move || {
                for n in 0..150 {
                    write_executable(&stub, format!("#!/bin/sh\nexit 0\n# {n}\n"))
                        .expect("write stub");
                    let status = Command::new(&stub).status();
                    assert!(
                        status.as_ref().is_ok_and(|s| s.success()),
                        "exec {n} of {}: {status:?}",
                        stub.display()
                    );
                }
            })
        })
        .collect();
    let results: Vec<_> = writers.into_iter().map(|h| h.join()).collect();
    stop.store(true, Ordering::Relaxed);
    for h in spawners {
        h.join().expect("spawner thread");
    }
    for r in results {
        r.expect("writer thread");
    }
}
