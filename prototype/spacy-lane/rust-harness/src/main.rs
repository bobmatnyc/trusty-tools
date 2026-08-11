//! Measurement rig for #5399 bake-off lane B (spaCy POS filtering, resident).
//!
//! Subcommands: `eval` runs the shared evaluation set and prints pass/fail per
//! row; `bench` measures cold start and steady-state per-extraction latency;
//! `hold` keeps the sidecar resident so `footprint`/`vmmap` can sample its RSS;
//! `determinism` re-parses the set N times and hashes the output.

mod gate;
mod sidecar;

use anyhow::Result;
use std::time::Instant;

// Absolute: `Command::new` resolves a RELATIVE program against the child's
// `current_dir`, not the parent's, so a relative interpreter path plus
// `.current_dir(PROJ)` looks for the venv inside itself and fails ENOENT.
const PROJ: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../python");
const PY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../python/.venv/bin/python");

/// One required triple: subject, predicate, object.
type Expected = Option<(&'static str, &'static str, &'static str)>;

/// The shared evaluation set, verbatim from the bake-off brief.
/// `expected` is `None` for a required NO-triple row.
const CASES: &[(&str, Expected)] = &[
    ("match exhaustiveness is a hard requirement here", None),
    ("confirm the squash is an ancestor of origin main", None),
    ("rustc is a compiler", Some(("rustc", "is-a", "compiler"))),
    ("librs is a fast parser", Some(("librs", "is-a", "parser"))),
    (
        "trusty-memory uses redb for persistence",
        Some(("trusty-memory", "uses", "redb")),
    ),
    ("the daemon is a member of the process group", None),
    (
        "tantivy is a search library",
        Some(("tantivy", "is-a", "library")),
    ),
];

/// Cases beyond the shared set, chosen to expose where this approach is weak.
const EXTRA: &[&str] = &[
    "serde is a serialization framework",
    "clap is a command line argument parser",
    "the extractor is a subset of the pipeline",
    "redb is an embedded key value store",
    "tokio is a runtime",
    "wgpu is a graphics abstraction",
    "zstd is a fast compressor",
];

fn run_eval() -> Result<()> {
    let mut sc = sidecar::Sidecar::spawn(PY, PROJ)?;
    let texts: Vec<&str> = CASES.iter().map(|(t, _)| *t).collect();
    let docs = sc.analyze(&texts)?;

    let mut pass = 0usize;
    println!("\n{:<50} {:<28} {:<28} VERDICT", "INPUT", "EXPECTED", "GOT");
    println!("{}", "-".repeat(120));
    for ((text, expected), doc) in CASES.iter().zip(docs.iter()) {
        let out = gate::extract(text, doc);
        let got = out
            .triples
            .iter()
            .map(|(s, p, o)| format!("{s} --{p}--> {o}"))
            .collect::<Vec<_>>();
        let exp_s = match expected {
            None => "NO triple".to_string(),
            Some((s, p, o)) => format!("{s} --{p}--> {o}"),
        };
        let got_s = if got.is_empty() {
            "NO triple".to_string()
        } else {
            got.join("; ")
        };
        let ok = match expected {
            None => out.triples.is_empty(),
            Some((s, p, o)) => out
                .triples
                .iter()
                .any(|(a, b, c)| a == s && b == p && c == o),
        };
        if ok {
            pass += 1;
        }
        println!(
            "{:<50} {:<28} {:<28} {}",
            text,
            exp_s,
            got_s,
            if ok { "PASS" } else { "FAIL" }
        );
        for r in &out.rejects {
            println!("{:<50}   rejected: {r:?}", "");
        }
    }
    println!("\nshared set: {pass}/{} pass", CASES.len());

    println!("\n--- additional probes (no required answer; looking for false rejects) ---");
    let docs2 = sc.analyze(EXTRA)?;
    for (text, doc) in EXTRA.iter().zip(docs2.iter()) {
        let out = gate::extract(text, doc);
        let got = out
            .triples
            .iter()
            .map(|(s, p, o)| format!("{s} --{p}--> {o}"))
            .collect::<Vec<_>>()
            .join("; ");
        println!(
            "{:<50} => {}",
            text,
            if got.is_empty() {
                "NO triple".into()
            } else {
                got
            }
        );
        for r in &out.rejects {
            println!("{:<50}    rejected: {r:?}", "");
        }
    }
    Ok(())
}

fn run_bench() -> Result<()> {
    // Cold start: process spawn through first servable reply.
    let t0 = Instant::now();
    let mut sc = sidecar::Sidecar::spawn(PY, PROJ)?;
    let cold = t0.elapsed();
    println!(
        "cold_start_to_ready_ms = {:.1}",
        cold.as_secs_f64() * 1000.0
    );

    let texts: Vec<&str> = CASES.iter().map(|(t, _)| *t).collect();
    // Warm the wire path.
    for _ in 0..20 {
        sc.analyze(&texts[..1])?;
    }

    // Single-sentence round trip — the shape a kg_extract call actually makes.
    let mut single = Vec::new();
    for _ in 0..200 {
        let t = Instant::now();
        sc.analyze(&texts[..1])?;
        single.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    single.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "per_call_1_sentence_ms: p50={:.3} p90={:.3} p99={:.3} min={:.3} max={:.3} (n=200)",
        single[100], single[180], single[198], single[0], single[199]
    );

    // Batched: 7 sentences in one round trip (drawer-sized).
    let mut batch = Vec::new();
    for _ in 0..200 {
        let t = Instant::now();
        sc.analyze(&texts)?;
        batch.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    batch.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "per_call_7_sentences_ms: p50={:.3} p90={:.3} p99={:.3} (n=200, = {:.3} ms/sentence at p50)",
        batch[100],
        batch[180],
        batch[198],
        batch[100] / 7.0
    );

    // The model-free floor: how much of the above is wire, not spaCy.
    let mut ping = Vec::new();
    for _ in 0..200 {
        let t = Instant::now();
        sc.analyze(&[])?;
        ping.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    ping.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "wire_floor_empty_batch_ms: p50={:.3} p90={:.3} (n=200)",
        ping[100], ping[180]
    );
    Ok(())
}

fn run_hold() -> Result<()> {
    let mut sc = sidecar::Sidecar::spawn(PY, PROJ)?;
    let texts: Vec<&str> = CASES.iter().map(|(t, _)| *t).collect();
    for _ in 0..50 {
        sc.analyze(&texts)?;
    }
    println!("SIDECAR_PID={}", sc.pid());
    println!("READY_FOR_FOOTPRINT");
    // Hold the child open long enough for an external sampler.
    std::thread::sleep(std::time::Duration::from_secs(45));
    Ok(())
}

fn run_determinism(runs: usize) -> Result<()> {
    let texts: Vec<&str> = CASES.iter().map(|(t, _)| *t).collect();
    let mut seen: Option<String> = None;
    for r in 0..runs {
        // A FRESH process each round: this is what catches load-order or
        // hash-seed nondeterminism, which an in-process loop would hide.
        let mut sc = sidecar::Sidecar::spawn(PY, PROJ)?;
        let docs = sc.analyze(&texts)?;
        let rendered: String = CASES
            .iter()
            .zip(docs.iter())
            .map(|((t, _), d)| {
                let o = gate::extract(t, d);
                format!("{t}|{:?}|{:?}\n", o.triples, o.rejects)
            })
            .collect();
        match &seen {
            None => seen = Some(rendered),
            Some(prev) => {
                if *prev != rendered {
                    println!("DETERMINISM: MISMATCH on run {r}");
                    return Ok(());
                }
            }
        }
    }
    println!("DETERMINISM: {runs} fresh-process runs byte-identical");
    Ok(())
}

/// What the caller sees when the Python side is absent, or dies mid-session.
///
/// Why: "does the daemon degrade or fall over" is a distribution question, not
/// a nicety — a resident sidecar is a second process that can be OOM-killed,
/// crash on a bad input, or never start at all on a machine without the venv.
fn run_failmode() -> Result<()> {
    // 1. Interpreter absent entirely (fresh machine, no bootstrap).
    match sidecar::Sidecar::spawn("/nonexistent/python", PROJ) {
        Ok(_) => println!("MISSING_INTERPRETER: unexpectedly succeeded"),
        Err(e) => println!("MISSING_INTERPRETER: clean Err -> {e}"),
    }

    // 2. Healthy sidecar killed mid-session, then a request issued.
    let mut sc = sidecar::Sidecar::spawn(PY, PROJ)?;
    let texts = ["rustc is a compiler"];
    let before = sc.analyze(&texts)?;
    println!(
        "PRE_KILL: {} doc(s), {} chunks",
        before.len(),
        before[0].noun_chunks.len()
    );
    let pid = sc.pid();
    std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status()?;
    std::thread::sleep(std::time::Duration::from_millis(400));
    match sc.analyze(&texts) {
        Ok(_) => println!("POST_KILL: unexpectedly succeeded"),
        Err(e) => println!("POST_KILL: clean Err -> {e}"),
    }

    // 3. A request that makes the Python handler raise.
    let mut sc2 = sidecar::Sidecar::spawn(PY, PROJ)?;
    match sc2.request_raw("analyze", serde_json::json!({ "texts": 42 })) {
        Ok(v) => println!("BAD_PARAMS: unexpectedly succeeded -> {v}"),
        Err(e) => println!("BAD_PARAMS: clean Err -> {e}"),
    }
    // Prove the loop survived that error rather than wedging.
    let after = sc2.analyze(&texts)?;
    println!(
        "POST_ERROR_RECOVERY: {} doc(s) — loop still serving",
        after.len()
    );
    Ok(())
}

fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("failmode") => run_failmode(),
        Some("table") => run_eval(),
        Some("bench") => run_bench(),
        Some("hold") => run_hold(),
        Some("determinism") => run_determinism(5),
        other => {
            eprintln!("usage: harness <eval|bench|hold|determinism> (got {other:?})");
            std::process::exit(2);
        }
    }
}
