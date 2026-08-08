//! Environment-driven configuration for the embedding cache (issue #5024).
//!
//! Why: the cache is on by default, so every knob that could turn it into a
//! disk-space problem needs an operator-visible override — and a malformed
//! value must fall back to the safe default rather than to "unbounded".
//!
//! What: [`CacheConfig::resolve`], reading `TRUSTY_EMBED_CACHE`,
//! `TRUSTY_EMBED_CACHE_MAX_MB`, `TRUSTY_EMBED_CACHE_DIR`, and
//! `TRUSTY_EMBED_CACHE_REDB_CACHE_MB`.
//!
//! Test: `config_defaults`, `config_disabled_by_env`,
//! `config_zero_max_mb_disables`, `config_malformed_max_mb_falls_back` in
//! `super::tests`.

use std::path::PathBuf;

use super::store::DEFAULT_MAX_MB;

/// Master on/off switch. Truthy-negative values disable the cache entirely.
pub(super) const ENABLE_ENV: &str = "TRUSTY_EMBED_CACHE";
/// Ceiling in megabytes. `0` disables the cache — there is deliberately no
/// value that means "unbounded".
pub(super) const MAX_MB_ENV: &str = "TRUSTY_EMBED_CACHE_MAX_MB";
/// Absolute path override for the cache file's directory. Test isolation.
pub(super) const DIR_ENV: &str = "TRUSTY_EMBED_CACHE_DIR";
/// redb page-cache ceiling for the cache database, in megabytes.
pub(super) const REDB_CACHE_MB_ENV: &str = "TRUSTY_EMBED_CACHE_REDB_CACHE_MB";

/// redb page cache for the embed-cache database.
///
/// Why: this database is touched in bursts during a reindex and never on the
/// query hot path, so a large page cache would add to the daemon's resident set
/// for no latency benefit — the opposite of what the memory-reduction work on
/// this daemon was for. 16 MB is enough to keep the B-tree's internal nodes
/// warm through a batch.
/// What: overridable via [`REDB_CACHE_MB_ENV`].
const DEFAULT_REDB_CACHE_MB: usize = 16;

/// Resolved cache settings, or the decision not to run one.
pub(super) struct CacheConfig {
    pub(super) path: PathBuf,
    pub(super) max_bytes: u64,
    pub(super) redb_cache_bytes: usize,
}

impl CacheConfig {
    /// Resolve settings from the environment.
    ///
    /// Why: `None` is the "do not build a cache" answer, and every failure
    /// path — switched off, zero ceiling, unresolvable data directory — lands
    /// there rather than raising. A host that cannot host the cache indexes
    /// exactly as it did before.
    /// What: `None` when [`ENABLE_ENV`] is falsy, when [`MAX_MB_ENV`] is `0`,
    /// or when the data directory cannot be resolved and [`DIR_ENV`] is unset.
    /// A malformed [`MAX_MB_ENV`] warns and uses [`DEFAULT_MAX_MB`].
    /// Test: see module docs.
    pub(super) fn resolve() -> Option<Self> {
        if let Ok(v) = std::env::var(ENABLE_ENV) {
            if matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            ) {
                tracing::info!("embed cache: disabled by {ENABLE_ENV}={v:?}");
                return None;
            }
        }

        let max_mb = match std::env::var(MAX_MB_ENV) {
            Ok(v) if !v.trim().is_empty() => match v.trim().parse::<u64>() {
                Ok(0) => {
                    tracing::info!("embed cache: disabled by {MAX_MB_ENV}=0");
                    return None;
                }
                Ok(n) => n,
                Err(_) => {
                    tracing::warn!(
                        "embed cache: {MAX_MB_ENV}={v:?} is not a valid u64; \
                         using default ({DEFAULT_MAX_MB} MB)"
                    );
                    DEFAULT_MAX_MB
                }
            },
            _ => DEFAULT_MAX_MB,
        };

        let dir = match std::env::var(DIR_ENV) {
            Ok(v) if !v.trim().is_empty() => {
                let p = PathBuf::from(v.trim());
                if !p.is_absolute() {
                    tracing::warn!(
                        "embed cache: {DIR_ENV}={p:?} is not absolute — ignoring \
                         and using the platform data directory"
                    );
                    default_dir()?
                } else {
                    p
                }
            }
            _ => default_dir()?,
        };

        let redb_cache_mb = std::env::var(REDB_CACHE_MB_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_REDB_CACHE_MB);

        Some(Self {
            path: dir.join("embeddings.redb"),
            max_bytes: max_mb * 1024 * 1024,
            redb_cache_bytes: redb_cache_mb * 1024 * 1024,
        })
    }
}

/// `<data-dir>/embed-cache`, or `None` when the data directory is unresolvable.
///
/// Why: the cache is machine-wide by design — one file serving every index, so
/// two worktrees of the same repo share entries. It lives beside the per-index
/// corpora under the daemon's own data directory rather than in a cache
/// directory the OS may purge mid-reindex.
/// What: `trusty_common::resolve_data_dir("trusty-search")` joined with
/// `embed-cache`; a resolution failure is logged and downgraded to `None`.
/// Test: `config_defaults`.
fn default_dir() -> Option<PathBuf> {
    match trusty_common::resolve_data_dir("trusty-search") {
        Ok(d) => Some(d.join("embed-cache")),
        Err(e) => {
            tracing::warn!("embed cache: could not resolve data dir ({e}) — cache disabled");
            None
        }
    }
}
