//! Output-quality leg: what each extractor recovers, and what survives the
//! chunker.
//!
//! The second question is the one that decides whether structure recovery is
//! worth anything at the index level. `core::extract` hands every one of
//! these formats to `chunk_ast`, which finds no tree-sitter grammar and falls
//! through to `chunk_text(file, content, 150, 50)` — a flat 150-line sliding
//! window with 50-line stride. Markdown table pipes and `##` headings are
//! just characters to that chunker, so the gain has to show up as retrieved
//! terms inside a chunk, not as prettier output.

use crate::engines::{Engine, Outcome};
use crate::fixtures::{Corpus, Fixture};

/// Window and stride `chunk_ast` applies to extension-unknown content — the
/// path every EXTRACT_EXTS document takes.
const WINDOW: usize = 150;
const STRIDE: usize = 50;

pub fn run(corpus: &Corpus) {
    let fixtures = corpus.benign();

    println!("## Extracted-text shape\n");
    println!("| fixture | native | anydoc |");
    println!("|---|---|---|");
    for f in &fixtures {
        println!(
            "| {} | {} | {} |",
            f.name,
            Engine::Native.extract(&f.path).summary(),
            Engine::Anydoc.extract(&f.path).summary()
        );
    }

    println!("\n## Structure recovery (docx)\n");
    for f in fixtures.iter().filter(|f| f.format == "docx") {
        structure_report(f);
    }

    println!("\n## Chunk-level impact\n");
    println!(
        "Both texts chunked with chunk_text(window={WINDOW}, stride={STRIDE}) — the production path.\n"
    );
    println!("| fixture | native chunks | anydoc chunks | native lines | anydoc lines | native chars | anydoc chars |");
    println!("|---|---:|---:|---:|---:|---:|---:|");
    for f in &fixtures {
        let n = Engine::Native.extract(&f.path);
        let a = Engine::Anydoc.extract(&f.path);
        let nc = chunk_count(&f.name, n.text());
        let ac = chunk_count(&f.name, a.text());
        println!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            f.name,
            nc,
            ac,
            n.text().lines().count(),
            a.text().lines().count(),
            n.text().len(),
            a.text().len()
        );
    }

    println!("\n## Sample output (structured-report.docx, first 900 chars each)\n");
    if let Some(f) = fixtures.iter().find(|f| f.name == "structured-report.docx") {
        for engine in [Engine::Native, Engine::Anydoc] {
            let out = engine.extract(&f.path);
            println!("### {}\n", engine.label());
            println!("```\n{}\n```\n", head(out.text(), 900));
        }
    }
}

fn chunk_count(name: &str, text: &str) -> usize {
    trusty_search::core::chunker::chunk_text(name, text, WINDOW, STRIDE).len()
}

/// Count the structural signals each extractor preserved.
///
/// `table cells recovered` is the load-bearing number: our extractor emits a
/// `<w:tbl>` cell as an anonymous paragraph, so the cell TEXT survives but
/// every row/column boundary is gone. Counting the delimiter characters is
/// the difference between "the words are present" and "the table is present".
fn structure_report(f: &Fixture) {
    let n = Engine::Native.extract(&f.path);
    let a = Engine::Anydoc.extract(&f.path);
    println!("**{}** — {}\n", f.name, f.note);
    println!("| signal | native | anydoc |");
    println!("|---|---:|---:|");
    for (label, count) in [
        ("markdown heading lines (`#`)", count_headings as fn(&str) -> usize),
        ("table delimiter rows (`| --- |`)", count_table_rules),
        ("table pipe characters", count_pipes),
    ] {
        println!("| {} | {} | {} |", label, count(n.text()), count(a.text()));
    }
    // Cell-text presence is separate from cell-structure presence.
    let probe = "col 0";
    println!(
        "| occurrences of table cell text `{}` | {} | {} |",
        probe,
        n.text().matches(probe).count(),
        a.text().matches(probe).count()
    );
    println!(
        "| heading text `Quarterly Operations Review` present | {} | {} |",
        present(&n, "Quarterly Operations Review"),
        present(&a, "Quarterly Operations Review")
    );
    println!();
}

fn present(o: &Outcome, needle: &str) -> &'static str {
    if o.text().contains(needle) {
        "yes"
    } else {
        "no"
    }
}

fn count_headings(s: &str) -> usize {
    s.lines().filter(|l| l.trim_start().starts_with('#')).count()
}

fn count_table_rules(s: &str) -> usize {
    s.lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with('|') && t.contains("---")
        })
        .count()
}

fn count_pipes(s: &str) -> usize {
    s.matches('|').count()
}

fn head(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut cut = n;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &s[..cut])
}
