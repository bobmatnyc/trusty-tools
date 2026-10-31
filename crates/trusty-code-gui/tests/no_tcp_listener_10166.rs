//! The GUI's bridge holds no TCP LISTEN socket (#10166).
//!
//! Why: ADR-0032 makes trusty-console the only TCP surface, and #10166 retires
//! the last listener on the trusty-code side — the loopback port the webview
//! bridge used to bind. `scripts/check_no_tcp_listeners.sh` is textual, so it
//! cannot see a bind reached through a helper or a macro; this test asks the OS
//! what the process actually holds after starting the bridge.
//!
//! What: one test that first proves the probe can see a listener (a throwaway
//! ephemeral-port bind in this process), then starts the bridge exactly as
//! `trusty_code_gui::run` does — `bridge::serve::start(None)` under
//! `tauri::async_runtime::block_on` — and asserts the process holds zero TCP
//! LISTEN sockets while the bridge is alive. One test, not two: the control's
//! listener would otherwise race the real assertion inside the same process.
//!
//! The probe is copied from `crates/trusty-code/tests/daemon_no_tcp_6637.rs`
//! rather than shared: a cross-crate test dependency would weld two compile
//! graphs (`scripts/check_dev_dep_edges.sh`). The subject pid is this test
//! process, alive by construction, so the copy drops that file's `kill(pid, 0)`
//! liveness check and its `libc` dependency.
//!
//! Test: `the_bridge_binds_no_tcp_listener`.

/// Every TCP socket this process holds in LISTEN state, one line each.
///
/// Why: an empty list is evidence only when the scan demonstrably ran and could
/// read the tables, so each failure to do so panics instead of returning empty.
/// What: macOS runs `lsof` (a missing `lsof` or any stderr output fails);
/// Linux matches this process's socket inodes against the LISTEN (`0A`) rows of
/// `/proc/self/net/tcp{,6}`.
/// Test: the positive control inside `the_bridge_binds_no_tcp_listener`.
#[cfg(target_os = "macos")]
fn tcp_listeners() -> Vec<String> {
    let pid = std::process::id();
    let out = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-iTCP", "-sTCP:LISTEN"])
        .output()
        .expect("lsof must run on macOS");
    assert!(
        out.stderr.is_empty(),
        "lsof failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1) // the header
        .map(str::to_owned)
        .collect()
}

#[cfg(target_os = "linux")]
fn tcp_listeners() -> Vec<String> {
    let mut inodes = std::collections::HashSet::new();
    for entry in std::fs::read_dir("/proc/self/fd").expect("read this process's fd table") {
        let Ok(target) = std::fs::read_link(entry.expect("fd entry").path()) else {
            continue;
        };
        let target = target.to_string_lossy().into_owned();
        if let Some(inode) = target
            .strip_prefix("socket:[")
            .and_then(|s| s.strip_suffix(']'))
        {
            inodes.insert(inode.to_owned());
        }
    }
    let mut listening = Vec::new();
    for table in ["tcp", "tcp6"] {
        // An unreadable table is a failure, never an empty list.
        let text = std::fs::read_to_string(format!("/proc/self/net/{table}"))
            .unwrap_or_else(|e| panic!("read /proc/self/net/{table}: {e}"));
        for row in text.lines().skip(1) {
            let cols: Vec<&str> = row.split_whitespace().collect();
            // local_address is col 1, st col 3, inode col 9.
            if cols.len() > 9 && cols[3] == "0A" && inodes.contains(cols[9]) {
                listening.push(format!("{table} {}", cols[1]));
            }
        }
    }
    listening
}

/// Why (#10166): the webview bridge must reach the daemon without opening any
/// TCP port, and an empty probe must not be able to pass for that.
/// Test: this is the test.
#[test]
fn the_bridge_binds_no_tcp_listener() {
    // Positive control: the probe sees a listener this process holds. Without
    // it, a probe that always returned empty would pass the assertion below.
    let control = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind the control");
    let port = control.local_addr().expect("control addr").port();
    let seen = tcp_listeners();
    assert!(
        seen.iter()
            .any(|row| row.contains(&format!(":{port}")) || row.contains(&format!(":{port:04X}"))),
        "the probe must see the control listener on port {port}: {seen:?}"
    );
    drop(control);
    assert!(
        tcp_listeners().is_empty(),
        "the dropped control must leave no listener behind"
    );

    // Start the bridge the way `trusty_code_gui::run` does, and keep it alive
    // for the scan.
    let bridge = tauri::async_runtime::block_on(trusty_code_gui::bridge::serve::start(None))
        .expect("the bridge must start");
    let listening = tcp_listeners();
    assert!(
        listening.is_empty(),
        "the GUI bridge must hold no TCP LISTEN socket (#10166): {listening:?}"
    );
    drop(bridge);
}
