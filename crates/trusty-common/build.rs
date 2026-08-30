//! Build script: detect the zero-feature build so the crate's unit-test target
//! can refuse to report a vacuous green (#4901).
//!
//! Why: `trusty-common` declares `default = []` and gates 25+ modules behind
//! opt-in features, so `cargo test -p trusty-common` — the command CLAUDE.md
//! prescribed as the per-crate check — compiles none of them. It runs 328 of
//! the crate's ~2062 tests and exits 0, including when a file in `memory_core`
//! does not compile at all; PR #4899 shipped a green run on exactly that basis
//! before the correct code existed. Cargo hands the resolved feature set to a
//! build script but never to `cfg`, so "no features at all" has to be detected
//! here. The alternative — a `#[cfg(not(any(feature = …)))]` list in `lib.rs` —
//! would silently stop covering every feature added after it was written.
//! What: emits `trusty_common_build_script_ran` unconditionally (so the guard's
//! delivery mechanism is itself testable), `trusty_common_no_features` when
//! Cargo activated no feature at all, and `trusty_common_default_not_empty`
//! when the manifest's `default` set stops being `[]` — the precondition that
//! makes discounting `CARGO_FEATURE_DEFAULT` correct. `lib.rs` turns the latter
//! two into `compile_error!`s for the `cfg(test)` build only, so a plain
//! `cargo build` / `cargo check` and all 20 consumer crates are unaffected.
//! Test: `src/lib.rs` refuses to compile at all when
//! `trusty_common_build_script_ran` is absent, so this script ceasing to run is
//! a build failure rather than a silent one. The guard's own behaviour is
//! demonstrated in the PR for #4901 by breaking a `memory_core` file and
//! re-running both command forms.

fn main() {
    // Re-run only when the manifest changes: the feature set is the sole input.
    println!("cargo::rerun-if-changed=Cargo.toml");
    println!("cargo::rustc-check-cfg=cfg(trusty_common_build_script_ran)");
    println!("cargo::rustc-check-cfg=cfg(trusty_common_no_features)");
    println!("cargo::rustc-check-cfg=cfg(trusty_common_default_not_empty)");
    println!("cargo::rustc-cfg=trusty_common_build_script_ran");

    // Cargo sets `CARGO_FEATURE_<NAME>` for each activated feature. `default`
    // is itself a feature and is always activated, so it is present even for
    // the bare build and has to be discounted — this crate's `default` is `[]`,
    // and the day it stops being empty this discount stops being correct.
    let any_real_feature_enabled = std::env::vars_os().any(|(key, _)| {
        let key = key.to_string_lossy();
        key.starts_with("CARGO_FEATURE_") && key != "CARGO_FEATURE_DEFAULT"
    });

    if !any_real_feature_enabled {
        println!("cargo::rustc-cfg=trusty_common_no_features");
    }

    // #4901: the discount above is only correct while `default` is empty. Give
    // a non-empty `default` its own cfg so the guard says it went inert rather
    // than going quiet — `default = ["foo"]` sets `CARGO_FEATURE_FOO` on the
    // bare run, `any_real_feature_enabled` becomes true, and the zero-feature
    // guard stops firing with nothing turning red.
    if !default_feature_set_is_empty() {
        println!("cargo::rustc-cfg=trusty_common_default_not_empty");
    }
}

/// Why (#4901): reads the one manifest fact the zero-feature guard depends on.
/// What: scans `Cargo.toml` for the `default` key inside `[features]` and
/// reports whether its value is the empty array. Fails CLOSED — an unreadable
/// manifest, a missing `[features]` table, or a `default` spelled in a shape
/// this scan does not recognise all report "not empty", which turns into a
/// `cfg(test)`-scoped `compile_error!` rather than a silently inert guard.
/// Text-scanned rather than parsed because a build script that pulled in a TOML
/// parser would put a build-dependency on every consumer of this crate;
/// `tests/feature_coverage.rs` does the real parse, where `toml` is already a
/// dev-dependency.
/// Test: `default_feature_set_is_empty` (`tests/feature_coverage.rs`) asserts
/// the same fact through a real TOML parse, so the two disagree loudly if this
/// scan ever reads the manifest wrong.
fn default_feature_set_is_empty() -> bool {
    let Ok(manifest) = std::fs::read_to_string("Cargo.toml") else {
        return false;
    };
    let mut in_features = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_features = line == "[features]";
            continue;
        }
        if in_features && line.starts_with("default") {
            let Some((key, value)) = line.split_once('=') else {
                return false;
            };
            return key.trim() == "default" && value.trim() == "[]";
        }
    }
    false
}
