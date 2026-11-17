//! #10548: the exact `node --check <file>` shape, split out of
//! `pm_guard_secret_script.rs` for the 500-SLOC cap.

use crate::commands::pm_guard_bash::{RedirectRole, input_redirect_operand, redirect_role};

/// Whether node's arguments `args` are exactly `--check <file>` or
/// `-c <file>`: a syntax check that parses the file and never runs it.
///
/// Why: a syntax check cannot read a secret, so its body is not judged
/// (#10548). Any other shape can run code — `node --check inspect a.cjs`
/// dispatches the `inspect` subcommand, which runs `a.cjs`.
/// What: an allowlist. With output redirects (`2>&1`, `> out`) set aside, the
/// words must be exactly one `--check`/`-c` then one operand. Any other
/// option, operand or subcommand, an input redirect or a here-document
/// answers false, and the command takes the run path.
/// Test: `node_check_of_a_member_access_body_allows_10548`,
/// `a_node_run_beside_a_check_is_judged_10548`.
pub(super) fn node_check_only(args: &[String]) -> bool {
    let mut words = Vec::new();
    let mut i = 0;
    while let Some(token) = args.get(i) {
        i += 1;
        if token.starts_with("<<") || input_redirect_operand(token).is_some() {
            return false;
        }
        match redirect_role(token) {
            RedirectRole::None => words.push(token.as_str()),
            // #10548: an output redirect's target is no node operand.
            RedirectRole::TargetFollows => i += 1,
            _ => {}
        }
    }
    // #10548 round 3: the exact shape only, never a denylist of subcommands.
    matches!(words.as_slice(), ["--check" | "-c", file] if !file.starts_with('-'))
}
