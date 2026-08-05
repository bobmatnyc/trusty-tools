//! Real-document leg: run both extractors over a directory of actual
//! `.pdf` / `.docx` / `.xlsx` files.
//!
//! Synthetic fixtures answer "does the parser handle the shape I built"; only
//! real documents answer "does it handle what a user's repo contains". The
//! synthetic PDF corpus already produced one disagreement between the two
//! extractors, which is exactly the kind of result that needs a real sample
//! before it is believed or dismissed.
//!
//! PRIVACY: this reads whatever directory it is pointed at, which in practice
//! is the operator's own documents. Nothing here prints a filename, a path, or
//! a byte of extracted content — rows are labelled by format and index, and
//! the only per-file values reported are sizes, timings, and outcome
//! categories. Keep it that way if this harness is ever rerun.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::engines::{fmt_ms, Engine, Outcome};

/// Outcome buckets that matter for an adoption decision. "Both failed" is
/// separated from "one failed" because a document neither extractor reads is
/// a property of the document, not a difference between the candidates.
#[derive(Default)]
struct Tally {
    both_ok: usize,
    both_err: usize,
    native_only: usize,
    anydoc_only: usize,
}

pub fn run(dir: &Path) {
    let mut files = collect(dir);
    files.sort();

    println!("## Real-document corpus\n");
    println!(
        "Source: a directory of {} actual documents on the measurement host. Filenames and \
         content are deliberately omitted; rows are labelled by format and index.\n",
        files.len()
    );

    let mut per_format: std::collections::BTreeMap<String, Tally> = Default::default();
    let mut rows: Vec<String> = Vec::new();
    let mut counter: std::collections::BTreeMap<String, usize> = Default::default();
    let mut native_errs: std::collections::BTreeMap<String, usize> = Default::default();
    let mut anydoc_errs: std::collections::BTreeMap<String, usize> = Default::default();

    for path in &files {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let idx = counter.entry(ext.clone()).or_insert(0);
        *idx += 1;
        let label = format!("{ext}-{idx:03}");
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        let t = Instant::now();
        let n = Engine::Native.extract(path);
        let n_ms = t.elapsed();
        let t = Instant::now();
        let a = Engine::Anydoc.extract(path);
        let a_ms = t.elapsed();

        for (o, bucket) in [(&n, &mut native_errs), (&a, &mut anydoc_errs)] {
            if let Some(m) = failure_message(o) {
                *bucket.entry(m).or_insert(0) += 1;
            }
        }

        let tally = per_format.entry(ext.clone()).or_default();
        // A native "ok" carrying the no-extractable-text warning is counted as
        // a failure to extract: zero indexed text is zero indexed text,
        // whatever the return type says. Otherwise the comparison would score
        // our extractor a win for producing an empty string.
        match (usable(&n), usable(&a)) {
            (true, true) => tally.both_ok += 1,
            (false, false) => tally.both_err += 1,
            (true, false) => tally.native_only += 1,
            (false, true) => tally.anydoc_only += 1,
        }

        rows.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            label,
            size,
            n.text().trim().len(),
            a.text().trim().len(),
            fmt_ms(n_ms),
            fmt_ms(a_ms),
            short(&n),
            short(&a),
        ));
    }

    println!("### Outcome summary\n");
    println!("| format | files | both extracted | native only | anydoc only | neither |");
    println!("|---|---:|---:|---:|---:|---:|");
    for (fmt, t) in &per_format {
        println!(
            "| {} | {} | {} | {} | {} | {} |",
            fmt,
            t.both_ok + t.both_err + t.native_only + t.anydoc_only,
            t.both_ok,
            t.native_only,
            t.anydoc_only,
            t.both_err
        );
    }

    println!("\n### Failure categories\n");
    println!(
        "Error text only — anydoc's and ours both describe the parse, not the file, so no \
         filename leaks through here.\n"
    );
    println!("| engine | occurrences | message |");
    println!("|---|---:|---|");
    let mut cats: Vec<(&str, String, usize)> = Vec::new();
    for (engine, msgs) in [("native", &native_errs), ("anydoc", &anydoc_errs)] {
        for (msg, n) in msgs {
            cats.push((engine, msg.clone(), *n));
        }
    }
    cats.sort_by_key(|c| std::cmp::Reverse(c.2));
    for (engine, msg, n) in cats {
        println!("| {} | {} | {} |", engine, n, msg);
    }

    println!("\n### Per-file\n");
    println!(
        "| file | bytes | native chars | anydoc chars | native ms | anydoc ms | native | anydoc |"
    );
    println!("|---|---:|---:|---:|---:|---:|---|---|");
    for r in &rows {
        println!("{r}");
    }
}

/// A one-line, page-count-normalised failure description, or `None` when the
/// extractor produced usable text. Digit runs are collapsed so "40 pages" and
/// "3 pages" bucket together instead of producing one category per document.
fn failure_message(o: &Outcome) -> Option<String> {
    let raw = match o {
        Outcome::Err(e) => e.clone(),
        Outcome::Ok { text, warning } if text.trim().is_empty() => warning
            .clone()
            .unwrap_or_else(|| "ok but empty text".to_string()),
        Outcome::Ok { .. } => return None,
    };
    let line = raw.lines().next().unwrap_or("").trim();
    let mut out = String::with_capacity(line.len());
    let mut in_digits = false;
    for c in line.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push('N');
                in_digits = true;
            }
        } else {
            in_digits = false;
            out.push(c);
        }
    }
    Some(out)
}

/// Did this extractor actually produce indexable text?
fn usable(o: &Outcome) -> bool {
    match o {
        Outcome::Ok { text, .. } => !text.trim().is_empty(),
        Outcome::Err(_) => false,
    }
}

fn short(o: &Outcome) -> &'static str {
    match o {
        Outcome::Ok { text, .. } if text.trim().is_empty() => "empty",
        Outcome::Ok {
            warning: Some(_), ..
        } => "ok+warn",
        Outcome::Ok { .. } => "ok",
        Outcome::Err(_) => "err",
    }
}

/// Every extractable-extension file under `dir`, bounded by the same 10 MiB
/// cap the walker applies so the sample matches what production would index.
fn collect(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let ext = p
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !trusty_search::core::extract::is_extractable_ext(&ext) {
                continue;
            }
            if std::fs::metadata(&p).map(|m| m.len()).unwrap_or(u64::MAX)
                > trusty_search::core::extract::MAX_OFFICE_FILE_BYTES
            {
                continue;
            }
            out.push(p);
        }
    }
    out
}
