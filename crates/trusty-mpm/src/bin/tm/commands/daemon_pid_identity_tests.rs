//! #9034 regression tests for daemon pid identity. These run the REAL
//! production predicate against real child processes — no injected closure.

use super::*;

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_string()).collect()
}

/// Kills and reaps its child on drop, so a failing assertion leaves no
/// process behind.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
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
fn classify_process_daemon_word_after_the_subcommand_is_not_daemon() {
    // #9034: `daemon` counts only as the subcommand, never as a later
    // argument — `tm stop` must not kill a build or a doctor run.
    let lease = argv(&[
        "tm",
        "build-lease",
        "--wait-secs",
        "1800",
        "--",
        "cargo",
        "test",
        "-p",
        "trusty-mpm",
        "daemon",
    ]);
    assert_eq!(classify_process("tm", &lease), PidIdentity::NotDaemon);
    assert_eq!(
        classify_process("tm", &argv(&["tm", "doctor", "daemon"])),
        PidIdentity::NotDaemon
    );
    assert_eq!(
        classify_process("tm", &argv(&["tm", "--", "daemon"])),
        PidIdentity::NotDaemon
    );
    let global = argv(&["tm", "--url", "http://127.0.0.1:7880", "daemon"]);
    assert_eq!(classify_process("tm", &global), PidIdentity::Daemon);
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
    let child = KillOnDrop(
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep"),
    );
    let identity = wait_for_identity(child.0.id(), PidIdentity::NotDaemon);
    assert_ne!(identity, PidIdentity::Daemon, "sleep is not a tm daemon");
}

#[test]
fn find_daemon_pids_finds_a_tm_daemon_process() {
    // A process named `tm` whose argv is exactly `tm daemon`: a copy of bash
    // running the script `./daemon`, which blocks in the `read` builtin on a
    // piped stdin — no child process. This is what `tm stop` must find.
    let tmp = tempfile::tempdir().expect("tempdir");
    let tm = tmp.path().join("tm");
    std::fs::copy("/bin/bash", &tm).expect("copy bash as tm");
    std::fs::write(tmp.path().join("daemon"), "read -r _\n").expect("write script");
    let child = KillOnDrop(
        std::process::Command::new(&tm)
            .arg("daemon")
            .current_dir(tmp.path())
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("spawn fake tm daemon"),
    );
    let pid = child.0.id();
    let identity = wait_for_identity(pid, PidIdentity::Daemon);
    let found = super::super::daemon::find_daemon_pids().contains(&pid);
    drop(child);
    assert_eq!(identity, PidIdentity::Daemon, "`tm … daemon` is a daemon");
    assert!(found, "find_daemon_pids must list a running tm daemon");
}
