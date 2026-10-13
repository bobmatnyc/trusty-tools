//! Embedding abstraction (re-export from `trusty-embedder`).
//!
//! Why: This module historically owned the `Embedder` trait + `FastEmbedder`
//! implementation. Both have moved to the shared `trusty-embedder` crate so
//! trusty-memory and trusty-search ship the same code. This file now exists
//! purely as a stable re-export surface — existing call sites importing
//! `trusty_common::memory_core::embed::{Embedder, FastEmbedder}` keep working.
//! What: Re-exports the unified `Embedder` trait, `FastEmbedder`, and the
//! `EMBED_DIM` constant from `crate::embedder` (the absorbed embedder
//! surface).
//! Test: Covered upstream in the `embedder` module's own test suite.

pub use crate::embedder::{EMBED_DIM, Embedder, FastEmbedder, embed_one};
