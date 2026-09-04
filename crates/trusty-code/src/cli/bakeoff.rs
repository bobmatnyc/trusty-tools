//! `tcode bakeoff-gate` — the operator surface for the #5441 milestone exit
//! gate.
//!
//! Why: the gate is only useful if the external bake-off runner and a human
//! closing a milestone can both invoke it the same way and get the same
//! verdict. Putting it on the `tcode` binary means the gate ships with, and is
//! versioned alongside, the build whose evidence it judges — a gate installed
//! separately could drift from the report shape it cross-checks.
//! What: argument-shaped setup over [`trusty_code::bakeoff`] — load the
//! candidate bundle, preflight it against the caller's pins, optionally compare
//! it to a baseline, render human or JSON, and map the verdict to an exit code.
//! Every decision lives in the library module; this file only wires.
//! Test: `tests/bakeoff_gate_e2e.rs` drives the real binary.

use std::path::Path;

use trusty_code::bakeoff::{
    Baseline, ComparisonInputs, GateReport, Pins, compare_against_baseline, load_bundle, preflight,
};

/// Exit code for a bundle that failed the gate.
///
/// Why: distinct from 0 (passed) and from the 2 an argument error would use, so
/// a CI step can tell "the gate said no" from "the gate could not run".
/// What: `1`.
/// Test: `bakeoff_gate_e2e::gate_fails_on_mock_evidence`.
pub const EXIT_GATE_FAILED: i32 = 1;

/// Exit code for a gate that could not reach a verdict.
///
/// Why: an unreadable dispositions file or a missing bundle root is an operator
/// mistake, not evidence of a regression, and must never be reported as one.
/// What: `2`.
/// Test: `bakeoff_gate_e2e::gate_reports_an_unusable_bundle_root_distinctly`.
pub const EXIT_UNUSABLE: i32 = 2;

/// Run the gate over one bundle.
///
/// Why: one entry point so `main.rs` stays clap definitions plus dispatch.
/// What: preflights `bundle`, compares it to `baseline` when given, prints the
/// combined report in the requested form, and returns the process exit code.
/// A missing bundle root or unreadable dispositions file returns
/// [`EXIT_UNUSABLE`] without a verdict.
/// Test: `bakeoff_gate_e2e::*`.
pub fn run(
    bundle: &Path,
    baseline: Option<&Path>,
    pins: Pins,
    tolerance_pct: f64,
    json: bool,
) -> i32 {
    if !bundle.is_dir() {
        eprintln!(
            "tcode bakeoff-gate: no bundle directory at {}",
            bundle.display()
        );
        return EXIT_UNUSABLE;
    }

    let candidate = load_bundle(bundle);
    let inputs = match ComparisonInputs::load(&candidate, tolerance_pct) {
        Ok(inputs) => inputs,
        Err(e) => {
            eprintln!("tcode bakeoff-gate: {e}");
            return EXIT_UNUSABLE;
        }
    };

    let mut report: GateReport = preflight(&candidate, &pins);
    if let Some(baseline_root) = baseline {
        if !baseline_root.is_dir() {
            eprintln!(
                "tcode bakeoff-gate: no baseline directory at {}",
                baseline_root.display()
            );
            return EXIT_UNUSABLE;
        }
        let baseline = Baseline(load_bundle(baseline_root));
        report.absorb(compare_against_baseline(&candidate, &baseline, &inputs));
    } else {
        report
            .notes
            .push("no baseline given — regression comparison was not performed".to_string());
    }

    if json {
        println!("{}", report.render_json());
    } else {
        print!("{}", report.render_human());
    }

    if report.passed() { 0 } else { EXIT_GATE_FAILED }
}
