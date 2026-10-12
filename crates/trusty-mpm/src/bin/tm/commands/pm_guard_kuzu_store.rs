//! `tm hook --pm-guard` — the kuzu-store deny rule (#8625). Red-first
//! skeleton: the rule allows everything until the implementation lands.

use std::path::Path;

use serde_json::Value;

/// The audit label of a kuzu-store deny (ruling Q7).
pub(crate) const KUZU_STORE_RULE: &str = "kuzu-store-read";

/// The kuzu-store decision for one tool call: `Some(reason)` denies.
pub(crate) fn evaluate_kuzu_store_access(
    _tool_name: &str,
    _tool_input: Option<&Value>,
    _cwd: &Path,
    _home: Option<&Path>,
) -> Option<String> {
    None
}

#[cfg(test)]
#[path = "pm_guard_kuzu_store_tests.rs"]
mod tests;
