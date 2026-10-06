//! Tests for the `tm list` row formatters in `standalone.rs` (#9227).
//!
//! Why: `tm list` prints every stored registry URL, including one stored
//! before #9124 that still embeds a token.
//! Test: this IS the test module.

use super::*;

/// The synthetic password; no assertion message here prints a row.
const PASSWORD: &str = "pa'ss9227";

/// Why (#9227): free-text redaction ends the authority at the quote, so the
/// table and the JSON both printed `u:pa'ss9227@host` unchanged.
/// Test: this test.
#[test]
fn fleet_rows_never_print_a_quoted_password() {
    let entry = RegistryEntry {
        alias: "proj".to_owned(),
        url: format!("https://u:{PASSWORD}@host/o/r"),
        git_ref: "default".to_owned(),
        created_at: "2026-10-05T00:00:00Z".to_owned(),
    };
    let table = fleet_table_row(&entry, false, 5);
    let json = fleet_json_row(&entry, false, Path::new("/managed")).to_string();
    for (name, row) in [("table", &table), ("json", &json)] {
        assert!(
            !row.contains("ss9227"),
            "{name}: a password fragment survived"
        );
        assert!(
            !row.contains("u:pa"),
            "{name}: the user:password pair survived"
        );
        assert!(
            row.contains("https://***@host/o/r"),
            "{name}: URL not shown masked"
        );
    }
}
