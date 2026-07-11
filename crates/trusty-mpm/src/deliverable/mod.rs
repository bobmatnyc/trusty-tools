//! Deliverable/Milestone data model, central stores, and state machine
//! (DOC-35 §10, epic #2108; issues #2378 WI-12 + #2380 WI-14).
//!
//! Why: the L3 substrate needs a deterministic ledger of *what work exists*, its
//! tier, its lifecycle state, and which sessions touched it (§10.1) — the same
//! class of bookkeeping fact as the project and session registries. `tm manager`
//! (#2109) reasons OVER this ledger but must not own or invent it. Everything
//! here is a pure function of stored state: no LLM, no inference, single-project
//! scope (§11 boundary).
//! What: re-exports the record types ([`Deliverable`], [`Milestone`] and their
//! value/id types), the [`DeliverableStatus`] state machine (§10.3, #2380), the
//! generic on-disk [`store`] (central sibling stores to `projects.json`, §10.7,
//! §13 Q5), and the [`DeliverableManager`] that fronts both stores for the CRUD
//! routes.
//! Test: each submodule carries inline unit tests; run with
//! `cargo test -p trusty-mpm deliverable`.

pub mod manager;
pub mod milestone;
pub mod record;
pub mod status;
pub mod store;

pub use manager::DeliverableManager;
pub use milestone::{Milestone, MilestoneId, MilestoneStatus};
pub use record::{Deliverable, DeliverableId, DeliverableKind, EstimationTier};
pub use status::{DeliverableStatus, TransitionError, validate_transition};
pub use store::{DeliverableStore, Keyed, MilestoneStore, StoreError};
