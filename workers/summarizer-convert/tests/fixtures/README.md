# Converter fixtures

Every document here was written for this project; none is third-party or
personal. `make.sh` regenerates all but the hand-written ones (pandoc 3 and
python3).

| File | How it was made | Known text |
|---|---|---|
| `source.md` | Written by hand; the source of the others | "forty two lights", "three were dark" |
| `survey.docx` | `pandoc source.md -o survey.docx` | same |
| `survey.pptx` | `pandoc source.md -o survey.pptx` | same |
| `survey.html` | `pandoc source.md -s -o survey.html` | same |
| `survey.pdf` | `make_pdf.py`: a one-page PDF with Helvetica text lines | same |
| `scanned.pdf` | `make_pdf.py` with no lines: a page with no text, as a scan gives | none |
| `tables.pdf` | `make_table_pdf.py`: three pages; page 2 holds a ruled 3x2 grid | "Lights", "42" in one table row |
| `survey.eml` | Written by hand: a plain-text e-mail with example.org addresses | same |
| `survey.xlsx` | `make_xlsx.py`: two sheets, `Lights` (pier counts) and `Keepers` (lamp checks), standard library only | "forty two lights, three were dark", "replaced every dark lamp" |

The malformed XLSX cases (a truncated `survey.xlsx`, a ZIP with a broken
sheet, plain text renamed `.xlsx`) are written by the tests into a temporary
directory, so no broken file is checked in.
