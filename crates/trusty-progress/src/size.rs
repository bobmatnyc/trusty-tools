//! Human-readable byte-size formatting.
//!
//! Centralises the rustup-style size column (`8.46 MiB`) so every caller
//! renders sizes identically.

/// Bytes in one binary kibibyte.
const KIB: u64 = 1024;
/// Bytes in one binary mebibyte.
const MIB: u64 = KIB * 1024;
/// Bytes in one binary gibibyte.
const GIB: u64 = MIB * 1024;
/// Bytes in one binary tebibyte.
const TIB: u64 = GIB * 1024;

/// Why: The rustup component table shows every size with two decimal places in
/// MiB (`8.46 MiB`); reproducing that exactly keeps the shared style faithful
/// to the target output Bob specified in #1315.
/// What: Formats a raw byte count as a fixed two-decimal mebibyte string,
/// e.g. `8_870_000` → `"8.46 MiB"`. Always uses MiB regardless of magnitude so
/// the component column stays aligned (matching rustup's behaviour).
/// Test: `size::tests::mib_matches_rustup_samples` pins the exact strings from
/// the issue (`8.46 MiB`, `2.89 MiB`, `26.20 MiB`).
pub fn format_mib(bytes: u64) -> String {
    let mib = bytes as f64 / MIB as f64;
    format!("{mib:.2} MiB")
}

/// Why: For general-purpose progress (downloads, indexing) a fixed-MiB column
/// reads poorly across many orders of magnitude; callers that are not
/// rendering the rustup component table want an auto-scaled unit instead.
/// What: Formats a byte count with the largest binary unit that keeps the
/// mantissa below 1024, with two decimals for KiB and up (whole bytes for
/// `< 1 KiB`). e.g. `512` → `"512 B"`, `1536` → `"1.50 KiB"`,
/// `1_572_864` → `"1.50 MiB"`.
/// Test: `size::tests::auto_scales_across_units` covers each unit boundary.
pub fn format_bytes(bytes: u64) -> String {
    if bytes < KIB {
        return format!("{bytes} B");
    }
    let (value, unit) = if bytes < MIB {
        (bytes as f64 / KIB as f64, "KiB")
    } else if bytes < GIB {
        (bytes as f64 / MIB as f64, "MiB")
    } else if bytes < TIB {
        (bytes as f64 / GIB as f64, "GiB")
    } else {
        (bytes as f64 / TIB as f64, "TiB")
    };
    format!("{value:.2} {unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The two-decimal MiB column is the load-bearing visual detail of the
    /// rustup style; these exact strings appear in the issue's target output.
    /// What: Asserts `format_mib` reproduces the three sample sizes.
    /// Test: This is the test.
    #[test]
    fn mib_matches_rustup_samples() {
        // 8.46 MiB ≈ 8.46 * 1048576 = 8_870_953 bytes (rounds to 8.46).
        assert_eq!(format_mib(8_870_953), "8.46 MiB");
        // 2.89 MiB ≈ 3_030_384 bytes.
        assert_eq!(format_mib(3_030_384), "2.89 MiB");
        // 26.20 MiB ≈ 27_472_691 bytes.
        assert_eq!(format_mib(27_472_691), "26.20 MiB");
    }

    /// Why: Zero and sub-MiB inputs must not panic or render `NaN`; the column
    /// should still show two decimals so it stays aligned.
    /// What: Asserts edge values format predictably.
    /// Test: This is the test.
    #[test]
    fn mib_handles_small_values() {
        assert_eq!(format_mib(0), "0.00 MiB");
        assert_eq!(format_mib(MIB), "1.00 MiB");
        assert_eq!(format_mib(MIB / 2), "0.50 MiB");
    }

    /// Why: The auto-scaling helper must pick the right unit at each binary
    /// boundary so general progress readouts are legible.
    /// What: Checks every unit tier and the whole-byte path.
    /// Test: This is the test.
    #[test]
    fn auto_scales_across_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(KIB + KIB / 2), "1.50 KiB");
        assert_eq!(format_bytes(MIB + MIB / 2), "1.50 MiB");
        assert_eq!(format_bytes(GIB + GIB / 2), "1.50 GiB");
        assert_eq!(format_bytes(TIB + TIB / 2), "1.50 TiB");
    }
}
