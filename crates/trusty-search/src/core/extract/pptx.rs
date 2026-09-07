//! PPTX (PowerPoint) text extraction (issue #6938).
//!
//! Why: `.pptx` was absent from [`super::EXTRACT_EXTS`] and no extractor
//! existed, so every deck in an indexed tree produced zero chunks — 53 files
//! under one measured corpus root. A `.pptx` is the same OOXML zip a `.docx`
//! is, so this module mirrors [`super::docx`] and shares its container and
//! run-text helpers ([`super::ooxml`]) rather than parsing zip or XML a second
//! way.
//!
//! What: [`extract`] collects `ppt/slides/slide<N>.xml`, orders them by `N`,
//! and streams each one collecting `<a:t>` run text with a paragraph break at
//! each `</a:p>`. A slide's speaker notes (`ppt/notesSlides/notesSlide<N>.xml`,
//! same DrawingML shape) are appended directly after that slide's own text, so
//! a chunk that lands on a slide carries what was said about it. Slide layouts
//! and masters are boilerplate and stay out of scope.
//!
//! Ordering is by parsed integer, never by entry name: zip entries arrive in
//! whatever order the writer emitted, and `slide10.xml` sorts before
//! `slide2.xml` as a string.
//!
//! Test: `test_extracts_slides_in_numeric_order`,
//! `test_slide_paragraphs_split_on_a_p`, `test_notes_follow_their_own_slide`,
//! `test_deck_with_no_slides_errors`, `test_not_a_zip_errors`.

use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;

use super::ooxml::{push_entity_ref, push_run_text, read_entry_bounded, MAX_PART_BYTES};
use super::{ExtractError, Extracted};

/// Zip entry prefix holding one slide's body, per the OOXML PresentationML
/// package convention.
const SLIDE_PREFIX: &str = "ppt/slides/slide";

/// Zip entry prefix holding one slide's speaker notes.
const NOTES_PREFIX: &str = "ppt/notesSlides/notesSlide";

/// The extension every slide and notes part carries.
const PART_SUFFIX: &str = ".xml";

/// Extract slide text from a `.pptx` file.
///
/// Why/What: see module docs. Decompression of each part is bounded by
/// [`MAX_PART_BYTES`] (zip-bomb defence; see that constant's docs). A deck
/// carrying no slide part at all is an error rather than empty text — that is
/// what a truncated download or a mislabelled file looks like, and reporting
/// it as a successful empty extraction is how #6938's decks went unnoticed.
/// Test: `test_extracts_slides_in_numeric_order`,
/// `test_deck_with_no_slides_errors`, `test_not_a_zip_errors`.
pub fn extract(path: &Path) -> Result<Extracted, ExtractError> {
    let file = std::fs::File::open(path).map_err(|source| ExtractError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| ExtractError::Pptx(e.to_string()))?;

    let mut slides = slide_numbers(&archive);
    if slides.is_empty() {
        return Err(ExtractError::Pptx(format!(
            "no {SLIDE_PREFIX}<N>{PART_SUFFIX} part in the package"
        )));
    }
    slides.sort_unstable();

    let mut out = String::new();
    for n in slides {
        push_part(
            &mut out,
            &mut archive,
            &format!("{SLIDE_PREFIX}{n}{PART_SUFFIX}"),
        )?;
        // #6938: notes belong to the slide they annotate, so they are appended
        // before the next slide rather than collected into a trailing block.
        let notes = format!("{NOTES_PREFIX}{n}{PART_SUFFIX}");
        if archive.index_for_name(&notes).is_some() {
            push_part(&mut out, &mut archive, &notes)?;
        }
    }

    Ok(Extracted {
        text: out,
        warning: None,
    })
}

/// Slide numbers present in the package, unordered.
///
/// Why: the caller needs the numbers, not the names, because ordering is
/// numeric and the notes part for a slide is named by the same number.
/// What: parses `<N>` out of `ppt/slides/slide<N>.xml`, ignoring any other
/// entry (including `ppt/slides/_rels/…`, whose name carries a suffix past
/// `.xml` and so never parses).
/// Test: `test_extracts_slides_in_numeric_order`.
fn slide_numbers(archive: &zip::ZipArchive<std::fs::File>) -> Vec<u32> {
    archive
        .file_names()
        .filter_map(|name| {
            name.strip_prefix(SLIDE_PREFIX)
                .and_then(|rest| rest.strip_suffix(PART_SUFFIX))
                .and_then(|digits| digits.parse::<u32>().ok())
        })
        .collect()
}

/// Read one XML part and append its paragraphs to `out`.
///
/// Why: slides and notes parse identically, so both routes go through here.
/// Test: `test_notes_follow_their_own_slide`.
fn push_part(
    out: &mut String,
    archive: &mut zip::ZipArchive<std::fs::File>,
    name: &str,
) -> Result<(), ExtractError> {
    let entry = archive
        .by_name(name)
        .map_err(|e| ExtractError::Pptx(format!("{name}: {e}")))?;
    let declared = entry.size();
    let xml = read_entry_bounded(entry, declared, name, MAX_PART_BYTES, ExtractError::Pptx)?;
    out.push_str(&paragraphs_from_slide_xml(&xml)?);
    Ok(())
}

/// Parse a slide or notes part into text with a blank line between paragraphs
/// (matching the plaintext/document chunker's paragraph-break convention, and
/// [`super::docx`]'s output).
///
/// Why: isolated from [`extract`] so the parse is unit-testable against
/// literal XML without a zip container.
/// What: streams `<a:t>` run text into the current paragraph buffer, flushing
/// on `</a:p>`. A part truncated mid-paragraph — the file watcher reads decks
/// while PowerPoint is still writing them — still emits everything parsed so
/// far rather than dropping the trailing run.
/// Test: `test_slide_paragraphs_split_on_a_p`,
/// `test_slide_text_resolves_entity_references`,
/// `test_unterminated_paragraph_still_emits_its_text`.
fn paragraphs_from_slide_xml(xml: &str) -> Result<String, ExtractError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut out = String::new();
    let mut para = String::new();
    let mut in_text = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"t" => in_text = true,
            Ok(Event::End(e)) if e.local_name().as_ref() == b"t" => in_text = false,
            Ok(Event::Text(t)) if in_text => push_run_text(&mut para, &t, ExtractError::Pptx)?,
            Ok(Event::GeneralRef(r)) if in_text => {
                push_entity_ref(&mut para, &r, ExtractError::Pptx)?
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"p" => {
                flush(&mut out, &mut para);
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(ExtractError::Pptx(e.to_string())),
            _ => {}
        }
    }
    flush(&mut out, &mut para);
    Ok(out)
}

/// Append `para` to `out` as its own paragraph and clear it.
///
/// An empty or whitespace-only paragraph emits nothing: a deck's shapes carry
/// plenty of them, and each would otherwise widen the gap between two real
/// paragraphs.
fn flush(out: &mut String, para: &mut String) {
    let text = para.trim();
    if !text.is_empty() {
        out.push_str(text);
        out.push_str("\n\n");
    }
    para.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

    /// One `<a:p>` holding one run of `text`.
    fn para(text: &str) -> String {
        format!("<a:p><a:r><a:t>{text}</a:t></a:r></a:p>")
    }

    /// A slide/notes part wrapping `paras` in the shape PowerPoint writes.
    fn part(paras: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld {NS}><p:cSld><p:spTree><p:sp><p:txBody>{paras}</p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
        )
    }

    /// Zip up a `.pptx` from `(entry name, contents)` pairs.
    fn build_pptx(entries: &[(String, String)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            for (name, body) in entries {
                zip.start_file(name.as_str(), opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    /// Write a deck of `slides` (plus any `notes`) to a temp `.pptx` and
    /// extract it. `notes` maps a slide number to its speaker-note text.
    fn extract_deck(slides: &[(u32, &str)], notes: &[(u32, &str)]) -> String {
        let mut entries: Vec<(String, String)> = slides
            .iter()
            .map(|(n, text)| (format!("{SLIDE_PREFIX}{n}{PART_SUFFIX}"), part(&para(text))))
            .collect();
        entries.extend(
            notes
                .iter()
                .map(|(n, text)| (format!("{NOTES_PREFIX}{n}{PART_SUFFIX}"), part(&para(text)))),
        );
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("deck.pptx");
        std::fs::write(&path, build_pptx(&entries)).unwrap();
        extract(&path).expect("extraction must succeed").text
    }

    /// #6938: zip entries arrive in writer order and `slide10` sorts before
    /// `slide2` as a string, so ordering must parse the number.
    #[test]
    fn test_extracts_slides_in_numeric_order() {
        let text = extract_deck(
            &[(10, "Slide ten"), (2, "Slide two"), (1, "Slide one")],
            &[],
        );
        let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines, vec!["Slide one", "Slide two", "Slide ten"], "{text}");
    }

    #[test]
    fn test_slide_paragraphs_split_on_a_p() {
        let xml = part(&format!("{}{}", para("Title"), para("Bullet")));
        let text = paragraphs_from_slide_xml(&xml).unwrap();
        assert_eq!(text, "Title\n\nBullet\n\n");
    }

    /// A single paragraph split across several `<a:r>` runs (bold + plain) is
    /// one paragraph, not several.
    #[test]
    fn test_multiple_runs_join_into_one_paragraph() {
        let xml = part("<a:p><a:r><a:t>Half </a:t></a:r><a:r><a:t>and half</a:t></a:r></a:p>");
        assert_eq!(
            paragraphs_from_slide_xml(&xml).unwrap(),
            "Half and half\n\n"
        );
    }

    #[test]
    fn test_slide_text_resolves_entity_references() {
        let xml = part("<a:p><a:r><a:t>Tom &amp; Jerry &#233;</a:t></a:r></a:p>");
        let text = paragraphs_from_slide_xml(&xml).unwrap();
        assert_eq!(text.trim(), "Tom & Jerry é");
    }

    /// The watcher reads decks mid-write, so a part ending inside an open
    /// `<a:p>` must still surface the run it had parsed.
    #[test]
    fn test_unterminated_paragraph_still_emits_its_text() {
        let xml = "<p:sld><p:txBody><a:p><a:r><a:t>Truncated mid";
        let text = paragraphs_from_slide_xml(xml).unwrap();
        assert_eq!(text.trim(), "Truncated mid");
    }

    /// Notes belong to the slide they annotate, not to a trailing block.
    #[test]
    fn test_notes_follow_their_own_slide() {
        let text = extract_deck(
            &[(1, "Slide one"), (2, "Slide two")],
            &[(1, "Note for one"), (2, "Note for two")],
        );
        let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines,
            vec!["Slide one", "Note for one", "Slide two", "Note for two"],
            "{text}"
        );
    }

    /// A slide with no notes part must not borrow the next slide's notes.
    #[test]
    fn test_slide_without_notes_is_unaffected() {
        let text = extract_deck(
            &[(1, "Slide one"), (2, "Slide two")],
            &[(2, "Note for two")],
        );
        let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines,
            vec!["Slide one", "Slide two", "Note for two"],
            "{text}"
        );
    }

    /// A zip carrying no slide part is a truncated or mislabelled file, not an
    /// empty deck — reporting it as a clean empty extraction is exactly how
    /// #6938's decks stayed invisible.
    #[test]
    fn test_deck_with_no_slides_errors() {
        let entries = [("readme.txt".to_string(), "not a deck".to_string())];
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("bad.pptx");
        std::fs::write(&path, build_pptx(&entries)).unwrap();

        let err = extract(&path).expect_err("a zip with no slide part must error");
        assert!(matches!(err, ExtractError::Pptx(_)), "{err}");
    }

    /// Corrupt input errors the same way docx's does — an `ExtractError`, and
    /// never a panic out of the zip or XML parser.
    #[test]
    fn test_not_a_zip_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("not-a-zip.pptx");
        std::fs::write(&path, b"this is definitely not a zip file").unwrap();

        let err = extract(&path).expect_err("non-zip input must error");
        assert!(matches!(err, ExtractError::Pptx(_)), "{err}");
    }

    /// Truncated container bytes: the zip layer must report, not panic.
    #[test]
    fn test_truncated_zip_errors() {
        let entries = [(
            format!("{SLIDE_PREFIX}1{PART_SUFFIX}"),
            part(&para("Slide one")),
        )];
        let mut bytes = build_pptx(&entries);
        bytes.truncate(bytes.len() / 2);
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("truncated.pptx");
        std::fs::write(&path, bytes).unwrap();

        assert!(extract(&path).is_err(), "a truncated container must error");
    }

    /// `ppt/slides/_rels/slide1.xml.rels` sits under the slide prefix and must
    /// not be mistaken for a slide.
    #[test]
    fn test_slide_rels_entries_are_not_slides() {
        let entries = [
            (
                format!("{SLIDE_PREFIX}1{PART_SUFFIX}"),
                part(&para("Slide one")),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels".to_string(),
                "<Relationships/>".to_string(),
            ),
        ];
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("deck.pptx");
        std::fs::write(&path, build_pptx(&entries)).unwrap();

        let text = extract(&path).expect("extraction must succeed").text;
        assert_eq!(text.trim(), "Slide one");
    }
}
