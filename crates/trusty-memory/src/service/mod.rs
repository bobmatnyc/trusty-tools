//! `MemoryService` — pure business-logic facade over `AppState`.
//!
//! Why: `web.rs` previously hosted ~5700 lines that mingled axum extraction,
//! JSON wire shapes, and business logic. Moving the logic into a struct with
//! `anyhow::Result<Value>` methods lets the HTTP handlers stay one-liners
//! and lets non-HTTP callers (chat tool dispatch, future RPC bridges) reuse
//! the same code paths without dragging axum types around.
//! What: A zero-cost wrapper around [`AppState`](crate::AppState) exposing one async method
//! per logical operation. Each method returns either `anyhow::Result<Value>`
//! (for handlers that already wrap errors with `ApiError::internal`) or a
//! domain-specific result the handler maps into JSON.
//! Test: Each method is covered indirectly via the corresponding HTTP test in
//! `web::tests` (the handlers delegate here verbatim).
//!
//! Hard constraint (issue #151): no behaviour change. Every method's success
//! and failure shapes match what the handler used to produce inline.

pub mod core;
pub mod core_kg;
// #8246: ranked recall, split from `core.rs` at the SLOC cap.
pub mod core_recall;
pub mod helpers;
// #7125: the bounded batch fan-out the recall-all family runs on.
pub mod recall_stream;
// #9544: `palace_rename`, kept out of `core.rs` for the SLOC cap.
pub mod rename;
pub mod types;
pub mod user_config;

// #6286: the assertions the retired `web::tests` suite carried, ported to
// direct `MemoryService` calls. Split by the module each one covers.
#[cfg(test)]
#[path = "core_kg_tests.rs"]
mod core_kg_tests;
// #9141: `recall_all` in `core_recall.rs` keeps the empty-palace skip.
#[cfg(test)]
#[path = "core_recall_tests.rs"]
mod core_recall_tests;
#[cfg(test)]
#[path = "core_tests.rs"]
pub(crate) mod core_tests;
#[cfg(test)]
#[path = "helpers_tests.rs"]
mod helpers_tests;
// #7125: the bounded fan-out's failure path has no route in from outside the
// crate, so its coverage lives here rather than in tests/.
#[cfg(test)]
#[path = "recall_stream_tests.rs"]
mod recall_stream_tests;
// #9299: the ADR-0071 recall_all scope and coverage criteria.
#[cfg(test)]
#[path = "recall_scope_tests.rs"]
mod recall_scope_tests;
// #9544: the daemon half of `palace_rename`.
#[cfg(test)]
#[path = "rename_tests.rs"]
mod rename_tests;

// Re-export the full public surface so external call sites
// (`crate::service::X`) keep resolving exactly as they did against the former
// monolithic module. `load_user_config`, `dream_config_from_user_config`, and
// `LoadedUserConfig` moved from `helpers` to `user_config` (issue #2593
// follow-up, to keep `helpers.rs` under the 500-SLOC cap); re-exported from
// their new home here so `crate::service::X` call sites are unaffected.
pub use core::MemoryService;
pub use helpers::{
    drawer_content_preview, drawer_snippet, enrich_gap_exploration, palace_info_from,
    recall_entry_json, refresh_gaps_cache, service_result_to_anyhow, DRAWER_PREVIEW_MAX_CHARS,
    DRAWER_SNIPPET_MAX_CHARS,
};
// #4670 added KgNeighborsPayload / KgNodeView / KgSeedPayload — the
// progressive graph-exploration payloads.
pub use types::{
    CreateDrawerBody, CreatePalaceBody, DreamStatusPayload, KgAssertBody, KgGraphPayload,
    KgNeighborsPayload, KgNodeView, KgSeedPayload, ListDrawersQuery, PalaceInfo, ServiceError,
    ServiceResult, StatusPayload,
};
pub use user_config::{dream_config_from_user_config, load_user_config, LoadedUserConfig};
