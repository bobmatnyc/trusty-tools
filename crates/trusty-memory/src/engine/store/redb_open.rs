//! Memory-core's view of the shared redb open-with-quarantine surface (#702).
//!
//! Why: this module used to own the classifier, the quarantine-path helper and
//! the recovery policy. #5063 found the classifier duplicated byte-for-byte in
//! five crates, and the reason none of them reused this one is that it sits
//! behind `memory-core` — a feature that drags in usearch, git2 and a bundled
//! ORT embedder. The shared items moved to [`crate::redb_open`], behind the
//! light `redb-open` feature that `memory-core` now implies.
//!
//! What: re-exports those items unchanged, so every existing path — including
//! the public `trusty_common::memory_core::store::redb_open::*` and the
//! `memory_core::store` re-exports — resolves exactly as before. There is no
//! second implementation here; this file is the alias and nothing else.
//!
//! Test: the behaviour is pinned by `crate::redb_open`'s tests, notably
//! `classifier_pins_the_four_recoverable_arms`.

// #5063: one classifier for the whole workspace. Adding an implementation back
// into this file re-creates the duplication the issue closed.
pub use crate::redb_open::{
    INCOMPATIBLE_SUFFIX, backup_incompatible_file, incompatible_backup_path,
    is_incompatible_format, open_or_recreate,
};
