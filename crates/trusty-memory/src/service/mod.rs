//! `MemoryService` — pure business-logic facade over `AppState`.
//!
//! Why: `web.rs` previously hosted ~5700 lines that mingled axum extraction,
//! JSON wire shapes, and business logic. Moving the logic into a struct with
//! `anyhow::Result<Value>` methods lets the HTTP handlers stay one-liners
//! and lets non-HTTP callers (chat tool dispatch, future RPC bridges) reuse
//! the same code paths without dragging axum types around.
//! What: A zero-cost wrapper around [`AppState`] exposing one async method
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
pub mod helpers;
pub mod types;

// Re-export the full public surface so external call sites
// (`crate::service::X`) keep resolving exactly as they did against the former
// monolithic module.
pub use core::MemoryService;
pub use helpers::{
    drawer_content_preview, drawer_snippet, enrich_gap_exploration, load_user_config,
    palace_info_from, recall_entry_json, refresh_gaps_cache, service_result_to_anyhow,
    LoadedUserConfig, DRAWER_PREVIEW_MAX_CHARS, DRAWER_SNIPPET_MAX_CHARS,
};
pub use types::{
    CreateDrawerBody, CreatePalaceBody, DreamStatusPayload, KgAssertBody, KgGraphPayload,
    ListDrawersQuery, PalaceInfo, ServiceError, ServiceResult, StatusPayload,
};
