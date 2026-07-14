//! Gmail service sub-modules.
//!
//! Why: Gmail surface is large; we split by concern (messages, labels,
//! filters/organize, settings) to keep each file under 800 lines.
//! What: Re-exports each sub-module so callers see `gmail::compose_email`,
//! `gmail::manage_gmail_labels`, etc.
//! Test: Each sub-module has its own tests where applicable.

pub mod compose;
pub mod labels;
pub mod messages;
pub mod organize;
pub mod settings;
