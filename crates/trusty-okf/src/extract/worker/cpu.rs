//! A worker's spent CPU time, read by the host (#10746 S10 last change).
//!
//! Why: `RLIMIT_CPU` counts a process's CPU over its whole life, up to 500
//! items. Without a check, a run of valid items would spend the budget and
//! the kernel would kill a valid item mid-request. The host reads what the
//! worker has spent and recycles it while one item's allowance is left.
//! What: [`spent`] returns the largest CPU time (user plus system) of the
//! spawned process or any of its descendants — `RLIMIT_CPU` binds each
//! process on its own. Linux reads `/proc/<pid>/stat` (`USER_HZ` is 100 by
//! ABI) and walks the parent links down from the spawned PID, so it finds
//! the worker under `bwrap --new-session`, which moves the worker out of the
//! host's process group and session. Other systems run `/bin/ps -o time= -p
//! <pid>` for the spawned process (on macOS the launcher and the limiter
//! `exec` the worker, and the sandbox denies `fork`, so that process is the
//! worker). `None` when it cannot be read; the caller then recycles (fails
//! closed).
//! Test: `cpu_budget_recycles_before_a_valid_item_is_killed`,
//! `cpu_budget_recycles_a_sandboxed_worker`.

/// `/proc` reports CPU in `USER_HZ` ticks, fixed at 100 by the Linux ABI.
#[cfg(target_os = "linux")]
const USER_HZ: f64 = 100.0;

/// CPU seconds spent by the busiest process in the tree rooted at `root`,
/// the PID the host spawned.
#[cfg(target_os = "linux")]
pub(crate) fn spent(root: i32) -> Option<f64> {
    // #10746: keyed on the spawned PID's process tree, not its process
    // group: `bwrap --new-session` puts the worker in a session (and group)
    // of its own, two processes below the PID the host spawned.
    let mut procs: Vec<(i32, i32, f64)> = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // After `(comm)`: state ppid pgrp ... utime (index 11) stime (12).
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let f: Vec<&str> = rest.split_whitespace().collect();
        let field = |i: usize| f.get(i).and_then(|t| t.parse::<u64>().ok());
        let (Some(ppid), Some(utime), Some(stime)) = (field(1), field(11), field(12)) else {
            continue;
        };
        procs.push((pid, ppid as i32, (utime + stime) as f64 / USER_HZ));
    }
    let mut best: Option<f64> = None;
    let mut frontier = vec![root];
    let mut seen = std::collections::HashSet::new();
    while let Some(parent) = frontier.pop() {
        if !seen.insert(parent) {
            continue;
        }
        for (pid, ppid, secs) in &procs {
            if *pid == parent {
                best = Some(best.map_or(*secs, |b: f64| b.max(*secs)));
            } else if *ppid == parent {
                frontier.push(*pid);
            }
        }
    }
    best
}

/// CPU seconds spent by the spawned process `root` (see the module doc).
#[cfg(not(target_os = "linux"))]
pub(crate) fn spent(root: i32) -> Option<f64> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "time=", "-p", &root.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // `[[hh:]mm:]ss.ss`
    let text = String::from_utf8(out.stdout).ok()?;
    text.trim().split(':').try_fold(0.0, |acc: f64, part| {
        part.parse::<f64>().ok().map(|v| acc * 60.0 + v)
    })
}
