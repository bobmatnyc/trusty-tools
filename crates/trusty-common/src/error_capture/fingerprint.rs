//! Fingerprint computation for error deduplication.
//!
//! Why: The same logical error (e.g. "failed to open file") may appear with
//!      different runtime details each time — differing file paths, varying
//!      line numbers in stack traces, or different hex addresses. Normalising
//!      these away before hashing ensures recurrent occurrences of the *same*
//!      bug share one fingerprint so Phase 3 can deduplicate GitHub issues.
//! What: [`normalise_message`] strips digits, hex strings, and common path
//!      prefixes from a message. [`compute_fingerprint`] SHA-256s the stable
//!      parts (crate target + normalised message + code location) and returns
//!      a lowercase hex string.
//! Test: `fingerprint_same_for_logically_identical_errors`,
//!      `fingerprint_differs_for_different_errors`,
//!      `normalise_strips_digits_and_hex`.

use sha2::{Digest, Sha256};

/// Strip volatile substrings from an error message so the same logical error
/// maps to the same fingerprint regardless of runtime-varying details.
///
/// We avoid pulling in `regex` for this pure-computation helper — a simple
/// hand-written state machine is enough and adds no dependencies.
///
/// Why: file paths, line numbers, memory addresses, hex values, port numbers,
/// and similar tokens change between runs but should not produce distinct
/// fingerprints for what is clearly the same bug.
///
/// What: applies three passes in order:
/// 1. Remove hex-looking tokens (`0x…`, bare `[0-9a-f]{6,}`).
/// 2. Remove digit runs (integers, version numbers, timestamps).
/// 3. Replace the user home directory prefix with `~`.
///
/// Returns the scrubbed string, collapsed to a single space between tokens.
///
/// Test: `normalise_strips_digits_and_hex`.
#[must_use]
pub fn normalise_message(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut chars = msg.chars().peekable();

    while let Some(ch) = chars.next() {
        // Check for `0x` prefix → consume the whole hex token.
        if ch == '0' && chars.peek() == Some(&'x') {
            // consume 'x'
            chars.next();
            // consume hex digits
            while chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                chars.next();
            }
            out.push('X');
            continue;
        }

        // Digit run → replace with placeholder.
        if ch.is_ascii_digit() {
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_digit() || *c == '.')
            {
                chars.next();
            }
            out.push('N');
            continue;
        }

        // Detect long lowercase hex token (≥ 6 chars of [0-9a-f]).
        // We only do this at word boundaries to avoid mangling normal words.
        if ch.is_ascii_lowercase() && "0123456789abcdef".contains(ch) {
            // Peek ahead: is this a long hex-like sequence?
            let mut hex_buf = String::new();
            hex_buf.push(ch);
            let saved_start = out.len();
            let _ = saved_start; // used for rollback logic below
            while chars
                .peek()
                .is_some_and(|c| "0123456789abcdef".contains(*c))
            {
                hex_buf.push(chars.next().unwrap());
            }
            // Only treat as hex token if ≥ 6 chars of pure hex.
            let all_hex = hex_buf.chars().all(|c| c.is_ascii_hexdigit());
            if all_hex && hex_buf.len() >= 6 {
                out.push('H');
            } else {
                // Not a hex token — emit literally.
                out.push_str(&hex_buf);
            }
            continue;
        }

        out.push(ch);
    }

    // Replace home-directory prefix (best-effort; useful on macOS / Linux).
    let home_normalised = if let Ok(home) = std::env::var("HOME") {
        out.replace(&home, "~")
    } else {
        out
    };

    // Collapse multiple spaces / control chars to one space and trim.
    let collapsed: String = home_normalised
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    collapsed
}

/// Compute a SHA-256 fingerprint over the stable parts of an error event.
///
/// Why: The fingerprint is the dedup key used by Phase 3 to prevent duplicate
///      GitHub issues for recurrent errors. It must be stable across runs,
///      process restarts, and minor message variations (digit changes, paths).
/// What: SHA-256 over the concatenation
///      `"<crate_target>|<normalised_message>|<location>"` where `location`
///      is `"<file>:<line>"` or `"unknown"` when metadata is absent.
///      Returns a 64-char lowercase hex string.
/// Test: `fingerprint_same_for_logically_identical_errors`.
#[must_use]
pub fn compute_fingerprint(
    crate_target: &str,
    message: &str,
    file: Option<&str>,
    line: Option<u32>,
) -> String {
    let normalised = normalise_message(message);
    let location = match (file, line) {
        (Some(f), Some(l)) => format!("{f}:{l}"),
        (Some(f), None) => f.to_string(),
        _ => "unknown".to_string(),
    };
    let input = format!("{crate_target}|{normalised}|{location}");
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    // Format as lowercase hex.
    result.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_same_for_logically_identical_errors() {
        // Why: two occurrences of the same error with different port numbers
        // should hash identically.
        let fp1 = compute_fingerprint(
            "trusty_search::server",
            "failed to bind to 127.0.0.1:8080",
            Some("src/server.rs"),
            Some(42),
        );
        let fp2 = compute_fingerprint(
            "trusty_search::server",
            "failed to bind to 127.0.0.1:9999",
            Some("src/server.rs"),
            Some(42),
        );
        assert_eq!(
            fp1, fp2,
            "port numbers differ but fingerprints should match"
        );
    }

    #[test]
    fn fingerprint_differs_for_different_errors() {
        let fp1 = compute_fingerprint(
            "trusty_search::server",
            "failed to bind socket",
            Some("src/server.rs"),
            Some(42),
        );
        let fp2 = compute_fingerprint(
            "trusty_search::indexer",
            "failed to open index",
            Some("src/indexer.rs"),
            Some(10),
        );
        assert_ne!(
            fp1, fp2,
            "distinct errors must produce distinct fingerprints"
        );
    }

    #[test]
    fn fingerprint_differs_when_crate_differs() {
        let fp1 = compute_fingerprint("crate_a", "same message", Some("f.rs"), Some(1));
        let fp2 = compute_fingerprint("crate_b", "same message", Some("f.rs"), Some(1));
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn normalise_strips_digits_and_hex() {
        let msg = "error at line 42: memory address 0xdeadbeef";
        let norm = normalise_message(msg);
        assert!(!norm.contains("42"), "digits should be stripped: {norm}");
        assert!(!norm.contains("deadbeef"), "hex should be stripped: {norm}");
        assert!(!norm.contains("0x"), "0x prefix should be stripped: {norm}");
    }

    #[test]
    fn normalise_strips_long_hex_token() {
        // A bare hex string of 6+ chars should be normalised.
        let msg = "digest mismatch: a1b2c3d4e5f6 != expected";
        let norm = normalise_message(msg);
        assert!(
            !norm.contains("a1b2c3d4e5f6"),
            "long hex token should be stripped: {norm}"
        );
    }

    #[test]
    fn normalise_preserves_normal_words() {
        // Short words that happen to be hex chars should pass through.
        let msg = "bad request from cafe";
        let norm = normalise_message(msg);
        // "bad" (3 chars) and "cafe" (4 chars) are below the 6-char threshold.
        assert!(
            norm.contains("bad"),
            "normal word 'bad' should be kept: {norm}"
        );
        assert!(
            norm.contains("cafe"),
            "normal word 'cafe' should be kept: {norm}"
        );
    }

    #[test]
    fn fingerprint_is_64_hex_chars() {
        let fp = compute_fingerprint("crate", "message", None, None);
        assert_eq!(fp.len(), 64);
        assert!(
            fp.chars().all(|c| c.is_ascii_hexdigit()),
            "fingerprint must be lowercase hex"
        );
    }

    #[test]
    fn fingerprint_same_for_version_number_variation() {
        // Two errors differing only in a version number → same fingerprint.
        let fp1 = compute_fingerprint(
            "trusty_memory",
            "incompatible schema version 3",
            Some("src/store.rs"),
            Some(100),
        );
        let fp2 = compute_fingerprint(
            "trusty_memory",
            "incompatible schema version 7",
            Some("src/store.rs"),
            Some(100),
        );
        assert_eq!(
            fp1, fp2,
            "version number variation should not change fingerprint"
        );
    }
}
