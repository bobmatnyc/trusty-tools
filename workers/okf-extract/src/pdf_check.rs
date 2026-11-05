//! Proof that an empty PDF really has no text layer (SPEC-OKF-01 Appendix B
//! `no_text_layer`). #10746 S10b fix round.
//!
//! Why: `no_text_layer` is an `Ok` reply the host fingerprints, so an item
//! labelled with it is never extracted again. xberg returns the same empty
//! result for an image-only page and for a page whose content stream fails
//! to decode, so the label must not rest on xberg's empty text alone. Any
//! case this module cannot prove is `extract_error`, which the host retries.
//! What: [`no_text_layer`] is `true` only when the PDF loads inside a parse
//! budget, every node of its page tree resolves to a page or a page-tree
//! node and there is at least one page, every content stream of every page
//! decodes in full (no filter, or `FlateDecode` only, with no `DecodeParms`)
//! and parses in full into operators, no page shows text (`Tj`, `TJ`, `'`,
//! `"`), and every XObject a page draws resolves to an image. Anything
//! else, including a form XObject it does not descend into, is `false`.
//! The parse budget keeps lopdf's parsed objects (about 300 bytes of heap
//! per input byte at worst) under half the address space `RLIMIT_AS`
//! leaves the worker, and under 1 GiB.
//! Test: `scanned_pdf_is_empty_text_with_a_note`,
//! `empty_pdf_without_a_readable_page_is_an_extract_error` in
//! `tests/fixtures.rs`; the unit tests below, one per #10746 finding.
//!
//! # Spec References
//! - SPEC-OKF-01 Appendix B "Error codes" (`no_text_layer`), §3 (p) A.

use std::cell::Cell;
use std::collections::HashSet;
use std::io::Read;

use flate2::read::ZlibDecoder;
use lopdf::content::Content;
use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId, Stream};

/// The most decoded content bytes inspected, over all pages.
const DECODE_BUDGET: usize = 64 * 1024 * 1024;

/// The most bytes one object stream may decode to while the PDF loads.
const OBJECT_STREAM_CAP: usize = 8 * 1024 * 1024;

/// The parse budget's ceiling, in estimated heap bytes.
const PARSE_BUDGET_CAP: usize = 1024 * 1024 * 1024;

/// The deepest page tree walked.
const PAGE_TREE_DEPTH: usize = 256;

/// Operators that show text.
const TEXT_OPS: [&str; 4] = ["Tj", "TJ", "'", "\""];

thread_local! {
    /// #10746: the parse budget left while lopdf loads; `None` once spent.
    static LOAD_BUDGET: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Whether `bytes` is a PDF with pages, every page readable, and no text.
pub fn no_text_layer(bytes: &[u8]) -> bool {
    // #10746: a parse that could pass RLIMIT_AS is refused before it starts.
    let Some(left) = parse_budget().checked_sub(parse_cost(bytes)) else {
        return false;
    };
    LOAD_BUDGET.set(Some(left));
    let opts = LoadOptions {
        max_decompressed_size: Some(OBJECT_STREAM_CAP),
        filter: Some(charge_object_stream),
        ..LoadOptions::default()
    };
    let doc = Document::load_mem_with_options(bytes, opts);
    // #10746: an object stream over the budget fails the whole load.
    let (Ok(doc), Some(left)) = (doc, LOAD_BUDGET.take()) else {
        return false;
    };
    // #10746: a page lenient loading dropped fails the walk, not the count.
    let Some(pages) = page_tree(&doc) else {
        return false;
    };
    let mut decoded_left = DECODE_BUDGET;
    !pages.is_empty()
        && pages
            .iter()
            .all(|&page| page_has_no_text(&doc, page, left, &mut decoded_left))
}

/// Load filter: charges each object stream's parse cost before lopdf
/// expands it, and keeps no object once the budget is spent.
fn charge_object_stream(id: ObjectId, obj: &mut Object) -> Option<(ObjectId, Object)> {
    if let Object::Stream(s) = obj
        && s.dict.has_type(b"ObjStm")
    {
        // #10746: object streams multiply the parse input; charge each one.
        let cost = decode(s, OBJECT_STREAM_CAP).map(|d| parse_cost(&d));
        let left = LOAD_BUDGET
            .get()
            .zip(cost)
            .and_then(|(l, c)| l.checked_sub(c));
        LOAD_BUDGET.set(left);
    }
    LOAD_BUDGET.get()?;
    // lopdf keeps `obj` for a top-level object and the returned copy for one
    // inside an object stream, so both must hold it. The copy's transient
    // heap is inside the budget's 2x headroom margin.
    Some((id, obj.clone()))
}

fn page_has_no_text(doc: &Document, page: ObjectId, left: usize, decoded_left: &mut usize) -> bool {
    // #10746: decoded bytes are capped by the parse budget left as well.
    let cap = (*decoded_left).min(left / 2);
    let mut data = Vec::new();
    for id in doc.get_page_contents(page) {
        let Ok(stream) = doc.get_object(id).and_then(Object::as_stream) else {
            return false;
        };
        let Some(decoded) = decode(stream, cap.saturating_sub(data.len())) else {
            return false;
        };
        data.extend_from_slice(&decoded);
        data.push(b'\n');
    }
    *decoded_left = decoded_left.saturating_sub(data.len());
    // #10746: a page whose operators could pass RLIMIT_AS is not parsed.
    if parse_cost(&data) > left {
        return false;
    }
    // #10746: strict, so a bad token is an error, not the end of the page.
    let Ok(content) = Content::decode_strict(&data) else {
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

/// A stream's content decoded in full, at most `limit` bytes. `None` for
/// any filter but `FlateDecode`, any `DecodeParms`, or a stream that does
/// not inflate to its end with a valid checksum.
fn decode(stream: &Stream, limit: usize) -> Option<Vec<u8>> {
    let raw = &stream.content;
    if raw.iter().all(u8::is_ascii_whitespace) {
        return Some(Vec::new());
    }
    // #10746: lopdf returns a truncated or corrupt inflate as a partial Ok,
    // and its LZW and ASCII85 decoders stop early the same way, so only a
    // strict FlateDecode chain counts.
    if stream.dict.has(b"DecodeParms") {
        return None;
    }
    let filters = match stream.dict.get(b"Filter") {
        Ok(_) => stream.filters().ok()?,
        Err(_) => Vec::new(),
    };
    let mut out: Option<Vec<u8>> = None;
    for filter in filters {
        if filter != b"FlateDecode" {
            return None;
        }
        let src = out.as_deref().unwrap_or(raw);
        let mut buf = Vec::new();
        let take = u64::try_from(limit).ok()?.saturating_add(1);
        ZlibDecoder::new(src)
            .take(take)
            .read_to_end(&mut buf)
            .ok()?;
        if buf.len() > limit {
            return None;
        }
        out = Some(buf);
    }
    match out {
        Some(decoded) => Some(decoded),
        None => (raw.len() <= limit).then(|| raw.clone()),
    }
}

/// Every page of the page tree, or `None` when any node does not resolve
/// to a page or a page-tree node, a node repeats, or the tree is too deep.
fn page_tree(doc: &Document) -> Option<Vec<ObjectId>> {
    // #10746: lopdf's own page iterator skips a kid it cannot resolve.
    let root = doc
        .catalog()
        .ok()?
        .get(b"Pages")
        .ok()?
        .as_reference()
        .ok()?;
    let mut pages = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![(root, 0)];
    while let Some((id, depth)) = stack.pop() {
        if depth > PAGE_TREE_DEPTH || !seen.insert(id) {
            return None;
        }
        let node = doc.get_dictionary(id).ok()?;
        match node.get_type().ok()? {
            b"Page" => pages.push(id),
            b"Pages" => {
                let kids = doc.dereference(node.get(b"Kids").ok()?).ok()?.1;
                for kid in kids.as_array().ok()?.iter().rev() {
                    stack.push((kid.as_reference().ok()?, depth + 1));
                }
            }
            _ => return None,
        }
    }
    Some(pages)
}

/// An upper bound on the heap lopdf takes to parse `bytes`: 2 bytes per
/// input byte, plus a weight per token start. The weights are above lopdf
/// 0.45.0's measured worst cases (an empty array `[]`: 303 heap bytes per
/// input byte; operators and numbers: 266 and 94).
fn parse_cost(bytes: &[u8]) -> usize {
    let mut cost: usize = 0;
    let mut prev_regular = false;
    for &b in bytes {
        let regular = !matches!(
            b,
            0 | 9
                | 10
                | 12
                | 13
                | 32
                | b'('
                | b')'
                | b'<'
                | b'>'
                | b'['
                | b']'
                | b'{'
                | b'}'
                | b'/'
                | b'%'
        );
        let weight = match b {
            b'[' | b'{' => 840,
            b'<' => 420,
            b'(' | b'/' => 360,
            _ if regular && !prev_regular => 640,
            _ => 0,
        };
        cost = cost.saturating_add(2 + weight);
        prev_regular = regular;
    }
    cost
}

/// The parse budget: half the address space `RLIMIT_AS` leaves this
/// process, at most [`PARSE_BUDGET_CAP`]. Linux reads `/proc/self`; an
/// unreadable limit there is a zero budget. Elsewhere `RLIMIT_AS` is not
/// applied (the host skips it on macOS), so the budget is the cap.
fn parse_budget() -> usize {
    #[cfg(target_os = "linux")]
    {
        let read = |p| std::fs::read_to_string(p).ok();
        budget_from(
            read("/proc/self/limits").as_deref(),
            read("/proc/self/status").as_deref(),
        )
        .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        PARSE_BUDGET_CAP
    }
}

/// The budget from `/proc/self/limits` and `/proc/self/status` text.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn budget_from(limits: Option<&str>, status: Option<&str>) -> Option<usize> {
    const KEY: &str = "Max address space";
    let soft = limits?
        .lines()
        .find_map(|l| l.strip_prefix(KEY))?
        .split_whitespace()
        .next()?;
    if soft == "unlimited" {
        return Some(PARSE_BUDGET_CAP);
    }
    let limit: u64 = soft.parse().ok()?;
    let vm_kib: u64 = status?
        .lines()
        .find_map(|l| l.strip_prefix("VmSize:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let headroom = limit.saturating_sub(vm_kib.saturating_mul(1024)) / 2;
    Some(
        usize::try_from(headroom)
            .unwrap_or(usize::MAX)
            .min(PARSE_BUDGET_CAP),
    )
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

#[cfg(test)]
mod tests {
    //! #10746: one regression test per pdf_check finding of the S10b delta
    //! review. Each fixture is a PDF written here by hand; lopdf is never
    //! mocked.

    use std::io::Write;

    use flate2::Compression;
    use flate2::write::ZlibEncoder;

    use super::{PARSE_BUDGET_CAP, budget_from, no_text_layer};

    /// The image a page draws, and the text a page must not hide.
    const IMAGE_OPS: &[u8] = b"q 400 0 0 400 100 200 cm /Im1 Do Q\n";
    const TEXT_OPS: &[u8] = b"BT /F1 12 Tf 72 720 Td (Hidden) Tj ET\n";

    /// A PDF from object bodies (object `n` is `objs[n - 1]`, 1 the catalog).
    fn pdf_from(objs: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        let n = objs.len() + 1;
        out.extend_from_slice(format!("xref\n0 {n}\n0000000000 65535 f \n").as_bytes());
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        out
    }

    fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
        let mut s = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        s.extend_from_slice(data);
        s.extend_from_slice(b"\nendstream");
        s
    }

    fn zlib(data: &[u8], level: Compression) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), level);
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    const PAGE: &str = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
        /Resources << /Font << /F1 3 0 R >> /XObject << /Im1 4 0 R >> >> /Contents";

    /// Objects 1-4 (catalog, pages over `kids`, font, image) of every fixture.
    fn head(kids: &[u32]) -> Vec<Vec<u8>> {
        let refs: Vec<String> = kids.iter().map(|k| format!("{k} 0 R")).collect();
        vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                refs.join(" "),
                kids.len()
            )
            .into_bytes(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            stream(
                "/Type /XObject /Subtype /Image /Width 8 /Height 8 \
                 /ColorSpace /DeviceGray /BitsPerComponent 8",
                &[0x80; 64],
            ),
        ]
    }

    /// A one-page PDF: object 5 the page, object 6 its content stream.
    fn one_page(content_dict: &str, content: &[u8]) -> Vec<u8> {
        let mut objs = head(&[5]);
        objs.push(format!("{PAGE} 6 0 R >>").into_bytes());
        objs.push(stream(content_dict, content));
        pdf_from(&objs)
    }

    /// Why: the fixtures below must fail for their defect alone.
    /// What: the same one-page image-only PDF, uncompressed and deflated,
    /// is proof of no text layer.
    /// Test: self-contained.
    #[test]
    fn image_only_page_is_proof_of_no_text_layer() {
        assert!(no_text_layer(&one_page("", IMAGE_OPS)));
        let z = zlib(IMAGE_OPS, Compression::default());
        assert!(no_text_layer(&one_page("/Filter /FlateDecode", &z)));
    }

    /// Why: #10746 finding 1 — a truncated FlateDecode stream decoded
    /// partially and was read as the whole page.
    /// What: the image ops, padding, then a text op, deflated and cut in
    /// half, are not proof: the text past the cut is unread.
    /// Test: self-contained.
    #[test]
    fn truncated_flate_content_is_not_proof() {
        let mut content = IMAGE_OPS.to_vec();
        content.extend_from_slice(&b"q Q\n".repeat(2000));
        content.extend_from_slice(TEXT_OPS);
        let z = zlib(&content, Compression::none());
        let cut = &z[..z.len() / 2];
        assert!(!no_text_layer(&one_page("/Filter /FlateDecode", cut)));
    }

    /// Why: #10746 finding 2 — `Content::decode` stopped at the first bad
    /// token without an error, so text operators after it were never seen.
    /// What: image ops, a stray `)`, then a text op: not proof.
    /// Test: self-contained.
    #[test]
    fn content_that_stops_parsing_early_is_not_proof() {
        let mut content = IMAGE_OPS.to_vec();
        content.extend_from_slice(b") ");
        content.extend_from_slice(TEXT_OPS);
        assert!(!no_text_layer(&one_page("", &content)));
    }

    /// Why: #10746 finding 3 — lenient loading dropped a page object that
    /// failed to parse, so the verdict covered the other pages only.
    /// What: page 1 is image-only; page 2's dictionary is unterminated and
    /// its content shows text. The document is not proof.
    /// Test: self-contained.
    #[test]
    fn a_page_that_fails_to_parse_is_not_proof() {
        let mut objs = head(&[5, 7]);
        objs.push(format!("{PAGE} 6 0 R >>").into_bytes());
        objs.push(stream("", IMAGE_OPS));
        objs.push(format!("{PAGE} 8 0 R >").into_bytes());
        objs.push(stream("", TEXT_OPS));
        assert!(!no_text_layer(&pdf_from(&objs)));
    }

    /// Why: #10746 finding 4 — parsed objects take far more memory than
    /// their bytes (about 300x for dense operands), so a small crafted PDF
    /// could push the worker past `RLIMIT_AS`.
    /// What: an image page whose deflated content carries 2 million number
    /// operands (4 MB, estimated over the 1 GiB parse budget) is not proof.
    /// Test: self-contained.
    #[test]
    fn content_over_the_parse_budget_is_not_proof() {
        let mut content = IMAGE_OPS.to_vec();
        content.extend_from_slice(&b"0 ".repeat(2_000_000));
        content.extend_from_slice(b"n\n");
        let z = zlib(&content, Compression::best());
        assert!(!no_text_layer(&one_page("/Filter /FlateDecode", &z)));
    }

    /// Why: #10746 finding 4 — the budget must sit below the worker's real
    /// `RLIMIT_AS`, not a fixed guess.
    /// What: with a 4 GiB limit and 1 GiB mapped, the budget is the cap;
    /// with 1 GiB mapped of 1.5 GiB it is half the 512 MiB left; no limit
    /// is the cap; an unreadable limit is no budget.
    /// Test: self-contained.
    #[test]
    fn parse_budget_is_half_the_address_space_rlimit_as_leaves() {
        let limits = |soft: &str| {
            format!(
                "Max cpu time  unlimited  unlimited  seconds
Max address space  {soft}  {soft}  bytes
"
            )
        };
        let status = "Name:\tworker\nVmSize:\t 1048576 kB\n";
        let gib: u64 = 1 << 30;
        let at = |soft: String| budget_from(Some(&limits(&soft)), Some(status));
        assert_eq!(at((4 * gib).to_string()), Some(PARSE_BUDGET_CAP));
        assert_eq!(at((3 * gib / 2).to_string()), Some(256 << 20));
        assert_eq!(at("unlimited".into()), Some(PARSE_BUDGET_CAP));
        assert_eq!(budget_from(None, Some(status)), None);
    }
}
