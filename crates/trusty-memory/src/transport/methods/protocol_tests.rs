//! Pins [`PROTOCOL_VERSION`] to the wire surface it promises (#9288).
//!
//! Why: a version constant only means something if a breaking change cannot
//! ship under it. This file records what protocol 1 serves, and fails when a
//! method name, the frame budget or a client-read error code changes while the
//! constant stays put.
//! What: one pinned surface per protocol version. A name the pin lists must
//! still be served; adding a name passes, because an additive change is not a
//! bump (ADR-0007 rule 3). Never edit a released version's pin to make this
//! pass — bump [`PROTOCOL_VERSION`] and add the new version's pin instead.
//! Test: this file.

use super::protocol::PROTOCOL_VERSION;
use crate::transport::api_error::{CODE_NOT_FOUND, CODE_REFUSED};
use crate::transport::uds::{FOLDED_METHODS, MAX_FRAME_BYTES, STREAM_METHODS};

/// What one protocol version promises a client.
struct PinnedSurface {
    /// Every unary method name, folded and dispatcher alike.
    methods: &'static [&'static str],
    /// The streaming method names.
    streams: &'static [&'static str],
    /// The frame budget, which a client mirrors.
    max_frame_bytes: u64,
    /// `(name, code)` for every code a client branches on.
    codes: &'static [(&'static str, i64)],
}

/// Protocol 1: the wire as of #9288.
const V1: PinnedSurface = PinnedSurface {
    methods: &[
        // Folded methods (`transport::uds::FOLDED_METHODS`).
        "memory.health",
        "memory.protocol",
        "memory.status",
        "memory.config",
        "memory.palace_get",
        "memory.palaces_list",
        "memory.drawers_list",
        "memory.drawer_create",
        "memory.drawer_delete",
        "memory.kg_all",
        "memory.kg_count",
        "memory.kg_subjects_with_counts",
        "memory.kg_graph",
        "memory.kg_graph_seed",
        "memory.kg_graph_neighbors",
        "memory.kg_delete_triple",
        "memory.dream_status",
        "memory.palace_dream_status",
        "memory.dream_run",
        "memory.activity",
        "memory.logs_tail",
        "memory.admin_stop",
        "memory.remember_async",
        "memory.chat_providers",
        "memory.messages_list",
        "memory.message_send",
        "memory.message_mark_read",
        // Dispatcher protocol arms (`transport::rpc::method_names`).
        "initialize",
        "notifications/initialized",
        "notifications/cancelled",
        "ping",
        "rpc.discover",
        "tools/list",
        "tools/call",
        "hook_fired",
        // Dispatcher tool methods.
        "add_alias",
        "console_metrics",
        "discover_aliases",
        "dream_consolidate_room",
        "get_prompt_context",
        "kg_assert",
        "kg_bootstrap",
        "kg_gaps",
        "kg_list_subjects",
        "kg_query",
        "kg_retract_triple",
        "list_prompt_facts",
        "memory_forget",
        "memory_list",
        "memory_note",
        "memory_recall",
        "memory_recall_all",
        "memory_recall_deep",
        "memory_remember",
        "memory_send_message",
        "palace_compact",
        "palace_create",
        "palace_dream",
        "palace_embed_sweep",
        "palace_info",
        "palace_list",
        "palace_reembed",
        "palace_unalias",
        "palace_update",
        "palace_verify_embedded",
        "remove_prompt_fact",
        "room_create",
        "room_list",
        "room_rename",
        "task_add",
        "task_complete",
        "task_list",
        "wing_create",
        "wing_list",
        "wing_rename",
    ],
    streams: &["memory.chat", "memory.activity_stream"],
    max_frame_bytes: 32 * 1024 * 1024,
    codes: &[("CODE_NOT_FOUND", -32004), ("CODE_REFUSED", -32006)],
};

/// The pin for `version`, if one was recorded.
fn pinned(version: u64) -> Option<&'static PinnedSurface> {
    match version {
        1 => Some(&V1),
        _ => None,
    }
}

/// Why (#9288): a removed or renamed method, a changed frame budget, or a
/// changed error code breaks every client built against the old wire. Shipping
/// one without a `PROTOCOL_VERSION` bump is what a client cannot detect.
/// What: looks up the pin for the current version and asserts the live surface
/// still serves all of it.
/// Test: itself.
#[test]
fn protocol_version_is_pinned_to_its_wire_surface() {
    let Some(pin) = pinned(PROTOCOL_VERSION) else {
        panic!(
            "PROTOCOL_VERSION {PROTOCOL_VERSION} has no pinned surface: add one in \
             protocol_tests.rs beside V1"
        );
    };

    let mut served: Vec<&str> = FOLDED_METHODS.to_vec();
    served.extend(crate::transport::rpc::method_names());
    let missing: Vec<&str> = pin
        .methods
        .iter()
        .copied()
        .filter(|name| !served.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "protocol {PROTOCOL_VERSION} promises {missing:?}, which the daemon no longer \
         serves: bump PROTOCOL_VERSION and pin the new surface"
    );

    for stream in pin.streams {
        assert!(
            STREAM_METHODS.contains(stream),
            "protocol {PROTOCOL_VERSION} promises the stream {stream}: bump PROTOCOL_VERSION"
        );
    }
    assert_eq!(
        MAX_FRAME_BYTES, pin.max_frame_bytes,
        "the frame budget changed under protocol {PROTOCOL_VERSION}: bump PROTOCOL_VERSION"
    );
    let live = [
        ("CODE_NOT_FOUND", CODE_NOT_FOUND),
        ("CODE_REFUSED", CODE_REFUSED),
    ];
    for (name, code) in pin.codes {
        assert!(
            live.contains(&(*name, *code)),
            "{name} is no longer {code} under protocol {PROTOCOL_VERSION}: bump PROTOCOL_VERSION"
        );
    }
}

/// Why (#9288): `FOLDED_METHODS` is ungated and spells the handshake as a
/// literal, because `trusty_common::memory_rpc` exists only with the
/// `memory-rpc` feature. The literal must stay the shared constant.
/// Test: itself.
#[test]
fn folded_protocol_name_is_the_shared_constant() {
    assert!(
        FOLDED_METHODS.contains(&trusty_common::memory_rpc::METHOD_PROTOCOL),
        "FOLDED_METHODS must list {}",
        trusty_common::memory_rpc::METHOD_PROTOCOL
    );
}
