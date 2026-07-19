//! Same-origin guard for destructive console write routes — now a thin
//! re-export of the shared `trusty_common::server::origin_guard` primitive.
//!
//! Why: the guard originated here (#1222 review #3, router-wide since #3268,
//! bind-aware since #3269, landed in #3280) but the sibling trusty-* daemons
//! needed the exact same defence, so the implementation was lifted verbatim
//! into `trusty-common` (architecture review tranche 1, #3304). The console now
//! CONSUMES the one shared implementation rather than carrying its own copy —
//! there is exactly one origin-guard implementation in the workspace. This
//! module is kept as a stable re-export so every existing `crate::routes::
//! origin_guard::…` reference (and the `server/tests.rs` regression suite that
//! exercises the guard's semantics) compiles unchanged.
//! What: re-exports [`SelfOrigins`], [`guard_write_origin`],
//! [`origin_is_loopback`], and [`origin_matches_self`] from the shared crate.
//! Test: the semantics are now unit-tested in `trusty_common::server::
//! origin_guard`; the console's end-to-end wiring is still covered by
//! `crate::server::tests` (`write_route_rejects_cross_origin`,
//! `proxy_route_allows_self_origin_write`, …).

pub use trusty_common::server::origin_guard::{
    SelfOrigins, guard_write_origin, origin_is_loopback, origin_matches_self,
};
