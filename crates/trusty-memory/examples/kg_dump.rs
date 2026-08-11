//! Dump every pattern-derived KG triple from a corpus, for before/after diffing.
//!
//! Why: the #5399 eval set is a dozen hand-picked sentences. Whether a change
//! to the extractor is a net win depends on what it does to prose nobody chose,
//! and the only way to see that is to run both builds over the same real corpus
//! and diff. This is how the "304 pairs dropped, 196 recoverable" figure behind
//! the shipped re-walk policy was obtained.
//! What: reads lines from stdin, prints `subject|predicate|object<TAB>source`
//! for each pattern-derived triple. Uses only the public API, so it compiles
//! unchanged against any revision of the extractor.
//! Test: none; this is a measurement tool, and its output is the artefact.
//!
//! Run: `git ls-files '*.md' | xargs cat | cargo run --release -p trusty-memory \
//!       --example kg_dump > after.txt`

use std::io::{BufRead, Write};
use trusty_memory::kg_extract::{extract_triples, ExtractInput};
use uuid::Uuid;

const PATTERN_PREDICATES: &[&str] = &["is-a", "works-at", "uses", "depends-on"];

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let id = Uuid::nil();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        for t in extract_triples(&ExtractInput {
            drawer_id: id,
            content: trimmed,
            tags: &[],
            room: None,
        }) {
            if PATTERN_PREDICATES.contains(&t.predicate.as_str()) {
                let _ = writeln!(
                    out,
                    "{}|{}|{}\t{}",
                    t.subject, t.predicate, t.object, trimmed
                );
            }
        }
    }
}
