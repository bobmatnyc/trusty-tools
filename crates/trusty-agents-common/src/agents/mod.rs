//! Agent compose/deploy/manifest machinery, extracted from `trusty-mpm` (#2892).
//!
//! Why: `trusty-mpm` and `trusty-code` both need to resolve `extends:`
//! inheritance chains, track which deployed agent files a harness owns, and
//! write composed agents into a target directory without clobbering
//! user-owned files. This logic was originally trusty-mpm-only
//! (`crates/trusty-mpm/src/core/{agent_builder,agent_manifest,agent_deployer,
//! agent_metadata,frontmatter}.rs`); moving it here (mirroring the precedent
//! set by `ToolExecutor`/`AgentRunner`) lets both harnesses share one
//! implementation instead of forking it.
//! What: six submodules — [`frontmatter`] (the shared `key: value` line
//! parser, plus the `skills:` list-value grammar), [`builder`] (the
//! `extends:` inheritance-chain composer, filesystem-backed), [`builder_in_memory`]
//! (the disk-free counterpart — resolves `extends:` against an embedded
//! asset map instead of a `source_dir`, issue #2958 Slice E1), [`manifest`]
//! (the checksum + atomic-write ownership ledger), [`deployer`] (writes
//! composed agents into a target directory, consulting the manifest to avoid
//! clobbering user edits, and records each processed agent's declared
//! `skills:` for co-deployment), and [`metadata`] (a read-only projection of
//! a deployed agent's frontmatter for display/diagnostics, used by
//! `deployer` and by trusty-mpm's `tm doctor` / `tm agent` surfaces).
//! `trusty-mpm` re-exports every public item from
//! `crate::core::{agent_builder,agent_manifest,agent_deployer,agent_metadata,
//! frontmatter}` for source compatibility with its existing call sites.
//! Test: `cargo test -p trusty-agents-common agents::` exercises every
//! submodule in place; `cargo test -p trusty-mpm` exercises the re-exported
//! call sites end-to-end.

pub mod builder;
pub mod builder_in_memory;
pub mod deployer;
pub mod frontmatter;
pub mod manifest;
pub mod metadata;
