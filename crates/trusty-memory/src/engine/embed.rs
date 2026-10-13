//! Embedding abstraction (re-export from `trusty-embedder`).
//!
//! Why: This module historically owned the `Embedder` trait + `FastEmbedder`
//! implementation. Both have moved to the shared `trusty-embedder` crate so
//! trusty-memory and trusty-search ship the same code. This file now exists
//! purely as a stable re-export surface — existing call sites importing
//! `trusty_memory::engine::embed::{Embedder, FastEmbedder}` keep working.
//! What: Re-exports the unified `Embedder` trait, `FastEmbedder`, and the
//! `EMBED_DIM` constant from `trusty_common::embedder` (the absorbed embedder
//! surface).
//! Test: Covered upstream in the `embedder` module's own test suite.

pub use trusty_common::embedder::{EMBED_DIM, Embedder, FastEmbedder, embed_one};
