//! Proof that an empty PDF really has no text layer (SPEC-OKF-01 Appendix B
//! `no_text_layer`). #10746 S10b fix round.
//!
//! Why: `no_text_layer` is an `Ok` reply the host fingerprints, so an item
//! labelled with it is never extracted again. xberg returns the same empty
//! result for an image-only page and for a page whose content stream fails
//! to decode, so the label must not rest on xberg's empty text alone. Any
//! case this module cannot prove is `extract_error`, which the host retries.
//! What: [`no_text_layer`] is `true` only when the PDF loads, has at least
//! one page, every content stream of every page decodes (inside a bounded
//! budget; a non-blank stream that decodes to nothing counts as a failure,
//! because lopdf hides a failed inflate that way) into valid operators, no page shows text (`Tj`, `TJ`, `'`, `"`),
//! and every XObject a page draws resolves to an image. Anything else,
//! including a form XObject it does not descend into, is `false`.
//! Test: `scanned_pdf_is_empty_text_with_a_note`,
//! `empty_pdf_without_a_readable_page_is_an_extract_error` in
//! `tests/fixtures.rs`.
//!
//! # Spec References
//! - SPEC-OKF-01 Appendix B "Error codes" (`no_text_layer`), §3 (p) A.

use lopdf::content::Content;
use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId};

/// The most decoded content bytes inspected, over all pages.
const DECODE_BUDGET: usize = 64 * 1024 * 1024;

/// Operators that show text.
const TEXT_OPS: [&str; 4] = ["Tj", "TJ", "'", "\""];

/// Whether `bytes` is a PDF with pages, every page readable, and no text.
pub fn no_text_layer(bytes: &[u8]) -> bool {
    let opts = LoadOptions {
        max_decompressed_size: Some(DECODE_BUDGET),
        ..LoadOptions::default()
    };
    let Ok(doc) = Document::load_mem_with_options(bytes, opts) else {
        return false;
    };
    let pages = doc.get_pages();
    let mut budget = DECODE_BUDGET;
    !pages.is_empty()
        && pages
            .values()
            .all(|&page| page_has_no_text(&doc, page, &mut budget))
}

fn page_has_no_text(doc: &Document, page: ObjectId, budget: &mut usize) -> bool {
    let mut data = Vec::new();
    for id in doc.get_page_contents(page) {
        let Ok(stream) = doc.get_object(id).and_then(Object::as_stream) else {
            return false;
        };
        let Ok(decoded) = stream.decompressed_content_with_limit(*budget) else {
            return false;
        };
        // lopdf logs a failed inflate and returns empty output, so a stream
        // whose bytes decode to nothing did not decode.
        if decoded.is_empty() && !stream.content.iter().all(u8::is_ascii_whitespace) {
            return false;
        }
        *budget = budget.saturating_sub(decoded.len());
        data.extend_from_slice(&decoded);
        data.push(b'\n');
    }
    let Ok(content) = Content::decode(&data) else {
        return false;
    };
    content.operations.iter().all(|op| {
        if TEXT_OPS.contains(&op.operator.as_str()) {
            return false;
        }
        if op.operator == "Do" {
            return match op.operands.first().and_then(|o| o.as_name().ok()) {
                Some(name) => xobject_is_image(doc, page, name),
                None => false,
            };
        }
        true
    })
}

/// Whether the page's XObject resource `name` is an image.
fn xobject_is_image(doc: &Document, page: ObjectId, name: &[u8]) -> bool {
    let Ok((inline, ids)) = doc.get_page_resources(page) else {
        return false;
    };
    let dicts = inline
        .into_iter()
        .chain(ids.into_iter().filter_map(|id| doc.get_dictionary(id).ok()));
    dicts
        .filter_map(|res| deref_dict(doc, res.get(b"XObject").ok()?))
        .find_map(|xobjects| xobjects.get(name).ok())
        .and_then(|o| doc.dereference(o).ok())
        .and_then(|(_, o)| o.as_stream().ok())
        .is_some_and(|s| {
            s.dict
                .get(b"Subtype")
                .and_then(Object::as_name)
                .is_ok_and(|n| n == b"Image")
        })
}

fn deref_dict<'a>(doc: &'a Document, o: &'a Object) -> Option<&'a Dictionary> {
    doc.dereference(o).ok()?.1.as_dict().ok()
}
