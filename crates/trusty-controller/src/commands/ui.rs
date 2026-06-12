//! `tctl ui` — print or open the controller web-UI URL.
//!
//! Why: Provides a clean entry point for the DOC-7 web UI (DOC-5 §1.2). The
//! URL is derived from `tctl port`; this command either prints it or launches
//! the user's default browser.
//!
//! What: Phase-0 stub. Phase-1 will read the port from the state dir and
//! either print `http://127.0.0.1:<port>` or call `open::that` (the `open`
//! crate already in the workspace) unless `--print` is set.
//!
//! Test: `run(false, false)` does not panic.

use crate::output;

/// Handle `tctl ui [--print]`.
///
/// Why: Phase-0 entry point for the web-UI URL command.
///
/// What: `print` = skip browser launch, just print the URL. `json` = machine
/// output. Both are stubs in Phase 0.
///
/// Test: Call with both `print` values; neither should panic.
pub fn run(print: bool, json: bool) {
    let cmd = if print { "ui --print" } else { "ui" };
    output::print_not_yet_implemented(cmd, json);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_panic() {
        run(false, false);
        run(true, true);
    }
}
