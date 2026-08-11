//! Measure the cost of the #5399 WordNet POS pass.
//!
//! Why: the pass sits on `memory_remember`'s write path, so its cost is a
//! standing claim that needs re-checking whenever the table, the walk, or the
//! lookup changes. A number nobody can reproduce is not evidence.
//! What: reports table size and record count, the cost of constructing a
//! lookup, per-`mask` probe latency, resident-memory delta, and per-call
//! `extract_triples` latency over a fixed corpus. It touches only the public
//! API, so it also compiles against the pre-#5399 extractor — which is how the
//! "before" latency column is obtained.
//! Test: none; this is a measurement tool, and its output is the artefact.
//!
//! Run: `SKIP_UI_BUILD=1 cargo run --release -p trusty-memory --example wordnet_measure`

use std::time::Instant;
use trusty_memory::kg_extract::{extract_triples, ExtractInput};
use trusty_memory::wordnet_pos::WordNetPos;
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

/// Words spanning hit, miss, and both ends of the table's sort order.
const PROBES: &[&str] = &[
    "compiler",
    "hard",
    "fast",
    "requirement",
    "rustc",
    "tantivy",
    "zzzzz",
    "aardvark",
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
    let warm: Vec<u8> = vec![0; 1 << 20];
    std::hint::black_box(&warm);
    let rss_before = rss_kib();

    // Construction: no parse, no allocation — this is the number that replaced
    // the spike's 6.6-9.8 ms table build.
    let t0 = Instant::now();
    let wn = std::hint::black_box(WordNetPos::shipped());
    let construct = t0.elapsed();

    // First real probe faults in the pages the binary search touches.
    let t1 = Instant::now();
    std::hint::black_box(wn.mask("compiler"));
    let first_probe = t1.elapsed();

    let rss_after = rss_kib();
    println!("lemmas                {}", wn.lemma_count());
    println!("construct_ns          {}", construct.as_nanos());
    println!(
        "first_probe_us        {:.3}",
        first_probe.as_secs_f64() * 1e6
    );
    println!("rss_before_kib        {rss_before}");
    println!("rss_after_kib         {rss_after}");
    println!(
        "rss_delta_kib         {}",
        rss_after.saturating_sub(rss_before)
    );

    // ---- steady-state probe latency ----------------------------------------
    for _ in 0..10_000 {
        for w in PROBES {
            std::hint::black_box(wn.mask(w));
        }
    }
    let probe_iters = 200_000usize;
    let t2 = Instant::now();
    for _ in 0..probe_iters {
        for w in PROBES {
            std::hint::black_box(wn.mask(w));
        }
    }
    let probes = probe_iters * PROBES.len();
    println!(
        "mask_ns_per_probe     {:.1}",
        t2.elapsed().as_secs_f64() * 1e9 / probes as f64
    );

    // ---- per-extraction latency --------------------------------------------
    let id = Uuid::new_v4();
    let tags: Vec<String> = vec!["rust".into(), "kg".into()];
    let run = |iters: usize| {
        let t = Instant::now();
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
        t.elapsed().as_secs_f64() * 1e6 / (iters * CORPUS.len()) as f64
    };
    run(200);
    let iters = 20_000usize;
    let per_call = run(iters);
    println!("extract_calls         {}", iters * CORPUS.len());
    println!("extract_us_per_call   {per_call:.3}");
}
