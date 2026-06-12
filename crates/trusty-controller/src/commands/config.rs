//! `tctl config` — read-only effective merged config for each member.
//!
//! Why: Surfaces the config each member is actually running with — system +
//! project layers merged, secrets redacted (DOC-1 D8 / DOC-3 §7). Read-only;
//! never mutates.
//!
//! What: Phase-0 stub. Phase-1 will dispatch `<binary> config --json
//! --scope all` to each selected member and render the per-member
//! `ConfigData` envelope.
//!
//! Test: `run(&[], false)` does not panic.

use crate::output;

/// Handle `tctl config [<members>…]`.
///
/// Why: Phase-0 entry point for the config verb.
///
/// What: `members` names specific members; empty = all enabled members.
///
/// Test: Call with both member lists and JSON modes; none should panic.
pub fn run(members: &[String], json: bool) {
    let mut label = "config".to_owned();
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
        run(&[], false);
        run(&["trusty-search".to_owned()], true);
    }
}
