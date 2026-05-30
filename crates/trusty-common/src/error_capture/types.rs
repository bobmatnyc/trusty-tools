//! Captured-error record type and serialisation.
//!
//! Why: A single canonical struct for every ERROR-level event tapped by
//!      [`BugCaptureLayer`] lets Phase 2 consume a stable, versioned schema
//!      without coupling the layer implementation to the query API.
//! What: [`CapturedError`] carries timestamp, crate attribution, message,
//!      code location, OS/arch, and a dedup fingerprint. Fully `serde`-round-
//!      trippable so it can be stored as JSON-Lines and read back.
//! Test: see the `tests` module in `mod.rs` — serde round-trips and field
//!      coverage are exercised there via the full layer integration path.

use serde::{Deserialize, Serialize};

/// A single captured ERROR-level tracing event.
///
/// Why: Phase 2 (MCP + HTTP surface) will read these records and present them
///      to the user for consent-gated GitHub filing. The struct must carry
///      everything needed for a useful bug report without any further lookups.
/// What: all fields derive `Clone`, `Debug`, `Serialize`, `Deserialize` so
///      records travel across the ring-buffer → JSONL → Phase-2 query boundary
///      with no extra conversion step.
/// Test: `captured_error_serde_round_trip` in the parent module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapturedError {
    /// Unix timestamp (seconds since epoch) at the moment the event was
    /// recorded. Stored as `u64` (no negative epoch, no sub-second
    /// precision needed for dedup purposes).
    pub timestamp_secs: u64,

    /// The `target` of the tracing event — typically the crate/module path
    /// that emitted the error (e.g. `trusty_search::indexer`). This is the
    /// correct attribution even when the event fires in shared library code.
    pub crate_target: String,

    /// Version string of the host binary, captured at layer-install time via
    /// the caller supplying `env!("CARGO_PKG_VERSION")`. May be `"unknown"`.
    pub crate_version: String,

    /// Formatted error message (the tracing event's `message` field).
    pub message: String,

    /// Additional key=value fields from the event, formatted as
    /// `"key=value key2=value2"`. Empty string when no extra fields present.
    pub fields: String,

    /// Source file path from tracing metadata, if available (e.g.
    /// `"src/indexer.rs"`). `None` when the event metadata lacks location.
    pub file: Option<String>,

    /// Source line number from tracing metadata. `None` when unavailable.
    pub line: Option<u32>,

    /// Operating system name from `std::env::consts::OS`
    /// (e.g. `"macos"`, `"linux"`, `"windows"`).
    pub os: String,

    /// CPU architecture from `std::env::consts::ARCH`
    /// (e.g. `"aarch64"`, `"x86_64"`).
    pub arch: String,

    /// SHA-256 fingerprint (hex, 64 chars) over
    /// `(crate_target + normalised_message + location)`.
    /// Two events that differ only in digits, hex strings, or path prefixes
    /// produce the **same** fingerprint — enabling dedup without exact matching.
    pub fingerprint: String,
}

impl CapturedError {
    /// Construct a human-readable one-line summary for display / logging.
    ///
    /// Why: Phase 2 `list_recent_errors` wants a concise line per record
    ///      without re-implementing formatting.
    /// What: returns `"[<crate_target>] <message> (<file>:<line>)"`.
    /// Test: `captured_error_summary_format` in the parent module.
    #[must_use]
    pub fn summary(&self) -> String {
        let loc = match (&self.file, self.line) {
            (Some(f), Some(l)) => format!(" ({f}:{l})"),
            (Some(f), None) => format!(" ({f})"),
            _ => String::new(),
        };
        format!("[{}] {}{}", self.crate_target, self.message, loc)
    }
}
