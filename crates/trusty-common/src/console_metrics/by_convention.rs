//! The `<svc>.console_metrics` UDS method and the `by_*` breakdown convention
//! (#10906, owner ruling D95).
//!
//! Why: D95 has each service report its own counters over its own socket
//! (ADR-0032: UDS only), pulled by the reader, never pushed. Several services
//! answer, and one reader renders them all, so the method name and the shape
//! of a breakdown are fixed here once rather than per service.
//!
//! # The contract
//!
//! **Method.** A service whose socket serves metrics registers the unary
//! JSON-RPC method `<svc>.console_metrics`, where `<svc>` is the prefix its
//! other methods use: `events.console_metrics` on the trusty-events socket,
//! `console.console_metrics` on the console socket. It accepts any params,
//! including none, and answers a [`super::ConsoleMetricsReport`] as the plain
//! result object: not wrapped in the MCP `content[0].text` envelope that
//! [`super::serialise_report`] builds for the stdio `console_metrics` tool.
//!
//! **`metrics` payload.** The report's `metrics` is a JSON object.
//!
//! 1. A key `by_<dimension>` is a breakdown. `<dimension>` is a singular
//!    snake_case noun (`by_source`, `by_project`, `by_route`).
//! 2. A breakdown's value is a JSON object, never an array. Its keys are that
//!    dimension's values as strings; it is `{}` when nothing was counted, and
//!    the key is never omitted.
//! 3. A breakdown's entries are either all counts (non-negative integers) or
//!    all body objects. One breakdown never mixes the two.
//! 4. A body object may hold breakdowns of its own (`by_source.mpm.by_domain`),
//!    under the same rules.
//! 5. Every other key (`uptime_secs`, a total, a named sub-object such as
//!    `messages`) is a plain field. A plain object field may hold breakdowns.
//! 6. Breakdown keys come from a bounded set. A value outside it is counted
//!    under a reserved key the service documents (for example
//!    [`crate::control_bus::UNATTRIBUTED_PROJECT`]), so a caller cannot grow
//!    the maps without limit.
//! 7. Adding a key is additive. Renaming or removing one, or changing a
//!    breakdown's entry form, bumps
//!    [`super::ConsoleMetricsReport::metrics_schema_version`].
//!
//! [`check_by_convention`] checks rules 1 to 5 mechanically, so each service
//! tests its payload against the same definition.
//!
//! Test: `super::by_convention_tests`.

use std::fmt;

use serde_json::{Map, Value};

/// The prefix that marks a `metrics` key as a breakdown.
pub const BY_PREFIX: &str = "by_";

/// The first place a `metrics` payload breaks the `by_*` convention.
///
/// What: `path` is the dotted key path to the offending value (`metrics` for
/// the root); `reason` says which rule failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByConventionViolation {
    /// Dotted key path to the offending value.
    pub path: String,
    /// The rule that failed.
    pub reason: &'static str,
}

impl fmt::Display for ByConventionViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

impl std::error::Error for ByConventionViolation {}

/// Check a report's `metrics` against the `by_*` convention (module docs,
/// rules 1 to 5).
///
/// Why: a service and its readers agree on the shape by testing against one
/// checker, not by each restating the rules.
/// What: `metrics` must be an object. Every `by_<dimension>` key, at any depth
/// reached through objects, must name a dimension and hold an object whose
/// entries are all counts or all objects; object entries and plain object
/// fields are checked recursively.
///
/// # Errors
///
/// The first [`ByConventionViolation`] found, in key order.
///
/// Test: `super::by_convention_tests::a_conforming_payload_passes`,
/// `super::by_convention_tests::each_rule_violation_is_reported_with_its_path`.
pub fn check_by_convention(metrics: &Value) -> Result<(), ByConventionViolation> {
    let Some(object) = metrics.as_object() else {
        return Err(violation("metrics", "metrics is not a JSON object"));
    };
    check_object("metrics", object)
}

fn check_object(path: &str, object: &Map<String, Value>) -> Result<(), ByConventionViolation> {
    for (key, value) in object {
        let here = format!("{path}.{key}");
        if let Some(dimension) = key.strip_prefix(BY_PREFIX) {
            if dimension.is_empty() {
                return Err(violation(&here, "a breakdown key names no dimension"));
            }
            check_breakdown(&here, value)?;
        } else if let Some(inner) = value.as_object() {
            check_object(&here, inner)?;
        }
    }
    Ok(())
}

fn check_breakdown(path: &str, value: &Value) -> Result<(), ByConventionViolation> {
    let Some(entries) = value.as_object() else {
        return Err(violation(path, "a breakdown is not a JSON object"));
    };
    let counts = entries.values().filter(|v| v.is_u64()).count();
    let bodies = entries.values().filter(|v| v.is_object()).count();
    if counts + bodies != entries.len() {
        return Err(violation(
            path,
            "a breakdown entry is neither a count nor an object",
        ));
    }
    if counts > 0 && bodies > 0 {
        return Err(violation(path, "a breakdown mixes counts and objects"));
    }
    for (key, entry) in entries {
        if let Some(body) = entry.as_object() {
            check_object(&format!("{path}.{key}"), body)?;
        }
    }
    Ok(())
}

fn violation(path: &str, reason: &'static str) -> ByConventionViolation {
    ByConventionViolation {
        path: path.to_string(),
        reason,
    }
}
