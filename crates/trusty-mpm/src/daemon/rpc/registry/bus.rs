//! The peer bus's four request/response verbs as RPC methods (#6288 slice 5).
//!
//! Why the caller is trusty-console and not a peer: the 2026-08-28 owner ruling
//! on #6288 is that cross-instance peers reach this daemon ONLY through
//! trusty-console (ADR-0032). The bus therefore stays on the mpm socket with
//! console as its one caller; console's own slice-9 work gains the public,
//! authenticated route. No peer ever dials this daemon directly.
//!
//! Why `subscribe` is absent: `GET /api/v1/bus/subscribe/{instance_id}` is SSE
//! and needs the `RpcStreamMethod` seam slice 6 owns. It is a named in-slice
//! dependency, not an oversight — its HTTP handler is untouched, and no RPC
//! method is registered for it here.
//!
//! What the fail-closed contract looks like over the socket: DOC-60 §4's
//! sequence — structural validation of the caller identity, the
//! `assistant_instance` edge check, sender verification against the registry,
//! target resolution, then the delivery attempt with the durable record written
//! to state what actually happened — all lives inside `PeerBus::publish`, which
//! this slice did not touch. Every rejection reaches a socket caller as a
//! [`BusError`] carrying the same discrimination the HTTP status gave it, via
//! the `From<BusError> for RpcError` mapping in `bus::error`.
//!
//! Test: the `parity_bus_*` and `rpc_bus_*` cases in `super::tests`.
//!
//! [`BusError`]: crate::daemon::bus::BusError

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::RpcRouter;

use crate::daemon::bus::envelope::BusEnvelope;
use crate::daemon::bus::registry::InstanceMeta;
use crate::daemon::bus::routes as bus;
use crate::daemon::state::DaemonState;

use super::NoParams;

/// `mpm.bus.deregister` parameters — the instance id from the URL path.
#[derive(Debug, Deserialize)]
pub struct InstanceParams {
    /// The instance id to remove.
    pub instance_id: String,
}

/// Mount the four bus methods. `subscribe` is slice 6's; see the module doc.
///
/// Test: `rpc_router_registers_every_documented_method`,
/// `rpc_bus_does_not_register_subscribe`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let held = Arc::clone(state);
    let r = router.typed::<bus::RegisterInstanceRequest, InstanceMeta, _, _>(
        "mpm.bus.register",
        move |p| {
            let s = Arc::clone(&held);
            async move { bus::register_op(&s, p).map_err(Into::into) }
        },
    );

    let held = Arc::clone(state);
    let r = r.typed::<InstanceParams, bus::DeregisterAck, _, _>("mpm.bus.deregister", move |p| {
        let s = Arc::clone(&held);
        async move { bus::deregister_op(&s, &p.instance_id).map_err(Into::into) }
    });

    let held = Arc::clone(state);
    let r = r.typed::<NoParams, bus::ListInstancesResponse, _, _>("mpm.bus.list", move |_| {
        let s = Arc::clone(&held);
        async move { Ok(bus::list_op(&s)) }
    });

    let held = Arc::clone(state);
    r.typed::<bus::PublishRequest, BusEnvelope, _, _>("mpm.bus.publish", move |p| {
        let s = Arc::clone(&held);
        async move { bus::publish_op(&s, p).map_err(Into::into) }
    })
}
