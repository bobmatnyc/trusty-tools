//! Randomness and the grant clock, as injectable seams.
//!
//! Why: nonces, generation uuids, grant ids, lease tokens and command ids all
//! reach the wire. The phase 0 fixtures were captured with Python's
//! `secrets`/`uuid4`/`os.urandom` replaced by one seeded stream, so a replay
//! can only be byte-identical if the Rust broker draws from the same stream in
//! the same order. Production draws from the OS.
//! What: [`Entropy`] fills bytes; [`token_hex`], [`token_urlsafe`] and
//! [`uuid4`] match Python's `secrets.token_hex`, `secrets.token_urlsafe` and
//! `str(uuid.uuid4())` for the same bytes. [`Monotonic`] is `codes.clock`.
//! Test: `captured_exchanges_replay_byte_identically` in `tests/parity_broker.rs`.

use std::sync::OnceLock;
use std::time::Instant;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// A source of random bytes.
pub trait Entropy: Send + Sync {
    /// `n` random bytes (one Python `token_*`/`urandom`/`uuid4` call).
    fn bytes(&self, n: usize) -> Vec<u8>;
}

/// The operating system's CSPRNG (`getentropy`).
#[derive(Debug, Default, Clone, Copy)]
pub struct OsEntropy;

impl Entropy for OsEntropy {
    fn bytes(&self, n: usize) -> Vec<u8> {
        let mut out = vec![0u8; n];
        for chunk in out.chunks_mut(256) {
            // SAFETY: the pointer and length describe a live, writable buffer of
            // at most 256 bytes, the getentropy limit.
            let rc = unsafe { libc::getentropy(chunk.as_mut_ptr().cast(), chunk.len()) };
            // getentropy only fails for a bad buffer or length, both excluded above.
            assert_eq!(rc, 0, "getentropy failed");
        }
        out
    }
}

/// `secrets.token_hex(n)`.
pub fn token_hex(e: &dyn Entropy, n: usize) -> String {
    hex::encode(e.bytes(n))
}

/// `secrets.token_urlsafe(n)`: unpadded URL-safe base64 of `n` bytes.
pub fn token_urlsafe(e: &dyn Entropy, n: usize) -> String {
    URL_SAFE_NO_PAD.encode(e.bytes(n))
}

/// `str(uuid.UUID(bytes=<16 bytes>, version=4))`.
pub fn uuid4(e: &dyn Entropy) -> String {
    let mut b = e.bytes(16);
    b.resize(16, 0);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// The grant clock (`codes.clock`, `time.monotonic` in Python), in seconds.
pub trait Monotonic: Send + Sync {
    /// Seconds on a clock that never goes back.
    fn now(&self) -> f64;
}

/// `time.monotonic`, measured from the first call in this process.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemMonotonic;

impl Monotonic for SystemMonotonic {
    fn now(&self) -> f64 {
        static START: OnceLock<Instant> = OnceLock::new();
        // Offset by one hour so a fresh process never sees a grace deadline at 0.
        START.get_or_init(Instant::now).elapsed().as_secs_f64() + 3600.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Vec<u8>);
    impl Entropy for Fixed {
        fn bytes(&self, n: usize) -> Vec<u8> {
            self.0.iter().copied().cycle().take(n).collect()
        }
    }

    #[test]
    fn token_shapes_match_python() {
        let e = Fixed((0u8..=255).collect());
        assert_eq!(token_hex(&e, 6), "000102030405");
        assert_eq!(token_urlsafe(&e, 18), "AAECAwQFBgcICQoLDA0ODxAR");
        assert_eq!(uuid4(&e), "00010203-0405-4607-8809-0a0b0c0d0e0f");
        assert_eq!(OsEntropy.bytes(300).len(), 300);
    }
}
