//! Platform-specific total RAM detection.
//!
//! Why: tier selection drives every memory cap; we'd rather fall back to the
//! conservative Medium tier than guess wrong on an unsupported OS.
//! What: dispatches to a `#[cfg]`-gated platform implementation
//! (`sysctl hw.memsize` on macOS, `/proc/meminfo` parsing on Linux, clamped to
//! any enclosing cgroup memory ceiling on Linux — issue #3657).
//! Test: `test_ram_detection_returns_nonzero` in `super::tests` asserts > 0
//! on the host running the suite (CI runs Linux/macOS, both supported); the
//! cgroup-clamp parsing has its own pure-function unit tests below.

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

/// cgroup v2 unified memory ceiling (bytes, or the literal string `"max"` for
/// "no cgroup-level ceiling").
#[cfg(target_os = "linux")]
const CGROUP_V2_MEMORY_MAX_PATH: &str = "/sys/fs/cgroup/memory.max";
/// cgroup v1 memory ceiling (bytes; an unconstrained v1 cgroup reports a
/// sentinel near `i64::MAX`, not a real ceiling — see [`parse_cgroup_v1_limit`]).
#[cfg(target_os = "linux")]
const CGROUP_V1_MEMORY_LIMIT_PATH: &str = "/sys/fs/cgroup/memory/memory.limit_in_bytes";

#[cfg(target_os = "linux")]
fn detect_linux_ram_mb() -> Option<u64> {
    let host_mb = detect_linux_host_ram_mb()?;
    match detect_cgroup_memory_limit_mb(host_mb) {
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
/// cgroup-level ceiling applies (no cgroup files present, or the cgroup is
/// unconstrained).
///
/// Why (issue #3657): `detect_linux_host_ram_mb` reads `/proc/meminfo`, which
/// reports the HOST's total physical RAM regardless of any cgroup (systemd
/// `MemoryMax=`/`MemoryHigh=`, Docker `--memory`, Kubernetes resource limits)
/// confining THIS process to a smaller ceiling. `MemoryPolicy`'s 25%-of-RAM
/// auto-tune then divides by the wrong (too-large) denominator: on a host
/// with far more physical RAM than the cgroup allows this one service, the
/// computed `TRUSTY_MEMORY_LIMIT_MB` soft ceiling can land ABOVE the cgroup's
/// hard ceiling, so the issue #2846 enforcement ticker's high-water check
/// never trips before the kernel's cgroup OOM-killer does — exactly what
/// production observed on `i-0076` (`MemoryHigh=24 GiB` / `MemoryMax=28 GiB`,
/// RSS climbed to 26.4 GiB with no reclaim-sweep log ever emitted).
/// What: tries cgroup v2 (`memory.max`) first, falling back to cgroup v1
/// (`memory.limit_in_bytes`). Returns `None` when neither file is readable,
/// neither value parses, or the reported value is not actually a ceiling
/// (`"max"` under v2; a sentinel at/above `host_ram_mb` under v1, which is
/// how an unconstrained v1 cgroup reports "no limit" — see
/// [`parse_cgroup_v1_limit`]).
/// Test: `test_parse_cgroup_v2_max_bytes`, `test_parse_cgroup_v2_unlimited`,
/// `test_parse_cgroup_v1_limit_bytes`, `test_parse_cgroup_v1_unlimited_sentinel`.
#[cfg(target_os = "linux")]
fn detect_cgroup_memory_limit_mb(host_ram_mb: u64) -> Option<u64> {
    if let Ok(text) = std::fs::read_to_string(CGROUP_V2_MEMORY_MAX_PATH) {
        if let Some(mb) = parse_cgroup_v2_max(&text) {
            return Some(mb);
        }
    }
    if let Ok(text) = std::fs::read_to_string(CGROUP_V1_MEMORY_LIMIT_PATH) {
        if let Some(mb) = parse_cgroup_v1_limit(&text, host_ram_mb) {
            return Some(mb);
        }
    }
    None
}

/// Parse a cgroup v2 `memory.max` file's contents into MB. `None` for the
/// literal `"max"` sentinel (explicit "no ceiling") or unparseable content.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn parse_cgroup_v1_limit(text: &str, host_ram_mb: u64) -> Option<u64> {
    let bytes: u64 = text.trim().parse().ok()?;
    let mb = bytes / (1024 * 1024);
    if mb == 0 || mb >= host_ram_mb {
        return None;
    }
    Some(mb)
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    /// A real cgroup v2 ceiling (e.g. systemd `MemoryMax=28G`) parses to the
    /// expected MB value.
    #[test]
    fn test_parse_cgroup_v2_max_bytes() {
        // 28 GiB in bytes.
        let text = "30064771072\n";
        assert_eq!(parse_cgroup_v2_max(text), Some(28 * 1024));
    }

    /// The literal `"max"` sentinel means "no cgroup v2 ceiling" — must
    /// return `None`, not attempt to parse it as a number.
    #[test]
    fn test_parse_cgroup_v2_unlimited() {
        assert_eq!(parse_cgroup_v2_max("max\n"), None);
        assert_eq!(parse_cgroup_v2_max("max"), None);
    }

    /// A real cgroup v1 ceiling below host RAM parses to the expected MB
    /// value.
    #[test]
    fn test_parse_cgroup_v1_limit_bytes() {
        // 28 GiB in bytes, host has 128 GiB — the cgroup limit is the real
        // constraint.
        let text = "30064771072\n";
        assert_eq!(parse_cgroup_v1_limit(text, 128 * 1024), Some(28 * 1024));
    }

    /// An unconstrained cgroup v1 hierarchy reports a near-`i64::MAX` byte
    /// sentinel — this must be recognised as "no real ceiling" (it dwarfs any
    /// real host's RAM) rather than clamping every workload to a nonsense
    /// multi-exabyte "limit".
    #[test]
    fn test_parse_cgroup_v1_unlimited_sentinel() {
        let text = "9223372036854771712\n";
        assert_eq!(parse_cgroup_v1_limit(text, 128 * 1024), None);
    }

    /// A cgroup v1 value reported at or above host RAM (not just the classic
    /// sentinel) is still "no real constraint" — the host's own RAM is
    /// already the binding ceiling in that case.
    #[test]
    fn test_parse_cgroup_v1_at_host_ram_is_unlimited() {
        let host_mb = 16 * 1024;
        let bytes = host_mb * 1024 * 1024;
        assert_eq!(parse_cgroup_v1_limit(&bytes.to_string(), host_mb), None);
    }

    /// `detect_cgroup_memory_limit_mb` on the actual CI/test host must never
    /// panic and, if it does detect a ceiling, that ceiling must be > 0.
    /// Why: exercises the real `/sys/fs/cgroup/*` read path (many CI runners
    /// execute inside a container, where a real cgroup v2 `memory.max` is
    /// commonly a genuine finite value) without asserting a specific number,
    /// since the actual ceiling is host/CI-environment-dependent.
    #[test]
    fn test_detect_cgroup_memory_limit_mb_smoke() {
        if let Some(mb) = detect_cgroup_memory_limit_mb(u64::MAX) {
            assert!(mb > 0, "a detected cgroup ceiling must be > 0 MB, got {mb}");
        }
    }
}
