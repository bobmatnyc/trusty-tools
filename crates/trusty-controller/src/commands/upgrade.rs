//! `tctl upgrade` — move members to BOM-pinned versions and restart.
//!
//! Why: The mutating half of the UUC3 flow (DOC-9 §3). After install, upgrade
//! is the primary mechanism for moving the stack to a new known-good tuple.
//!
//! What: Phase-0 stub. Phase-1 will implement the full DOC-9 flow:
//! detect → changelog + blast-radius warn → per-member `cargo install --locked`
//! / `uv tool upgrade` → health-gate → connection-safe restart → verify-after.
//!
//! The backing primitives in `trusty_common::update` (`perform_upgrade`,
//! `verify_installed_binary`, `upgrade_and_restart`, `is_launchd_supervised`)
//! are already available and will be composed here.
//!
//! Test: `run(false, false, false, false, &[], false)` does not panic.

use crate::output;

/// Handle `tctl upgrade [<members>…] [--check] [--latest] [--exclude-self]`.
///
/// Why: Phase-0 entry point for the upgrade action (DOC-9 §3).
///
/// What: All parameters are accepted and validated by clap; the underlying
/// implementation is a Phase-0 stub.
///
/// - `check`: dry-run mode (equivalent to `tctl updates`).
/// - `latest`: target crates.io HEAD rather than the BOM pin.
/// - `exclude_self`: skip self-upgrade of `tctl` itself (DOC-9 §8).
/// - `members`: named subset; empty = all enabled members.
/// - `yes`: bypass blast-radius confirmation (DOC-3 §5 / DOC-5 §3.3).
///
/// Test: Call with various combinations; none should panic.
pub fn run(
    check: bool,
    latest: bool,
    exclude_self: bool,
    yes: bool,
    members: &[String],
    json: bool,
) {
    let mut label = "upgrade".to_owned();
    if check {
        label.push_str(" --check");
    }
    if latest {
        label.push_str(" --latest");
    }
    if exclude_self {
        label.push_str(" --exclude-self");
    }
    if yes {
        label.push_str(" --yes");
    }
    if !members.is_empty() {
        label.push(' ');
        label.push_str(&members.join(" "));
    }
    output::print_not_yet_implemented(&label, json);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_panic() {
        run(false, false, false, false, &[], false);
        run(true, false, false, false, &[], false);
        run(false, true, false, false, &[], true);
        run(
            false,
            false,
            false,
            true,
            &["trusty-search".to_owned()],
            false,
        );
    }
}
