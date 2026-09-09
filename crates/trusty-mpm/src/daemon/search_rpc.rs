//! The one way this crate calls the running trusty-search daemon (#6285, #7237).
//!
//! Why: this module used to BE that client — it derived the socket path, wrote
//! one frame, read one back, and owned [`SearchRpcError`]. #7237 needed the same
//! client from `trusty_common::search_index`, which sits BELOW trusty-mpm in the
//! dependency graph and therefore could not use it: index registration went on
//! POSTing `http://127.0.0.1:7878/indexes`, a listener ADR-0032 retired, and
//! every fresh project's session launch logged a transport failure and withheld
//! its index id. The workspace's common-entry-point rule allows exactly one
//! implementation, so the client moved DOWN to [`trusty_common::search_rpc`] and
//! this module re-exports it. Every consumer here — `tm doctor`'s two search
//! probes, the pinned-index probe, `search_gc`, `index_delete_guard` — keeps
//! naming `search_rpc::…` and is unchanged.
//!
//! What: a re-export, and nothing else. The method-name literals, the socket
//! derivation, the `TRUSTY_SEARCH_SOCKET` override, and the typed refusal all
//! live in trusty-common now; their contracts are documented there.
//!
//! Test: `search_rpc_codes_match_the_daemon_error_table` below pins the one
//! thing the move could silently break — trusty-common carries its own copies of
//! the JSON-RPC codes, because it cannot import this crate's table either.
//! Everything else is covered where it now lives (`trusty_common::search_rpc`)
//! and end-to-end through `doctor_tests::search_*`,
//! `doctor_search_pin_tests::pinned_but_missing_index_is_fail`,
//! `search_gc_guard_tests::sweep_skips_a_candidate_whose_status_probe_failed`
//! and `index_delete_guard::tests::delete_over_a_stale_socket_is_a_transport_failure`.

pub use trusty_common::search_rpc::{
    CODE_CONFLICT, CODE_NOT_FOUND, METHOD_HEALTH, METHOD_INDEX_CREATE, METHOD_INDEX_DELETE,
    METHOD_INDEX_REINDEX, METHOD_INDEX_STATUS, METHOD_INDEXES_LIST, SearchRpcError,
    TRUSTY_SEARCH_SOCKET_ENV, call_at, call_blocking, search_socket,
};

#[cfg(test)]
mod tests {
    /// Why: [`SearchRpcError::is_not_found`] used to read
    /// `crate::daemon::error::CODE_NOT_FOUND` directly. trusty-common cannot,
    /// so it carries its own copy, and a renumbering on either side would make
    /// this crate's probes misread the daemon's verdict with nothing failing to
    /// compile. This is the check that turns that into a test failure.
    /// Test: itself.
    #[test]
    fn search_rpc_codes_match_the_daemon_error_table() {
        assert_eq!(
            super::CODE_NOT_FOUND,
            crate::daemon::error::CODE_NOT_FOUND,
            "trusty-common's copy of the 404 code drifted from this crate's"
        );
        assert_eq!(
            super::CODE_CONFLICT,
            crate::daemon::error::CODE_CONFLICT,
            "trusty-common's copy of the 409 code drifted from this crate's"
        );
    }
}
