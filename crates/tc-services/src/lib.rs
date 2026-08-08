//! tc-services: shared service implementations for the Trusty ecosystem.
//!
//! Why: API integrations (CTO DB, Granola, Google Workspace) were
//! reimplemented independently in trusty-agents, trusty-izzie, and the Python
//! CTO bot. This crate consolidates the *service-layer adapters* — schema
//! emission + dispatch — into one host-agnostic place so every Rust consumer
//! reuses the same code instead of re-deriving it.
//! What: Each module exposes a `Service`-shaped type with `all()` (one
//! service per published tool) and `execute(args)` (run the call). Modules
//! return plain result types (no host-framework traits) so callers can wrap
//! them in whatever tool abstraction they use.
//! Test: Per-module unit tests; see `cto_db`.

pub mod cto_db; // CTO SQLite service (migrated from trusty-agents, #484 Phase 1)
pub mod granola; // Native Granola API client (#488 Phase 2)
pub mod gworkspace; // Google Workspace bridge — Calendar + Tasks (#488 Phase 2)
