//! Hook records to `message` events (#9167).

use trusty_common::control_bus::MessageEvent;

use crate::core::hook::HookEventRecord;

/// Env var that turns message bodies on.
pub const MESSAGE_BODY_ENV: &str = "TRUSTY_MPM_EVENTS_MESSAGE_BODY";

/// Longest body carried, in bytes.
pub const MAX_BODY_BYTES: usize = 4096;

/// The `message` events one hook record implies.
pub fn messages(_record: &HookEventRecord, _bodies: bool) -> Vec<MessageEvent> {
    Vec::new()
}

/// The relay marker in `text`, if any.
pub fn relay_marker(_text: &str) -> Option<String> {
    None
}
