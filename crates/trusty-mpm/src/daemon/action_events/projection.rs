//! Hook record to bus event projection, with `parent_id` threading (#6849).
//!
//! Why: DOC-73 §3.4. tm's hooks arrive as Claude Code records — PascalCase
//! names and an untyped payload — and the dashboard tree (§5.2) needs typed
//! actions joined by a causal edge. This module is the adapter between the
//! two: it is the only place that reads Claude Code's payload field names on
//! the way to the bus, so the `ActionEvent` contract in `trusty-common` stays
//! harness-neutral.
//! What: [`Threader::project`] turns one [`HookEventRecord`] into the events
//! it implies (see the table on [`Threader::project`]).
//! Test: `projection_tests.rs`.
//!
//! # Spec References
//! - [`SPEC-UNIDASH-03~draft`](docs/specs/DOC-73-unified-mpm-code-agents-dashboard.md#SPEC-UNIDASH-03~draft)

use std::path::PathBuf;

use trusty_common::control_bus::HarnessEvent;

use crate::core::hook::HookEventRecord;

/// Sessions tracked at once; the least recently touched is evicted.
pub const MAX_SESSIONS: usize = 256;

/// Open tool calls (and open worktrees) tracked per session.
pub const MAX_OPEN_PER_SESSION: usize = 256;

/// Per-session causal context for the projection.
pub struct Threader {
    _home: Option<PathBuf>,
}

impl Threader {
    /// A threader that shows paths outside a session's `cwd` relative to
    /// `home`.
    pub fn new(home: Option<PathBuf>) -> Self {
        Self { _home: home }
    }

    /// Project one hook record onto the bus events it implies.
    pub fn project(&mut self, _record: &HookEventRecord) -> Vec<HarnessEvent> {
        Vec::new()
    }
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
