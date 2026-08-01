//! Tests for the per-assistant home + OKG store model (#4325).
//!
//! Why: the home is a USER-OWNED directory the app generates, so the two things
//! worth pinning are the ones a user can break — the layout contract itself,
//! and what the system says when a user changes it from outside.
//! What: `instance_tests` (id validation), `home_tests` (layout, resolution,
//! idempotent creation), `store_root_tests` (the new `[[stores]] root` field's
//! confinement), `health_tests` (detection of missing/malformed entries).
//! Test: this module IS the test surface.

mod health_tests;
mod home_tests;
mod instance_tests;
mod store_root_tests;
