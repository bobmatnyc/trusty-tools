//! Per-connection context threaded through every method handler (#2054).
//!
//! Why: `session.attach` needs a way to push server-initiated JSON-RPC
//! notifications (session lifecycle/events) back to the SAME connection
//! that called it, asynchronously, after the initial response has already
//! been sent. Most handlers (`ping`, `health`, `session.list`, …) have no
//! use for this, but the [`Router`](super::router::Router) dispatch path
//! must thread it uniformly so a handful of methods aren't a special case
//! with a different call signature.
//! What: [`ConnectionContext`] carries a `connection_id` (identifies which
//! connection/subscriber this call came from — one per STDIO process
//! lifetime, one per HTTP `POST /rpc` call) and a [`NotifySender`] the
//! connection's transport drains and writes to the wire. Over STDIO, the
//! transport loop keeps one long-lived receiver for the whole process and
//! interleaves notifications with request/response lines on stdout. Over
//! HTTP, each `POST /rpc` call gets a throwaway channel — any queued
//! notification vanishes once the response is written, because HTTP's real
//! streaming path for `session.attach` is the dedicated `GET
//! /sessions/{id}/events` SSE route (see `crate::serve::http`), not this
//! channel. A handler that spawns a background forwarder using `notify`
//! (like `session.attach`) must treat a failed `send` as "the connection is
//! gone" and stop.
//! Test: `context::tests::connection_context_is_constructible`.

use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Channel used to push a fully-formed JSON-RPC wire object (a `Response`
/// serialised to `Value`, or a server-initiated notification) to the
/// connection's outbound stream, asynchronously, outside the normal
/// request/response cycle.
///
/// Why: `serde_json::Value` (rather than `trusty_common::mcp::Response`) is
/// the channel item type because a server-initiated *notification*
/// (`{"jsonrpc":"2.0","method":"session.event","params":{...}}`) has no
/// `result`/`error` field — it isn't a `Response` at all — so the channel
/// must be able to carry either shape. The transport writes whatever
/// arrives verbatim as one NDJSON line.
/// What: `mpsc::UnboundedSender<Value>`. Unbounded because session events
/// are rare relative to request/response traffic and a bounded channel
/// would require a backpressure policy (drop? block the emitter?) that no
/// current requirement calls for.
pub type NotifySender = mpsc::UnboundedSender<Value>;

/// Per-connection state passed to every JSON-RPC method handler.
///
/// Why: threading this uniformly (rather than adding a second handler trait
/// just for `session.attach`) keeps the dispatch path — and every future
/// method — on one call signature. See the module docs for the STDIO vs.
/// HTTP distinction.
/// What: `connection_id` is a fresh `Uuid` per STDIO process invocation or
/// per HTTP `POST /rpc` call; `notify` is that connection's outbound
/// notification sink.
/// Test: `context::tests::connection_context_is_constructible`.
#[derive(Clone)]
pub struct ConnectionContext {
    pub connection_id: Uuid,
    pub notify: NotifySender,
}

impl ConnectionContext {
    /// Build a fresh context with a brand-new `connection_id` and the given
    /// notify sender.
    ///
    /// Why: every transport builds exactly one context per connection
    /// (STDIO: once per process; HTTP: once per `POST /rpc` call).
    /// What: `Uuid::new_v4()` for the id, `notify` stored verbatim.
    /// Test: `context::tests::connection_context_is_constructible`.
    pub fn new(notify: NotifySender) -> Self {
        Self {
            connection_id: Uuid::new_v4(),
            notify,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A context must be constructible and expose a usable notify sender.
    #[test]
    fn connection_context_is_constructible() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let ctx = ConnectionContext::new(tx);
        ctx.notify.send(serde_json::json!({"ok": true})).unwrap();
        assert_eq!(rx.try_recv().unwrap(), serde_json::json!({"ok": true}));
    }
}
