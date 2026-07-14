//! Google Docs service sub-modules.
//!
//! Why: Docs has a rich `batchUpdate` API that splits cleanly along editing
//! concern lines (core content, comments, formatting, tables).
//! What: Each sub-module exposes functions that produce a `batchUpdate`
//! request body and POST it.
//! Test: Per-sub-module.

pub mod comments;
pub mod core;
pub mod formatting;
pub mod header_footer;
pub mod images;
pub mod paragraphs;
pub mod table_format;
pub mod table_ops;
pub mod table_preset;
pub mod table_style;
pub mod tabs;
pub mod templates;
