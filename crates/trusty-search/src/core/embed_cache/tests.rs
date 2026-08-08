//! Unit coverage for the machine-wide embedding cache (issue #5024).
//!
//! Every test builds its own store in a `TempDir` — none of them can reach the
//! daemon's real cache file.

use std::sync::Arc;

use tempfile::TempDir;

use super::config::{CacheConfig, DIR_ENV, ENABLE_ENV, MAX_MB_ENV};
use super::key::CacheKey;
use super::store::{EmbedCacheStore, DEFAULT_MAX_MB};
use super::EmbedCache;

const DIM: usize = 4;

fn store(dir: &TempDir, max_bytes: u64) -> EmbedCacheStore {
    EmbedCacheStore::open(&dir.path().join("embeddings.redb"), max_bytes, 1024 * 1024)
        .expect("open test store")
}

fn vec_of(seed: f32) -> Vec<f32> {
    (0..DIM).map(|i| seed + i as f32).collect()
}

// ── Key derivation ────────────────────────────────────────────────────────────

/// The same identity and content must always produce the same key — otherwise
/// the cache could never hit across processes.
#[test]
fn key_is_stable_for_same_inputs() {
    assert_eq!(
        CacheKey::derive("model|384|fp32", "fn main() {}"),
        CacheKey::derive("model|384|fp32", "fn main() {}"),
    );
}

/// The correctness guarantee this whole design rests on: a change of embedder
/// identity — model, weight quantization, dimension, or compute precision —
/// must produce a different key, so the old model's vectors MISS instead of
/// being served for the new one.
#[test]
fn key_changes_when_identity_changes() {
    let content = "fn authenticate() {}";
    let base = CacheKey::derive("all-MiniLM-L6-v2|384|fp32", content);

    // Weight quantization (the int8 ONNX variant is a different model).
    assert_ne!(
        base,
        CacheKey::derive("all-MiniLM-L6-v2-int8|384|fp32", content),
        "an int8-quantized model must not read the fp32 model's vectors"
    );
    // Output dimension.
    assert_ne!(
        base,
        CacheKey::derive("all-MiniLM-L6-v2|768|fp32", content),
        "a dimension change must force a miss"
    );
    // Compute precision.
    assert_ne!(
        base,
        CacheKey::derive("all-MiniLM-L6-v2|384|fp16", content),
        "an fp16 compute change must force a miss"
    );
    // A different model entirely.
    assert_ne!(
        base,
        CacheKey::derive("bge-small-en|384|fp32", content),
        "a different model must force a miss"
    );
}

/// Length-prefixing the identity keeps it from bleeding into the content: two
/// different (identity, content) pairs must not hash the same byte stream.
#[test]
fn key_separates_identity_from_content() {
    assert_ne!(CacheKey::derive("a", "bc"), CacheKey::derive("ab", "c"));
}

/// Different content under one identity must produce different keys.
#[test]
fn key_changes_when_content_changes() {
    assert_ne!(
        CacheKey::derive("m|4|fp32", "alpha"),
        CacheKey::derive("m|4|fp32", "beta"),
    );
}

// ── Store round-trip ──────────────────────────────────────────────────────────

/// A written vector must come back byte-identical — a cache hit and a cache
/// miss have to be indistinguishable to the index.
#[test]
fn roundtrip_hits_after_write() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir, DEFAULT_MAX_MB * 1024 * 1024);
    let k = CacheKey::derive("m|4|fp32", "alpha");

    assert!(s.get_batch(&[k], DIM).unwrap()[0].is_none(), "empty = miss");

    s.write_batch(&[(k, vec_of(1.0))], &[]).unwrap();
    let hit = s.get_batch(&[k], DIM).unwrap().remove(0).expect("hit");
    assert_eq!(hit.vector, vec_of(1.0));
}

/// The cache must survive a process restart — the whole point is that a
/// worktree created tomorrow reuses today's vectors.
#[test]
fn reopen_preserves_entries() {
    let dir = TempDir::new().unwrap();
    let k = CacheKey::derive("m|4|fp32", "alpha");
    {
        let s = store(&dir, DEFAULT_MAX_MB * 1024 * 1024);
        s.write_batch(&[(k, vec_of(2.0))], &[]).unwrap();
    }
    let s = store(&dir, DEFAULT_MAX_MB * 1024 * 1024);
    assert_eq!(
        s.get_batch(&[k], DIM).unwrap().remove(0).expect("hit").vector,
        vec_of(2.0)
    );
}

/// A stored vector whose width does not match what the caller expects must be
/// a MISS, not a hit. This is the last line of defence if an identity ever
/// failed to capture a model change: re-embedding is always safe, serving a
/// wrong-width vector is not.
#[test]
fn wrong_dimension_entry_is_a_miss() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir, DEFAULT_MAX_MB * 1024 * 1024);
    let k = CacheKey::derive("m|4|fp32", "alpha");
    s.write_batch(&[(k, vec_of(1.0))], &[]).unwrap();

    assert!(
        s.get_batch(&[k], DIM + 1).unwrap()[0].is_none(),
        "a width mismatch must be reported as a miss"
    );
}

/// A value too short to hold even the sequence header must not panic or index
/// out of bounds — it is simply a miss.
#[test]
fn truncated_entry_is_a_miss() {
    assert!(super::store::decode_entry_for_test(&[0u8; 3], DIM).is_none());
    assert!(super::store::decode_entry_for_test(&[], DIM).is_none());
}

// ── Bounding and eviction ─────────────────────────────────────────────────────

/// The ceiling is the whole reason this cache is safe to enable by default: a
/// machine-wide vector store that never evicts would reach tens of gigabytes.
#[test]
fn eviction_drops_oldest_first() {
    let dir = TempDir::new().unwrap();
    // Room for ~2 entries: each 4-dim entry costs 8 + 16 + 96 = 120 bytes.
    let s = store(&dir, 260);

    let keys: Vec<CacheKey> = (0..3)
        .map(|i| CacheKey::derive("m|4|fp32", &format!("chunk-{i}")))
        .collect();
    for (i, k) in keys.iter().enumerate() {
        s.write_batch(&[(*k, vec_of(i as f32))], &[]).unwrap();
    }

    assert!(
        s.get_batch(&[keys[0]], DIM).unwrap()[0].is_none(),
        "the oldest entry must be the one evicted"
    );
    assert!(
        s.get_batch(&[keys[2]], DIM).unwrap()[0].is_some(),
        "the newest entry must survive"
    );
    assert!(s.total_bytes().unwrap() <= 260, "total must be under ceiling");
}

/// Touching an entry must move it to the back of the eviction queue, so the
/// vectors a live index actually depends on are not the first discarded.
#[test]
fn touch_protects_an_entry_from_eviction() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir, 260);
    let a = CacheKey::derive("m|4|fp32", "a");
    let b = CacheKey::derive("m|4|fp32", "b");
    let c = CacheKey::derive("m|4|fp32", "c");

    s.write_batch(&[(a, vec_of(0.0)), (b, vec_of(1.0))], &[])
        .unwrap();
    // Refresh `a`, making `b` the oldest.
    let a_seq = s.get_batch(&[a], DIM).unwrap().remove(0).unwrap().seq;
    s.write_batch(&[], &[(a, a_seq)]).unwrap();
    // Insert `c`, forcing one eviction.
    s.write_batch(&[(c, vec_of(2.0))], &[]).unwrap();

    assert!(
        s.get_batch(&[a], DIM).unwrap()[0].is_some(),
        "the touched entry must survive"
    );
    assert!(
        s.get_batch(&[b], DIM).unwrap()[0].is_none(),
        "the untouched older entry must be evicted"
    );
}

/// The ceiling must hold across many batches, not just within one — the
/// accounting is what keeps the file from growing forever.
#[test]
fn ceiling_is_respected_across_batches() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir, 1200);
    for i in 0..200 {
        let k = CacheKey::derive("m|4|fp32", &format!("chunk-{i}"));
        s.write_batch(&[(k, vec_of(i as f32))], &[]).unwrap();
    }
    assert!(
        s.total_bytes().unwrap() <= 1200,
        "accounted total drifted above the ceiling: {}",
        s.total_bytes().unwrap()
    );
    assert!(s.len().unwrap() <= 10, "entry count grew past the ceiling");
}

/// Re-inserting the same key must replace, not double-count — otherwise the
/// running total would inflate and evict healthy entries.
#[test]
fn reinsert_does_not_double_count() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir, DEFAULT_MAX_MB * 1024 * 1024);
    let k = CacheKey::derive("m|4|fp32", "alpha");
    s.write_batch(&[(k, vec_of(1.0))], &[]).unwrap();
    let after_first = s.total_bytes().unwrap();
    s.write_batch(&[(k, vec_of(9.0))], &[]).unwrap();

    assert_eq!(s.total_bytes().unwrap(), after_first, "cost double-counted");
    assert_eq!(s.len().unwrap(), 1);
    assert_eq!(
        s.get_batch(&[k], DIM).unwrap().remove(0).unwrap().vector,
        vec_of(9.0),
        "the newer value must win"
    );
}

/// Touching a key that was evicted between the read and the write must be a
/// silent no-op, not an error or a resurrected empty entry.
#[test]
fn touching_an_evicted_key_is_a_noop() {
    let dir = TempDir::new().unwrap();
    let s = store(&dir, DEFAULT_MAX_MB * 1024 * 1024);
    let ghost = CacheKey::derive("m|4|fp32", "never-written");

    s.write_batch(&[], &[(ghost, 42)]).unwrap();
    assert_eq!(s.len().unwrap(), 0, "a touch must not create an entry");
}

// ── Async facade ──────────────────────────────────────────────────────────────

/// The batch facade must map hits back onto the caller's slot order and hand
/// back the keys for the misses.
#[tokio::test]
async fn lookup_hits_after_store() {
    let dir = TempDir::new().unwrap();
    let cache = EmbedCache::open_at(&dir.path().join("e.redb"), DEFAULT_MAX_MB * 1024 * 1024)
        .expect("open");

    let contents = ["alpha", "beta", "gamma"];
    let first = cache.lookup("m|4|fp32", &contents, DIM).await;
    assert_eq!(first.hit_count(), 0, "empty cache must be an all-miss");

    // Store only the middle one.
    cache
        .store_batch(vec![(first.keys[1], vec_of(7.0))], Vec::new())
        .await;

    let second = cache.lookup("m|4|fp32", &contents, DIM).await;
    assert_eq!(second.hit_count(), 1);
    assert!(second.hits[0].is_none());
    assert_eq!(second.hits[1].as_deref(), Some(vec_of(7.0).as_slice()));
    assert!(second.hits[2].is_none());
    assert_eq!(second.touches.len(), 1, "the hit owes an LRU refresh");
}

/// An identity change must miss end-to-end through the facade, not just at the
/// key layer — this is the regression guard the issue asks for.
#[tokio::test]
async fn identity_change_forces_a_miss_end_to_end() {
    let dir = TempDir::new().unwrap();
    let cache = EmbedCache::open_at(&dir.path().join("e.redb"), DEFAULT_MAX_MB * 1024 * 1024)
        .expect("open");

    let contents = ["fn authenticate() {}"];
    let stored = cache.lookup("all-MiniLM-L6-v2|4|fp32", &contents, DIM).await;
    cache
        .store_batch(vec![(stored.keys[0], vec_of(3.0))], Vec::new())
        .await;

    // Same content, same dimension, DIFFERENT model.
    let other_model = cache
        .lookup("all-MiniLM-L6-v2-int8|4|fp32", &contents, DIM)
        .await;
    assert_eq!(
        other_model.hit_count(),
        0,
        "a different model must never read the first model's vector"
    );

    // Same content, same model, DIFFERENT compute precision.
    let other_precision = cache.lookup("all-MiniLM-L6-v2|4|fp16", &contents, DIM).await;
    assert_eq!(
        other_precision.hit_count(),
        0,
        "a precision change must never read the fp32 vector"
    );

    // The original identity still hits — the miss above is targeted, not a
    // blanket failure.
    let same = cache.lookup("all-MiniLM-L6-v2|4|fp32", &contents, DIM).await;
    assert_eq!(same.hit_count(), 1);
}

/// An empty batch must not open a transaction or panic.
#[tokio::test]
async fn empty_batch_is_a_noop() {
    let dir = TempDir::new().unwrap();
    let cache =
        EmbedCache::open_at(&dir.path().join("e.redb"), DEFAULT_MAX_MB * 1024 * 1024).unwrap();
    let l = cache.lookup("m|4|fp32", &[], DIM).await;
    assert_eq!(l.hits.len(), 0);
    cache.store_batch(Vec::new(), Vec::new()).await;
}

// ── Failure paths ─────────────────────────────────────────────────────────────

/// Opening a cache where the file cannot be created must degrade to `None`
/// rather than raising — a host with an unwritable data directory must still
/// index, uncached.
#[test]
fn open_at_unwritable_path_returns_none() {
    let dir = TempDir::new().unwrap();
    // A directory occupying the database's own path: redb cannot create a file
    // there, so the open must fail and be absorbed.
    let occupied = dir.path().join("embeddings.redb");
    std::fs::create_dir_all(&occupied).unwrap();
    assert!(
        EmbedCache::open_at(&occupied, DEFAULT_MAX_MB * 1024 * 1024).is_none(),
        "an unopenable cache must degrade to None, not panic"
    );
}

/// A read against a store whose file was destroyed underneath it must be
/// reported as an all-miss so the caller embeds everything, rather than
/// surfacing an error into a reindex that would otherwise succeed.
#[tokio::test]
async fn lookup_after_backing_file_removed_is_all_miss() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("e.redb");
    let cache = EmbedCache::open_at(&path, DEFAULT_MAX_MB * 1024 * 1024).unwrap();
    let contents = ["alpha"];
    let l = cache.lookup("m|4|fp32", &contents, DIM).await;
    cache
        .store_batch(vec![(l.keys[0], vec_of(1.0))], Vec::new())
        .await;

    // Truncating the file corrupts the pages the next read touches.
    std::fs::write(&path, b"not a redb file").unwrap();
    let after = cache.lookup("m|4|fp32", &contents, DIM).await;
    assert_eq!(
        after.hit_count(),
        0,
        "a damaged store must read as an all-miss, never as an error"
    );

    // And a write against the damaged store must also be absorbed silently.
    cache
        .store_batch(vec![(after.keys[0], vec_of(2.0))], Vec::new())
        .await;
}

// ── Concurrency ───────────────────────────────────────────────────────────────

/// Two indexes reindexing at once share one cache file. redb serialises the
/// writers; what must hold is that every write lands and the accounting stays
/// consistent — no lost entries, no inflated total.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_do_not_lose_entries() {
    let dir = TempDir::new().unwrap();
    let cache = Arc::new(
        EmbedCache::open_at(&dir.path().join("e.redb"), DEFAULT_MAX_MB * 1024 * 1024).unwrap(),
    );

    let mut tasks = Vec::new();
    for w in 0..4u32 {
        let cache = Arc::clone(&cache);
        tasks.push(tokio::spawn(async move {
            for i in 0..25u32 {
                let content = format!("worker-{w}-chunk-{i}");
                let l = cache.lookup("m|4|fp32", &[content.as_str()], DIM).await;
                cache
                    .store_batch(vec![(l.keys[0], vec_of(i as f32))], l.touches)
                    .await;
            }
        }));
    }
    for t in tasks {
        t.await.expect("worker panicked");
    }

    for w in 0..4u32 {
        for i in 0..25u32 {
            let content = format!("worker-{w}-chunk-{i}");
            let l = cache.lookup("m|4|fp32", &[content.as_str()], DIM).await;
            assert_eq!(l.hit_count(), 1, "lost entry for {content}");
        }
    }
}

/// Concurrent readers and writers against one cache must not deadlock or
/// observe a torn vector.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_read_write_yields_intact_vectors() {
    let dir = TempDir::new().unwrap();
    let cache = Arc::new(
        EmbedCache::open_at(&dir.path().join("e.redb"), DEFAULT_MAX_MB * 1024 * 1024).unwrap(),
    );
    let seeded = cache.lookup("m|4|fp32", &["shared"], DIM).await;
    cache
        .store_batch(vec![(seeded.keys[0], vec_of(5.0))], Vec::new())
        .await;

    let mut tasks = Vec::new();
    for w in 0..4u32 {
        let cache = Arc::clone(&cache);
        tasks.push(tokio::spawn(async move {
            for i in 0..25u32 {
                let l = cache.lookup("m|4|fp32", &["shared"], DIM).await;
                if let Some(v) = &l.hits[0] {
                    assert_eq!(v.len(), DIM, "torn vector observed");
                    assert_eq!(v, &vec_of(5.0), "value changed under concurrent load");
                }
                let other = format!("w{w}-{i}");
                let m = cache.lookup("m|4|fp32", &[other.as_str()], DIM).await;
                cache
                    .store_batch(vec![(m.keys[0], vec_of(i as f32))], l.touches)
                    .await;
            }
        }));
    }
    for t in tasks {
        t.await.expect("worker panicked");
    }
}

// ── Configuration ─────────────────────────────────────────────────────────────

/// `TRUSTY_EMBED_CACHE=0` and `TRUSTY_EMBED_CACHE_MAX_MB=0` must both resolve
/// to "no cache" — an operator needs a way to switch this off entirely.
#[test]
#[serial_test::serial(embed_cache_env)]
fn config_disabled_by_env() {
    let dir = TempDir::new().unwrap();
    let _g = EnvGuard::set(&[
        (DIR_ENV, Some(dir.path().to_str().unwrap())),
        (ENABLE_ENV, Some("0")),
        (MAX_MB_ENV, None),
    ]);
    assert!(CacheConfig::resolve().is_none(), "{ENABLE_ENV}=0 must disable");

    let _g2 = EnvGuard::set(&[(ENABLE_ENV, None), (MAX_MB_ENV, Some("0"))]);
    assert!(
        CacheConfig::resolve().is_none(),
        "a zero ceiling must disable rather than mean unbounded"
    );
}

/// A malformed ceiling must fall back to the default, never to unbounded.
#[test]
#[serial_test::serial(embed_cache_env)]
fn config_malformed_max_mb_falls_back() {
    let dir = TempDir::new().unwrap();
    let _g = EnvGuard::set(&[
        (DIR_ENV, Some(dir.path().to_str().unwrap())),
        (ENABLE_ENV, None),
        (MAX_MB_ENV, Some("not-a-number")),
    ]);
    let cfg = CacheConfig::resolve().expect("must still resolve");
    assert_eq!(cfg.max_bytes, DEFAULT_MAX_MB * 1024 * 1024);
}

/// The default configuration is enabled, bounded, and lands under the
/// requested directory.
#[test]
#[serial_test::serial(embed_cache_env)]
fn config_defaults() {
    let dir = TempDir::new().unwrap();
    let _g = EnvGuard::set(&[
        (DIR_ENV, Some(dir.path().to_str().unwrap())),
        (ENABLE_ENV, None),
        (MAX_MB_ENV, None),
    ]);
    let cfg = CacheConfig::resolve().expect("enabled by default");
    assert_eq!(cfg.max_bytes, DEFAULT_MAX_MB * 1024 * 1024);
    assert!(cfg.path.starts_with(dir.path()));
    assert!(cfg.max_bytes > 0, "the default must be bounded");
}

/// Restore environment variables when the guard drops so a failing assertion
/// cannot leak state into the next test.
struct EnvGuard(Vec<(String, Option<String>)>);

impl EnvGuard {
    fn set(pairs: &[(&str, Option<&str>)]) -> Self {
        let prev = pairs
            .iter()
            .map(|(k, _)| ((*k).to_string(), std::env::var(k).ok()))
            .collect();
        for (k, v) in pairs {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        Self(prev)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.0 {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}
