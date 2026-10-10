//! Tests for process ancestry (#9070).

use super::*;
use crate::server::grant_fakes::FakeProcs;

fn tree() -> FakeProcs {
    FakeProcs::sample_tree()
}

const CHILD: u32 = 20;
const CHILD_START: StartTime = StartTime(200);

#[test]
fn descendant_check_accepts_child_and_grandchild() {
    let procs = tree();
    assert_eq!(
        is_self_or_descendant(&procs, 20, CHILD, CHILD_START),
        Ok(true)
    );
    assert_eq!(
        is_self_or_descendant(&procs, 30, CHILD, CHILD_START),
        Ok(true)
    );
}

#[test]
fn descendant_check_refuses_sibling_parent_and_unrelated() {
    let procs = tree();
    for pid in [21, 10, 50, 1] {
        assert_eq!(
            is_self_or_descendant(&procs, pid, CHILD, CHILD_START),
            Ok(false),
            "pid {pid}"
        );
    }
}

#[test]
fn descendant_check_fails_closed_on_unreadable_table() {
    let procs = tree();
    procs.set_unreadable();
    assert_eq!(
        is_self_or_descendant(&procs, 30, CHILD, CHILD_START),
        Err(ProcessError::Unreadable { pid: 30 })
    );
    // A pid missing from the table is unreadable too, never "not related".
    let procs = tree();
    assert_eq!(
        is_self_or_descendant(&procs, 99, CHILD, CHILD_START),
        Err(ProcessError::Unreadable { pid: 99 })
    );
}

#[test]
fn descendant_check_refuses_a_parent_loop() {
    let procs = FakeProcs::default();
    procs.add(60, 61, 1).add(61, 60, 1);
    assert_eq!(
        is_self_or_descendant(&procs, 60, CHILD, CHILD_START),
        Err(ProcessError::ChainTooDeep)
    );
}

#[test]
fn linux_stat_parser_reads_parent_and_start_time() {
    // `comm` holds a space and a `)`, so the split must start after the last `)`.
    let mut line = String::from("4242 (we ird) name) S 77");
    for field in 5..=21 {
        line.push_str(&format!(" {field}"));
    }
    line.push_str(" 987654 rest");
    assert_eq!(stat_parent(4242, &line), Ok(77));
    assert_eq!(stat_start_time(4242, &line), Ok(StartTime(987654)));

    let short = "4242 (sh) S 77 1 1";
    assert_eq!(stat_parent(4242, short), Ok(77));
    assert_eq!(
        stat_start_time(4242, short),
        Err(ProcessError::StartTimeUnreadable { pid: 4242 })
    );
    assert_eq!(
        stat_parent(4242, "garbage"),
        Err(ProcessError::Unreadable { pid: 4242 })
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod os_table {
    use super::super::*;

    #[test]
    fn os_table_reads_this_process() {
        let me = std::process::id();
        assert_eq!(
            OsProcessTable.parent(me),
            Ok(std::os::unix::process::parent_id())
        );
        let first = OsProcessTable.start_time(me).expect("own start time");
        assert_eq!(OsProcessTable.start_time(me), Ok(first));
    }

    #[test]
    fn os_table_sees_a_spawned_child_as_a_descendant() {
        let me = std::process::id();
        let my_start = OsProcessTable.start_time(me).expect("own start time");
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let child_pid = child.id();
        let child_start = OsProcessTable.start_time(child_pid);
        let down = is_self_or_descendant(&OsProcessTable, child_pid, me, my_start);
        let up =
            child_start.map(|start| is_self_or_descendant(&OsProcessTable, me, child_pid, start));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(down, Ok(true));
        assert_eq!(up, Ok(Ok(false)));
    }

    #[test]
    fn os_table_refuses_a_pid_that_does_not_exist() {
        // Above every platform's pid_max, and above i32::MAX for macOS.
        let pid = u32::MAX - 1;
        assert_eq!(
            OsProcessTable.parent(pid),
            Err(ProcessError::Unreadable { pid })
        );
        assert_eq!(
            OsProcessTable.start_time(pid),
            Err(ProcessError::Unreadable { pid })
        );
    }
}
