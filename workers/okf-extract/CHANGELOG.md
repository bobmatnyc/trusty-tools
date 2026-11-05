# Changelog

All notable changes to `trusty-okf-xberg-worker` are documented here.

## [Unreleased]

### Added

- `trusty-okf-xberg-worker`, the trusty-okf extraction worker (SPEC-OKF-01 §2.16, Appendix B; #10746 S10b). It extracts PDF, DOCX, XLSX and PPTX to Markdown through `xberg = "=1.3.10"` (features `pdf`, `office`, `excel` only: no OCR, model, URL or network feature), speaks protocol 1 to trusty-okf's `WorkerExtractor`, echoes its own SHA-256 of each input, reports `no_text_layer` only for a PDF it proves has pages and no text layer (any other empty PDF, such as one with no pages or an undecodable content stream, is `extract_error`) and an xberg failure as `extract_error` with no content excerpt, and gives byte-identical output for identical input. Its `hello` reports the sandbox it observes (`bwrap`, `seatbelt` or `none`) for the host's cross-check.
