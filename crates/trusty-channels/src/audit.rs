//! The channel audit record and log, shared by every channel consumer.
//!
//! Why: #9832 (#8433 D4): the tm daemon's route-change log, and its inbound
//! and egress audits (P4, P5), write the same record gchat already writes,
//! so no consumer defines a second record type that could carry text.
//! What: re-exports gchat's [`AuditRecord`], [`AuditEvent`], [`AuditLog`],
//! [`clip`] and the [`StateError`] a write returns.
//! Test: `audit_lines_never_contain_message_text`, `record_all_appends_every_line`.

pub use crate::gchat::error::StateError;
pub use crate::gchat::state::audit::{clip, AuditEvent, AuditLog, AuditRecord};
