//! Proof that a CLI subprocess test never touched the operator's REAL
//! `allowlist.toml` (issues #8175, #8737).
//!
//! Why: `AllowlistConfig::default_path()` resolves via `dirs::config_dir()`,
//! which reads `$HOME` (macOS) or `$XDG_CONFIG_HOME`/`$HOME` (Linux) — NOT
//! `TRUSTY_DATA_DIR`. `index remove` and `index relocate` both write the
//! allowlist, so a spawned `trusty-search` that inherits the operator's real
//! `HOME` can rewrite the real file even though every HTTP call targets a fake
//! router. Tests pin `HOME`/`XDG_CONFIG_HOME` to a tempdir; this guard proves
//! that pin held.
//! What: absence is a valid state (CI has no such file) — `exists: false` on
//! both sides is success. Presence is compared on bytes AND mtime, so even a
//! content-preserving rewrite is caught.
//! Test: used by `index_remove_env_conflict_8175.rs` and
//! `reindex_quantize_env_conflict_8737.rs`, capture before each subprocess and
//! `assert_unchanged` after.

use trusty_search::allowlist::AllowlistConfig;

pub struct RealAllowlistGuard {
    exists: bool,
    bytes: Option<Vec<u8>>,
    mtime: Option<std::time::SystemTime>,
}

impl RealAllowlistGuard {
    pub fn capture() -> Self {
        let path = AllowlistConfig::default_path();
        match std::fs::metadata(&path) {
            Ok(meta) => Self {
                exists: true,
                bytes: std::fs::read(&path).ok(),
                mtime: meta.modified().ok(),
            },
            Err(_) => Self {
                exists: false,
                bytes: None,
                mtime: None,
            },
        }
    }

    /// Called after the subprocess under test has exited, so any write it
    /// performed against the real path becomes a hard test failure.
    pub fn assert_unchanged(&self, label: &str) {
        let after = Self::capture();
        assert_eq!(
            self.exists, after.exists,
            "{label}: the real allowlist.toml went from present to absent or vice versa"
        );
        assert_eq!(
            self.bytes, after.bytes,
            "{label}: the real allowlist.toml content changed"
        );
        assert_eq!(
            self.mtime, after.mtime,
            "{label}: the real allowlist.toml mtime changed (even a no-op \
             rewrite bumps this, so an unchanged mtime is the strongest signal \
             nothing touched it)"
        );
    }
}
