//! Shared helpers for the phase 1 parity targets.
#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use trusty_browser::audit::Clock;
use trusty_browser::entropy::{Entropy, Monotonic};
use trusty_browser::pyjson::Json;

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn read(rel: &str) -> Vec<u8> {
    fs::read(fixtures().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

pub fn json(rel: &str) -> Json {
    Json::parse(&read(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

pub fn items(v: &Json) -> &[Json] {
    match v {
        Json::Array(items) => items,
        other => panic!("not an array: {other:?}"),
    }
}

pub fn s<'a>(v: &'a Json, key: &str) -> &'a str {
    v.get(key)
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("{key} missing in {v:?}"))
}

/// A private (0700) temp state directory.
pub fn state_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod");
    dir
}

/// The fixture vault key, as stored in the Keychain (base64).
pub fn fixture_key() -> String {
    s(&json("vault.json"), "key_b64").to_owned()
}

pub fn fixture_service() -> String {
    s(&json("vault.json"), "keychain_service").to_owned()
}

const SEED: &[u8] = b"trusty-browser-phase0-fixtures";

/// `capture.py` `_det`: SHA-256(SEED + str(counter)) blocks, one counter per block.
#[derive(Clone, Default)]
pub struct Det(pub Arc<AtomicU64>);

impl Entropy for Det {
    fn bytes(&self, n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        while out.len() < n {
            let i = self.0.fetch_add(1, Ordering::SeqCst);
            let mut h = Sha256::new();
            h.update(SEED);
            h.update(i.to_string().as_bytes());
            out.extend_from_slice(&h.finalize());
        }
        out.truncate(n);
        out
    }
}

/// `audit.time`: 1_790_000_000.0 + 0.25 per call.
pub struct Ticks(pub AtomicU64);

impl Clock for Ticks {
    fn now(&self) -> f64 {
        1_790_000_000.0 + self.0.fetch_add(1, Ordering::SeqCst) as f64 * 0.25
    }
}

/// `codes.clock = lambda: 1000.0`.
pub struct Fixed;

impl Monotonic for Fixed {
    fn now(&self) -> f64 {
        1000.0
    }
}
