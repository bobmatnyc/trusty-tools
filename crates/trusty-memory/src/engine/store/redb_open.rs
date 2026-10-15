//! Memory-core's view of the shared redb open-with-quarantine surface (#702).
//!
//! Why: this module used to own the classifier, the quarantine-path helper and
//! the recovery policy. #5063 found the classifier duplicated byte-for-byte in
//! five crates, and the reason none of them reused this one is that it sits
//! behind `memory-core` — a feature that drags in usearch, git2 and a bundled
//! ORT embedder. The shared items moved to [`trusty_common::redb_open`], behind the
//! light `redb-open` feature that `memory-core` now implies.
//!
//! What: re-exports those items unchanged for the engine's own call sites. The
//! module is crate-only since #9278; a caller outside the library uses
//! `trusty_common::redb_open` directly. There is no second implementation
//! here; this file is the alias and nothing else.
//!
//! Test: the behaviour is pinned by trusty-common's own tests, in
//! `crates/trusty-common/src/redb_open.rs` (#9271: a separate crate now).

// #5063: one classifier for the whole workspace. Adding an implementation back
// into this file re-creates the duplication the issue closed.
#[cfg(feature = "server")]
pub use trusty_common::redb_open::INCOMPATIBLE_SUFFIX;
pub use trusty_common::redb_open::{
    backup_incompatible_file, is_incompatible_format, open_or_recreate,
};
