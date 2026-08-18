//! Cross-machine memory sharing: the palace-targeted export/import primitive
//! (#5902).
//!
//! Why: the same fact recorded on two machines for one project must converge to
//! ONE memory. `Drawer.id` is a v4 UUID minted at write time, so nothing about a
//! drawer's identity derives from what it says, and until this module existed the
//! palace had no export or import at all — the two copies could not even meet.
//! `memory_core::content_hash` supplies the identity; this module is the transport.
//!
//! What this module is NOT: it is a primitive, not a workflow. There are no CLI
//! commands here, no git operations, and no `.trusty-memories/` layout — those are
//! a second PR. What it provides is a pair of functions a workflow can call:
//! [`export_palace_jsonl`] writes a palace's memories to a file, and
//! [`import_palace_jsonl`] reads such a file into a palace idempotently.
//!
//! The four properties the design rests on:
//! - **Idempotent by hash.** Importing the same export twice changes nothing.
//! - **Additive.** Importing a superset adds only what is new.
//! - **Convergent.** Two machines' exports containing the same fact produce one
//!   memory, not two.
//! - **Monotone in time.** A merge keeps the EARLIER `created_at`; a re-import can
//!   never make an old fact look freshly written.
//!
//! 🔴 SECURITY, stated once and loudly: nothing on the export path screens content
//! for secrets. `memory_core::filter::check_secret` runs at WRITE time only, so a
//! credential that predates the filter or slipped past its heuristics is already
//! in some palace and this module will copy it out verbatim. A local file is a fine
//! destination for that. A committed git file is not — the workflow in PR 2 must
//! re-scan every record it is about to commit. See #5902 and #1683.
//!
//! Design record: `docs/reference/shared-memory-identity.md` holds the seven
//! decisions this module implements and why each was chosen over its alternative.
//! Test: `share::tests` covers the four properties above end to end against a real
//! redb-backed palace; `content_hash`'s own tests pin normalization.

pub mod export;
pub mod import;
pub mod record;
pub mod supersede;

#[cfg(test)]
mod tests;

pub use export::{export_palace_jsonl, export_palace_records, write_jsonl};
pub use import::{
    ImportOutcome, ImportSummary, import_palace_jsonl, import_palace_records, merge_records,
};
pub use record::{RecordError, SHARE_FORMAT_VERSION, SharedMemoryRecord};
pub use supersede::{SUPERSEDED_BY, Supersession, assert_superseded_by, supersede_drawer};
