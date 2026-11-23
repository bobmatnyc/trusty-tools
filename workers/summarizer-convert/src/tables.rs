//! Places the tables Xberg detects in a PDF after the text of their page.
//!
//! Why: label/value pairs in slide-deck PDFs reach the summarizer as
//! unrelated lines; the table Xberg already detects keeps each row together
//! (spec `docs/plans/2026-11-23-converter-tables-spec.md`, PR #2).
//!
//! What: a PDF is extracted with Xberg's page markers on (no per-page
//! extraction). [`assemble`] splits the text at those markers, drops them,
//! and appends each table that passes [`keep`] after its page's text as a
//! `[table page= rows= cols=]` ... `[/table]` block. The page text is left
//! as it is (no de-duplication, owner ruling Q2). Other formats return
//! Xberg's text unchanged.
//! Test: `workers/summarizer-convert/src/tables.rs` (unit tests),
//! `pdf_tables_follow_their_page` in `tests/worker.rs`

use xberg::Table;
use xberg::core::config::PageConfig;

/// Page marker line. The private-use characters keep it from matching text.
const MARKER_OPEN: char = '\u{E000}';
const MARKER_WORD: &str = "trusty-page ";

/// Xberg page config that writes a marker line before each page's text.
pub fn page_markers() -> PageConfig {
    PageConfig {
        extract_pages: false,
        insert_page_markers: true,
        marker_format: format!("\n\n{MARKER_OPEN}{MARKER_WORD}{{page_num}}{MARKER_OPEN}\n\n"),
        ..PageConfig::default()
    }
}

/// Returns the page number of a marker line, or `None` for any other line.
fn marker_page(line: &str) -> Option<u32> {
    line.strip_prefix(MARKER_OPEN)?
        .strip_suffix(MARKER_OPEN)?
        .strip_prefix(MARKER_WORD)?
        .parse()
        .ok()
}

/// `(rows, cols)` of a table: its row count and its longest row.
fn shape(cells: &[Vec<String>]) -> (usize, usize) {
    (cells.len(), cells.iter().map(Vec::len).max().unwrap_or(0))
}

/// The false-positive gate (owner ruling Q3): at least 2 rows, at least 2
/// columns, and at least 60 percent of the `rows * cols` cells non-empty
/// after trimming. A missing cell in a short row counts as empty.
/// Test: `gate_keeps_two_by_two_at_sixty_percent`, `gate_drops_below_bounds`
pub fn keep(cells: &[Vec<String>]) -> bool {
    let (rows, cols) = shape(cells);
    if rows < 2 || cols < 2 {
        return false;
    }
    let filled = cells
        .iter()
        .flatten()
        .filter(|c| !c.trim().is_empty())
        .count();
    filled * 100 >= rows * cols * 60
}

/// One table block, marker to closing marker.
fn block(table: &Table) -> String {
    let (rows, cols) = shape(&table.cells);
    format!(
        "[table page={} rows={rows} cols={cols}]\n{}\n[/table]",
        table.page_number,
        table.markdown.trim()
    )
}

/// Builds the converted text for one document.
///
/// Why: tables must sit with the page they came from (owner ruling Q1:
/// placement by page number against page breaks, no per-page extraction).
/// What: for `pdf`, splits `content` at page marker lines into page texts
/// (each trimmed of blank lines at its ends), joins the non-empty pages with
/// one blank line, which is how Xberg separates pages without markers, and
/// puts each kept table's block after its page's text, in page order. A
/// table whose page has no marker goes at the end. Any other format returns
/// `content` unchanged and ignores `tables` (XLSX keeps its sheet text,
/// owner ruling Q5).
/// Test: `tables_follow_their_page`, `no_tables_gives_text_without_markers`,
/// `other_formats_are_unchanged`, `unmatched_page_goes_last`
pub fn assemble(format: &str, content: String, tables: &[Table]) -> String {
    if format != "pdf" {
        return content;
    }
    // (page number, text); text before the first marker has no page.
    let mut pages: Vec<(Option<u32>, Vec<&str>)> = vec![(None, Vec::new())];
    for line in content.lines() {
        match marker_page(line) {
            Some(n) => pages.push((Some(n), Vec::new())),
            None => pages.last_mut().expect("non-empty").1.push(line),
        }
    }
    let kept: Vec<&Table> = tables.iter().filter(|t| keep(&t.cells)).collect();
    let mut parts: Vec<String> = Vec::new();
    for (page, lines) in &pages {
        let text = lines.join("\n");
        let text = text.trim_matches('\n');
        if !text.is_empty() {
            parts.push(text.to_owned());
        }
        if let Some(n) = page {
            parts.extend(
                kept.iter()
                    .filter(|t| t.page_number == *n)
                    .map(|t| block(t)),
            );
        }
    }
    let placed = |t: &&Table| pages.iter().any(|(p, _)| *p == Some(t.page_number));
    parts.extend(kept.iter().filter(|t| !placed(t)).map(|t| block(t)));
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|c| (*c).to_owned()).collect())
            .collect()
    }

    fn table(page: u32, rows: &[&[&str]]) -> Table {
        let cells = cells(rows);
        let markdown = cells
            .iter()
            .map(|r| format!("| {} |", r.join(" | ")))
            .collect::<Vec<_>>()
            .join("\n");
        Table {
            cells,
            markdown,
            page_number: page,
            ..Table::default()
        }
    }

    fn marker(page: u32) -> String {
        format!("{MARKER_OPEN}{MARKER_WORD}{page}{MARKER_OPEN}")
    }

    /// Xberg's text for three pages with the markers `page_markers` asks for.
    fn three_pages() -> String {
        format!(
            "{}\n\nPage one.\n\n{}\n\nPage two a.\nPage two b.\n\n{}\n\nPage three.",
            marker(1),
            marker(2),
            marker(3)
        )
    }

    #[test]
    fn marker_format_round_trips() {
        let format = page_markers().marker_format;
        let line = format.trim().replace("{page_num}", "12");
        assert_eq!(marker_page(&line), Some(12));
        assert_eq!(marker_page("trusty-page 12"), None);
        assert_eq!(marker_page("Page 12"), None);
    }

    #[test]
    fn gate_keeps_two_by_two_at_sixty_percent() {
        // 2x2, all filled.
        assert!(keep(&cells(&[&["a", "b"], &["c", "d"]])));
        // 5x2 = 10 cells, 6 filled: exactly 60 percent.
        assert!(keep(&cells(&[
            &["a", "b"],
            &["c", "d"],
            &["e", "f"],
            &["", " "],
            &["", ""],
        ])));
    }

    #[test]
    fn gate_drops_below_bounds() {
        // One row.
        assert!(!keep(&cells(&[&["a", "b", "c"]])));
        // One column.
        assert!(!keep(&cells(&[&["a"], &["b"], &["c"]])));
        // 100 cells (10x10), 59 filled: 59 percent.
        let mut rows = vec![vec![String::new(); 10]; 10];
        for i in 0..59 {
            rows[i / 10][i % 10] = "x".into();
        }
        assert!(!keep(&rows));
        // Same table with 60 filled is kept.
        rows[5][9] = "x".into();
        assert!(keep(&rows));
        // Missing cells in a short row count as empty: 3 of 4.
        assert!(keep(&cells(&[&["a", "b"], &["c"]])));
        // 2 of 4.
        assert!(!keep(&cells(&[&["a", ""], &["c"]])));
    }

    #[test]
    fn tables_follow_their_page() {
        let t = table(2, &[&["Label", "Value"], &["Lights", "42"]]);
        let out = assemble("pdf", three_pages(), &[t]);
        assert_eq!(
            out,
            "Page one.\n\nPage two a.\nPage two b.\n\n\
             [table page=2 rows=2 cols=2]\n| Label | Value |\n| Lights | 42 |\n[/table]\n\n\
             Page three."
        );
    }

    #[test]
    fn gated_out_tables_are_not_emitted() {
        let wide = table(1, &[&["only", "one", "row"]]);
        let out = assemble("pdf", three_pages(), &[wide]);
        assert_eq!(out, "Page one.\n\nPage two a.\nPage two b.\n\nPage three.");
    }

    #[test]
    fn several_tables_on_a_page_keep_their_order() {
        let a = table(3, &[&["a", "b"], &["c", "d"]]);
        let b = table(1, &[&["e", "f"], &["g", "h"]]);
        let c = table(3, &[&["i", "j"], &["k", "l"]]);
        let out = assemble("pdf", three_pages(), &[a, b, c]);
        let first = out.find("| e | f |").unwrap();
        let page_two = out.find("Page two a.").unwrap();
        let a_at = out.find("| a | b |").unwrap();
        let c_at = out.find("| i | j |").unwrap();
        let page_three = out.find("Page three.").unwrap();
        assert!(
            first < page_two && page_three < a_at && a_at < c_at,
            "{out}"
        );
    }

    #[test]
    fn unmatched_page_goes_last() {
        let t = table(9, &[&["a", "b"], &["c", "d"]]);
        let out = assemble("pdf", three_pages(), &[t]);
        assert!(
            out.ends_with(
                "Page three.\n\n[table page=9 rows=2 cols=2]\n| a | b |\n| c | d |\n[/table]"
            ),
            "{out}"
        );
    }

    #[test]
    fn empty_page_with_a_table_still_gets_its_block() {
        let content = format!("{}\n\n{}\n\nPage two.", marker(1), marker(2));
        let t = table(1, &[&["a", "b"], &["c", "d"]]);
        let out = assemble("pdf", content, &[t]);
        assert_eq!(
            out,
            "[table page=1 rows=2 cols=2]\n| a | b |\n| c | d |\n[/table]\n\nPage two."
        );
    }

    #[test]
    fn no_tables_gives_text_without_markers() {
        let out = assemble("pdf", three_pages(), &[]);
        assert_eq!(out, "Page one.\n\nPage two a.\nPage two b.\n\nPage three.");
        // A page with no text adds no separator, as without markers.
        let content = format!("{}\n\n{}\n\nOnly text.", marker(1), marker(2));
        assert_eq!(assemble("pdf", content, &[]), "Only text.");
        assert_eq!(assemble("pdf", marker(1), &[]), "");
        assert_eq!(assemble("pdf", String::new(), &[]), "");
    }

    #[test]
    fn other_formats_are_unchanged() {
        let t = table(1, &[&["a", "b"], &["c", "d"]]);
        for format in ["xlsx", "pptx", "docx", "html", "eml"] {
            let text = "Sheet text\n\nrow a\trow b".to_owned();
            assert_eq!(
                assemble(format, text.clone(), std::slice::from_ref(&t)),
                text
            );
        }
    }
}
