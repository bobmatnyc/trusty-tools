//! Hand-built PDF fixtures with byte-accurate `xref` tables.
//!
//! The builder follows the same incremental-offset approach as the existing
//! `core::extract::pdf` tests: lopdf (under both `pdf-extract` and
//! `pdf-inspector`) resolves objects through the `xref` table, so an
//! approximate table simply fails to parse and would measure nothing.

use std::io::Write;

/// Assemble a PDF from pre-rendered object bodies (`"<< ... >>"` or
/// `"<< ... >>\nstream\n...\nendstream"`), numbering them 1..=n and emitting
/// a correct `xref` + trailer. Object 1 must be the Catalog.
fn assemble(objects: &[String]) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::with_capacity(objects.len());
    buf.extend_from_slice(b"%PDF-1.4\n");
    for (i, body) in objects.iter().enumerate() {
        offsets.push(buf.len());
        write!(buf, "{} 0 obj\n{}\nendobj\n", i + 1, body).unwrap();
    }
    let xref_start = buf.len();
    let size = objects.len() + 1;
    let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
    for off in &offsets {
        xref.push_str(&format!("{off:010} 00000 n \n"));
    }
    buf.extend_from_slice(xref.as_bytes());
    write!(
        buf,
        "trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF"
    )
    .unwrap();
    buf
}

fn content_stream(ops: &str) -> String {
    format!(
        "<< /Length {} >>\nstream\n{}\nendstream",
        ops.len(),
        ops
    )
}

/// Single page carrying one text-showing operator.
pub fn text_layer(text: &str) -> Vec<u8> {
    let ops = format!("BT\n/F1 12 Tf\n20 150 Td\n({text}) Tj\nET");
    assemble(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> \
         /MediaBox [0 0 200 200] /Contents 5 0 R >>"
            .to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        content_stream(&ops),
    ])
}

/// `pages` pages, each with its own content stream.
pub fn multi_page(pages: usize) -> Vec<u8> {
    // Object layout: 1 Catalog, 2 Pages, 3 Font, then (page, content) pairs.
    let first_page_obj = 4;
    let kids: String = (0..pages)
        .map(|i| format!("{} 0 R ", first_page_obj + i * 2))
        .collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    for i in 0..pages {
        let content_obj = first_page_obj + i * 2 + 1;
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 3 0 R >> >> \
             /MediaBox [0 0 400 400] /Contents {content_obj} 0 R >>"
        ));
        let ops = format!(
            "BT\n/F1 12 Tf\n20 350 Td\n(Page {i} discusses the operating margin and its drivers.) Tj\nET"
        );
        objects.push(content_stream(&ops));
    }
    assemble(&objects)
}

/// A structurally valid page with an empty content stream — no text operators
/// at all. This is what a scanned/image-only PDF looks like to a text-layer
/// extractor, without needing a real embedded raster.
pub fn image_only() -> Vec<u8> {
    assemble(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_string(),
        content_stream(""),
    ])
}

/// RUSTSEC-2026-0187 proof of concept: a page object carrying a
/// `depth`-deep nested array.
///
/// The advisory reports this as a stack overflow that aborts the process with
/// `SIGABRT` — not a `panic!`, so `catch_unwind` and `spawn_blocking`'s
/// JoinError containment do not stop it. Patched in lopdf 0.42.0, which
/// enforces a nesting cap and returns `Err`. `pdf-extract 0.12` (ours) pulls
/// 0.42.0; `pdf-inspector 0.1.7` (anydoc's) pulls 0.41.0.
pub fn deeply_nested_objects(depth: usize) -> Vec<u8> {
    let nested: String = "[".repeat(depth) + &"]".repeat(depth);
    assemble(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Deep {nested} >>"
        ),
        content_stream(""),
    ])
}
