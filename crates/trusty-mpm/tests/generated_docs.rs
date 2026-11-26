//! Generated documentation regions of trusty-mpm (#11001, ADR-0077 D7).
//!
//! Why: the IL schedule reference must match the preset `tm` ships.
//! What: renders `il_schedule::render_docs` into the `il-schedule` region of
//! `docs/il-schedule.md`; fails on drift, rewrites under `UPDATE_DOCS=1`.
//! Test: this file is the check (`docs/reference/generated-doc-regions.md`).

use std::path::Path;

use trusty_mpm::core::il_schedule::render_docs;

#[test]
fn il_schedule_reference_matches_the_preset() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/il-schedule.md");
    trusty_common::docgen::assert_region(
        &path,
        "il-schedule",
        &render_docs(),
        "UPDATE_DOCS=1 cargo test -p trusty-mpm --test integration generated_docs::",
    );
}
