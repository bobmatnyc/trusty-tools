//! Deterministic document fixtures for the anydoc evaluation spike.
//!
//! Why: the repo's own extraction tests build fixtures inline and throw them
//! away, so there is no corpus to benchmark two extractors against. Rather
//! than check binary documents into the tree, every fixture here is generated
//! byte-for-byte from code — the corpus is reproducible on any machine, and
//! the adversarial shapes stay auditable as source instead of opaque blobs.
//!
//! What: [`Corpus`] materialises a temp directory of realistic and hostile
//! `.docx` / `.xlsx` / `.pdf` files. `office` and `pdf` build the
//! well-formed ones; `hostile` builds the attack shapes.
//!
//! Test: none — this is a throwaway spike harness gated behind the
//! `anydoc-spike` feature, not production code.

pub mod hostile;
pub mod office;
pub mod pdf;

use std::path::{Path, PathBuf};

/// A materialised fixture: the file on disk plus what it is meant to probe.
pub struct Fixture {
    pub name: String,
    pub path: PathBuf,
    /// Extension-derived format label used to group benchmark rows.
    pub format: &'static str,
    /// One-line description of what this fixture exercises.
    pub note: String,
}

impl Fixture {
    pub fn size(&self) -> u64 {
        std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }
}

/// The generated corpus, rooted in a caller-owned directory.
pub struct Corpus {
    pub root: PathBuf,
}

impl Corpus {
    pub fn new(root: &Path) -> Self {
        std::fs::create_dir_all(root).expect("create corpus root");
        Self {
            root: root.to_path_buf(),
        }
    }

    fn write(&self, name: &str, format: &'static str, note: &str, bytes: Vec<u8>) -> Fixture {
        let path = self.root.join(name);
        std::fs::write(&path, bytes).expect("write fixture");
        Fixture {
            name: name.to_string(),
            path,
            format,
            note: note.to_string(),
        }
    }

    /// Well-formed documents used for the speed, memory, and quality legs.
    ///
    /// Sizes are chosen to span the realistic range rather than to flatter
    /// either extractor: a small memo, a structure-heavy report, and a
    /// multi-sheet workbook.
    pub fn benign(&self) -> Vec<Fixture> {
        vec![
            self.write(
                "small-memo.docx",
                "docx",
                "12 flat paragraphs, no structure — the shape our extractor already handles well",
                office::docx_flat(12),
            ),
            self.write(
                "structured-report.docx",
                "docx",
                "6 headings + 4 tables (5x4) + 60 paragraphs — the structure our extractor drops",
                office::docx_structured(),
            ),
            self.write(
                "large-report.docx",
                "docx",
                "2000 paragraphs + 40 tables — throughput case",
                office::docx_large(),
            ),
            self.write(
                "small-sheet.xlsx",
                "xlsx",
                "1 sheet, 20x6 populated cells",
                office::xlsx_grid(1, 20, 6),
            ),
            self.write(
                "multi-sheet.xlsx",
                "xlsx",
                "4 sheets, 200x8 populated cells each",
                office::xlsx_grid(4, 200, 8),
            ),
            self.write(
                "text-layer.pdf",
                "pdf",
                "single page, one text-showing operator",
                pdf::text_layer("Quarterly revenue rose 12 percent."),
            ),
            self.write(
                "multi-page.pdf",
                "pdf",
                "40 pages, one text run each",
                pdf::multi_page(40),
            ),
            self.write(
                "image-only.pdf",
                "pdf",
                "valid page, zero text operators — the scanned-document case",
                pdf::image_only(),
            ),
        ]
    }

    /// Hostile inputs. Every one stays under `MAX_OFFICE_FILE_BYTES` (10 MiB)
    /// on purpose: a file the walker's size gate already rejects tells us
    /// nothing about either parser. These are the ones that get through.
    pub fn adversarial(&self) -> Vec<Fixture> {
        vec![
            self.write(
                "zipbomb.docx",
                "docx",
                "48 KiB container, 512 MiB declared document.xml (DEFLATE null run)",
                hostile::docx_zip_bomb(),
            ),
            self.write(
                "nested-xml.docx",
                "docx",
                "100k-deep XML element nesting in document.xml",
                hostile::docx_deep_xml(100_000),
            ),
            self.write(
                "truncated.docx",
                "docx",
                "valid docx cut off mid-archive",
                hostile::truncated(office::docx_flat(200), 0.6),
            ),
            self.write(
                "malformed.docx",
                "docx",
                "zip magic followed by garbage — a corrupt central directory",
                hostile::malformed_archive(),
            ),
            self.write(
                "zipbomb.xlsx",
                "xlsx",
                "48 KiB container, 512 MiB declared sheet1.xml (the calamine leg)",
                hostile::xlsx_zip_bomb(),
            ),
            self.write(
                "truncated.xlsx",
                "xlsx",
                "valid xlsx cut off mid-archive",
                hostile::truncated(office::xlsx_grid(2, 300, 8), 0.6),
            ),
            self.write(
                "nested-objects.pdf",
                "pdf",
                "RUSTSEC-2026-0187 PoC: 10380-deep nested array in a page object",
                pdf::deeply_nested_objects(10_380),
            ),
            self.write(
                "truncated.pdf",
                "pdf",
                "valid PDF cut off before the xref table",
                hostile::truncated(pdf::multi_page(10), 0.5),
            ),
            self.write(
                "malformed.pdf",
                "pdf",
                "PDF header followed by garbage",
                hostile::malformed_pdf(),
            ),
        ]
    }

    /// Fixtures targeting anydoc issues #8 and #9 specifically.
    pub fn xlsx_bugs(&self) -> Vec<Fixture> {
        vec![
            self.write(
                "merged-range.xlsx",
                "xlsx",
                "issue #8: F1:O3 merge (3 rows x 10 cols) with only the anchor populated",
                office::xlsx_merged_range(),
            ),
            self.write(
                "hidden-content.xlsx",
                "xlsx",
                "issue #9: one hidden row and one hidden column alongside visible cells",
                office::xlsx_hidden(),
            ),
        ]
    }
}
