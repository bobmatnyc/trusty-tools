//! Cache-key derivation for the machine-wide embedding cache (issue #5024).
//!
//! Why: the cache is shared by every index on the host, so its key is the only
//! thing standing between "reuse an identical chunk's vector" and "serve a
//! vector produced by a different model". Keeping the derivation in one small
//! module — with the embedder identity a mandatory input rather than an
//! optional decoration — makes it impossible to build a key that omits it.
//!
//! What: [`CacheKey`], a 32-byte SHA-256 digest over a versioned domain
//! separator, the embedder identity, and the chunk content.
//!
//! Test: `key_changes_when_identity_changes`, `key_is_stable_for_same_inputs`,
//! `key_separates_identity_from_content` in `super::tests`.

use sha2::{Digest, Sha256};

/// Domain separator, versioned so a future change to the value encoding can
/// invalidate every existing entry by bumping the trailing digit rather than
/// requiring operators to delete a file.
const KEY_DOMAIN: &[u8] = b"trusty-search/embed-cache/v1";

/// A 32-byte content+identity digest addressing one cached embedding.
///
/// Why: SHA-256 over the full input is collision-resistant at a level where a
/// false hit is not a practical concern, and it is stable across processes and
/// builds — unlike `DefaultHasher`, which is seeded per process and would make
/// a persistent cache useless. Same reasoning as `reindex::hash::hash_content`.
/// What: a newtype over `[u8; 32]` so a raw digest cannot be mistaken for a
/// key derived without an identity.
/// Test: see module docs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct CacheKey([u8; 32]);

impl CacheKey {
    /// Derive the key for `content` as embedded by `identity`.
    ///
    /// Why: the identity is a *required* parameter, not an option, because a
    /// key without it would let one model's vectors satisfy another model's
    /// lookup. Callers that have no identity must not reach this function —
    /// they skip the cache entirely (see `EmbedCache::lookup`).
    /// What: `SHA-256(domain || len(identity) || identity || content)`. The
    /// identity's byte length is hashed before the identity itself so that no
    /// pair of (identity, content) inputs can be concatenated into the same
    /// byte stream as a different pair — without it, identity `"a"` + content
    /// `"bc"` and identity `"ab"` + content `"c"` would collide.
    /// Test: `key_separates_identity_from_content`.
    pub(crate) fn derive(identity: &str, content: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(KEY_DOMAIN);
        hasher.update((identity.len() as u64).to_le_bytes());
        hasher.update(identity.as_bytes());
        hasher.update(content.as_bytes());
        Self(hasher.finalize().into())
    }

    /// Borrow the digest for use as a redb key.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}
