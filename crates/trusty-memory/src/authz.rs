//! Minimal authorization seam for privileged operations (issue #1714).
//!
//! Why: `palace_create force=true` bypasses the project-slug validation gate
//! (see `tools::palace_ops::handle_palace_create` and
//! `service::core::MemoryService::create_palace`) with NO authorization check
//! today — any caller that can reach either surface can create/overwrite a
//! palace under an arbitrary slug. That is an accepted trade-off for the
//! current single-tenant / local deployment model, which is the only mode
//! this daemon ships in today. The spec envisions a future multi-tenant
//! chat-session-storage mode where a low-privilege tenant could otherwise
//! clobber another tenant's palace by colliding a slug. Building a real
//! multi-tenant auth system (caller identity, per-tenant capability grants, a
//! slug-ownership table) is explicitly out of scope for this fix — the PM
//! decision here is: preserve today's single-tenant behaviour unchanged (no
//! existing caller breaks) while adding the narrowest possible seam a future
//! auth layer can hook into without touching either `force`-bypass call site
//! again.
//! What: `AppState::multi_tenant_mode` (default `false`; opt-in via
//! `TRUSTY_MEMORY_MULTI_TENANT=1` through
//! `AppState::with_multi_tenant_mode_from_env`) gates the single check in
//! this module, [`authorize_force_palace_create`]. In single-tenant mode
//! (the default, unchanged from before this fix) the check is a no-op — every
//! existing caller keeps working exactly as before. Multi-tenant mode has no
//! capability model to consult yet, so it fails CLOSED: `force=true` is
//! refused outright rather than silently honoured.
//! TODO(#1714 follow-up): replace the fail-closed placeholder with a real
//! capability check once caller identity is threaded through MCP / HTTP
//! requests (e.g. a signed header verified against a tenant->slug ownership
//! table). Until then, multi-tenant mode intentionally cannot use `force` at
//! all.
//! Test: `authorize_force_palace_create_allows_single_tenant_default`,
//! `authorize_force_palace_create_denies_multi_tenant_without_capability` in
//! `lib_tests`.

use crate::AppState;
use anyhow::{anyhow, Result};

/// Gate `palace_create force=true` behind an explicit authorization signal.
///
/// Why/What: see module docs.
/// Test: see module docs.
pub fn authorize_force_palace_create(state: &AppState) -> Result<()> {
    if state.multi_tenant_mode {
        return Err(anyhow!(
            "force=true requires an authorization signal that is not yet implemented; \
             multi-tenant mode refuses force until a capability check lands (issue #1714)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        (AppState::new(tmp.path().to_path_buf()), tmp)
    }

    /// Why: single-tenant (the default) must be a total no-op so no existing
    /// `force=true` caller breaks. `#[tokio::test]` because `AppState::new`
    /// spawns the BM25 index worker task, which needs a runtime context.
    #[tokio::test]
    async fn authorize_force_palace_create_allows_single_tenant_default() {
        let (state, _tmp) = state();
        assert!(!state.multi_tenant_mode, "default must be single-tenant");
        authorize_force_palace_create(&state).expect("single-tenant mode is a no-op");
    }

    /// Why: multi-tenant mode has no capability model yet, so it must fail
    /// closed rather than silently behave like single-tenant mode.
    #[tokio::test]
    async fn authorize_force_palace_create_denies_multi_tenant_without_capability() {
        let (mut state, _tmp) = state();
        state.multi_tenant_mode = true;
        let err = authorize_force_palace_create(&state)
            .expect_err("multi-tenant mode must fail closed with no capability check");
        assert!(err.to_string().contains("authorization signal"));
    }
}
