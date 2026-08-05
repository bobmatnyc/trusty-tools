//! Throwaway measurement harness for the anydoc adoption spike.
//!
//! Why: the question "should anydoc replace `core::extract`" needs numbers on
//! speed, memory, output fidelity, and failure behaviour that no existing
//! test produces. This binary generates them. It is NOT production code and
//! nothing outside it may reference `anydoc` — see the `anydoc-spike` feature
//! in `Cargo.toml`, which gates both this example and the dependency, so a
//! default `cargo build` / `cargo test` / `cargo clippy --all-targets` never
//! compiles either.
//!
//! Usage:
//!
//! ```text
//! cargo run -p trusty-search --features anydoc-spike --example anydoc_spike -- all
//! ```
//!
//! Subcommands: `bench`, `quality`, `adversarial`, `xlsx-bugs`, `all`, and the
//! internal `run-one <native|anydoc|baseline> <path>` used for per-measurement
//! process isolation.
//!
//! Delete this directory, the `anydoc-spike` feature, the `[[example]]` block,
//! and the optional dependency together when the spike is retired.

mod adversarial;
mod bench;
mod engines;
mod fixtures;
mod quality;
mod xlsx_bugs;

use std::path::{Path, PathBuf};

use engines::Engine;
use fixtures::Corpus;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("all");

    if cmd == "run-one" {
        run_one(&args);
        return;
    }

    let dir = corpus_dir();
    let corpus = Corpus::new(&dir);
    eprintln!("corpus: {}", dir.display());

    println!("# anydoc spike measurements\n");
    println!("- host: {} {}", std::env::consts::OS, std::env::consts::ARCH);
    println!("- anydoc: 0.1.3");
    println!("- native: `trusty_search::core::extract::extract_text`");
    println!("- profile: {}\n", if cfg!(debug_assertions) { "debug" } else { "release" });

    match cmd {
        "bench" => bench::run(&corpus),
        "quality" => quality::run(&corpus),
        "adversarial" => adversarial::run(&corpus),
        "xlsx-bugs" => xlsx_bugs::run(&corpus),
        "all" => {
            quality::run(&corpus);
            println!("\n---\n");
            xlsx_bugs::run(&corpus);
            println!("\n---\n");
            bench::run(&corpus);
            println!("\n---\n");
            adversarial::run(&corpus);
        }
        other => {
            eprintln!("unknown subcommand: {other}");
            std::process::exit(2);
        }
    }
}

/// One extraction in a fresh process, reporting the outcome and this
/// process's peak RSS. The parent reads both from stdout; a child that dies
/// on a signal simply prints neither, which is itself the measurement.
fn run_one(args: &[String]) {
    let engine = args.get(1).map(String::as_str).unwrap_or("baseline");
    let path = args.get(2).map(PathBuf::from).unwrap_or_default();

    if engine != "baseline" {
        let e = Engine::parse(engine).unwrap_or_else(|| {
            eprintln!("unknown engine: {engine}");
            std::process::exit(2);
        });
        let outcome = e.extract(&path);
        // Consume the text so a lazy parser cannot defer work past the
        // measurement point.
        let checksum: u64 = outcome.text().bytes().map(u64::from).sum();
        println!("RESULT={}", outcome.summary());
        println!("CHECKSUM={checksum}");
    }
    println!("PEAK_RSS_BYTES={}", engines::peak_rss_bytes());
}

/// Corpus root. `TRUSTY_ANYDOC_SPIKE_DIR` overrides it; otherwise a scratch
/// directory under the system temp dir. Never touches a real
/// `TRUSTY_DATA_DIR` — this harness reads and writes nothing the daemon owns.
fn corpus_dir() -> PathBuf {
    std::env::var("TRUSTY_ANYDOC_SPIKE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(&std::env::temp_dir()).join("anydoc-spike-corpus"))
}
