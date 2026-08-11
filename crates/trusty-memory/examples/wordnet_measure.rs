//! #5399 lane-A measurement harness. Prototype only — not shipped.
//!
//! Why: the bake-off is decided on numbers, and a number nobody can reproduce
//! is not evidence. This binary produces every figure in the lane-A report from
//! one run so the owner (or lane B) can re-run it verbatim.
//! What: reports table load time, resident-memory delta around the load, and
//! per-call `extract_triples` latency over a fixed corpus. It touches only the
//! public API, so it also compiles and runs against the PRE-change extractor —
//! which is how the "before" latency column is obtained.
//! Run: `SKIP_UI_BUILD=1 cargo run --release -p trusty-memory --example wordnet_measure`

use std::time::Instant;
use trusty_memory::kg_extract::{extract_triples, ExtractInput};
use uuid::Uuid;

/// Drawer-shaped sentences: pattern hits, near-misses, and plain prose, so the
/// average is not dominated by whichever case happens to be cheapest.
const CORPUS: &[&str] = &[
    "rustc is a compiler",
    "librs is a fast parser",
    "tantivy is a search library",
    "trusty-memory uses redb for persistence",
    "the daemon is a member of the process group",
    "match exhaustiveness is a hard requirement here",
    "confirm the squash is an ancestor of origin main",
    "the extractor depends on trusty-common for the Triple type",
    "we moved the hydration task so an early SIGTERM during startup is safe",
    "no marker in this sentence at all, just ordinary prose about #5399",
];

/// Resident set size in KiB, via `ps`.
fn rss_kib() -> u64 {
    let pid = std::process::id();
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .expect("ps must be available");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

fn main() {
    // ---- resident memory + cold-start cost of the table itself --------------
    // Touch the allocator first so the delta is the table, not warm-up.
    let warm: Vec<u8> = vec![0; 1 << 20];
    std::hint::black_box(&warm);
    let rss_before = rss_kib();

    let t0 = Instant::now();
    let table = trusty_memory::wordnet_pos::WordNetPos::load();
    let load = t0.elapsed();
    std::hint::black_box(&table);

    let rss_after = rss_kib();
    println!("lemmas               {}", table.len());
    println!("load_ms              {:.2}", load.as_secs_f64() * 1e3);
    println!("rss_before_kib       {rss_before}");
    println!("rss_after_kib        {rss_after}");
    println!(
        "rss_delta_kib        {}",
        rss_after.saturating_sub(rss_before)
    );

    // Second load, to separate parse cost from page-fault cost on the embedded
    // text (the first load faults in all 6.3 MB of it).
    let t1 = Instant::now();
    let again = trusty_memory::wordnet_pos::WordNetPos::load();
    println!("load_ms_warm         {:.2}", t1.elapsed().as_secs_f64() * 1e3);
    drop(again);

    // ---- per-extraction latency --------------------------------------------
    // Prime the shared table so the first timed call does not absorb the parse.
    trusty_memory::wordnet_pos::preload();
    let id = Uuid::new_v4();
    let tags: Vec<String> = vec!["rust".into(), "kg".into()];

    for _ in 0..200 {
        for c in CORPUS {
            std::hint::black_box(extract_triples(&ExtractInput {
                drawer_id: id,
                content: c,
                tags: &tags,
                room: Some("Backend"),
            }));
        }
    }

    let iters = 20_000usize;
    let t2 = Instant::now();
    for _ in 0..iters {
        for c in CORPUS {
            std::hint::black_box(extract_triples(&ExtractInput {
                drawer_id: id,
                content: c,
                tags: &tags,
                room: Some("Backend"),
            }));
        }
    }
    let total = t2.elapsed();
    let calls = iters * CORPUS.len();
    println!("extract_calls        {calls}");
    println!(
        "extract_us_per_call  {:.3}",
        total.as_secs_f64() * 1e6 / calls as f64
    );
}
