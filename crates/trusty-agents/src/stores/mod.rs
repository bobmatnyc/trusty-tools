//! OKG store bindings — what an agent KNOWS (#3816/#3864, DOC-54
//! SPEC-AGENTS-04 §5.1).
//!
//! Why: Stores are the FIRST leg of the agent-config triple (stores / tools
//! / listeners). An agent ACTS via `[tools]`, REACTS via `[[listeners]]`, and
//! KNOWS via `[[stores]]`. Until this module landed the third leg existed
//! (`crate::listeners::config`) and the first did not — the GUI's "OKG
//! Stores" pane rendered a hardcoded placeholder and `vector_search` had no
//! way to be pointed at the agent's own corpus (#3864). A binding names one
//! OKG knowledge tree plus the trusty-search index built over it and,
//! optionally, the trusty-memory palace that holds the agent's structured
//! recall for the same domain.
//! What:
//! - `config` — declarative shapes parsed from `agent.toml` (`[[stores]]`
//!   array-of-tables, or the spec's `[stores] allow = [...]` shorthand).
//!   Pure serde data, per the standing "agents are declarative-only" rule
//!   (#2791).
//! - `binding` — resolving `okg://<agent>` to a real directory, and a KB tree
//!   back to the index it feeds (#3892). Until this landed the `okg://` URI was
//!   an opaque label nothing ever resolved.
//! - `index_feed` — pushing OKG ingest output into the bound trusty-search
//!   index, so the binding's "one store, two facets" semantics are true rather
//!   than aspirational (#3892 remediation (a)).
//! - `status` — live resolution of a binding against the running
//!   trusty-search / trusty-memory daemons, producing a per-store
//!   connected/not-connected report. Every failure mode degrades to
//!   `not_connected` with a human-readable reason; a bound store that
//!   cannot be resolved NEVER blocks agent boot.
//! Test: See each submodule's own unit tests.

pub mod binding;
pub mod config;
pub mod index_feed;
pub mod status;

pub use binding::{BoundIndex, bound_index_for_tree, okg_tree_path};
pub use config::{AgentStoreBinding, StoresConfig};
pub use index_feed::{HttpIndexFeed, IndexFeed, IndexFeedReport, feed_source};
pub use status::{StoreStatus, resolve_store_statuses};
