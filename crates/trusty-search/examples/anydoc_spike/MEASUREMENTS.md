# anydoc spike — raw measurements

Verbatim output of `examples/anydoc_spike`. Reproduce with:

```bash
cargo run -p trusty-search --features anydoc-spike --example anydoc_spike --release -- all
cargo run -p trusty-search --features anydoc-spike --example anydoc_spike --release -- pdf-sweep
cargo run -p trusty-search --features anydoc-spike --example anydoc_spike --release -- real <dir>
```

The recommendation drawn from these numbers lives in the spike PR body, not
here. This file is the evidence, kept separate so it can be regenerated
without editing prose.

## Measurement host

| | |
|---|---|
| platform | macOS 25.5.0, aarch64 (Apple Silicon) |
| profile | `--release` |
| anydoc | 0.1.3 (published 2026-08-04; repo created 2026-08-03) |
| native baseline | `trusty_search::core::extract::extract_text` at trusty-search 0.42.3 |
| third-party PDF reference | poppler `pdftotext`, and `qpdf --check` for fixture validity |

## How to read these tables

- **Timing** is in-process over N repetitions, sorted, min and median reported.
  Min is the least noisy estimate of parse cost on a shared laptop; the gap to
  the median says how much to trust either.
- **Peak RSS** is `getrusage(RUSAGE_SELF).ru_maxrss` reported by a child
  process that performed exactly one extraction, minus a no-op child baseline.
  One process per measurement, because `ru_maxrss` is a high-water mark and
  would otherwise attribute the largest run's peak to whichever ran last.
- **Adversarial outcomes** come from an isolated child under a 45s budget, so
  an outcome that kills the process (`SIGABRT` is not a catchable panic) is
  observable rather than fatal to the harness.
- The **real-document** section prints no filename, path, or extracted
  content. It was pointed at the operator's own documents; rows are labelled by
  format and index, and failure messages are digit-normalised so page counts do
  not fingerprint a file.


---

## Extracted-text shape

| fixture | native | anydoc |
|---|---|---|
| small-memo.docx | ok (710 bytes) | ok (709 bytes) |
| structured-report.docx | ok (5056 bytes) | ok (5356 bytes) |
| large-report.docx | ok (160800 bytes) | ok (164479 bytes) |
| small-sheet.xlsx | ok (1838 bytes) | ok (2162 bytes) |
| multi-sheet.xlsx | ok (105144 bytes) | ok (119839 bytes) |
| text-layer.pdf | ok (36 bytes) | ok (38 bytes) |
| multi-page.pdf | ok (2192 bytes) | err: unsupported input: PDF has no extractable text (TextBased, 40 pages): OCR is required |
| image-only.pdf | ok+warning(0 bytes): no extractable text (scanned/image PDF? OCR not supported) | err: unsupported input: PDF has no extractable text (Scanned, 1 pages): OCR is required |

## Structure recovery (docx)

**small-memo.docx** — 12 flat paragraphs, no structure — the shape our extractor already handles well

| signal | native | anydoc |
|---|---:|---:|
| markdown heading lines (`#`) | 0 | 0 |
| table delimiter rows (`| --- |`) | 0 | 0 |
| table pipe characters | 0 | 0 |
| occurrences of table cell text `col 0` | 0 | 0 |
| heading text `Quarterly Operations Review` present | no | no |

**structured-report.docx** — 6 headings + 4 tables (5x4) + 60 paragraphs — the structure our extractor drops

| signal | native | anydoc |
|---|---:|---:|
| markdown heading lines (`#`) | 0 | 6 |
| table delimiter rows (`| --- |`) | 0 | 4 |
| table pipe characters | 0 | 140 |
| occurrences of table cell text `col 0` | 4 | 4 |
| heading text `Quarterly Operations Review` present | yes | yes |

**large-report.docx** — 2000 paragraphs + 40 tables — throughput case

| signal | native | anydoc |
|---|---:|---:|
| markdown heading lines (`#`) | 0 | 0 |
| table delimiter rows (`| --- |`) | 0 | 40 |
| table pipe characters | 0 | 1920 |
| occurrences of table cell text `col 0` | 40 | 40 |
| heading text `Quarterly Operations Review` present | no | no |


## Chunk-level impact

Both texts chunked with chunk_text(window=150, stride=50) — the production path.

| fixture | native chunks | anydoc chunks | native lines | anydoc lines | native chars | anydoc chars |
|---|---:|---:|---:|---:|---:|---:|
| small-memo.docx | 1 | 1 | 24 | 23 | 710 | 709 |
| structured-report.docx | 4 | 2 | 294 | 165 | 5056 | 5356 |
| large-report.docx | 126 | 86 | 6400 | 4359 | 160800 | 164479 |
| small-sheet.xlsx | 1 | 1 | 22 | 22 | 1838 | 2162 |
| multi-sheet.xlsx | 15 | 15 | 808 | 819 | 105144 | 119839 |
| text-layer.pdf | 1 | 1 | 3 | 1 | 36 | 38 |
| multi-page.pdf | 1 | 0 | 3 | 0 | 2192 | 0 |
| image-only.pdf | 0 | 0 | 0 | 0 | 0 | 0 |

## Sample output (structured-report.docx, first 900 chars each)

### native

```
Quarterly Operations Review

This review covers the reporting period and its material variances.

Section 0: Regional Detail

Section 0 paragraph 0: operating expenditure tracked against plan.

Section 0 paragraph 1: operating expenditure tracked against plan.

Section 0 paragraph 2: operating expenditure tracked against plan.

Section 0 paragraph 3: operating expenditure tracked against plan.

Section 0 paragraph 4: operating expenditure tracked against plan.

Section 0 paragraph 5: operating expenditure tracked against plan.

Section 0 paragraph 6: operating expenditure tracked against plan.

Section 0 paragraph 7: operating expenditure tracked against plan.

Section 0 paragraph 8: operating expenditure tracked against plan.

Section 0 paragraph 9: operating expenditure tracked against plan.

Section 0 paragraph 10: operating expenditure tracked against plan.

Section 0 paragraph 11: o…
```

### anydoc

```
# Quarterly Operations Review

This review covers the reporting period and its material variances.

## Section 0: Regional Detail

Section 0 paragraph 0: operating expenditure tracked against plan.

Section 0 paragraph 1: operating expenditure tracked against plan.

Section 0 paragraph 2: operating expenditure tracked against plan.

Section 0 paragraph 3: operating expenditure tracked against plan.

Section 0 paragraph 4: operating expenditure tracked against plan.

Section 0 paragraph 5: operating expenditure tracked against plan.

Section 0 paragraph 6: operating expenditure tracked against plan.

Section 0 paragraph 7: operating expenditure tracked against plan.

Section 0 paragraph 8: operating expenditure tracked against plan.

Section 0 paragraph 9: operating expenditure tracked against plan.

Section 0 paragraph 10: operating expenditure tracked against plan.

Section 0 paragraph …
```


---

## `merged-range.xlsx`

issue #8: F1:O3 merge (3 rows x 10 cols) with only the anchor populated

### native — ok (36 bytes)

```
== Merge Example ==
Merged heading


```

### anydoc — ok (32 bytes)

```
|  |
| --- |
| Merged heading |

```

**Issue #8 assessment**

| check | native | anydoc |
|---|---|---|
| anchor text `Merged heading` present | yes | yes |
| non-empty output lines | 2 | 3 |
| extracted chars | 36 | 32 |

Index-level reading: the merge span is layout metadata. What reaches a chunk is the anchor's text either way, so a clipped span changes the rendered table shape without removing a searchable term — as long as the anchor text itself survives.

## `hidden-content.xlsx`

issue #9: one hidden row and one hidden column alongside visible cells

### native — ok (64 bytes)

```
== Visibility Example ==
Visible row	Hidden column
Hidden row	


```

### anydoc — ok (72 bytes)

```
|  |  |
| --- | --- |
| Visible row | Hidden column |
| Hidden row |  |

```

**Issue #9 assessment**

| check | native | anydoc |
|---|---|---|
| `Visible row` in extracted text | yes | yes |
| `Hidden row` in extracted text | yes | yes |
| `Hidden column` in extracted text | yes | yes |

Index-level reading: hidden cells becoming searchable text is a behaviour question, not a fidelity one — it makes content the author suppressed retrievable. Whether that is a defect depends on whether the index is meant to mirror what a reader sees or what the file contains. Both extractors' behaviour is reported above rather than judged here.


---

## Speed (wall-clock per file, milliseconds)

Sample: N repetitions per (engine, file) in one process, sorted; min and median reported.

| fixture | format | bytes | N | native min | native med | anydoc min | anydoc med | ratio (med) |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| small-memo.docx | docx | 1643 | 100 | 0.03 | 0.03 | 0.05 | 0.07 | 2.10x |
| structured-report.docx | docx | 2217 | 100 | 0.04 | 0.06 | 0.19 | 0.27 | 4.36x |
| large-report.docx | docx | 11674 | 100 | 0.44 | 0.57 | 3.71 | 4.65 | 8.09x |
| small-sheet.xlsx | xlsx | 2087 | 100 | 0.07 | 0.09 | 0.12 | 0.16 | 1.79x |
| multi-sheet.xlsx | xlsx | 26446 | 100 | 1.50 | 1.94 | 4.69 | 5.45 | 2.81x |
| text-layer.pdf | pdf | 608 | 100 | 0.03 | 0.04 | 0.06 | 0.14 | 3.97x |
| multi-page.pdf | pdf | 12785 | 100 | 0.33 | 0.46 | 0.59 | 0.73 | 1.58x |
| image-only.pdf | pdf | 413 | 100 | 0.02 | 0.02 | 0.04 | 0.08 | 3.74x |

## Peak RSS (isolated child process, MiB)

Baseline (re-exec that parses nothing): 6.4 MiB. Deltas below subtract it.

| fixture | format | bytes | native peak | native Δ | anydoc peak | anydoc Δ |
|---|---|---:|---:|---:|---:|---:|
| small-memo.docx | docx | 1643 | 7.0 | 0.6 | 7.8 | 1.5 |
| structured-report.docx | docx | 2217 | 7.0 | 0.7 | 8.5 | 2.1 |
| large-report.docx | docx | 11674 | 7.6 | 1.2 | 16.0 | 9.6 |
| small-sheet.xlsx | xlsx | 2087 | 7.5 | 1.1 | 8.0 | 1.6 |
| multi-sheet.xlsx | xlsx | 26446 | 8.3 | 1.9 | 9.9 | 3.5 |
| text-layer.pdf | pdf | 608 | 7.2 | 0.9 | 10.0 | 3.6 |
| multi-page.pdf | pdf | 12785 | 7.6 | 1.2 | 11.0 | 4.6 |
| image-only.pdf | pdf | 413 | 7.1 | 0.7 | 8.0 | 1.7 |

---

## Adversarial parity

Each cell is one isolated child process, budget 45s. Both extractors sit behind the same 10 MiB `MAX_OFFICE_FILE_BYTES` gate in production; every fixture here is under it.

| fixture | bytes | native outcome | native ms | native peak MiB | anydoc outcome | anydoc ms | anydoc peak MiB |
|---|---:|---|---:|---:|---|---:|---:|
| zipbomb.docx | 522560 | err: docx extraction failed: word/document.xml declares 536870953 uncompressed bytes, over the 52428800 byte cap | 152.83 | 6.9 | err: resource limit exceeded (max_entry_bytes): word/document.xml declares 536870953 decompressed bytes | 89.13 | 8.0 |
| nested-xml.docx | 6125 | ok (200004 bytes) | 103.65 | 9.7 | err: resource limit exceeded (max_xml_depth): element nesting exceeds 256 | 175.18 | 10.8 |
| truncated.docx | 1315 | err: docx extraction failed: invalid Zip archive: Could not find EOCD | 174.98 | 6.8 | err: malformed document: not a readable zip archive: invalid Zip archive: Could not find EOCD | 175.10 | 6.8 |
| malformed.docx | 8196 | err: docx extraction failed: invalid Zip archive: Could not find EOCD | 170.46 | 6.8 | err: malformed document: not a readable zip archive: invalid Zip archive: Could not find EOCD | 171.99 | 6.8 |
| zipbomb.xlsx | 523141 | ok (10 bytes) | 143.33 | 521.4 | ok (0 bytes) | 413.60 | 1034.2 |
| truncated.xlsx | 11743 | err: spreadsheet extraction failed: Xlsx error: Zip error: invalid Zip archive: Could not find EOCD | 25.51 | 6.9 | err: malformed document: unreadable workbook: Cannot detect file format | 119.59 | 7.0 |
| nested-objects.pdf | 21182 | ok+warning(0 bytes): no extractable text (scanned/image PDF? OCR not supported) | 175.13 | 7.2 | **SIGABRT** (uncatchable — process dies) | 173.37 | 0.0 |
| truncated.pdf | 1712 | err: pdf extraction failed: PDF error: failed parsing cross reference table: invalid start value | 154.73 | 6.9 | err: malformed document: invalid PDF structure | 171.11 | 6.9 |
| malformed.pdf | 8201 | err: pdf extraction failed: PDF error: failed parsing cross reference table: invalid start value | 172.77 | 6.9 | err: malformed document: invalid PDF structure | 156.27 | 6.9 |

### What each fixture probes

- `zipbomb.docx` — 48 KiB container, 512 MiB declared document.xml (DEFLATE null run)
- `nested-xml.docx` — 100k-deep XML element nesting in document.xml
- `truncated.docx` — valid docx cut off mid-archive
- `malformed.docx` — zip magic followed by garbage — a corrupt central directory
- `zipbomb.xlsx` — 48 KiB container, 512 MiB declared sheet1.xml (the calamine leg)
- `truncated.xlsx` — valid xlsx cut off mid-archive
- `nested-objects.pdf` — RUSTSEC-2026-0187 PoC: 10380-deep nested array in a page object
- `truncated.pdf` — valid PDF cut off before the xref table
- `malformed.pdf` — PDF header followed by garbage


---

## PDF page-count sweep

| pages | bytes | poppler pdftotext chars | native chars | anydoc |
|---:|---:|---:|---:|---|
| 1 | 629 | 54 | 54 | err: unsupported input: PDF has no extractable text (TextBased, 1 pages): OCR is required |
| 2 | 936 | 111 | 108 | err: unsupported input: PDF has no extractable text (TextBased, 2 pages): OCR is required |
| 3 | 1245 | 168 | 162 | err: unsupported input: PDF has no extractable text (TextBased, 3 pages): OCR is required |
| 4 | 1557 | 225 | 216 | err: unsupported input: PDF has no extractable text (TextBased, 4 pages): OCR is required |
| 5 | 1868 | 282 | 270 | err: unsupported input: PDF has no extractable text (TextBased, 5 pages): OCR is required |
| 8 | 2801 | 453 | 432 | err: unsupported input: PDF has no extractable text (TextBased, 8 pages): OCR is required |
| 16 | 5296 | 915 | 870 | err: unsupported input: PDF has no extractable text (TextBased, 16 pages): OCR is required |
| 40 | 12785 | 2307 | 2190 | err: unsupported input: PDF has no extractable text (TextBased, 40 pages): OCR is required |


---

## Real-document corpus

Source: a directory of 67 actual documents on the measurement host. Filenames and content are deliberately omitted; rows are labelled by format and index.

### Outcome summary

| format | files | both extracted | native only | anydoc only | neither |
|---|---:|---:|---:|---:|---:|
| docx | 2 | 1 | 0 | 0 | 1 |
| pdf | 58 | 38 | 7 | 0 | 13 |
| xlsx | 7 | 5 | 0 | 0 | 2 |

### Failure categories

Error text only — anydoc's and ours both describe the parse, not the file, so no filename leaks through here.

| engine | occurrences | message |
|---|---:|---|
| native | 13 | pdf extraction failed: PDF error: couldn't parse input: invalid file header |
| anydoc | 13 | malformed document: not a PDF: file is empty |
| anydoc | 5 | unsupported input: PDF has no extractable text (TextBased, N pages): OCR is required |
| native | 2 | spreadsheet extraction failed: Xlsx error: Zip error: invalid Zip archive: Could not find EOCD |
| anydoc | 2 | malformed document: unreadable workbook: Cannot detect file format |
| anydoc | 2 | unsupported input: PDF has no extractable text (ImageBased, N pages): OCR is required |
| native | 1 | docx extraction failed: invalid Zip archive: Could not find EOCD |
| anydoc | 1 | malformed document: not a readable zip archive: invalid Zip archive: Could not find EOCD |

### Per-file

| file | bytes | native chars | anydoc chars | native ms | anydoc ms | native | anydoc |
|---|---:|---:|---:|---:|---:|---|---|
| pdf-001 | 0 | 0 | 0 | 0.04 | 0.02 | err | err |
| pdf-002 | 100848 | 70259 | 68265 | 9.09 | 20.55 | ok | ok |
| pdf-003 | 672057 | 36316 | 0 | 6.23 | 4.43 | ok | err |
| pdf-004 | 895901 | 138 | 0 | 3.04 | 1.75 | ok | err |
| pdf-005 | 0 | 0 | 0 | 0.05 | 0.01 | err | err |
| docx-001 | 12092 | 6445 | 6850 | 0.42 | 1.88 | ok | ok |
| pdf-006 | 1782465 | 94979 | 0 | 17.37 | 15.22 | ok | err |
| pdf-007 | 33515 | 1851 | 1783 | 1.37 | 1.23 | ok | ok |
| pdf-008 | 33515 | 1851 | 1783 | 1.28 | 1.50 | ok | ok |
| pdf-009 | 168731 | 1868 | 1794 | 1.99 | 2.79 | ok | ok |
| pdf-010 | 1367062 | 14947 | 14679 | 19.36 | 7.07 | ok | ok |
| pdf-011 | 721126 | 39602 | 0 | 6.04 | 4.06 | ok | err |
| pdf-012 | 613735 | 22624 | 0 | 4.24 | 3.15 | ok | err |
| pdf-013 | 129980 | 16939 | 16618 | 4.64 | 6.65 | ok | ok |
| pdf-014 | 1805168 | 49419 | 0 | 11.76 | 12.02 | ok | err |
| pdf-015 | 324817 | 18452 | 17008 | 19.10 | 28.25 | ok | ok |
| pdf-016 | 1770089 | 32153 | 32886 | 38.09 | 42.04 | ok | ok |
| pdf-017 | 127595 | 5896 | 5748 | 4.65 | 4.82 | ok | ok |
| pdf-018 | 124230 | 7858 | 7582 | 5.34 | 5.45 | ok | ok |
| pdf-019 | 219382 | 11422 | 10946 | 9.88 | 9.37 | ok | ok |
| pdf-020 | 3645188 | 88290 | 88389 | 35.99 | 73.96 | ok | ok |
| pdf-021 | 466189 | 14193 | 13895 | 10.41 | 10.20 | ok | ok |
| pdf-022 | 114157 | 7190 | 7071 | 4.92 | 4.74 | ok | ok |
| pdf-023 | 0 | 0 | 0 | 0.04 | 0.02 | err | err |
| pdf-024 | 158947 | 5478 | 5339 | 5.86 | 4.92 | ok | ok |
| pdf-025 | 181116 | 13196 | 12798 | 8.58 | 8.07 | ok | ok |
| pdf-026 | 6504263 | 60228 | 56493 | 26.31 | 56.56 | ok | ok |
| xlsx-001 | 7776 | 2076 | 2516 | 0.77 | 0.48 | ok | ok |
| xlsx-002 | 13866 | 11333 | 13158 | 0.67 | 1.10 | ok | ok |
| xlsx-003 | 13866 | 11333 | 13158 | 0.80 | 0.76 | ok | ok |
| xlsx-004 | 7797 | 2104 | 2544 | 0.36 | 0.28 | ok | ok |
| pdf-027 | 322138 | 6838 | 5167 | 5.10 | 3.66 | ok | ok |
| pdf-028 | 80483 | 6783 | 6648 | 5.26 | 10.55 | ok | ok |
| pdf-029 | 109803 | 4853 | 4733 | 3.51 | 2.99 | ok | ok |
| pdf-030 | 228639 | 16601 | 15951 | 11.47 | 11.82 | ok | ok |
| pdf-031 | 186016 | 18102 | 5878 | 8.59 | 6.00 | ok | ok |
| pdf-032 | 183380 | 7198 | 4706 | 6.92 | 4.32 | ok | ok |
| pdf-033 | 0 | 0 | 0 | 0.03 | 0.01 | err | err |
| pdf-034 | 66334 | 1782 | 1729 | 1.63 | 5.00 | ok | ok |
| pdf-035 | 0 | 0 | 0 | 0.02 | 0.01 | err | err |
| pdf-036 | 251302 | 7166 | 7024 | 5.56 | 8.92 | ok | ok |
| pdf-037 | 149027 | 14830 | 14411 | 6.85 | 7.25 | ok | ok |
| pdf-038 | 121328 | 8046 | 7942 | 4.08 | 5.45 | ok | ok |
| pdf-039 | 169308 | 1590 | 1349 | 2.20 | 2.32 | ok | ok |
| pdf-040 | 66212 | 909 | 858 | 1.97 | 2.02 | ok | ok |
| pdf-041 | 169207 | 1508 | 1293 | 2.17 | 2.48 | ok | ok |
| xlsx-005 | 0 | 0 | 0 | 0.03 | 0.01 | err | err |
| docx-002 | 0 | 0 | 0 | 0.04 | 0.01 | err | err |
| pdf-042 | 274790 | 6749 | 7346 | 11.54 | 14.86 | ok | ok |
| pdf-043 | 175099 | 13789 | 13242 | 7.80 | 8.16 | ok | ok |
| pdf-044 | 0 | 0 | 0 | 0.03 | 0.01 | err | err |
| pdf-045 | 0 | 0 | 0 | 0.02 | 0.01 | err | err |
| pdf-046 | 0 | 0 | 0 | 0.01 | 0.01 | err | err |
| pdf-047 | 169693 | 9616 | 9424 | 5.43 | 5.92 | ok | ok |
| pdf-048 | 146474 | 8 | 0 | 0.41 | 0.34 | ok | err |
| pdf-049 | 147872 | 9674 | 9552 | 6.10 | 5.96 | ok | ok |
| pdf-050 | 0 | 0 | 0 | 0.05 | 0.01 | err | err |
| pdf-051 | 437182 | 20438 | 18786 | 15.40 | 30.41 | ok | ok |
| pdf-052 | 0 | 0 | 0 | 0.03 | 0.01 | err | err |
| pdf-053 | 0 | 0 | 0 | 0.02 | 0.01 | err | err |
| pdf-054 | 0 | 0 | 0 | 0.01 | 0.01 | err | err |
| pdf-055 | 179177 | 4375 | 4390 | 4.75 | 4.92 | ok | ok |
| pdf-056 | 28333 | 700 | 849 | 1.53 | 1.38 | ok | ok |
| pdf-057 | 28501 | 684 | 833 | 1.15 | 1.51 | ok | ok |
| xlsx-006 | 8185 | 1763 | 1841 | 0.32 | 0.25 | ok | ok |
| pdf-058 | 0 | 0 | 0 | 0.02 | 0.01 | err | err |
| xlsx-007 | 0 | 0 | 0 | 0.02 | 0.01 | err | err |
