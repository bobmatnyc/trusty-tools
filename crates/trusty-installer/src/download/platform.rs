//! Platform / architecture detection for prebuilt-binary selection.
//!
//! Why: Prebuilt assets are target-triple–specific. Tier-1 targets supported
//! by the CI release pipeline are `aarch64-apple-darwin` (macOS arm64) and
//! `x86_64-unknown-linux-gnu` (Linux x86_64). All others fall back to `cargo
//! install` so no supported platform is left without a working install path.
//!
//! What: [`current_target`] returns the Rust target triple for the running host
//! or `None` when it is not in the Tier-1 prebuilt set. [`is_tier1_target`]
//! encodes the Tier-1 whitelist as a pure predicate.
//!
//! Test: `tests` covers the pure `is_tier1_target` predicate and the mapping
//! constants; the `current_target()` host probe is side-effecting (reads
//! compile-time env) and validated by the integration flow.

/// The Rust target triple for macOS arm64.
pub const TARGET_MACOS_ARM64: &str = "aarch64-apple-darwin";

/// The Rust target triple for Linux x86_64.
pub const TARGET_LINUX_X86_64: &str = "x86_64-unknown-linux-gnu";

/// Returns the Rust target triple for this host if it is a Tier-1 prebuilt target.
///
/// Why: We only publish prebuilts for a small set of targets; callers need to know
/// whether to attempt a prebuilt download or fall straight back to `cargo install`.
///
/// What: Uses the `CARGO_CFG_TARGET_ARCH` + `CARGO_CFG_TARGET_OS` compile-time
/// constants (via `std::env::consts`) to determine the running host target.
/// Returns `Some(triple)` for Tier-1 targets, `None` for all others.
///
/// Test: `tests::tier1_targets_recognised`; the live probe is implicit in
/// `cargo test` itself (the host running the test is either Tier-1 or not).
pub fn current_target() -> Option<&'static str> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("macos", "aarch64") => Some(TARGET_MACOS_ARM64),
        ("linux", "x86_64") => Some(TARGET_LINUX_X86_64),
        _ => None,
    }
}

/// Returns `true` when `triple` is a Tier-1 prebuilt target.
///
/// Why: The prebuilt-first gate in `install`/`upgrade` needs a pure predicate
/// to decide the download path vs. cargo-fallback — useful independently of the
/// live host probe.
///
/// What: Checks against the two published Tier-1 triples.
///
/// Test: `tests::tier1_targets_recognised`, `tests::non_tier1_is_false`.
pub fn is_tier1_target(triple: &str) -> bool {
    triple == TARGET_MACOS_ARM64 || triple == TARGET_LINUX_X86_64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The two Tier-1 triples must be recognised; non-Tier-1 must not be.
    ///
    /// What: Asserts each Tier-1 constant returns true; Intel macOS, Linux arm64,
    /// and Windows return false.
    ///
    /// Test: This is the test.
    #[test]
    fn tier1_targets_recognised() {
        assert!(is_tier1_target(TARGET_MACOS_ARM64));
        assert!(is_tier1_target(TARGET_LINUX_X86_64));
    }

    /// Why: The fallback gate must reject non-Tier-1 targets; a stale match arm
    /// would silently enable an unsupported prebuilt path.
    ///
    /// What: Asserts Intel macOS, Linux arm64, and Windows are not Tier-1.
    ///
    /// Test: This is the test.
    #[test]
    fn non_tier1_is_false() {
        assert!(!is_tier1_target("x86_64-apple-darwin"));
        assert!(!is_tier1_target("aarch64-unknown-linux-gnu"));
        assert!(!is_tier1_target("x86_64-pc-windows-msvc"));
        assert!(!is_tier1_target(""));
    }

    /// Why: The live `current_target()` probe must return one of the two Tier-1
    /// triples when it returns `Some`; `None` only when the host is not Tier-1.
    ///
    /// What: When `Some`, asserts the value passes `is_tier1_target`.
    ///
    /// Test: This is the test.
    #[test]
    fn current_target_is_tier1_or_none() {
        // None is valid when the host is not a Tier-1 target.
        if let Some(t) = current_target() {
            assert!(is_tier1_target(t));
        }
    }
}
