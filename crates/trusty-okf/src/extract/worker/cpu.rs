//! A worker's spent CPU time, read by the host (#10746 S10 last change).
//!
//! Why: `RLIMIT_CPU` counts a process's CPU over its whole life, up to 500
//! items. Without a check, a run of valid items would spend the budget and
//! the kernel would kill a valid item mid-request. The host reads what the
//! worker has spent and recycles it while one item's allowance is left.
//! What: [`spent`] returns the largest CPU time (user plus system) of any
//! process in the worker's process group — `RLIMIT_CPU` binds each process
//! on its own. Linux reads `/proc/<pid>/stat` (`USER_HZ` is 100 by ABI);
//! other systems run `/bin/ps -o time= -p <pgid>` for the group leader (on
//! macOS the sandbox denies `fork`, so the leader is the worker). `None`
//! when it cannot be read; the caller then recycles (fails closed).
//! Test: `cpu_budget_recycles_before_a_valid_item_is_killed`.

/// `/proc` reports CPU in `USER_HZ` ticks, fixed at 100 by the Linux ABI.
#[cfg(target_os = "linux")]
const USER_HZ: f64 = 100.0;

/// CPU seconds spent by the busiest process of group `pgid`.
#[cfg(target_os = "linux")]
pub(crate) fn spent(pgid: i32) -> Option<f64> {
    let mut best: Option<f64> = None;
    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else { continue };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // After `(comm)`: state ppid pgrp ... utime (index 11) stime (12).
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let f: Vec<&str> = rest.split_whitespace().collect();
        if f.get(2).and_then(|g| g.parse::<i32>().ok()) != Some(pgid) {
            continue;
        }
        let ticks = |i: usize| f.get(i).and_then(|t| t.parse::<u64>().ok());
        let secs = (ticks(11)? + ticks(12)?) as f64 / USER_HZ;
        best = Some(best.map_or(secs, |b: f64| b.max(secs)));
    }
    best
}

/// CPU seconds spent by the group leader `pgid` (see the module doc).
#[cfg(not(target_os = "linux"))]
pub(crate) fn spent(pgid: i32) -> Option<f64> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "time=", "-p", &pgid.to_string()])
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
