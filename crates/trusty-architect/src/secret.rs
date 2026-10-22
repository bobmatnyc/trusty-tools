//! The secret check shared by the PM ledger writer and the dashboard store.
//!
//! Why: DOC-82 LDG-2.3.5 refuses a write holding a secret-shaped value in
//! both stores, and the shapes live in `tm` (`scan_for_credentials`), which
//! this leaf crate cannot depend on. So each writer takes a scanner, and the
//! walk over a JSON value is written once (#10063 PR 2: moved out of
//! `ledger/writer/mod.rs`, no behaviour change).
//! What: [`SecretScan`] (a redacted shape name, never the value, for a
//! secret-shaped text) and [`scan_strings`] (the first string in a value,
//! object keys included, that the scanner flags).
//! Test: `ledger_writer_refuses_a_secret_after_an_escaped_newline`,
//! `secret_value_refused_and_not_logged`.

use serde_json::Value;

/// A secret-shape scanner: a redacted shape name (never the value) for a
/// secret-shaped text.
pub type SecretScan<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The first string in `v` (object keys included) that `scan` flags.
pub(crate) fn scan_strings(v: &Value, scan: SecretScan<'_>) -> Option<String> {
    match v {
        Value::String(s) => scan(s),
        Value::Array(a) => a.iter().find_map(|x| scan_strings(x, scan)),
        Value::Object(o) => o
            .iter()
            .find_map(|(k, x)| scan(k).or_else(|| scan_strings(x, scan))),
        _ => None,
    }
}
