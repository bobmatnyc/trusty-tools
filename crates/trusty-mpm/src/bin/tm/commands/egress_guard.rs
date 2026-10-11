//! Seam (#8454 S1): `tm hook --egress-guard`. Implementation follows.

use crate::commands::egress_guard_families::FamilyTable;
use crate::commands::hook_stdin::HookStdinError;

/// The guard's decision for one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Let the call through the normal permission flow.
    Allow,
    /// Refuse the call; `check` is the audit slug.
    Deny {
        check: &'static str,
        reason: String,
    },
}

/// Seam: decide one call.
pub(crate) fn decide(
    _read: &Result<serde_json::Value, HookStdinError>,
    _table: &FamilyTable,
) -> Verdict {
    Verdict::Allow
}

/// Seam: run `run`, mapping a panic to a deny.
pub(crate) async fn deny_on_panic<F>(run: F) -> Option<serde_json::Value>
where
    F: std::future::Future<Output = Option<serde_json::Value>>,
{
    run.await
}

/// Seam: `tm hook --egress-guard` entry point.
pub(crate) async fn egress_guard(_url: &str) -> anyhow::Result<()> {
    anyhow::bail!("tm hook --egress-guard is not implemented yet (#8454)")
}

#[cfg(test)]
#[path = "egress_guard_tests.rs"]
mod tests;
