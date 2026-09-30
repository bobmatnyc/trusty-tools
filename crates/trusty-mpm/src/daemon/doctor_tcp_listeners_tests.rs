//! Tests for the `tcp_listeners` doctor row (#8926).

use super::probe::{parse_proc_net_tcp, procfs_listening_sockets};
use super::*;

/// A console row, one temporary row, and one source-only row.
const ALLOW: &str = "# comment\n\
permanent\ttrusty-console\ttrusty-console\tcrates/trusty-console/src/\tADR-0032\tconsole\n\
temporary\ttrusty-mpm\ttrusty-mpm,tm\tcrates/trusty-mpm/src/daemon/mod.rs\t#6288\ttm daemon\n\
source-only\ttrusty-code\t-\tcrates/trusty-code/src/serve/http.rs\t#6637\topt-in\n";

fn allow() -> Vec<AllowEntry> {
    parse_allowlist(ALLOW).expect("fixture allowlist parses")
}

fn sock(pid: u32, process: &str, port: u16) -> ListenSocket {
    ListenSocket {
        pid,
        process: process.to_string(),
        addr: "127.0.0.1".to_string(),
        port,
    }
}

fn listening(socks: Vec<ListenSocket>) -> Result<ProbeReport, String> {
    Ok(ProbeReport {
        listeners: socks,
        uninspected: Vec::new(),
    })
}

#[test]
fn only_the_console_listening_is_ok() {
    let row = classify(listening(vec![sock(10, "trusty-console", 7788)]), &allow());
    assert_eq!(row.name, "tcp_listeners");
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(
        row.message
            .contains("trusty-console (pid 10) 127.0.0.1:7788 allowed (ADR-0032)")
    );

    let none = classify(listening(Vec::new()), &allow());
    assert_eq!(none.status, CheckStatus::Ok, "{}", none.message);
    assert!(none.message.contains("no trusty-* process listens on TCP"));
}

#[test]
fn a_non_trusty_listener_is_ignored() {
    let row = classify(listening(vec![sock(5, "postgres", 5432)]), &allow());
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(!row.message.contains("postgres"));
}

#[test]
fn a_temporary_listener_warns_naming_its_issue() {
    let row = classify(
        listening(vec![sock(10, "trusty-console", 7788), sock(11, "tm", 7880)]),
        &allow(),
    );
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(
        row.message
            .contains("tm (pid 11) 127.0.0.1:7880 TEMPORARY until #6288"),
        "{}",
        row.message
    );
}

#[test]
fn a_listener_no_live_row_names_fails() {
    // tagent has no row at all; trusty-code has only a source-only row, which
    // never excuses a live listener.
    for process in ["tagent", "trusty-code"] {
        let row = classify(
            listening(vec![sock(11, "tm", 7880), sock(12, process, 8080)]),
            &allow(),
        );
        assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
        assert!(
            row.message.contains(&format!(
                "{process} (pid 12) 127.0.0.1:8080 is not allowlisted"
            )),
            "{}",
            row.message
        );
    }
}

#[test]
fn a_failed_probe_is_unknown_never_ok() {
    let row = classify(
        Err("proc_listallpids: Operation not permitted".into()),
        &allow(),
    );
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
    assert!(
        row.message
            .contains("could not list TCP listeners: proc_listallpids")
    );
}

#[test]
fn an_uninspected_trusty_process_is_unknown_never_ok() {
    let report = ProbeReport {
        listeners: vec![sock(10, "trusty-console", 7788)],
        uninspected: vec![Uninspected {
            pid: 13,
            process: "trusty-search".into(),
            error: "Permission denied".into(),
        }],
    };
    let row = classify(Ok(report.clone()), &allow());
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
    assert!(
        row.message
            .contains("could not read the sockets of trusty-search (pid 13)")
    );

    // A FAIL still outranks the gap.
    let mut failing = report;
    failing.listeners.push(sock(12, "tagent", 8080));
    assert_eq!(classify(Ok(failing), &allow()).status, CheckStatus::Fail);
}

#[test]
fn malformed_allowlist_rows_are_refused() {
    for (text, needle) in [
        (
            "permanent\tx\tx\tcrates/x/\tADR-0032\n",
            "6 tab-separated fields",
        ),
        (
            "forever\tx\tx\tcrates/x/\tADR-0032\tr\n",
            "unknown kind `forever`",
        ),
        ("temporary\tx\t-\tcrates/x/\t#1\tr\n", "names no process"),
    ] {
        let err = parse_allowlist(text).expect_err(text);
        assert!(err.contains(needle), "{err}");
    }
}

#[test]
fn embedded_allowlist_keeps_the_console_as_the_only_permanent_row() {
    let rows = parse_allowlist(ALLOWLIST_TSV).expect("embedded allowlist parses");
    let permanent: Vec<_> = rows
        .iter()
        .filter(|r| r.kind == AllowKind::Permanent)
        .collect();
    assert_eq!(permanent.len(), 1);
    assert_eq!(permanent[0].crate_name, "trusty-console");
    assert_eq!(permanent[0].issue, "ADR-0032");
    for r in rows.iter().filter(|r| r.kind != AllowKind::Permanent) {
        assert!(
            r.issue.starts_with('#'),
            "{} cites {}",
            r.crate_name,
            r.issue
        );
    }
}

#[test]
fn trusty_process_names_match_prefix_and_short_names() {
    for name in ["trusty-console", "trusty-search", "tm", "tagent", "tcode"] {
        assert!(is_trusty_process(name), "{name}");
    }
    for name in ["postgres", "trusty", "tmux", "node"] {
        assert!(!is_trusty_process(name), "{name}");
    }
}

const TCP_TABLE: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:1EC8 00000000:0000 0A 00000000:00000000 00:00000000 00000000   501        0 111 1 0000000000000000 100 0 0 10 0\n   1: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000   501        0 222 1 0000000000000000 100 0 0 10 0\n   2: 0100007F:D431 0100007F:1EC8 01 00000000:00000000 00:00000000 00000000   501        0 333 1 0000000000000000 20 4 30 10 -1\n";

#[test]
fn parse_proc_net_tcp_decodes_ipv4_and_ipv6() {
    let v4 = parse_proc_net_tcp(TCP_TABLE).expect("parses");
    assert_eq!(v4.get(&111), Some(&("127.0.0.1".to_string(), 7880)));
    assert_eq!(v4.get(&222), Some(&("127.0.0.1".to_string(), 8080)));
    assert!(
        !v4.contains_key(&333),
        "an ESTABLISHED row is not a listener"
    );

    let loopback6 = [0u32, 0, 0, 1u32.to_be()]
        .iter()
        .map(|w| format!("{w:08X}"))
        .collect::<String>();
    let tcp6 = format!(
        "  sl  local_address remote_address st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n   0: {loopback6}:1EC6 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000 501 0 444 1\n"
    );
    let v6 = parse_proc_net_tcp(&tcp6).expect("parses");
    assert_eq!(v6.get(&444), Some(&("::1".to_string(), 7878)));
}

#[test]
fn parse_proc_net_tcp_refuses_a_malformed_row() {
    let err = parse_proc_net_tcp("header\n   0: 0100007F 0A\n").expect_err("short row");
    assert!(err.contains("unparseable /proc/net/tcp row"), "{err}");
}

/// A fake `/proc` with pid dirs whose `exe` and `fd/*` are symlinks.
#[cfg(unix)]
fn fake_proc(root: &std::path::Path, pid: u32, exe: &str, inodes: &[u64]) {
    use std::os::unix::fs::symlink;
    let dir = root.join(pid.to_string());
    std::fs::create_dir_all(dir.join("fd")).unwrap();
    symlink(format!("/usr/local/bin/{exe}"), dir.join("exe")).unwrap();
    for (fd, inode) in inodes.iter().enumerate() {
        symlink(
            format!("socket:[{inode}]"),
            dir.join("fd").join(fd.to_string()),
        )
        .unwrap();
    }
}

#[cfg(unix)]
#[test]
fn procfs_probe_maps_listen_inodes_to_trusty_pids() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("net")).unwrap();
    std::fs::write(tmp.path().join("net/tcp"), TCP_TABLE).unwrap();
    fake_proc(tmp.path(), 100, "tm", &[111, 333]);
    fake_proc(tmp.path(), 200, "postgres", &[222]);

    let report = procfs_listening_sockets(tmp.path(), is_trusty_process).expect("probe runs");
    assert_eq!(report.listeners, vec![sock(100, "tm", 7880)]);
    assert!(report.uninspected.is_empty());
}

#[cfg(unix)]
#[test]
fn procfs_probe_without_a_tcp_table_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let err = procfs_listening_sockets(tmp.path(), is_trusty_process).expect_err("no net/tcp");
    assert!(err.contains("net/tcp"), "{err}");
}

/// The real probe on this host sees a socket this test process holds.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_host_probe_finds_a_listener_this_test_holds() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = held.local_addr().unwrap().port();
    let pid = std::process::id();
    // The probe keeps any process; the assertion picks this one out.
    let report = super::probe::listening_sockets(|_| true).expect("probe runs on this host");
    assert!(
        report
            .listeners
            .iter()
            .any(|s| s.pid == pid && s.port == port && s.addr == "127.0.0.1"),
        "pid {pid} port {port} missing from {:?}",
        report
            .listeners
            .iter()
            .filter(|s| s.pid == pid)
            .collect::<Vec<_>>()
    );
    drop(held);
}
