//! Optional readbacks a sidecar adds to a success frame's `result` (epic #3524
//! slice 5, #9284).
//!
//! Why: `StdioEmbedderClient` learns the sidecar's real device and input
//! window from the first response frame; the parsing lives apart from the
//! client to keep that file under the line cap.
//! What: two minimal deserializers that skip the `embeddings` array.
//! Test: `extract_response_*` in `stdio_tests.rs`.

/// Extract the optional real backend `device` from a response frame's
/// `result` object, without touching the (possibly large) `embeddings` array
/// (epic #3524 slice 5, issue #3493 P1).
///
/// Why: only the Python/MPS sidecar's `protocol.py` emits this field —
/// `result: {"embeddings": [...], "device": "mps"}`. The reference Rust
/// `trusty-embedderd` omits it entirely, so this returns `None` on that path
/// and callers keep using the predicted `ExecutionProvider`.
/// What: a minimal, torch/embeddings-free deserialize; `None` on any parse
/// failure, an error frame, or a missing/absent `device` field.
/// Test: `extract_response_device_*` in `stdio_tests.rs`.
pub(super) fn extract_response_device(line: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct DeviceOnly {
        #[serde(default)]
        result: Option<DeviceField>,
    }
    #[derive(serde::Deserialize)]
    struct DeviceField {
        #[serde(default)]
        device: Option<String>,
    }
    let parsed: DeviceOnly = serde_json::from_str(line).ok()?;
    parsed.result?.device
}

/// Extract the optional input `window` from a response frame's `result`
/// object (#9284).
///
/// Why: both sidecars add `result.window`, the model's input window in
/// tokens, so trusty-search can report it. An older sidecar omits it.
/// What: `None` on a parse failure, an error frame, an absent field, or a
/// zero window.
/// Test: `extract_response_window_*` in `stdio_tests.rs`.
pub(super) fn extract_response_window(line: &str) -> Option<usize> {
    #[derive(serde::Deserialize)]
    struct WindowOnly {
        #[serde(default)]
        result: Option<WindowField>,
    }
    #[derive(serde::Deserialize)]
    struct WindowField {
        #[serde(default)]
        window: Option<usize>,
    }
    let parsed: WindowOnly = serde_json::from_str(line).ok()?;
    parsed.result?.window.filter(|w| *w > 0)
}
