//! #9034 regression tests for daemon pid identity. These run the REAL
//! production predicate against real child processes — no injected closure.

use super::*;

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_string()).collect()
}

/// Poll `pid_identity(pid)` until it reports `want` or 5 s pass; a just
/// spawned child can still carry the parent's argv until its `exec` lands.
fn wait_for_identity(pid: u32, want: PidIdentity) -> PidIdentity {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let got = pid_identity(pid);
        if got == want || std::time::Instant::now() > deadline {
            return got;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn classify_process_daemon() {
    let cmd = argv(&["/x/trusty-mpm", "daemon", "--addr", "127.0.0.1:7880"]);
    assert_eq!(classify_process("trusty-mpm", &cmd), PidIdentity::Daemon);
    assert_eq!(
        classify_process("tm", &argv(&["tm", "daemon"])),
        PidIdentity::Daemon
    );
}

#[test]
fn classify_process_cli_is_not_daemon() {
    assert_eq!(
        classify_process("tm", &argv(&["tm", "status"])),
        PidIdentity::NotDaemon
    );
    assert_eq!(
        classify_process("sleep", &argv(&["sleep", "daemon"])),
        PidIdentity::NotDaemon
    );
}

#[test]
fn classify_process_empty_argv_is_unknown() {
    // #9034: an unreadable argv is never a verdict.
    assert_eq!(classify_process("tm", &[]), PidIdentity::Unknown);
}

#[test]
fn real_pid_identity_reads_argv_for_this_process() {
    // #9034 HIGH: `ProcessRefreshKind::nothing()` left `cmd()` empty for every
    // process, so no pid could ever be identified as a daemon.
    let me = std::process::id();
    let target = Pid::from_u32(me);
    let sys = refresh_with_cmd(ProcessesToUpdate::Some(&[target]));
    let (_, cmd) = name_and_cmd(sys.process(target).expect("this process is listed"));
    assert!(!cmd.is_empty(), "argv must be loaded for this process");
    assert_ne!(pid_identity(me), PidIdentity::Unknown);
}

#[test]
fn real_pid_identity_never_calls_sleep_a_daemon() {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let identity = wait_for_identity(child.id(), PidIdentity::NotDaemon);
    let _ = child.kill();
    let _ = child.wait();
    assert_ne!(identity, PidIdentity::Daemon, "sleep is not a tm daemon");
}

#[test]
fn find_daemon_pids_finds_a_tm_daemon_process() {
    // A process named `tm` with a `daemon` argument: a copy of bash run as
    // `tm -c 'sleep 30; :' daemon`. This is what `tm stop` must find.
    let tmp = tempfile::tempdir().expect("tempdir");
    let tm = tmp.path().join("tm");
    std::fs::copy("/bin/bash", &tm).expect("copy bash as tm");
    let mut child = std::process::Command::new(&tm)
        .args(["-c", "sleep 30; :", "daemon"])
        .spawn()
        .expect("spawn fake tm daemon");
    let pid = child.id();
    let identity = wait_for_identity(pid, PidIdentity::Daemon);
    let found = super::super::daemon::find_daemon_pids().contains(&pid);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(identity, PidIdentity::Daemon, "`tm … daemon` is a daemon");
    assert!(found, "find_daemon_pids must list a running tm daemon");
}
