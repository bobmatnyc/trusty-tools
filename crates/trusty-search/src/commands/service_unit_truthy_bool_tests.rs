//! Tests for [`super::parse_truthy_bool`] (issue #4823).
//!
//! Why: kept in its own sibling file so `service_unit.rs` stays under the
//! 500-SLOC production cap (`scripts/check_line_cap.sh`) — see the split note
//! in that file's module doc.
//! What: every accepted truthy/falsey spelling, and that an unrecognised
//! spelling is rejected rather than defaulted.
//! Test: this file — `cargo test -p trusty-search parse_truthy_bool`.

use super::*;

/// Why: `TRUSTY_NO_AUTO_DISCOVER=1` is the spelling the README and the
/// #314 changelog document, and it is what already sits in the reporter's
/// `daemon.env`. Rejecting it aborts daemon startup (#4823 comment).
/// What: asserts every accepted truthy and falsey spelling, case- and
/// whitespace-insensitively.
/// Test: this function.
#[test]
fn parse_truthy_bool_accepts_numeric_and_word_spellings() {
    for raw in ["1", "true", "TRUE", " True ", "yes", "on"] {
        assert_eq!(
            parse_truthy_bool(raw),
            Ok(true),
            "{raw:?} must parse as true"
        );
    }
    for raw in ["", "0", "false", "FALSE", " no ", "off"] {
        assert_eq!(
            parse_truthy_bool(raw),
            Ok(false),
            "{raw:?} must parse as false"
        );
    }
}

/// Why: loosening the parser must not degrade into "anything unknown is
/// false" — that would silently re-enable a scan the operator disabled.
/// What: asserts a typo is an error, not a default.
/// Test: this function.
#[test]
fn parse_truthy_bool_rejects_garbage() {
    assert!(parse_truthy_bool("ture").is_err());
    assert!(parse_truthy_bool("2").is_err());
    assert!(parse_truthy_bool("enabled").is_err());
}
