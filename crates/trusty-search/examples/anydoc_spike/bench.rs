//! Speed and peak-memory leg.
//!
//! Timing runs in-process over many repetitions; memory runs one child
//! process per (engine, file) so `ru_maxrss` attributes its high-water mark
//! to exactly one extraction. The child baseline — a re-exec that parses
//! nothing — is subtracted so the reported figure is the extraction's cost,
//! not the process's.

use std::path::Path;
use std::process::Command;

use crate::engines::{fmt_mib, fmt_ms, time_reps, Engine};
use crate::fixtures::Corpus;

/// Repetition counts, scaled down for the fixtures that are slow enough that
/// the noise floor stops mattering.
fn reps_for(size: u64) -> usize {
    if size > 2 * 1024 * 1024 {
        10
    } else if size > 128 * 1024 {
        30
    } else {
        100
    }
}

pub fn run(corpus: &Corpus) {
    let fixtures = corpus.benign();

    println!("## Speed (wall-clock per file, milliseconds)\n");
    println!("Sample: N repetitions per (engine, file) in one process, sorted; min and median reported.\n");
    println!(
        "| fixture | format | bytes | N | native min | native med | anydoc min | anydoc med | ratio (med) |"
    );
    println!("|---|---|---:|---:|---:|---:|---:|---:|---:|");

    for f in &fixtures {
        let size = f.size();
        let n = reps_for(size);
        let (n_min, n_med, _) = time_reps(n, || {
            let _ = Engine::Native.extract(&f.path);
        });
        let (a_min, a_med, _) = time_reps(n, || {
            let _ = Engine::Anydoc.extract(&f.path);
        });
        let ratio = a_med.as_secs_f64() / n_med.as_secs_f64().max(f64::MIN_POSITIVE);
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {:.2}x |",
            f.name,
            f.format,
            size,
            n,
            fmt_ms(n_min),
            fmt_ms(n_med),
            fmt_ms(a_min),
            fmt_ms(a_med),
            ratio
        );
    }

    println!("\n## Peak RSS (isolated child process, MiB)\n");
    let baseline = child_peak_rss(None, Path::new("/dev/null"));
    println!(
        "Baseline (re-exec that parses nothing): {} MiB. Deltas below subtract it.\n",
        fmt_mib(baseline)
    );
    println!("| fixture | format | bytes | native peak | native Δ | anydoc peak | anydoc Δ |");
    println!("|---|---|---:|---:|---:|---:|---:|");
    for f in &fixtures {
        let n = child_peak_rss(Some(Engine::Native), &f.path);
        let a = child_peak_rss(Some(Engine::Anydoc), &f.path);
        println!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            f.name,
            f.format,
            f.size(),
            fmt_mib(n),
            fmt_mib(n.saturating_sub(baseline)),
            fmt_mib(a),
            fmt_mib(a.saturating_sub(baseline)),
        );
    }
}

/// Re-exec this harness as `run-one`, returning the child's reported peak RSS
/// in bytes. `None` runs the no-op baseline.
fn child_peak_rss(engine: Option<Engine>, path: &Path) -> u64 {
    let exe = std::env::current_exe().expect("current_exe");
    let label = engine.map(|e| e.label()).unwrap_or("baseline");
    let out = Command::new(exe)
        .arg("run-one")
        .arg(label)
        .arg(path)
        .output()
        .expect("spawn run-one child");
    parse_rss(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_rss(stdout: &str) -> u64 {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("PEAK_RSS_BYTES="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}
