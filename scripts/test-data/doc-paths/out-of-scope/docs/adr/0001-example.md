# ADR-0001: example

An ADR records a past state, so `crates/trusty-controller/src/main.rs` must keep
naming the crate as it was called when the decision was taken. The scope
excludes `docs/adr/` for that reason.

This tree therefore holds no in-scope file at all, and the gate must refuse the
run with SCAN FLOOR rather than report a clean pass over nothing.
