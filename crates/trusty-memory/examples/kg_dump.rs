//! Dump every pattern-derived KG triple from a corpus, for before/after diffing.
//!
//! Why: the #5399 eval set is a dozen hand-picked sentences. Whether a change
//! to the extractor is a net win depends on what it does to prose nobody chose,
//! and the only way to see that is to run both builds over the same real corpus
//! and diff. This is how the "304 pairs dropped, 196 recoverable" figure behind
//! the shipped re-walk policy was obtained.
//! What: prints `subject|predicate|object<TAB>source` for each pattern-derived
//! triple. Two input shapes, because they measure different things:
//!   - default: one line of stdin per extraction;
//!   - `--whole-file`: stdin is a list of PATHS, each read whole and extracted
//!     in one call.
//!
//! Uses only the public API, so it compiles unchanged against any revision of
//! the extractor.
//! Test: none; this is a measurement tool, and its output is the artefact.
//!
//! 🔴 `--whole-file` is the shape production actually uses —
//! `auto_extract_and_assert` and `kg_rebuild` both hand the extractor a whole
//! multi-line drawer body. A line-scoped run cannot see any defect that needs
//! two lines to appear, which is how #5399's newline-boundary bug reached
//! review unmeasured. Prefer it; keep the line mode only to reproduce the
//! earlier figures.
//!
//! Run: `git ls-files '*.md' | cargo run --release -p trusty-memory \
//!       --example kg_dump -- --whole-file > after.txt`

use std::io::{BufRead, Write};
use trusty_memory::kg_extract::{extract_triples, ExtractInput};
use uuid::Uuid;

const PATTERN_PREDICATES: &[&str] = &["is-a", "works-at", "uses", "depends-on"];

fn main() {
    let whole_file = std::env::args().any(|a| a == "--whole-file");
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for line in stdin.lock().lines() {
        let Ok(line) = line else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (content, label) = if whole_file {
            match std::fs::read_to_string(trimmed) {
                Ok(body) => (body, trimmed.to_string()),
                Err(e) => {
                    eprintln!("skip {trimmed}: {e}");
                    continue;
                }
            }
        } else {
            (trimmed.to_string(), trimmed.to_string())
        };
        for t in extract_triples(&ExtractInput {
            drawer_id: Uuid::nil(),
            content: &content,
            tags: &[],
            room: None,
        }) {
            if PATTERN_PREDICATES.contains(&t.predicate.as_str()) {
                let _ = writeln!(out, "{}|{}|{}\t{}", t.subject, t.predicate, t.object, label);
            }
        }
    }
}
