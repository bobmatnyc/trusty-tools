//! `tctl port` — report the controller's own bound port/address.
//!
//! Why: Provides a clean, scriptable way to discover the controller's HTTP
//! port/address without parsing log output (DOC-7 / DOC-5 §1.2). Follows
//! the `trusty-search port` / `trusty-memory port` pattern already in the
//! repo.
//!
//! What: Phase-0 stub. Phase-1 will read the controller's own `port.lock`
//! file (or the state dir) and print the bound port or `host:port` string.
//!
//! Test: `run(false, false, false)` does not panic.

use crate::output;

/// Handle `tctl port [--addr] [--json]`.
///
/// Why: Phase-0 entry point for the port-reporting command.
///
/// What: `addr` = emit `host:port`; `json_port` = emit `{"addr":…,"port":N}`.
/// Both are stubs in Phase 0.
///
/// Test: Call with all flag combinations; none should panic.
pub fn run(addr: bool, json_port: bool, json: bool) {
    let mut label = "port".to_owned();
    if addr {
        label.push_str(" --addr");
    }
    if json_port {
        label.push_str(" --json-port");
    }
    output::print_not_yet_implemented(&label, json);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_panic() {
        run(false, false, false);
        run(true, false, false);
        run(false, true, false);
        run(false, false, true);
    }
}
