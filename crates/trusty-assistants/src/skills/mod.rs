//! The skill catalog that derives an assistant's tool allowlist.
//!
//! Why (#11632): skill loading, ranking, prompt injection and the skill
//! management CLI were cut with the agent loop, but the durable memory tools
//! re-check every call against the patterns an assistant's `[skills].allow`
//! grants (`tools::assistant_memory::operation_granted`, #7396). That check
//! must not change, so the catalog it reads stays.
//! What: `manifest` (the built-in and authored skill catalog and
//! `effective_tool_patterns`) and `sources` (where authored skills are read
//! from).
//! Test: `manifest::tests`, `sources::tests`.

pub mod manifest;
pub mod sources;
