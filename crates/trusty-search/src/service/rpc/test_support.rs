//! Helpers the socket-parity test modules share (#9535).
//!
//! Why: the parity tests call each route's `*_report` core for the HTTP side
//! (#9535: the router is going), and every family's test file had grown its own
//! copy of the same three adapters. One copy keeps the HTTP-side encoding the
//! same for every family.
//! What: [`http_body`] and [`http_refusal`] unwrap a core's answer; [`params`]
//! decodes a JSON request into the core's request type.
//! Test: the `*_over_the_socket_matches_the_http_body` cases in `admin_tests.rs`,
//! `queries_tests.rs`, `reads_tests.rs` and `writes_tests.rs` use these.

use axum::http::StatusCode;

/// A core's 200 body, encoded the way axum's `Json` writes it (#9535).
///
/// Serialised to bytes and parsed back, not `serde_json::to_value`, so an
/// `f32` field gets the same shortest representation HTTP and `as_http_body`
/// give it.
pub(super) fn http_body<T: serde::Serialize>(
    answer: Result<T, (StatusCode, serde_json::Value)>,
) -> serde_json::Value {
    match answer {
        Ok(body) => {
            let bytes = serde_json::to_vec(&body).expect("encode the body");
            serde_json::from_slice(&bytes).expect("decode the body")
        }
        Err((status, body)) => panic!("the core must answer 200, got {status}: {body}"),
    }
}

/// A core's refusal as `(status, body)`, asserting it is a 4xx or 5xx (#9535).
pub(super) fn http_refusal<T: serde::Serialize>(
    answer: Result<T, (StatusCode, serde_json::Value)>,
) -> (StatusCode, serde_json::Value) {
    match answer {
        Ok(body) => panic!(
            "the core must refuse, got 200: {}",
            serde_json::to_value(body).expect("encode the body")
        ),
        Err((status, body)) => {
            assert!(
                status.is_client_error() || status.is_server_error(),
                "a refusal must be 4xx/5xx, got {status}: {body}"
            );
            (status, body)
        }
    }
}

/// A JSON request decoded as the core's request type.
pub(super) fn params<T: serde::de::DeserializeOwned>(json: serde_json::Value) -> T {
    serde_json::from_value(json).expect("decode the request")
}
