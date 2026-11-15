# Changelog

All notable changes to `trusty-okf-xberg-worker` are documented here.

## [Unreleased]

### Changed

- Moved to `xberg = "=1.4.0"`, which resolves calamine 0.38.0 and so fixes RUSTSEC-2026-0436 (a crafted .xls/.xlsb could panic the CFB parser or make it allocate without bound). The vendored calamine 0.36.1 patch from #11093 is gone, `lopdf` follows xberg to 0.47.0, and the worker's `rust-version`, `rust-toolchain.toml` and CI now use Rust 1.99, which xberg 1.4.0 requires; the root workspace toolchain is unchanged. `hello` now reports `xberg_version: "1.4.0"` (#11154).

### Added

- `trusty-okf-xberg-worker`, the trusty-okf extraction worker (SPEC-OKF-01 §2.16, Appendix B; #10746 S10b). It extracts PDF, DOCX, XLSX and PPTX to Markdown through `xberg = "=1.3.10"` (features `pdf`, `office`, `excel` only: no OCR, model, URL or network feature), speaks protocol 1 to trusty-okf's `WorkerExtractor`, echoes its own SHA-256 of each input, reports `no_text_layer` only for a PDF it proves has pages and no text layer (any other empty PDF, such as one with no pages, a page-tree node that does not resolve, a content stream that does not decode or parse in full, or one whose parse would pass the memory budget kept under `RLIMIT_AS`, is `extract_error`) and an xberg failure as `extract_error` with no content excerpt, and gives byte-identical output for identical input. Its `hello` reports the sandbox it observes (`bwrap`, `seatbelt` or `none`) for the host's cross-check.
