//! Human and JSON output renderers for `tctl`.
//!
//! Why: Centralising rendering logic avoids scattering `println!`/`serde_json`
//! calls across every command handler, and makes the output shapes testable in
//! isolation.
//!
//! What: `render_json` serialises any `serde::Serialize` value to stdout.
//! `print_not_yet_implemented` renders the Phase-0 stub message in both human
//! and JSON modes. Both helpers enforce the repo's stdout-for-data /
//! stderr-for-logs rule (CLAUDE.md).
//!
//! Test: `render_json` on a simple struct writes valid JSON followed by `\n`;
//! `print_not_yet_implemented` with `json = true` emits `{"status":"not_yet_implemented",...}`.

use anyhow::Result;
use serde::Serialize;

/// Serialise `value` as pretty-printed JSON to stdout and append a newline.
///
/// Why: DOC-5 §4.2 requires all `--json` output to go to stdout so callers can
/// pipe it. Using a single helper ensures consistent formatting across commands.
///
/// What: Calls `serde_json::to_string_pretty`, writes to stdout, and appends
/// `\n`. Returns an error if serialisation fails (never in practice for
/// well-typed structs).
///
/// Test: Call with a small `serde_json::json!({…})` value and assert the output
/// contains the expected keys on stdout (redirect stdout in the test).
pub fn render_json<T: Serialize>(value: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    println!("{json}");
    Ok(())
}

/// Print a structured "not yet implemented" result for a Phase-0 command stub.
///
/// Why: Phase-0 commands that are not yet fully implemented must not panic or
/// return an opaque error — they must return a structured, discoverable result
/// so the CLI surface is complete and testable (per task spec).
///
/// What: Constructs a `NotYetImplemented` value for `command`. When `json` is
/// `true`, serialises it to stdout via `render_json`. When `json` is `false`,
/// prints a one-line human message. Using `NotYetImplemented` as the canonical
/// stub type ensures the JSON shape is consistent and testable in isolation.
///
/// Test: Call with `json = true` and capture stdout; assert the JSON contains
/// `"command"` and `"phase"` keys matching the constructed value.
pub fn print_not_yet_implemented(command: &str, json: bool) {
    let nyi = NotYetImplemented::new(command);
    if json {
        // Safe: `NotYetImplemented` is a well-typed struct; serialisation
        // cannot fail. Falls back to empty string on the impossible error.
        println!("{}", serde_json::to_string_pretty(&nyi).unwrap_or_default());
    } else {
        println!(
            "tctl {}: not yet implemented (Phase 0 scaffold). \
             See https://github.com/bobmatnyc/trusty-tools/issues/920 for the roadmap.",
            nyi.command
        );
    }
}

/// Print a version line to stdout, in human or JSON format.
///
/// Why: `tctl version` is the one command that is fully implemented in Phase 0
/// (it is the capability-discovery entry point, DOC-5 §4.2 / DOC-1 D3b).
///
/// What: Emits either a human `tctl v<version>  stack_version: <stack>  contract_floor: 1`
/// line, or the JSON capability-discovery object
/// `{"tool","tool_version","stack_version","contract_floor","contract_target"}`.
///
/// Test: Call with `json = false` and check stdout contains `"tctl v"`;
/// call with `json = true` and parse the output as JSON, asserting `"tool"` == `"trusty-controller"`.
pub fn print_version(json: bool, stack_version: &str) {
    let tool_version = env!("CARGO_PKG_VERSION");
    if json {
        let obj = serde_json::json!({
            "tool": "trusty-controller",
            "tool_version": tool_version,
            "stack_version": stack_version,
            "contract_floor": 1,
            "contract_target": 1
        });
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
    } else {
        println!("tctl v{tool_version}  stack_version: {stack_version}  contract_floor: 1");
    }
}

/// A Phase-0 stub result returned by commands that are not yet implemented.
///
/// Why: Returning a typed value from command handlers (rather than just printing)
/// makes them testable without capturing stdout.
///
/// What: Carries the command name and a constant phase marker so callers can
/// assert on the returned data in unit tests.
#[derive(Debug, Clone, Serialize)]
pub struct NotYetImplemented {
    /// The command that returned this result.
    pub command: String,
    /// Phase marker — always `"0"` for Phase-0 stubs.
    pub phase: &'static str,
    /// Human-readable explanation.
    pub message: String,
}

impl NotYetImplemented {
    /// Construct a `NotYetImplemented` result for the given command name.
    ///
    /// Why: A constructor avoids duplicating the boilerplate message template
    /// across all stub handlers.
    ///
    /// What: Fills in `command`, `phase = "0"`, and a standard `message`.
    ///
    /// Test: `NotYetImplemented::new("install").command` == `"install"`.
    pub fn new(command: &str) -> Self {
        Self {
            command: command.to_owned(),
            phase: "0",
            message: format!(
                "`tctl {command}` is scaffolded but not yet fully implemented. \
                 See issue #920."
            ),
        }
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `NotYetImplemented::new` round-trips through JSON.
    #[test]
    fn not_yet_implemented_serialises() {
        let nyi = NotYetImplemented::new("install");
        let json = serde_json::to_string(&nyi).expect("serialises");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parses");
        assert_eq!(v["command"], "install");
        assert_eq!(v["phase"], "0");
    }

    /// `render_json` does not panic on a simple value.
    #[test]
    fn render_json_does_not_panic() {
        let obj = serde_json::json!({"key": "value"});
        // We can't capture stdout in a unit test without external machinery,
        // but we can verify it returns `Ok`.
        assert!(render_json(&obj).is_ok());
    }
}
