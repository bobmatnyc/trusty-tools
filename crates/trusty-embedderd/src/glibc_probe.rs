//! Fast-fail glibc compatibility probe for the bundled (statically-linked)
//! ONNX Runtime — issue #2222 ("Alternative 3" from #2218's investigation).
//!
//! Why: `readiness::run_bounded` (issue #1633) already turns an ORT
//! provider-init hang into a loud failure instead of an indefinite hang, but
//! it still has to wait out the full `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS`
//! (default 180 s) before it can report anything, because a generic future
//! timeout has no way to distinguish "legitimately slow" from "structurally
//! cannot succeed." A host's glibc version is fixed for the lifetime of the
//! OS install — on Amazon Linux 2023 (glibc 2.34) running the default
//! `bundled-ort` feature (which links a statically-bundled ONNX Runtime
//! built assuming glibc >= 2.38, see `trusty-common/Cargo.toml`), every
//! single restart will deadlock and burn the full timeout, forever, until an
//! operator reads the eventual error message and rebuilds. This probe checks
//! the one fact that predicts that outcome perfectly — the runtime glibc
//! version — *before* calling `FastEmbedder::new()`, so the failure is
//! immediate and structured instead of a 180 s wait.
//!
//! What: `check_bundled_ort_glibc_compat()` (Linux/glibc builds compiled
//! with the `bundled-ort` feature only) reads the running glibc's version
//! via `gnu_get_libc_version()` and compares it against
//! [`MIN_GLIBC_VERSION`] (2.38 — the floor the bundled ORT build assumes).
//! Below that floor, it returns a descriptive `anyhow::Error` naming issue
//! #2222 and the two remediations (the prebuilt `x86_64-linux-al2023`
//! release tarball, or rebuilding with `--features load-dynamic` +
//! `ORT_DYLIB_PATH`) instead of letting the caller reach the doomed ORT
//! init. If the version cannot be read or parsed, the probe is a no-op
//! (`Ok(())`) — it must never itself block startup on an unrelated
//! introspection failure; the existing 180 s `run_bounded` timeout remains
//! the correct backstop for that case, and for any hang the probe does not
//! predict.
//!
//! Compiled only for `target_os = "linux"` + `target_env = "gnu"` (musl and
//! non-Linux targets have no `gnu_get_libc_version()` symbol and no
//! glibc-floor problem to probe for in the first place) — non-Linux and
//! `load-dynamic`-only builds skip this module's real logic entirely; the
//! call site in `lib.rs` is behind the same `cfg` plus `feature =
//! "bundled-ort"` (a `load-dynamic` build dlopens a host-supplied
//! `libonnxruntime.so` at runtime, so the bundled-ORT glibc floor does not
//! apply to it).
//!
//! Test: `parse_glibc_version_*` and `meets_minimum_*` below exercise the
//! pure version-parsing and comparison logic directly (no `cfg` gate, so
//! these run on every platform, including this crate's macOS dev/CI
//! target). The `gnu_get_libc_version()` FFI call itself is not
//! unit-tested — it is a single, side-effect-free libc accessor with no
//! branching logic of its own; the coverage that matters is on what we do
//! with its output, exercised above.

/// Minimum glibc version (major, minor) the bundled (statically-linked) ONNX
/// Runtime build requires. Mirrors the floor documented in
/// `trusty-common/Cargo.toml`'s `embedder-bundled-ort` feature comment.
pub(crate) const MIN_GLIBC_VERSION: (u32, u32) = (2, 38);

/// Parse a glibc version string such as `"2.34"`, `"2.38"`, or `"2.38.1"`
/// into a `(major, minor)` pair, ignoring any patch component.
///
/// Why: `gnu_get_libc_version()` returns strings in the `"<major>.<minor>"`
/// form historically, but downstream glibc releases have occasionally added
/// a third component; ignoring it keeps the comparison robust without
/// needing to track glibc's own versioning scheme changes.
/// What: splits on `.`, parses the first two components as `u32`; returns
/// `None` if either component is missing or non-numeric.
/// Test: `parse_glibc_version_two_component`,
/// `parse_glibc_version_three_component_ignores_patch`,
/// `parse_glibc_version_boundary`, `parse_glibc_version_rejects_malformed`.
pub(crate) fn parse_glibc_version(raw: &str) -> Option<(u32, u32)> {
    let mut parts = raw.trim().split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    Some((major, minor))
}

/// Whether `version` satisfies `minimum`, compared as `(major, minor)`
/// tuples (lexicographic — Rust's derived tuple `Ord`).
///
/// Test: `meets_minimum_below_boundary_fails`, `meets_minimum_at_boundary_passes`,
/// `meets_minimum_above_boundary_passes`, `meets_minimum_higher_major_passes`.
pub(crate) fn meets_minimum(version: (u32, u32), minimum: (u32, u32)) -> bool {
    version >= minimum
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod linux_gnu {
    use std::ffi::CStr;

    use anyhow::{bail, Result};

    use super::{meets_minimum, parse_glibc_version, MIN_GLIBC_VERSION};

    /// Query the runtime glibc version via glibc's own
    /// `gnu_get_libc_version()` accessor.
    ///
    /// Why: this is the authoritative source for "what glibc is actually
    /// running" — unlike parsing `/etc/os-release` or shelling out to `ldd
    /// --version`, it reads the version glibc itself reports, with no
    /// distro-specific string format to keep up with.
    /// What: calls the C function, converts the returned pointer to an
    /// owned `String`. Returns `None` if the pointer is unexpectedly null or
    /// the returned bytes are not valid UTF-8 (glibc version strings are
    /// ASCII in practice, but we don't want a UTF-8 edge case to panic
    /// startup).
    /// Test: not unit-tested (see module-level doc) — exercised implicitly
    /// by the AL2023 CI gate (`.github/workflows/al2023-build.yml`), which
    /// runs the built binary's code path on real glibc 2.34.
    fn runtime_glibc_version_string() -> Option<String> {
        // SAFETY: `gnu_get_libc_version()` returns a pointer to a static,
        // NUL-terminated string owned by glibc itself for the lifetime of
        // the process (documented glibc behaviour) — never null on a glibc
        // target, never to be freed by the caller.
        let ptr = unsafe { libc::gnu_get_libc_version() };
        if ptr.is_null() {
            return None;
        }
        // SAFETY: `ptr` is non-null and, per the contract above, points at a
        // valid NUL-terminated C string for the lifetime of the process.
        let c_str = unsafe { CStr::from_ptr(ptr) };
        c_str.to_str().ok().map(str::to_owned)
    }

    /// Fail fast if the host glibc is older than the bundled ONNX Runtime
    /// requires. See the module-level doc for the full rationale.
    ///
    /// `pub(crate)` (not `pub(super)`): this is re-exported via `pub(crate)
    /// use linux_gnu::check_bundled_ort_glibc_compat;` below so `lib.rs` can
    /// call it as `glibc_probe::check_bundled_ort_glibc_compat()` — a
    /// re-export can never widen an item's visibility beyond its own
    /// declared visibility, so the definition here must be at least as
    /// permissive (`pub(crate)`) as the re-export.
    pub(crate) fn check_bundled_ort_glibc_compat() -> Result<()> {
        let Some(raw) = runtime_glibc_version_string() else {
            // Could not determine the version — don't block startup on an
            // unrelated probe failure; fall through to the (still-active)
            // `run_bounded` timeout backstop.
            return Ok(());
        };
        let Some(version) = parse_glibc_version(&raw) else {
            return Ok(());
        };
        if meets_minimum(version, MIN_GLIBC_VERSION) {
            return Ok(());
        }
        bail!(
            "host glibc {raw} is older than the {}.{} the bundled (statically-linked) ONNX \
             Runtime requires (issue #2222, root cause #1633). This host cannot run the \
             default `bundled-ort` build of trusty-embedderd — without this check, every \
             restart would instead hang for up to the TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS bound \
             (default 180s) before failing the same way. Remediation: (1) download the \
             prebuilt `x86_64-linux-al2023` tarball from the GitHub Releases page (already \
             built with the load-dynamic ORT mode), or (2) reinstall with `cargo install \
             trusty-search --no-default-features --features load-dynamic` and set \
             ORT_DYLIB_PATH=/path/to/a/host-compatible/libonnxruntime.so (e.g. an Ubuntu \
             20.04 / glibc 2.31 build).",
            MIN_GLIBC_VERSION.0,
            MIN_GLIBC_VERSION.1,
        )
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) use linux_gnu::check_bundled_ort_glibc_compat;

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the common two-component form (`"<major>.<minor>"`) is what
    /// `gnu_get_libc_version()` returns on every glibc release observed so
    /// far.
    /// What: parses "2.34" and asserts the exact tuple.
    /// Test: itself.
    #[test]
    fn parse_glibc_version_two_component() {
        assert_eq!(parse_glibc_version("2.34"), Some((2, 34)));
    }

    /// Why: some distros / future glibc releases may report a patch
    /// component; it must not break parsing.
    /// What: parses "2.38.1", asserts the patch component is dropped.
    /// Test: itself.
    #[test]
    fn parse_glibc_version_three_component_ignores_patch() {
        assert_eq!(parse_glibc_version("2.38.1"), Some((2, 38)));
    }

    /// Why: the exact floor value (2.38) must parse identically to any
    /// other value — no off-by-one in the parser itself.
    /// What: parses "2.38".
    /// Test: itself.
    #[test]
    fn parse_glibc_version_boundary() {
        assert_eq!(parse_glibc_version("2.38"), Some((2, 38)));
    }

    /// Why: a malformed or missing version string must never panic startup
    /// — the probe must degrade to a no-op, not crash the daemon.
    /// What: feeds empty, non-numeric, and single-component strings.
    /// Test: itself.
    #[test]
    fn parse_glibc_version_rejects_malformed() {
        assert_eq!(parse_glibc_version(""), None);
        assert_eq!(parse_glibc_version("not-a-version"), None);
        assert_eq!(parse_glibc_version("2"), None);
        assert_eq!(parse_glibc_version("2."), None);
    }

    /// Why: this is the exact AL2023 case (glibc 2.34) — must be rejected.
    /// What: (2, 34) vs the (2, 38) floor.
    /// Test: itself.
    #[test]
    fn meets_minimum_below_boundary_fails() {
        assert!(!meets_minimum((2, 34), MIN_GLIBC_VERSION));
    }

    /// Why: the exact floor must be accepted (>=, not >).
    /// What: (2, 38) vs the (2, 38) floor.
    /// Test: itself.
    #[test]
    fn meets_minimum_at_boundary_passes() {
        assert!(meets_minimum((2, 38), MIN_GLIBC_VERSION));
    }

    /// Why: newer glibc than the floor must always be accepted.
    /// What: (2, 40) vs the (2, 38) floor.
    /// Test: itself.
    #[test]
    fn meets_minimum_above_boundary_passes() {
        assert!(meets_minimum((2, 40), MIN_GLIBC_VERSION));
    }

    /// Why: a hypothetical future glibc 3.x must compare as newer even
    /// though its minor component (0) is numerically less than 38 — this
    /// guards against a naive minor-only comparison bug.
    /// What: (3, 0) vs the (2, 38) floor.
    /// Test: itself.
    #[test]
    fn meets_minimum_higher_major_passes() {
        assert!(meets_minimum((3, 0), MIN_GLIBC_VERSION));
    }
}
