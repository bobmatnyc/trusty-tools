//! The pairing proof exchanged in the `hello`/`challenge`/`proof` handshake.
//!
//! Why: the extension (JS) and the Python broker compute the same HMAC; the
//! Rust broker must reproduce Python's transcript bytes exactly, including an
//! unescaped non-ASCII email (`protocol.py` `pairing_proof`).
//! What: transcript = compact `json.dumps([...], ensure_ascii=False)` of the
//! eight fields; proof = lowercase hex HMAC-SHA256 keyed by the ASCII key.
//! Test: `proof_vectors_match_python_byte_for_byte` in `tests/parity_proof.rs`.

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::pyjson::Json;

/// Transcript domain tag.
pub const DOMAIN: &str = "trusty-browser-native-v1";
/// The only extension origin allowed to pair.
pub const EXTENSION_ORIGIN: &str = "chrome-extension://jabfhcolknbbdajilgpficniedhcfahc";

/// Which side of the handshake a proof speaks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The broker's `challenge.proof`.
    Broker,
    /// The extension's `proof.proof`.
    Extension,
}

impl Role {
    /// The transcript string for this role.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Broker => "broker",
            Role::Extension => "extension",
        }
    }
}

/// The `hello` fields that enter the transcript.
#[derive(Debug, Clone, Copy)]
pub struct Hello<'a> {
    /// Profile id.
    pub profile: &'a str,
    /// Account email as sent.
    pub email: &'a str,
    /// Worker incarnation.
    pub incarnation: &'a str,
    /// Client nonce.
    pub nonce: &'a str,
}

/// Why a proof could not be computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProofError {
    /// Python's `key.encode("ascii")` raises on a non-ASCII key.
    #[error("browser key is not ASCII")]
    NonAsciiKey,
}

/// The exact bytes the HMAC covers.
pub fn transcript(role: Role, hello: &Hello<'_>, server_nonce: &str) -> Vec<u8> {
    let items = [
        DOMAIN,
        role.as_str(),
        EXTENSION_ORIGIN,
        hello.profile,
        hello.email,
        hello.incarnation,
        hello.nonce,
        server_nonce,
    ];
    let array = Json::Array(items.into_iter().map(Json::from).collect());
    // Strings only, so no non-finite float can occur.
    array.dumps(false).unwrap_or_default().into_bytes()
}

/// Lowercase hex HMAC-SHA256 of the transcript keyed by `key`.
///
/// Why: one function for both roles. What: refuses a non-ASCII key like Python.
/// Test: `proof_vectors_match_python_byte_for_byte`.
pub fn pairing_proof(
    key: &str,
    role: Role,
    hello: &Hello<'_>,
    server_nonce: &str,
) -> Result<String, ProofError> {
    if !key.is_ascii() {
        return Err(ProofError::NonAsciiKey);
    }
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.as_bytes())
        .map_err(|_| ProofError::NonAsciiKey)?;
    mac.update(&transcript(role, hello, server_nonce));
    Ok(hex::encode(mac.finalize().into_bytes()))
}

/// Constant-time equality of two proof strings (`hmac.compare_digest`).
pub fn proofs_equal(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ascii_key_is_refused_and_compare_checks_length() {
        let hello = Hello {
            profile: "p",
            email: "e",
            incarnation: "i",
            nonce: "n",
        };
        assert_eq!(
            pairing_proof("ké", Role::Broker, &hello, "s"),
            Err(ProofError::NonAsciiKey)
        );
        assert!(proofs_equal("abc", "abc"));
        assert!(!proofs_equal("abc", "abd"));
        assert!(!proofs_equal("abc", "ab"));
    }
}
