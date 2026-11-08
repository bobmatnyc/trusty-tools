//! Bundle operations: open, create, put, move, verify, supersede, deprecate,
//! `index.md` and `log.md` regeneration, path confinement, graph read,
//! convert and migrate (SPEC-OKF-01 §2.3, §2.9-§2.11, §4.6-§4.8).
//!
//! Every entry point takes a bundle root. Filled by slices S3 and S6 of #10746.
