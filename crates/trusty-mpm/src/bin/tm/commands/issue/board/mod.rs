//! The standard GitHub Projects board: its embedded spec and the
//! `issue-state.yaml` `project:` block (#10275, epic #10274 Phase 1).
//!
//! Why: every repo that adopts the trusty-mpm workflow gets a board in one
//! standard format. This module holds that format as data compiled into `tm`,
//! plus the schema of the block that names a repo's own board.
//! What: [`spec`] (the `board-v1.yaml` types, embed and loader), [`validate`]
//! (the load-time rules) and [`project`] (the `project:` block and its check
//! against the spec).
//! Test: `board/tests.rs`.

// #10275: Phase 2 (`tm issue board`) is the first non-test reader; until then
// only the tests call into this module.
#![cfg_attr(not(test), allow(dead_code))]

pub(crate) mod project;
pub(crate) mod spec;
pub(crate) mod validate;

#[cfg(test)]
mod tests;
