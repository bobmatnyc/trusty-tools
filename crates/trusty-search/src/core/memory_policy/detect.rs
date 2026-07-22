//! Platform-specific total RAM detection.
//!
//! Why: tier selection drives every memory cap; we'd rather fall back to the
//! conservative Medium tier than guess wrong on an unsupported OS.
//! What: dispatches to a `#[cfg]`-gated platform implementation
//! (`sysctl hw.memsize` on macOS, `/proc/meminfo` parsing on Linux, clamped to
//! any enclosing cgroup memory ceiling on Linux — issue #3657).
//! Test: `test_ram_detection_returns_nonzero` in `super::tests` asserts > 0
//! on the host running the suite (CI runs Linux/macOS, both supported); the
//! cgroup resolution + parsing logic has its own pure-function/fabricated-
//! filesystem unit tests below, runnable on every platform (no `cfg` gate —
//! see the module-level rationale in `detect_cgroup_memory_limit_mb_with_roots`).

/// Detect total physical RAM in megabytes. Returns `None` if the platform
/// path is not implemented or the detection command failed.
///
/// Why: tier selection drives every memory cap; we'd rather fall back to the
/// conservative Medium tier than guess wrong on an unsupported OS.
/// What: dispatches to a `#[cfg]`-gated platform implementation
/// (`sysctl hw.memsize` on macOS, `/proc/meminfo` parsing on Linux).
/// Test: `test_ram_detection_returns_nonzero` asserts > 0 on the host
/// running the suite (CI runs Linux/macOS, both supported).
pub fn detect_total_ram_mb() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        detect_macos_ram_mb()
    }
    #[cfg(target_os = "linux")]
    {
        detect_linux_ram_mb()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn detect_macos_ram_mb() -> Option<u64> {
    use std::process::Command;
    // `sysctl -n hw.memsize` prints the byte count on its own line.
    let output = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let bytes: u64 = text.trim().parse().ok()?;
    Some(bytes / (1024 * 1024))
}

/// Real `/proc/self/cgroup` path — where the kernel tells us THIS process's
/// own cgroup membership (both v1 and v2 hierarchies, one line each).
#[cfg(target_os = "linux")]
const PROC_SELF_CGROUP_PATH: &str = "/proc/self/cgroup";
/// Real cgroup v2 unified mount point.
#[cfg(target_os = "linux")]
const CGROUP_V2_MOUNT_ROOT: &str = "/sys/fs/cgroup";
/// Real cgroup v1 memory-controller mount point.
#[cfg(target_os = "linux")]
const CGROUP_V1_MEMORY_MOUNT_ROOT: &str = "/sys/fs/cgroup/memory";

#[cfg(target_os = "linux")]
fn detect_linux_ram_mb() -> Option<u64> {
    let host_mb = detect_linux_host_ram_mb()?;
    match detect_cgroup_memory_limit_mb_with_roots(
        host_mb,
        PROC_SELF_CGROUP_PATH,
        CGROUP_V2_MOUNT_ROOT,
        CGROUP_V1_MEMORY_MOUNT_ROOT,
    ) {
        Some(cgroup_mb) if cgroup_mb < host_mb => {
            tracing::info!(
                "memory_policy: cgroup memory ceiling ({cgroup_mb} MB) is below host RAM \
                 ({host_mb} MB) — using the cgroup ceiling for the 25%-of-RAM auto-tune so \
                 TRUSTY_MEMORY_LIMIT_MB stays under what systemd/Docker/Kubernetes actually \
                 enforces (issue #3657: on a host with far more physical RAM than the cgroup \
                 allows this service, auto-tuning off host RAM alone can compute a soft ceiling \
                 ABOVE the cgroup's hard limit, so the memory-pressure enforcement ticker never \
                 crosses its high-water mark before the kernel's cgroup OOM-killer fires)"
            );
            Some(cgroup_mb)
        }
        _ => Some(host_mb),
    }
}

/// `/proc/meminfo`-only RAM detection — the host's total physical RAM,
/// independent of any cgroup this process happens to be confined to. Split
/// out from `detect_linux_ram_mb` so the cgroup clamp above can compare
/// against it (and so `parse_cgroup_v1_limit`'s "unconstrained" sentinel
/// check has something to compare against).
#[cfg(target_os = "linux")]
fn detect_linux_host_ram_mb() -> Option<u64> {
    // /proc/meminfo `MemTotal: NNNNN kB` (always kB, even on aarch64).
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            // rest looks like "  16384000 kB"
            let mut parts = rest.split_whitespace();
            let kb: u64 = parts.next()?.parse().ok()?;
            return Some(kb / 1024);
        }
    }
    None
}

/// Effective cgroup memory ceiling for this process, in MB, or `None` when no
/// cgroup-level ceiling applies (no cgroup files present, the cgroup is
/// unconstrained, or the process isn't confined by one at all).
///
/// Why (issue #3657, code-critic follow-up): a systemd-managed service on a
/// bare host — exactly the `i-0076` incident box, with its `MemoryMax=`
/// drop-in — lives at a NESTED cgroup path (e.g.
/// `/system.slice/trusty-search.service`), not the cgroupfs root. Reading
/// `memory.max`/`memory.limit_in_bytes` at the cgroupfs ROOT (the first cut
/// of this fix) either finds no file there at all or finds the root's own
/// (typically unconstrained) ceiling, silently returning `None` on the very
/// box this issue targets — reproducing the exact #2846 auto-tune bug this
/// function exists to close. The fix: resolve THIS process's own cgroup path
/// from `/proc/self/cgroup` first (v2: the single `0::<path>` line; v1: the
/// line whose comma-separated controller list contains `memory`), join it
/// with the cgroupfs mount, and read the per-cgroup file at that nested
/// location. Only when `/proc/self/cgroup` itself is unreadable or
/// unparseable (e.g. a from-scratch container without a proc mount) do we
/// fall back to asking at the mount root directly — better than detecting
/// nothing at all, and harmless since an unconstrained root cgroup parses
/// back out as "no ceiling" anyway (`"max"` under v2; the huge sentinel under
/// v1, both already handled by [`parse_cgroup_v2_max`]/[`parse_cgroup_v1_limit`]).
/// What: takes the `/proc/self/cgroup` path and the two mount roots as
/// parameters (rather than the hardcoded real paths) purely so tests can
/// point every one of them at a fabricated temp-dir "cgroupfs" and a
/// synthetic `/proc/self/cgroup`-shaped file, exercising the real
/// resolve-then-read path end to end without touching the actual host
/// filesystem or requiring Linux. [`detect_linux_ram_mb`] calls this with the
/// real `/proc/self/cgroup` path and real cgroup mount points.
/// Test: `test_v2_nested_cgroup_path_resolves_and_reads`,
/// `test_v1_nested_cgroup_path_resolves_and_reads`,
/// `test_falls_back_to_mount_root_when_proc_self_cgroup_missing`,
/// `test_parse_cgroup_v2_max_bytes`, `test_parse_cgroup_v2_unlimited`,
/// `test_parse_cgroup_v1_limit_bytes`, `test_parse_cgroup_v1_unlimited_sentinel`.
// `cfg(any(test, target_os = "linux"))`: these are portable pure functions
// (no Linux-specific syscalls — just `std::fs::read_to_string` against
// caller-supplied paths and string parsing) exercised by the tests on every
// platform, but only ever CALLED from production code on Linux
// (`detect_linux_ram_mb`). Without the `test` half of this gate they'd be
// flagged `dead_code` on a non-Linux, non-test build (e.g. a plain `cargo
// check` on a macOS dev box).
#[cfg(any(test, target_os = "linux"))]
fn detect_cgroup_memory_limit_mb_with_roots(
    host_ram_mb: u64,
    self_cgroup_path: &str,
    v2_mount_root: &str,
    v1_mount_root: &str,
) -> Option<u64> {
    let self_cgroup_text = std::fs::read_to_string(self_cgroup_path).ok();

    // cgroup v2: resolve THIS process's nested path from /proc/self/cgroup;
    // fall back to the mount root only when /proc/self/cgroup couldn't be
    // read/parsed at all.
    let v2_max_path = match self_cgroup_text
        .as_deref()
        .and_then(parse_cgroup_v2_self_path)
    {
        Some(nested) => format!("{}/memory.max", join_cgroup_mount(v2_mount_root, &nested)),
        None => format!("{}/memory.max", v2_mount_root.trim_end_matches('/')),
    };
    if let Ok(text) = std::fs::read_to_string(&v2_max_path) {
        if let Some(mb) = parse_cgroup_v2_max(&text) {
            return Some(mb);
        }
    }

    // cgroup v1: same idea, using the memory-controller's own line.
    let v1_limit_path = match self_cgroup_text
        .as_deref()
        .and_then(parse_cgroup_v1_self_path)
    {
        Some(nested) => format!(
            "{}/memory.limit_in_bytes",
            join_cgroup_mount(v1_mount_root, &nested)
        ),
        None => format!(
            "{}/memory.limit_in_bytes",
            v1_mount_root.trim_end_matches('/')
        ),
    };
    if let Ok(text) = std::fs::read_to_string(&v1_limit_path) {
        if let Some(mb) = parse_cgroup_v1_limit(&text, host_ram_mb) {
            return Some(mb);
        }
    }

    None
}

/// Join a cgroupfs mount root with a cgroup path read from
/// `/proc/self/cgroup` (which is always absolute, e.g. `/` for the root
/// cgroup or `/system.slice/trusty-search.service` for a systemd-scoped
/// service), producing a filesystem path with no doubled/missing slashes.
#[cfg(any(test, target_os = "linux"))]
fn join_cgroup_mount(mount_root: &str, cgroup_path: &str) -> String {
    let root = mount_root.trim_end_matches('/');
    let path = cgroup_path.trim_start_matches('/');
    if path.is_empty() {
        root.to_string()
    } else {
        format!("{root}/{path}")
    }
}

/// Parse `/proc/self/cgroup`'s cgroup v2 line (`0::<path>`) into the
/// process's cgroup v2 path, e.g. `/system.slice/trusty-search.service`.
/// `None` if no such line exists (v1-only host) or the file is empty/malformed.
#[cfg(any(test, target_os = "linux"))]
fn parse_cgroup_v2_self_path(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("0::") {
            let path = rest.trim();
            if !path.is_empty() {
                return Some(path.to_string());
            }
        }
    }
    None
}

/// Parse `/proc/self/cgroup`'s cgroup v1 memory-controller line
/// (`<hierarchy-id>:<controller,list>:<path>`, matching the line whose
/// comma-separated controller list contains `memory`) into the process's
/// cgroup v1 memory path. `None` if no memory-controller line exists (v2-only
/// host) or the file is empty/malformed.
#[cfg(any(test, target_os = "linux"))]
fn parse_cgroup_v1_self_path(text: &str) -> Option<String> {
    for line in text.lines() {
        let mut parts = line.splitn(3, ':');
        let _hierarchy_id = parts.next()?;
        let controllers = parts.next()?;
        let path = parts.next()?;
        if controllers.split(',').any(|c| c == "memory") {
            let path = path.trim();
            if !path.is_empty() {
                return Some(path.to_string());
            }
        }
    }
    None
}

/// Parse a cgroup v2 `memory.max` file's contents into MB. `None` for the
/// literal `"max"` sentinel (explicit "no ceiling") or unparseable content.
#[cfg(any(test, target_os = "linux"))]
fn parse_cgroup_v2_max(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    if trimmed == "max" {
        return None;
    }
    let bytes: u64 = trimmed.parse().ok()?;
    Some(bytes / (1024 * 1024))
}

/// Parse a cgroup v1 `memory.limit_in_bytes` file's contents into MB. An
/// unconstrained v1 cgroup reports a sentinel near `i64::MAX` bytes (commonly
/// `9_223_372_036_854_771_712`, i.e. `i64::MAX` rounded down to a page
/// boundary) rather than a real ceiling — nowhere close to any real host's
/// RAM, so anything at or above `host_ram_mb` is treated as "no real
/// ceiling" and returns `None`.
#[cfg(any(test, target_os = "linux"))]
fn parse_cgroup_v1_limit(text: &str, host_ram_mb: u64) -> Option<u64> {
    let bytes: u64 = text.trim().parse().ok()?;
    let mb = bytes / (1024 * 1024);
    if mb == 0 || mb >= host_ram_mb {
        return None;
    }
    Some(mb)
}

// Not `#[cfg(target_os = "linux")]`: every function exercised here is plain,
// portable Rust (string parsing + `std::fs` against paths the test supplies
// via a fabricated temp-dir "cgroupfs") — none of it depends on actually
// running on Linux, so these tests run (and catch regressions) on every CI
// platform and on a macOS dev box alike, not just in Linux CI. Only the
// *real*-path constants and `detect_linux_ram_mb`/`detect_linux_host_ram_mb`
// are `cfg(target_os = "linux")`-gated, since only those hardcode real
// `/proc` and `/sys/fs/cgroup` paths that are meaningless off Linux.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cgroup_v2_max_bytes() {
        // 28 GiB in bytes.
        let text = "30064771072\n";
        assert_eq!(parse_cgroup_v2_max(text), Some(28 * 1024));
    }

    #[test]
    fn test_parse_cgroup_v2_unlimited() {
        assert_eq!(parse_cgroup_v2_max("max\n"), None);
        assert_eq!(parse_cgroup_v2_max("max"), None);
    }

    #[test]
    fn test_parse_cgroup_v1_limit_bytes() {
        // 28 GiB in bytes, host has 128 GiB — the cgroup limit is the real
        // constraint.
        let text = "30064771072\n";
        assert_eq!(parse_cgroup_v1_limit(text, 128 * 1024), Some(28 * 1024));
    }

    #[test]
    fn test_parse_cgroup_v1_unlimited_sentinel() {
        let text = "9223372036854771712\n";
        assert_eq!(parse_cgroup_v1_limit(text, 128 * 1024), None);
    }

    #[test]
    fn test_parse_cgroup_v1_at_host_ram_is_unlimited() {
        let host_mb = 16 * 1024;
        let bytes = host_mb * 1024 * 1024;
        assert_eq!(parse_cgroup_v1_limit(&bytes.to_string(), host_mb), None);
    }

    // ---- cgroup path resolution (parse_cgroup_v{1,2}_self_path) ----------

    #[test]
    fn test_parse_cgroup_v2_self_path_root() {
        assert_eq!(parse_cgroup_v2_self_path("0::/\n"), Some("/".to_string()));
    }

    #[test]
    fn test_parse_cgroup_v2_self_path_nested_systemd_scope() {
        // Real-world shape on a systemd-managed service (the i-0076 case).
        let text = "0::/system.slice/trusty-search.service\n";
        assert_eq!(
            parse_cgroup_v2_self_path(text),
            Some("/system.slice/trusty-search.service".to_string())
        );
    }

    #[test]
    fn test_parse_cgroup_v2_self_path_absent_on_v1_only_host() {
        // No "0::" line at all — pure cgroup v1 host.
        let text = "11:memory:/system.slice/trusty-search.service\n\
                     10:cpu,cpuacct:/system.slice/trusty-search.service\n";
        assert_eq!(parse_cgroup_v2_self_path(text), None);
    }

    #[test]
    fn test_parse_cgroup_v1_self_path_finds_memory_line_among_others() {
        let text = "12:pids:/system.slice/trusty-search.service\n\
                     11:memory:/system.slice/trusty-search.service\n\
                     10:devices:/system.slice/trusty-search.service\n";
        assert_eq!(
            parse_cgroup_v1_self_path(text),
            Some("/system.slice/trusty-search.service".to_string())
        );
    }

    #[test]
    fn test_parse_cgroup_v1_self_path_combined_controller_list() {
        // Some kernels/configs co-mount several controllers on one hierarchy;
        // "memory" can appear anywhere in the comma-separated list.
        let text = "4:memory,ambient_capabilities:/docker/abc123\n";
        assert_eq!(
            parse_cgroup_v1_self_path(text),
            Some("/docker/abc123".to_string())
        );
    }

    #[test]
    fn test_parse_cgroup_v1_self_path_absent_on_v2_only_host() {
        // Pure cgroup v2 host: only the "0::" line, no numbered memory line.
        assert_eq!(parse_cgroup_v1_self_path("0::/\n"), None);
    }

    // ---- join_cgroup_mount --------------------------------------------

    #[test]
    fn test_join_cgroup_mount_root_cgroup_no_double_slash() {
        assert_eq!(join_cgroup_mount("/sys/fs/cgroup", "/"), "/sys/fs/cgroup");
    }

    #[test]
    fn test_join_cgroup_mount_nested_path() {
        assert_eq!(
            join_cgroup_mount("/sys/fs/cgroup", "/system.slice/trusty-search.service"),
            "/sys/fs/cgroup/system.slice/trusty-search.service"
        );
    }

    #[test]
    fn test_join_cgroup_mount_v1_memory_root() {
        assert_eq!(
            join_cgroup_mount(
                "/sys/fs/cgroup/memory",
                "/system.slice/trusty-search.service"
            ),
            "/sys/fs/cgroup/memory/system.slice/trusty-search.service"
        );
    }

    // ---- end-to-end resolution against a fabricated temp-dir cgroupfs ----
    //
    // These build a REAL directory tree (not just string manipulation) so the
    // test exercises the actual resolve-from-/proc/self/cgroup-then-read path
    // that production code runs, not merely the pure parsers above in
    // isolation — the code-critic finding this closes was specifically that
    // string-level tests wouldn't have caught the "reads the cgroupfs ROOT
    // instead of the process's own nested cgroup" bug.

    /// The nested cgroup v2 case: `/proc/self/cgroup` names a systemd-scoped
    /// path (exactly the `i-0076` shape), and only THAT nested directory
    /// carries a real, finite `memory.max` — the mount root's own file (if it
    /// even exists) would be `"max"` (unconstrained). Resolution must read
    /// the nested file, not the root's.
    #[test]
    fn test_v2_nested_cgroup_path_resolves_and_reads() {
        let dir = tempfile::tempdir().unwrap();
        let proc_self_cgroup = dir.path().join("proc_self_cgroup");
        std::fs::write(
            &proc_self_cgroup,
            "0::/system.slice/trusty-search.service\n",
        )
        .unwrap();

        let v2_root = dir.path().join("sys_fs_cgroup");
        // Root-level file: unconstrained ("max") — must NOT be what resolves.
        std::fs::create_dir_all(&v2_root).unwrap();
        std::fs::write(v2_root.join("memory.max"), "max\n").unwrap();
        // Nested service-scope file: the REAL, finite ceiling (28 GiB).
        let nested = v2_root.join("system.slice").join("trusty-search.service");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("memory.max"), "30064771072\n").unwrap();

        let v1_root = dir.path().join("sys_fs_cgroup_memory_unused");
        let mb = detect_cgroup_memory_limit_mb_with_roots(
            128 * 1024,
            proc_self_cgroup.to_str().unwrap(),
            v2_root.to_str().unwrap(),
            v1_root.to_str().unwrap(),
        );
        assert_eq!(
            mb,
            Some(28 * 1024),
            "must resolve+read the NESTED memory.max (28 GiB), not the root's \
             unconstrained \"max\""
        );
    }

    /// Same shape, cgroup v1: `/proc/self/cgroup`'s memory line names a
    /// nested systemd scope, and only that nested directory has the real
    /// finite `memory.limit_in_bytes` (the root's is the huge "unconstrained"
    /// sentinel).
    #[test]
    fn test_v1_nested_cgroup_path_resolves_and_reads() {
        let dir = tempfile::tempdir().unwrap();
        let proc_self_cgroup = dir.path().join("proc_self_cgroup");
        std::fs::write(
            &proc_self_cgroup,
            "11:memory:/system.slice/trusty-search.service\n\
             10:pids:/system.slice/trusty-search.service\n",
        )
        .unwrap();

        // No v2 mount at all (pure v1 host) — v2 lookup must miss cleanly.
        let v2_root = dir.path().join("sys_fs_cgroup_unused");

        let v1_root = dir.path().join("sys_fs_cgroup_memory");
        std::fs::create_dir_all(&v1_root).unwrap();
        std::fs::write(
            v1_root.join("memory.limit_in_bytes"),
            "9223372036854771712\n", // root: unconstrained sentinel
        )
        .unwrap();
        let nested = v1_root.join("system.slice").join("trusty-search.service");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("memory.limit_in_bytes"), "30064771072\n").unwrap();

        let mb = detect_cgroup_memory_limit_mb_with_roots(
            128 * 1024,
            proc_self_cgroup.to_str().unwrap(),
            v2_root.to_str().unwrap(),
            v1_root.to_str().unwrap(),
        );
        assert_eq!(
            mb,
            Some(28 * 1024),
            "must resolve+read the NESTED memory.limit_in_bytes (28 GiB), not \
             the root's unconstrained sentinel"
        );
    }

    /// When `/proc/self/cgroup` itself is unreadable (e.g. a stripped-down
    /// container without a proc mount), resolution must fall back to asking
    /// at the mount root directly rather than detecting nothing at all.
    #[test]
    fn test_falls_back_to_mount_root_when_proc_self_cgroup_missing() {
        let dir = tempfile::tempdir().unwrap();
        // Deliberately do not create this file.
        let missing_proc_self_cgroup = dir.path().join("does_not_exist");

        let v2_root = dir.path().join("sys_fs_cgroup");
        std::fs::create_dir_all(&v2_root).unwrap();
        std::fs::write(v2_root.join("memory.max"), "30064771072\n").unwrap();

        let v1_root = dir.path().join("sys_fs_cgroup_memory_unused");
        let mb = detect_cgroup_memory_limit_mb_with_roots(
            128 * 1024,
            missing_proc_self_cgroup.to_str().unwrap(),
            v2_root.to_str().unwrap(),
            v1_root.to_str().unwrap(),
        );
        assert_eq!(mb, Some(28 * 1024));
    }

    /// A fully unconstrained cgroup (root-level `"max"`, no nested scope) must
    /// still resolve to `None` — no false-positive ceiling.
    #[test]
    fn test_no_ceiling_when_root_is_unconstrained_and_no_proc_self_cgroup() {
        let dir = tempfile::tempdir().unwrap();
        let missing_proc_self_cgroup = dir.path().join("does_not_exist");

        let v2_root = dir.path().join("sys_fs_cgroup");
        std::fs::create_dir_all(&v2_root).unwrap();
        std::fs::write(v2_root.join("memory.max"), "max\n").unwrap();

        let v1_root = dir.path().join("sys_fs_cgroup_memory_unused");
        let mb = detect_cgroup_memory_limit_mb_with_roots(
            128 * 1024,
            missing_proc_self_cgroup.to_str().unwrap(),
            v2_root.to_str().unwrap(),
            v1_root.to_str().unwrap(),
        );
        assert_eq!(mb, None);
    }

    /// Resolution against the actual CI/test host's real `/proc/self/cgroup`
    /// and `/sys/fs/cgroup/*` (Linux only — these are real Linux paths) must
    /// never panic and, if it does detect a ceiling, that ceiling must be > 0.
    /// Why: exercises the real read path (many CI runners execute inside a
    /// container, where a real cgroup v2 `memory.max` is commonly a genuine
    /// finite value) without asserting a specific number, since the actual
    /// ceiling is host/CI-environment-dependent.
    #[test]
    #[cfg(target_os = "linux")]
    fn test_detect_cgroup_memory_limit_mb_smoke() {
        if let Some(mb) = detect_cgroup_memory_limit_mb_with_roots(
            u64::MAX,
            PROC_SELF_CGROUP_PATH,
            CGROUP_V2_MOUNT_ROOT,
            CGROUP_V1_MEMORY_MOUNT_ROOT,
        ) {
            assert!(mb > 0, "a detected cgroup ceiling must be > 0 MB, got {mb}");
        }
    }
}
