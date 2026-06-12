//! Platform-specific total RAM detection.
//!
//! Why: tier selection drives every memory cap; we'd rather fall back to the
//! conservative Medium tier than guess wrong on an unsupported OS.
//! What: dispatches to a `#[cfg]`-gated platform implementation
//! (`sysctl hw.memsize` on macOS, `/proc/meminfo` parsing on Linux).
//! Test: `test_ram_detection_returns_nonzero` in `super::tests` asserts > 0
//! on the host running the suite (CI runs Linux/macOS, both supported).

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

#[cfg(target_os = "linux")]
fn detect_linux_ram_mb() -> Option<u64> {
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
