//! The on-disk result cache.
//!
//! Why: a repeated file must cost nothing (plan section 1, step 5).
//!
//! What: one JSON file per key under `<dir>/v1/<first two hex>/<key>.json`.
//! The key is SHA-256 over the content hash, the converter name and the
//! output settings, so a changed file or a changed setting misses. Writes go
//! to a temporary file and are renamed into place, so concurrent writers and
//! a crash leave either the old entry or the new one. An unreadable or
//! corrupt entry counts as a miss and is overwritten. Converted text lives
//! beside the results as `<dir>/v1/text/<first two hex>/<key>.txt`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::record::CachedResult;

const LAYOUT: &str = "v1";

/// A result cache rooted at one directory.
#[derive(Debug)]
pub struct Cache {
    dir: PathBuf,
    counter: AtomicU64,
}

impl Cache {
    /// Opens (creating) the cache under `root`.
    pub fn open(root: &Path) -> Result<Self> {
        let dir = root.join(LAYOUT);
        std::fs::create_dir_all(&dir).map_err(|e| Error::Cache {
            path: dir.clone(),
            message: e.to_string(),
        })?;
        Ok(Cache {
            dir,
            counter: AtomicU64::new(0),
        })
    }

    /// The cache key for a file.
    ///
    /// Postcondition: equal inputs give equal keys; a change to any input
    /// gives a different key (up to SHA-256 collisions).
    pub fn key(content_hash: &str, converter: &str, settings: &str) -> String {
        let mut h = Sha256::new();
        for part in [content_hash, converter, settings] {
            h.update((part.len() as u64).to_le_bytes());
            h.update(part.as_bytes());
        }
        hex(&h.finalize())
    }

    /// The stored result for `key`, if present and readable.
    pub fn get(&self, key: &str) -> Option<CachedResult> {
        let path = self.entry_path(key);
        let bytes = std::fs::read(&path).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!(
                    "trusty-summarizer: warning: ignoring corrupt cache entry {}: {e}",
                    path.display()
                );
                None
            }
        }
    }

    /// Stores `result` under `key`.
    pub fn put(&self, key: &str, result: &CachedResult) -> Result<()> {
        let bytes = serde_json::to_vec(result).map_err(|e| Error::Cache {
            path: self.entry_path(key),
            message: e.to_string(),
        })?;
        self.write_atomic(&self.entry_path(key), &bytes)
    }

    /// The converted text stored under `key`, if present and UTF-8.
    pub fn get_text(&self, key: &str) -> Option<String> {
        let bytes = std::fs::read(self.text_path(key)).ok()?;
        String::from_utf8(bytes).ok()
    }

    /// Stores converted text under `key`.
    pub fn put_text(&self, key: &str, text: &str) -> Result<()> {
        self.write_atomic(&self.text_path(key), text.as_bytes())
    }

    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let key = path.file_stem().and_then(|s| s.to_str()).unwrap_or("entry");
        let parent = path.parent().expect("entry has a parent");
        let fail = |e: &dyn std::fmt::Display| Error::Cache {
            path: path.to_path_buf(),
            message: e.to_string(),
        };
        std::fs::create_dir_all(parent).map_err(|e| fail(&e))?;
        let tmp = parent.join(format!(
            ".{key}.{}.{}.tmp",
            std::process::id(),
            self.counter.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&tmp, bytes).map_err(|e| fail(&e))?;
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            fail(&e)
        })
    }

    fn text_path(&self, key: &str) -> PathBuf {
        self.dir
            .join("text")
            .join(&key[..2])
            .join(format!("{key}.txt"))
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        self.dir.join(&key[..2]).join(format!("{key}.json"))
    }
}

/// Lowercase hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// `sha256:<hex>` of `bytes`.
pub fn content_hash(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_change_with_every_input() {
        let base = Cache::key("h", "direct", "{}");
        assert_eq!(base, Cache::key("h", "direct", "{}"));
        assert_ne!(base, Cache::key("h2", "direct", "{}"));
        assert_ne!(base, Cache::key("h", "other", "{}"));
        assert_ne!(base, Cache::key("h", "direct", "{\"a\":1}"));
        // Length prefixes keep part boundaries unambiguous.
        assert_ne!(Cache::key("ab", "c", ""), Cache::key("a", "bc", ""));
    }

    #[test]
    fn content_hash_is_sha256() {
        assert_eq!(
            content_hash(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
