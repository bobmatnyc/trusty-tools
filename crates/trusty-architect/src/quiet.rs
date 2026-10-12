//! Sessions the poller should not pester: the port of the prototype's
//! `quiet-sessions.py`.
//!
//! Why: sessions the owner parked still raise an `idle` alert every 30
//! minutes, and throwaway test-fixture sessions raise `exited` when they end
//! (#9171 A2 ports this list and the fixture-name match).
//! What: [`read_quiet_sessions`] reads one session name per line from a file
//! the caller names (`#` starts a comment, blanks ignored), fresh on every
//! call; a missing file is an empty set. [`is_test_fixture_session`] matches
//! the `tm-xtest-*`, `tm-qa*-*` and `tm-r[0-9]*` globs.
//! Test: `tests/quiet_ported.rs`.

use std::collections::BTreeSet;
use std::path::Path;

use crate::pystr::{splitlines, strip};

/// Session names listed in `path`, one per line. Any read failure — a
/// missing file, a permission error, or non-UTF-8 bytes — is an empty set.
///
/// The non-UTF-8 case differs from the prototype, which raises
/// `UnicodeDecodeError` there; the poller treats this list as advisory, so an
/// unreadable list must not stop a cycle.
pub fn read_quiet_sessions(path: &Path) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    splitlines(&text)
        .into_iter()
        .map(|line| strip(line.split('#').next().unwrap_or_default()))
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// True for a throwaway test-fixture session name (`tm-xtest-*`, `tm-qa*-*`,
/// `tm-r[0-9]*`); such sessions never raise an `exited` alert.
pub fn is_test_fixture_session(session: &str) -> bool {
    session.starts_with("tm-xtest-")
        || session
            .strip_prefix("tm-qa")
            .is_some_and(|r| r.contains('-'))
        || session
            .strip_prefix("tm-r")
            .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
}
