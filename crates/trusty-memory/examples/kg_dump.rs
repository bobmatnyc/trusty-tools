//! #5399 lane-A corpus differ. Prototype only — not shipped.
//!
//! Why: the eval set is seven hand-picked sentences. Whether the POS pass is a
//! net win depends on what it does to prose nobody chose, so this dumps every
//! pattern triple from a real corpus and lets the two builds be diffed.
//! What: reads lines from stdin, prints `subject|predicate|object` for each
//! pattern-derived triple. Uses only the public API, so it compiles unchanged
//! against the pre-#5399 extractor.
//! Run: `find docs -name '*.md' | xargs cat | ./kg_dump > triples.txt`

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
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
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
