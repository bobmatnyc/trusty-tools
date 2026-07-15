//! Google Slides service.
//!
//! Why: Slides API surface is small for agent workflows: read deck, create
//! deck/slide, add content. We expose those three tools.
//! What: Re-exports `core` (fetch/search + structural ops) and `content`
//! (typed content authoring) sub-modules.
//! Test: Per-sub-module.

pub mod content;
pub mod core;
