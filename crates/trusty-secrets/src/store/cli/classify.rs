//! [`Verdict`]: what a finished CLI run means, read from its exit status and
//! its stderr, which is then dropped.
//!
//! Why: #7519 A3 — a missing item must read as a miss, and a locked,
//! signed-out or unknown failure must never read as one. Vendor CLIs say
//! which through stderr text, and stderr can echo a value, so the text is
//! matched against fixed markers and never kept.
//! What: [`Verdict`] and the crate-private `classify`.
//! Test: `classify_marker_table`, `runner_stderr_markers_map_to_verdicts`,
//! `runner_echoed_stdin_never_reaches_an_error`.

use super::spec::CliSpec;

/// What a finished run means.
///
/// What: `Ok` is a zero exit. A non-zero exit is `Locked` or `Missing` only
/// when the run had no stdin and its stderr carries one of the backend's
/// markers; anything else is `Other`, which callers treat as an error.
/// Test: `classify_marker_table`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// The CLI exited zero.
    Ok,
    /// The item does not exist.
    Missing,
    /// The CLI is locked or signed out.
    Locked,
    /// Any other failure.
    Other,
}

/// Classify one finished run.
///
/// Why: see the module docs. A run fed a value on stdin may echo it to
/// stderr, so its stderr is never read for meaning: Ok or Other only.
/// What: zero exit → `Ok`; stdin run → `Other`; else locked markers, then
/// missing markers, matched ASCII case-insensitively; else `Other`.
/// Test: `classify_marker_table`.
pub(crate) fn classify(success: bool, stderr: &[u8], had_stdin: bool, spec: &CliSpec) -> Verdict {
    if success {
        return Verdict::Ok;
    }
    // #7519: a value-bearing run's stderr may echo the value.
    if had_stdin {
        return Verdict::Other;
    }
    // #7519: locked first, so a signed-out CLI is never read as a miss.
    if contains_any(stderr, spec.locked_markers) {
        return Verdict::Locked;
    }
    if contains_any(stderr, spec.missing_markers) {
        return Verdict::Missing;
    }
    Verdict::Other
}

fn contains_any(haystack: &[u8], markers: &[&str]) -> bool {
    markers.iter().any(|marker| {
        let needle = marker.as_bytes();
        !needle.is_empty()
            && haystack
                .windows(needle.len())
                .any(|w| w.eq_ignore_ascii_case(needle))
    })
}
