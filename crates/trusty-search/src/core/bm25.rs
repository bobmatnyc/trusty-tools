//! BM25 lexical index — thin re-export of `trusty_common::bm25` (issue #156).
//!
//! Why: the BM25 implementation moved into `trusty-common` so the new
//! per-palace `trusty-bm25-daemon` subprocess and trusty-search both consume
//! the same tokenizer + scorer. Keeping this module as a re-export avoids
//! churning every call site in trusty-search.
//!
//! What: re-exports `tokenize` and the `BM25Index` struct under the historic
//! `Bm25Index` alias used throughout the indexer. The `bm25` feature on
//! `trusty-common` must be enabled (it is — see this crate's `Cargo.toml`).
//!
//! Test: covered by the original test suite (which now lives in
//! `crates/trusty-common/src/bm25.rs`) plus the integration tests in this
//! crate that exercise the re-exported types.

pub use trusty_common::bm25::{tokenize, BM25Index as Bm25Index};
