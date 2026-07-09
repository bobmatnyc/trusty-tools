//! Host-glibc probing and glibc-aware asset selection for ONNX-Runtime crates.
//!
//! Why: `trusty-search` and `trusty-analyze` bundle ONNX Runtime. Their native
//! `x86_64-unknown-linux-gnu` release asset is built on the CI `ubuntu-latest`
//! runner and carries a high glibc floor (`GLIBC_2.39`), so it fails to even
//! execute on Debian 12, Ubuntu 22.04, RHEL 9, and Amazon Linux 2023 with
//! `GLIBC_2.39 not found` (issue #1992). The release pipeline (#2037) already
//! ships a portable `x86_64-linux-al2023` load-dynamic asset for exactly these
//! hosts, but the installer never selected it — so a low-glibc host silently
//! downloaded a binary that could not run.
//!
//! What: [`host_glibc_version`] probes the running host's glibc via
//! `ldd --version`. [`select_asset_suffix`] is a pure, hermetic decision:
//! given a crate, its host target triple, and the (mockable) host glibc, it
//! returns the release asset suffix to download plus whether that asset is a
//! load-dynamic ORT build (which requires `ORT_DYLIB_PATH` at runtime).
//! [`ort_dylib_instructions`] renders the actionable runtime-setup note.
//!
//! Test: `tests` covers `parse_glibc_version`, `select_asset_suffix` across the
//! ORT/non-ORT × low/high/unknown-glibc × x86_64/arm64 matrix, and the
//! instruction renderer. The live `host_glibc_version` probe is side-effecting
//! and covered by the integration flow.

use super::platform::{TARGET_LINUX_ARM64, TARGET_LINUX_X86_64};

/// Crates whose native `x86_64-unknown-linux-gnu` asset bundles ONNX Runtime and
/// therefore carries a high glibc floor.
///
/// Why: Only these crates ship a portable `x86_64-linux-al2023` fallback asset;
/// every other crate's Linux asset is a glibc-2.17-baseline zigbuild (issue
/// #2037) that runs everywhere, so it needs no glibc-aware routing.
///
/// What: The set of crate names that receive AL2023 asset selection.
///
/// Test: `tests::select_*` exercise both members and a non-member.
pub const ORT_CRATES: [&str; 2] = ["trusty-search", "trusty-analyze"];

/// Minimum glibc `(major, minor)` the native x86_64 Linux ORT asset requires.
///
/// Why: The native asset is built on `ubuntu-latest` (Ubuntu 24.04 / glibc
/// 2.39); the bundled static ONNX Runtime additionally needs glibc ≥ 2.38. The
/// binding constraint is the runner glibc, so 2.39 is the effective floor a host
/// must meet to run the native asset.
///
/// What: `(2, 39)` — hosts below this must use the AL2023 asset instead.
///
/// Test: `tests::select_prefers_al2023_below_floor` /
/// `tests::select_keeps_native_at_floor`.
pub const NATIVE_LINUX_GLIBC_FLOOR: (u32, u32) = (2, 39);

/// Asset suffix for the portable AL2023 load-dynamic ORT build.
///
/// Why: Centralise the literal so the URL builders and the selection logic never
/// drift from the name the release workflow publishes.
///
/// What: `x86_64-linux-al2023` (matches `release.yml`'s `asset_suffix`).
///
/// Test: `tests::select_prefers_al2023_below_floor`.
pub const AL2023_ASSET_SUFFIX: &str = "x86_64-linux-al2023";

/// The chosen release asset for a crate on the running host.
///
/// Why: The caller needs both the asset suffix (to build the download URL) and
/// whether the asset is a load-dynamic ORT build so it can print the
/// `ORT_DYLIB_PATH` runtime-setup instructions after a successful install.
///
/// What: `suffix` is the `<crate>-<version>-<suffix>.tar.gz` infix; `load_dynamic`
/// is `true` when the asset dlopen()s `libonnxruntime.so` at startup.
///
/// Test: `tests::select_*`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetChoice {
    /// The asset suffix, e.g. `x86_64-linux-al2023` or `aarch64-apple-darwin`.
    pub suffix: String,
    /// `true` when the asset requires `ORT_DYLIB_PATH` at runtime.
    pub load_dynamic: bool,
}

/// Probe the running host's glibc version via `ldd --version`.
///
/// Why: Only the live host knows its glibc; we need it to decide whether the
/// native x86_64 ORT asset would even execute here.
///
/// What: Runs `ldd --version`, reads stdout, and delegates to the pure
/// [`parse_glibc_version`]. Returns `None` when `ldd` is absent or its output is
/// unparseable (e.g. musl hosts) — the caller then conservatively keeps the
/// native asset.
///
/// Test: Side-effecting; the parse half is covered by `tests::parse_*`.
pub fn host_glibc_version() -> Option<(u32, u32)> {
    let output = std::process::Command::new("ldd")
        .arg("--version")
        .output()
        .ok()?;
    // glibc's `ldd --version` prints to stdout; some libcs use stderr — try both.
    parse_glibc_version(&String::from_utf8_lossy(&output.stdout))
        .or_else(|| parse_glibc_version(&String::from_utf8_lossy(&output.stderr)))
}

/// Parse a `(major, minor)` glibc version from `ldd --version` output.
///
/// Why: The output format varies by distro — e.g. `ldd (Ubuntu GLIBC
/// 2.39-0ubuntu8.3) 2.39` or `ldd (GNU libc) 2.31` — so we scan for the trailing
/// bare `MAJOR.MINOR` token on the first line rather than a fixed column.
///
/// What: Takes the first line, finds the last whitespace-delimited token that
/// parses as `MAJOR.MINOR` (both non-negative integers), and returns it. Returns
/// `None` when no such token exists (non-glibc loaders like musl's).
///
/// Test: `tests::parse_ubuntu`, `tests::parse_gnu_libc`, `tests::parse_musl_none`.
pub(crate) fn parse_glibc_version(ldd_output: &str) -> Option<(u32, u32)> {
    let first_line = ldd_output.lines().next()?;
    // Scan tokens right-to-left: the bare `2.39` version is the last token on
    // glibc's banner line, after the parenthesised distro string.
    first_line
        .split_whitespace()
        .rev()
        .find_map(parse_major_minor)
}

/// Parse a single `MAJOR.MINOR` token into `(u32, u32)`.
///
/// Why: Extracted so the token scan in [`parse_glibc_version`] stays a one-liner
/// and the numeric parsing is unit-testable in isolation.
///
/// What: Splits on the first `.`; returns `Some` only when exactly two parts both
/// parse as `u32`. Trailing patch text (e.g. `2.39-0ubuntu8`) is rejected so the
/// scan falls through to the bare version token.
///
/// Test: Exercised via `tests::parse_*`.
fn parse_major_minor(token: &str) -> Option<(u32, u32)> {
    let (major, minor) = token.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Decide which release asset to download for `crate_name` on `target`.
///
/// Why: On a low-glibc x86_64 Linux host, the native ORT asset crashes with
/// `GLIBC_2.39 not found`; the portable AL2023 asset runs there instead. The ORT
/// arm64 Linux asset is always a load-dynamic build (issue #2037), so it needs
/// the `ORT_DYLIB_PATH` note regardless of glibc. This function is pure — the
/// host glibc is injected — so the whole decision matrix is hermetically testable.
///
/// What: For an ORT crate (`ORT_CRATES`):
/// - on `x86_64-unknown-linux-gnu` with a probed glibc **below**
///   [`NATIVE_LINUX_GLIBC_FLOOR`], selects [`AL2023_ASSET_SUFFIX`]
///   (`load_dynamic = true`);
/// - on `aarch64-unknown-linux-gnu`, keeps the arm64 suffix but flags
///   `load_dynamic = true` (that asset dlopen()s ONNX Runtime);
/// - otherwise (macOS, or x86_64 with glibc ≥ floor, or unknown glibc) keeps the
///   native `target` suffix (`load_dynamic = false`).
///
/// For every non-ORT crate it always returns the native `target` suffix.
/// When the host glibc cannot be probed (`None`) on x86_64, it conservatively
/// keeps the native asset rather than forcing a load-dynamic path that needs
/// extra runtime setup.
///
/// Test: `tests::select_prefers_al2023_below_floor`,
/// `tests::select_keeps_native_at_floor`, `tests::select_unknown_glibc_keeps_native`,
/// `tests::select_arm64_ort_is_load_dynamic`, `tests::select_non_ort_never_switches`.
pub fn select_asset_suffix(
    crate_name: &str,
    target: &str,
    host_glibc: Option<(u32, u32)>,
) -> AssetChoice {
    let native = AssetChoice {
        suffix: target.to_owned(),
        load_dynamic: false,
    };

    if !ORT_CRATES.contains(&crate_name) {
        return native;
    }

    match target {
        TARGET_LINUX_X86_64 => match host_glibc {
            Some(glibc) if glibc < NATIVE_LINUX_GLIBC_FLOOR => AssetChoice {
                suffix: AL2023_ASSET_SUFFIX.to_owned(),
                load_dynamic: true,
            },
            _ => native,
        },
        // The arm64 Linux ORT asset is a load-dynamic build (issue #2037): it
        // ships with the native `aarch64-unknown-linux-gnu` suffix but still
        // dlopen()s ONNX Runtime, so it needs the ORT_DYLIB_PATH note.
        TARGET_LINUX_ARM64 => AssetChoice {
            suffix: TARGET_LINUX_ARM64.to_owned(),
            load_dynamic: true,
        },
        _ => native,
    }
}

/// Render the actionable `ORT_DYLIB_PATH` runtime-setup note for a load-dynamic
/// ORT asset on `target`.
///
/// Why: The load-dynamic asset does not bundle `libonnxruntime.so`; the binary
/// dlopen()s it at startup and refuses to run without `ORT_DYLIB_PATH`.
/// Auto-downloading the ~200 MB ONNX Runtime and mutating the user's shell
/// profile is out of scope for a clean install step, so we hand the user the
/// exact commands instead of leaving a binary that silently fails to start.
///
/// What: Returns a multi-line string with the ONNX Runtime 1.20.1 download URL
/// and the `export ORT_DYLIB_PATH=…` line for the host architecture (`aarch64`
/// for arm64 targets, `x64` otherwise), mirroring the `ORT-RUNTIME-NOTE.txt`
/// the release workflow packages.
///
/// Test: `tests::instructions_mention_dylib_and_arch`.
pub fn ort_dylib_instructions(target: &str) -> String {
    let arch = if target.starts_with("aarch64") {
        "aarch64"
    } else {
        "x64"
    };
    format!(
        "This portable ONNX-Runtime build loads libonnxruntime.so at runtime and \
         will not start until ORT_DYLIB_PATH is set. Install ONNX Runtime 1.20.1 and \
         export the path:\n  \
         curl -fsSL https://github.com/microsoft/onnxruntime/releases/download/v1.20.1/onnxruntime-linux-{arch}-1.20.1.tgz | tar xz -C /opt\n  \
         export ORT_DYLIB_PATH=/opt/onnxruntime-linux-{arch}-1.20.1/lib/libonnxruntime.so.1.20.1\n\
         (add the export line to your shell profile to persist it)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: Ubuntu's banner embeds the version twice (`2.39-0ubuntu8.3` then a
    /// bare `2.39`); the bare trailing token is the authoritative version.
    /// What: Asserts the Ubuntu format parses to `(2, 39)`.
    /// Test: This is the test.
    #[test]
    fn parse_ubuntu() {
        let out = "ldd (Ubuntu GLIBC 2.39-0ubuntu8.3) 2.39\nCopyright (C) 2024 ...";
        assert_eq!(parse_glibc_version(out), Some((2, 39)));
    }

    /// Why: The plain GNU libc banner must parse too.
    /// What: Asserts `ldd (GNU libc) 2.31` → `(2, 31)`.
    /// Test: This is the test.
    #[test]
    fn parse_gnu_libc() {
        let out = "ldd (GNU libc) 2.31";
        assert_eq!(parse_glibc_version(out), Some((2, 31)));
    }

    /// Why: Debian 12 (glibc 2.36) is a primary #1992 target; it must parse below
    /// the floor.
    /// What: Asserts a Debian-style banner parses to `(2, 36)` and is `< floor`.
    /// Test: This is the test.
    #[test]
    fn parse_debian_below_floor() {
        let out = "ldd (Debian GLIBC 2.36-9+deb12u7) 2.36";
        let v = parse_glibc_version(out).expect("parses");
        assert_eq!(v, (2, 36));
        assert!(v < NATIVE_LINUX_GLIBC_FLOOR);
    }

    /// Why: A musl loader emits no `MAJOR.MINOR` glibc token; the probe must
    /// return `None` (→ caller keeps the native asset).
    /// What: Asserts musl-style output yields `None`.
    /// Test: This is the test.
    #[test]
    fn parse_musl_none() {
        let out = "musl libc (x86_64)\nVersion 1.2.4";
        // "1.2.4" is not MAJOR.MINOR (has a patch component) and there is no
        // bare MAJOR.MINOR token on the first line.
        assert_eq!(parse_glibc_version(out), None);
    }

    /// Why: Empty / garbage output must not panic and must return `None`.
    /// What: Asserts empty input yields `None`.
    /// Test: This is the test.
    #[test]
    fn parse_empty_none() {
        assert_eq!(parse_glibc_version(""), None);
    }

    /// Why: The core #1992 fix — an ORT crate on a below-floor x86_64 host must
    /// select the portable AL2023 asset, not the crashing native one.
    /// What: trusty-search on x86_64 Linux with glibc 2.35 → AL2023, load-dynamic.
    /// Test: This is the test.
    #[test]
    fn select_prefers_al2023_below_floor() {
        let choice = select_asset_suffix("trusty-search", TARGET_LINUX_X86_64, Some((2, 35)));
        assert_eq!(choice.suffix, AL2023_ASSET_SUFFIX);
        assert!(choice.load_dynamic);
    }

    /// Why: A host that meets the floor must keep the fast native static asset —
    /// no needless load-dynamic path.
    /// What: trusty-analyze on x86_64 Linux with glibc exactly 2.39 → native.
    /// Test: This is the test.
    #[test]
    fn select_keeps_native_at_floor() {
        let choice = select_asset_suffix("trusty-analyze", TARGET_LINUX_X86_64, Some((2, 39)));
        assert_eq!(choice.suffix, TARGET_LINUX_X86_64);
        assert!(!choice.load_dynamic);
    }

    /// Why: When glibc cannot be probed we must not gamble on load-dynamic setup;
    /// keeping native preserves prior behaviour.
    /// What: trusty-search on x86_64 Linux with `None` glibc → native.
    /// Test: This is the test.
    #[test]
    fn select_unknown_glibc_keeps_native() {
        let choice = select_asset_suffix("trusty-search", TARGET_LINUX_X86_64, None);
        assert_eq!(choice.suffix, TARGET_LINUX_X86_64);
        assert!(!choice.load_dynamic);
    }

    /// Why: The arm64 Linux ORT asset is load-dynamic regardless of glibc, so the
    /// ORT_DYLIB_PATH note must fire even though the suffix stays native.
    /// What: trusty-search on aarch64 Linux → native suffix, load_dynamic = true.
    /// Test: This is the test.
    #[test]
    fn select_arm64_ort_is_load_dynamic() {
        let choice = select_asset_suffix("trusty-search", TARGET_LINUX_ARM64, Some((2, 35)));
        assert_eq!(choice.suffix, TARGET_LINUX_ARM64);
        assert!(choice.load_dynamic);
    }

    /// Why: macOS ORT assets are static; no glibc routing applies there.
    /// What: trusty-analyze on macOS arm64 → native suffix, not load-dynamic.
    /// Test: This is the test.
    #[test]
    fn select_macos_ort_keeps_native() {
        let choice = select_asset_suffix("trusty-analyze", "aarch64-apple-darwin", Some((2, 30)));
        assert_eq!(choice.suffix, "aarch64-apple-darwin");
        assert!(!choice.load_dynamic);
    }

    /// Why: Non-ORT crates ship a glibc-2.17 baseline asset that runs everywhere;
    /// they must never be re-routed to AL2023.
    /// What: trusty-memory on a below-floor x86_64 host → native suffix.
    /// Test: This is the test.
    #[test]
    fn select_non_ort_never_switches() {
        let choice = select_asset_suffix("trusty-memory", TARGET_LINUX_X86_64, Some((2, 28)));
        assert_eq!(choice.suffix, TARGET_LINUX_X86_64);
        assert!(!choice.load_dynamic);
    }

    /// Why: The runtime note must be actionable — it must name ORT_DYLIB_PATH and
    /// the correct arch download.
    /// What: Asserts the x64 note mentions `ORT_DYLIB_PATH` and `x64`, and the
    /// arm64 note mentions `aarch64`.
    /// Test: This is the test.
    #[test]
    fn instructions_mention_dylib_and_arch() {
        let x64 = ort_dylib_instructions(TARGET_LINUX_X86_64);
        assert!(x64.contains("ORT_DYLIB_PATH"));
        assert!(x64.contains("onnxruntime-linux-x64-1.20.1"));

        let arm = ort_dylib_instructions(TARGET_LINUX_ARM64);
        assert!(arm.contains("onnxruntime-linux-aarch64-1.20.1"));
    }
}
