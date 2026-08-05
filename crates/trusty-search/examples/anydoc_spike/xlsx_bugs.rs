//! anydoc issues #8 and #9, reproduced against the installed Rust crate.
//!
//! Both were filed against the Python bindings; this leg establishes whether
//! they hold for `anydoc::to_markdown` on the same document shapes, and — the
//! part the issues do not answer — what they do to the text that would land
//! in an index chunk.

use crate::engines::Engine;
use crate::fixtures::Corpus;

pub fn run(corpus: &Corpus) {
    let fixtures = corpus.xlsx_bugs();

    for f in &fixtures {
        println!("## `{}`\n", f.name);
        println!("{}\n", f.note);
        for engine in [Engine::Native, Engine::Anydoc] {
            let out = engine.extract(&f.path);
            println!("### {} — {}\n", engine.label(), out.summary());
            println!("```\n{}\n```\n", out.text());
        }
        assess(&f.name, &f.path);
    }
}

fn assess(name: &str, path: &std::path::Path) {
    let native = Engine::Native.extract(path);
    let anydoc = Engine::Anydoc.extract(path);
    let (n, a) = (native.text(), anydoc.text());

    match name {
        "merged-range.xlsx" => {
            println!("**Issue #8 assessment**\n");
            println!("| check | native | anydoc |");
            println!("|---|---|---|");
            println!(
                "| anchor text `Merged heading` present | {} | {} |",
                yes(n.contains("Merged heading")),
                yes(a.contains("Merged heading"))
            );
            println!(
                "| non-empty output lines | {} | {} |",
                n.lines().filter(|l| !l.trim().is_empty()).count(),
                a.lines().filter(|l| !l.trim().is_empty()).count()
            );
            println!("| extracted chars | {} | {} |\n", n.len(), a.len());
            println!(
                "Index-level reading: the merge span is layout metadata. What reaches a chunk is \
                 the anchor's text either way, so a clipped span changes the rendered table shape \
                 without removing a searchable term — as long as the anchor text itself survives.\n"
            );
        }
        "hidden-content.xlsx" => {
            println!("**Issue #9 assessment**\n");
            println!("| check | native | anydoc |");
            println!("|---|---|---|");
            for probe in ["Visible row", "Hidden row", "Hidden column"] {
                println!(
                    "| `{}` in extracted text | {} | {} |",
                    probe,
                    yes(n.contains(probe)),
                    yes(a.contains(probe))
                );
            }
            println!();
            println!(
                "Index-level reading: hidden cells becoming searchable text is a behaviour \
                 question, not a fidelity one — it makes content the author suppressed \
                 retrievable. Whether that is a defect depends on whether the index is meant to \
                 mirror what a reader sees or what the file contains. Both extractors' behaviour \
                 is reported above rather than judged here.\n"
            );
        }
        _ => {}
    }
}

fn yes(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}
