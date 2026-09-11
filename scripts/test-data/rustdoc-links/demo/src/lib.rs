// Fixture source for the #7466 NO-CFG verification in
// scripts/check_rustdoc_links_selftest.sh. NOT a workspace member: it is read
// only as text, by the gate's NO-CFG check, against
// scripts/test-data/rustdoc-links/metadata-mini.json.
//
// `config` HAS a cfg site here, so a NO-CFG row claiming otherwise must fail.
// `nogate` and `heavy` are named nowhere, so a NO-CFG row for `nogate` holds.

#[cfg(feature = "config")]
pub mod config;
