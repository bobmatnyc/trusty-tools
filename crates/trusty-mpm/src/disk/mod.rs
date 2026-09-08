//! Disk accounting for the Disk dashboard (DOC-73 §16).
//!
//! Why: DOC-73 §16.6 item 1 makes a cached byte figure the prerequisite for
//! every other Disk issue — the sunburst spans many projects and worktrees at
//! hundreds of GB to TiB, and neither existing primitive is an index
//! (`trusty-common`'s `dir_size_bytes` and this crate's `measure_bytes_until`
//! both re-stat every file on every call). This module is where that index
//! lives, and where the Disk survey MCP tool (#6927) and the console routes
//! (§16.5) read their bytes from.
//! What: [`size_index::DirSizeIndex`], a path → bytes-with-timestamp cache
//! whose refresh re-reads only the directories whose contents actually
//! changed.
//! Test: `size_index_tests`.
//!
//! # Why trusty-mpm and not trusty-common (#6926)
//!
//! The issue hedged "likely in `trusty-common` alongside `dir_size_bytes`".
//! It lives here instead because §16.1 names trusty-mpm as the Disk
//! dashboard's data source (project registry, worktree discovery) and §16.4
//! states console reaches trusty-mpm only through MCP tools — so the index has
//! exactly one caller, and it is in this crate. CLAUDE.md's common-entry-point
//! rule scopes itself to capabilities shared ACROSS crates; promoting this one
//! before a second crate calls it would buy a published-crate version bump and
//! a workspace-wide gate for sharing that does not exist yet. Moving the module
//! to `trusty-common` is a file move plus a re-export the day a second crate
//! needs it.
//!
//! [`size_index::DirSizeIndex`]: crate::disk::size_index::DirSizeIndex

pub mod size_index;
// #6927: the Disk dashboard's worktree survey — the classification the
// `disk_survey` MCP tool returns, and the run that gathers its facts. Crate
// -internal: every type in it names a `pub(crate)` reclaim type, and the MCP
// tool is the only consumer.
pub(crate) mod survey;
pub(crate) mod survey_run;
