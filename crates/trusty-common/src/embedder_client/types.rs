//! JSON wire types for the trusty-embedderd HTTP API.
//!
//! Why: defining the wire types in the client library means both
//! `trusty-embedderd` (server side) and consumers (trusty-search, etc.) share
//! exactly the same structs — no schema drift between serialiser and
//! deserialiser.
//!
//! What: `EmbedRequest` is `POST /embed` request body; `EmbedResponse` is the
//! corresponding response body. Both derive `serde::Serialize` and
//! `serde::Deserialize`.
//!
//! Test: `roundtrip_json` below verifies that serialise→deserialise produces
//! the original value.

use serde::{Deserialize, Serialize};

/// Request body for `POST /embed`.
///
/// Why: encapsulates the batch of texts so the HTTP handler can decode a
/// single JSON object rather than a bare array.
///
/// What: `texts` is the ordered list of strings to embed; the response will
/// contain one vector per entry, in the same order.
///
/// Test: `roundtrip_json` below.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedRequest {
    /// Texts to embed. May be empty; an empty batch returns an empty response.
    pub texts: Vec<String>,
}

/// Response body for `POST /embed`.
///
/// Why: mirrors `EmbedRequest` — one vector per input text in the same order.
///
/// What: `vectors` is a `Vec<Vec<f32>>` where `vectors[i]` corresponds to
/// `texts[i]` in the request. Each inner `Vec<f32>` has length 384
/// (all-MiniLML6V2Q output dimension).
///
/// Test: `roundtrip_json` below.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedResponse {
    /// One 384-dimensional unit vector per input text.
    pub vectors: Vec<Vec<f32>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_json() {
        // Why: catch any accidental rename/field mismatch between struct fields
        // and serde attribute names before they hit the wire.
        // What: round-trip through serde_json and assert equality.
        // Test: this test.
        let req = EmbedRequest {
            texts: vec!["hello world".to_string(), "foo bar".to_string()],
        };
        let json = serde_json::to_string(&req).expect("serialize");
        let decoded: EmbedRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(req.texts, decoded.texts);

        let resp = EmbedResponse {
            vectors: vec![vec![0.1_f32, 0.2, 0.3], vec![0.4, 0.5, 0.6]],
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        let decoded: EmbedResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(resp.vectors.len(), decoded.vectors.len());
        assert_eq!(resp.vectors[0], decoded.vectors[0]);
    }
}
