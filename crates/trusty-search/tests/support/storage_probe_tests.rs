//! Unit tests for the read-only storage probe (#9993 G6).

use super::*;

fn write(path: &Path, bytes: usize) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, vec![b'x'; bytes]).expect("write fixture file");
}

/// A storage-shaped tree: 1000 + 2345 + 7 = 3352 file bytes.
fn fixture_tree(dir: &Path) {
    write(&dir.join("index.redb"), 1000);
    write(&dir.join("hnsw.usearch"), 2345);
    write(&dir.join("nested").join("hnsw.keys.json"), 7);
}

#[test]
fn snapshot_sums_exact_file_bytes_without_following_symlinks() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let outside = tempfile::tempdir().expect("outside dir");
    fixture_tree(dir.path());
    write(&outside.path().join("big.bin"), 50_000);
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path().join("big.bin"), dir.path().join("link"))
        .expect("symlink");

    let snap = snapshot(dir.path()).expect("snapshot");
    assert_eq!(snap.file_bytes(), 3352);
    let files: Vec<(String, u64)> = snap
        .files()
        .map(|e| (e.rel.display().to_string(), e.len))
        .collect();
    let nested = Path::new("nested").join("hnsw.keys.json");
    assert_eq!(
        files,
        vec![
            ("hnsw.usearch".to_string(), 2345),
            ("index.redb".to_string(), 1000),
            (nested.display().to_string(), 7),
        ]
    );
    assert!(snap.entries.iter().any(|e| e.kind == Kind::Dir));
    #[cfg(unix)]
    assert!(snap
        .entries
        .iter()
        .any(|e| e.kind == Kind::Symlink && e.rel == Path::new("link")));
}

#[test]
fn snapshot_refuses_a_missing_dir_a_file_and_an_empty_dir() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let missing = dir.path().join("missing");
    let error = snapshot(&missing).expect_err("a missing dir must be an error");
    assert!(error.contains(&missing.display().to_string()), "{error}");

    write(&dir.path().join("plain"), 3);
    let error = snapshot(&dir.path().join("plain")).expect_err("a file is not a storage dir");
    assert!(error.contains("not a directory"), "{error}");

    let empty = dir.path().join("empty");
    std::fs::create_dir_all(empty.join("sub")).expect("empty tree");
    let error = snapshot(&empty).expect_err("a tree with no files is an empty index");
    assert!(error.contains("holds no files"), "{error}");
}

#[cfg(unix)]
#[test]
fn snapshot_refuses_an_unreadable_subdirectory() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("scratch dir");
    fixture_tree(dir.path());
    let locked = dir.path().join("nested");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
    let result = snapshot(dir.path());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
    let error = result.expect_err("an unreadable directory must be an error, not a smaller sum");
    assert!(error.contains("read directory"), "{error}");
}

#[test]
fn unchanged_passes_a_read_only_probe_and_fails_on_a_write() {
    let dir = tempfile::tempdir().expect("scratch dir");
    fixture_tree(dir.path());
    let dirs = vec![StorageDir {
        layout: "data_dir",
        path: dir.path().to_path_buf(),
    }];
    let before = snapshots(&dirs).expect("before");
    let probed = snapshots(&dirs).expect("probe walk");
    rss_bytes(std::process::id()).expect("rss read");
    let after = snapshots(&dirs).expect("after");
    assert_eq!(unchanged(&before, &probed), Ok(()));
    assert_eq!(unchanged(&before, &after), Ok(()));

    // The check can fail: a same-length rewrite with a new mtime is caught.
    let redb = dir.path().join("index.redb");
    filetime::set_file_mtime(&redb, filetime::FileTime::from_unix_time(1_000_000, 0))
        .expect("set mtime");
    let touched = snapshots(&dirs).expect("touched");
    let error = unchanged(&before, &touched).expect_err("an mtime change must fail");
    assert!(error.contains("index.redb"), "{error}");

    // A new file is caught too.
    write(&dir.path().join("added"), 1);
    let grown = snapshots(&dirs).expect("grown");
    assert!(unchanged(&before, &grown).is_err());
}

#[test]
fn storage_dirs_reads_both_layouts_and_refuses_neither() {
    let data = tempfile::tempdir().expect("data dir");
    let root = tempfile::tempdir().expect("root");
    let error = storage_dirs(data.path(), "perf/x", root.path()).expect_err("neither layout");
    assert!(error.contains("no storage directory"), "{error}");

    // A colocated dir without index.redb is not a corpus (the daemon's rule).
    std::fs::create_dir_all(root.path().join(COLOCATED_DIR_NAME)).expect("bare dir");
    assert!(storage_dirs(data.path(), "perf/x", root.path()).is_err());

    // The id is sanitized the way the daemon names its directory.
    let legacy = data.path().join("indexes").join("perf_x");
    std::fs::create_dir_all(&legacy).expect("legacy dir");
    write(&root.path().join(COLOCATED_DIR_NAME).join("index.redb"), 10);
    let dirs = storage_dirs(data.path(), "perf/x", root.path()).expect("both layouts");
    assert_eq!(
        dirs,
        vec![
            StorageDir {
                layout: "data_dir",
                path: legacy,
            },
            StorageDir {
                layout: "colocated",
                path: root.path().join(COLOCATED_DIR_NAME),
            },
        ]
    );
}

#[test]
fn rss_reads_a_live_pid_and_refuses_unsupported_or_missing_pids() {
    let own = rss_bytes(std::process::id()).expect("this process has an RSS");
    assert!(own.bytes > 0 && own.bytes.is_multiple_of(1024), "{own:?}");

    let error = rss_bytes_on("windows", std::process::id()).expect_err("unsupported OS");
    assert!(error.contains("unsupported on windows"), "{error}");
    // Above both Linux's pid_max ceiling and macOS's PID_MAX: never a live pid.
    let missing = i32::MAX as u32;
    let error = rss_bytes(missing).expect_err("a missing pid must be an error, not 0");
    assert!(error.contains(&format!("no process {missing}")), "{error}");
    assert!(rss_bytes(0).is_err());
    assert!(rss_bytes(1).is_err());
}

#[test]
fn vmrss_parser_reads_kib_and_refuses_absent_or_zero() {
    let status = "Name:\ttrusty-search\nVmPeak:\t  900 kB\nVmRSS:\t  51234 kB\nThreads:\t9\n";
    assert_eq!(parse_vmrss_kib(status), Ok(51234));
    assert!(parse_vmrss_kib("Name:\tkthreadd\nThreads:\t1\n").is_err());
    assert!(parse_vmrss_kib("VmRSS:\t       0 kB\n").is_err());
    assert!(parse_vmrss_kib("VmRSS:\t   12 MB\n").is_err());
}

#[test]
fn sandbox_pid_requires_the_pidfile_and_a_matching_argv() {
    let sandbox = tempfile::tempdir().expect("sandbox dir");
    let data_dir = sandbox.path().join(SANDBOX_DATA_DIR);
    std::fs::create_dir_all(&data_dir).expect("data dir");
    let socket = data_dir.join("trusty-search.sock");
    let pidfile = sandbox.path().join(SANDBOX_PID_FILE);

    let error = sandbox_pid(&sandbox.path().join("trusty-search.sock"))
        .expect_err("a socket outside <sandbox>/data is refused");
    assert!(error.contains("not in a sandbox"), "{error}");
    let error = sandbox_pid(&socket).expect_err("no pid file");
    assert!(error.contains("no sandbox pid file"), "{error}");
    std::fs::write(&pidfile, "not-a-pid\n").expect("garbage pid file");
    assert!(sandbox_pid(&socket).is_err());
    std::fs::write(&pidfile, "1\n").expect("init pid");
    assert!(sandbox_pid(&socket).is_err());
    // This test process runs without --data-dir: refused.
    std::fs::write(&pidfile, format!("{}\n", std::process::id())).expect("own pid");
    let error = sandbox_pid(&socket).expect_err("a process of another data dir");
    assert!(error.contains("does not run with --data-dir"), "{error}");

    // A process whose argv names the data dir is accepted. `; :` keeps the
    // shell from exec-ing `sleep`, so its own argv stays the one checked.
    let mut child = Command::new("/bin/sh")
        .args(["-c", "sleep 30; :", "sandbox-probe", "--data-dir"])
        .arg(&data_dir)
        .spawn()
        .expect("spawn a stand-in daemon");
    std::fs::write(&pidfile, format!("{}\n", child.id())).expect("child pid");
    let accepted = sandbox_pid(&socket);
    let rss = rss_bytes(child.id());
    child.kill().expect("stop the stand-in");
    child.wait().expect("reap the stand-in");
    assert_eq!(accepted, Ok(child.id()));
    assert!(rss.expect("child rss").bytes > 0);
}

#[test]
fn storage_rows_carry_exact_bytes_and_refuse_an_empty_index() {
    let dir = tempfile::tempdir().expect("scratch dir");
    fixture_tree(dir.path());
    let dirs = vec![StorageDir {
        layout: "data_dir",
        path: dir.path().to_path_buf(),
    }];
    let snaps = snapshots(&dirs).expect("snapshot");
    let rss = RssReading {
        bytes: 4096,
        source: "test",
    };
    let rows = storage_rows(&dirs, &snaps, 8, rss, 42).expect("rows");
    let value = |metric: &str| {
        rows.iter()
            .find(|r| r.metric == metric)
            .and_then(|r| r.value)
            .unwrap_or_else(|| panic!("no row {metric}"))
    };
    assert_eq!(value("file data_dir/index.redb"), 1000.0);
    assert_eq!(value("file data_dir/hnsw.usearch"), 2345.0);
    assert_eq!(value("index storage total"), 3352.0);
    assert_eq!(value("bytes per chunk"), 419.0);
    assert_eq!(value("daemon rss"), 4096.0);
    assert_eq!(rows.len(), 6);
    assert!(rows
        .iter()
        .all(|r| r.dimension == "storage" && r.unit == "bytes" && r.n == Some(1)));

    let error = storage_rows(&dirs, &snaps, 0, rss, 42).expect_err("zero chunks");
    assert!(error.contains("0 chunks"), "{error}");
    let empty = Snapshot {
        dir: dir.path().to_path_buf(),
        entries: vec![Entry {
            rel: PathBuf::from("index.redb"),
            kind: Kind::File,
            len: 0,
            modified: SystemTime::UNIX_EPOCH,
        }],
    };
    let error = storage_rows(&dirs, &[empty], 8, rss, 42).expect_err("zero bytes");
    assert!(error.contains("0 bytes"), "{error}");
}
