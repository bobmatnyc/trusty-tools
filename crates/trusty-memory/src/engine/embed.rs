//! Embedding abstraction (re-export from `trusty_common::embedder`).
//!
//! Why: This module historically owned the `Embedder` trait + `FastEmbedder`
//! implementation. Both moved to trusty-common so trusty-memory and
//! trusty-search ship the same code. #9278 (A5, Q3) made the re-export
//! crate-only: a caller outside the crate names `trusty_common::embedder`, the
//! types' one path.
//! What: Re-exports the unified `Embedder` trait and `FastEmbedder` for the
//! crate's own `crate::engine::embed::*` call sites.
//! Test: Covered upstream in the `embedder` module's own test suite.

pub(crate) use trusty_common::embedder::{Embedder, FastEmbedder};
