//! Console-facing MCP tool descriptors (#1222 / P2).
//!
//! Why: trusty-console renders the Sessions tab by polling the daemon over MCP
//! (#1104 keeps HTTP only in the console). It needs three tools the original
//! catalog lacked: the service-agnostic `console_metrics` report, a
//! `supervisor_status` fleet snapshot, and `auto_resume_set` — the non-CLI
//! control for enabling/disabling supervisor auto-resume (RFC §6 Q6). Keeping
//! their descriptors here keeps `tools/core.rs` and `tools/session.rs` each well
//! under the 500-SLOC cap.
//! What: [`console_tools`] returns the three `{ name, description, inputSchema }`
//! descriptors in catalog order. The shared [`tool`] builder is re-exported from
//! the parent module.
//! Test: `super::tests::console_tools_present`,
//! `super::tests::catalog_names_match_constant`.

use serde_json::{Value, json};

use super::tool;

/// Build the three console-facing tool descriptors.
///
/// Why: the console poller (`console_metrics`), the Sessions supervisor widget
/// (`supervisor_status`), and the auto-resume toggle (`auto_resume_set`) each map
/// to one descriptor here; a dedicated builder keeps the catalog modular.
/// What: returns the three descriptors. `console_metrics` and `supervisor_status`
/// take no arguments; `auto_resume_set` requires a boolean `enabled`. All schemas
/// set `additionalProperties: false`.
/// Test: `super::tests::console_tools_present`.
pub(super) fn console_tools() -> Vec<Value> {
    vec![
        tool(
            "console_metrics",
            "Return the standard trusty-console metrics report for trusty-mpm: \
             service id, display name, version, coarse health status, and a \
             `metrics` payload carrying the managed-session fleet snapshot \
             (counts by lifecycle state) and the supervisor auto-resume control \
             state. Polled uniformly by trusty-console for the dashboard.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool(
            "supervisor_status",
            "Return the managed-session fleet snapshot and the supervisor \
             auto-resume control state as `{ fleet, auto_resume }`. `fleet` carries \
             counts by lifecycle state (provisioning/active/stopped/errored/\
             decommissioned), pending decisions, and last activity; `auto_resume` \
             carries the persisted desired flag, the supervisor's boot-time env \
             flag, and whether a restart is pending.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool(
            "auto_resume_set",
            "Enable or disable supervisor auto-resume by persisting the operator's \
             desired flag to `~/.trusty-mpm/auto_resume`. The 24/7 supervisor reads \
             this on its next sweep; the env var the supervisor booted with stays \
             in force until then (the response's `pending_restart` flags the \
             difference). This is the console's non-CLI auto-resume control.",
            json!({
                "type": "object",
                "properties": {
                    "enabled": {
                        "type": "boolean",
                        "description": "True to enable auto-resume of stopped sessions; false to disable."
                    }
                },
                "required": ["enabled"],
                "additionalProperties": false
            }),
        ),
    ]
}
