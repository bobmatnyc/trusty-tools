//! Regenerate `wordnet/lemma-pos.txt` from the four upstream WordNet index files.
//!
//! Why: #5399 vendors WordNet only to answer "which parts of speech can this
//! word be". The upstream `index.*` files carry sense counts, pointer symbols
//! and synset offsets to answer far more than that — 6.0 MiB of it — and the
//! extractor reads the first field of each line and nothing else. Shipping the
//! projection instead of the source keeps the vendored payload proportional to
//! the one question we ask. The generator is committed so the projection is
//! reproducible rather than a binary blob nobody can re-derive.
//! What: reads `index.noun`, `index.verb`, `index.adj`, `index.adv` from a
//! directory, folds them into one `lemma -> POS bitmask` map, drops multi-word
//! lemmas, and writes a byte-sorted `<lemma>\t<mask>` table with the Princeton
//! notice preserved as a `#` header. Prints a size report to stderr.
//! Test: `wordnet_pos::tests::the_shipped_table_is_sorted_and_parseable` proves
//! the committed output still satisfies every invariant the reader relies on.
//!
//! Run:
//! ```text
//! curl -O https://wordnetcode.princeton.edu/wn3.1.dict.tar.gz
//! tar xzf wn3.1.dict.tar.gz            # -> dict/index.{noun,verb,adj,adv}
//! SKIP_UI_BUILD=1 cargo run --release -p trusty-memory \
//!     --example wordnet_project -- dict crates/trusty-memory/wordnet/lemma-pos.txt
//! ```

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// POS bit layout, duplicated from `wordnet_pos` on purpose.
///
/// Why: the generator must not depend on the reader it feeds — if the bit
/// layout ever changes, a compile-time coupling would silently rewrite the
/// table's meaning instead of failing the reader's own assertions.
const NOUN: u8 = 1 << 0;
const VERB: u8 = 1 << 1;
const ADJ: u8 = 1 << 2;
const ADV: u8 = 1 << 3;

/// The Princeton notice, reproduced verbatim as the projection's header.
///
/// Why: the WordNet licence requires the copyright notice to appear on ALL
/// copies. The projection is a copy, and it discards the upstream files' own
/// headers, so the notice is carried explicitly here instead.
const NOTICE: &str = "\
This software and database is being provided to you, the LICENSEE, by
Princeton University under the following license.  By obtaining, using
and/or copying this software and database, you agree that you have
read, understood, and will comply with these terms and conditions.:

Permission to use, copy, modify and distribute this software and
database and its documentation for any purpose and without fee or
royalty is hereby granted, provided that you agree to comply with
the following copyright notice and statements, including the disclaimer,
and that the same appear on ALL copies of the software, database and
documentation, including modifications that you make for internal
use or for distribution.

WordNet 3.1 Copyright 2011 by Princeton University.  All rights reserved.

THIS SOFTWARE AND DATABASE IS PROVIDED \"AS IS\" AND PRINCETON
UNIVERSITY MAKES NO REPRESENTATIONS OR WARRANTIES, EXPRESS OR
IMPLIED.  BY WAY OF EXAMPLE, BUT NOT LIMITATION, PRINCETON
UNIVERSITY MAKES NO REPRESENTATIONS OR WARRANTIES OF MERCHANT-
ABILITY OR FITNESS FOR ANY PARTICULAR PURPOSE OR THAT THE USE
OF THE LICENSED SOFTWARE, DATABASE OR DOCUMENTATION WILL NOT
INFRINGE ANY THIRD PARTY PATENTS, COPYRIGHTS, TRADEMARKS OR
OTHER RIGHTS.

The name of Princeton University or Princeton may not be used in
advertising or publicity pertaining to distribution of the software
and/or database.  Title to copyright in this software, database and
any associated documentation shall at all times remain with
Princeton University and LICENSEE agrees to preserve same.";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let src = args
        .next()
        .ok_or("usage: wordnet_project <wordnet-dict-dir> <output-file>")?;
    let dst = args
        .next()
        .ok_or("usage: wordnet_project <wordnet-dict-dir> <output-file>")?;
    let src = Path::new(&src);

    // BTreeMap, not HashMap: the reader binary-searches the table, so byte-order
    // is a correctness property of the output, not a cosmetic one.
    let mut lemmas: BTreeMap<String, u8> = BTreeMap::new();
    let mut raw_bytes = 0usize;

    for (file, bit) in [
        ("index.noun", NOUN),
        ("index.verb", VERB),
        ("index.adj", ADJ),
        ("index.adv", ADV),
    ] {
        let path = src.join(file);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        raw_bytes += text.len();
        let mut kept = 0usize;
        for line in text.lines() {
            // Every licence-header line is indented; no data line is.
            if line.starts_with(' ') || line.is_empty() {
                continue;
            }
            let lemma = match line.split(' ').next() {
                // WordNet joins multi-word lemmas with `_`. The extractor only
                // ever looks up single whitespace-delimited tokens, so those
                // could never match — and they are over half of index.noun.
                Some(l) if !l.is_empty() && !l.contains('_') => l,
                _ => continue,
            };
            if lemma.starts_with('#') {
                return Err(format!("lemma {lemma:?} collides with the header marker").into());
            }
            *lemmas.entry(lemma.to_string()).or_insert(0) |= bit;
            kept += 1;
        }
        eprintln!(
            "{file:<12} {:>9} bytes  {kept:>7} single-word lemmas",
            text.len()
        );
    }

    let mut out = String::with_capacity(1 << 20);
    for line in NOTICE.lines() {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("#\n");
    out.push_str("# Projection of WordNet 3.1 index.{noun,verb,adj,adv} down to the only\n");
    out.push_str("# fact trusty-memory consults: which parts of speech a lemma can be.\n");
    out.push_str("# Regenerate with `cargo run --release -p trusty-memory --example\n");
    out.push_str("# wordnet_project -- <dict-dir> <this-file>`; see wordnet/README.md.\n");
    out.push_str("# Format: <lemma>\\t<mask>, byte-sorted by lemma, mask = decimal\n");
    out.push_str("# NOUN 1 | VERB 2 | ADJ 4 | ADV 8.\n");
    for (lemma, mask) in &lemmas {
        out.push_str(lemma);
        out.push('\t');
        out.push_str(&mask.to_string());
        out.push('\n');
    }

    std::fs::write(&dst, out.as_bytes())?;
    let mut err = std::io::stderr();
    writeln!(err, "---")?;
    writeln!(err, "lemmas          {}", lemmas.len())?;
    writeln!(err, "raw_bytes       {raw_bytes}")?;
    writeln!(err, "projected_bytes {}", out.len())?;
    writeln!(
        err,
        "reduction       {:.2}x",
        raw_bytes as f64 / out.len() as f64
    )?;
    Ok(())
}
