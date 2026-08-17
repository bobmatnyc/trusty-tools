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
//! delivery mechanism is itself testable), and `trusty_common_no_features` when
//! Cargo activated no feature at all. `lib.rs` turns the latter into a
//! `compile_error!` for the `cfg(test)` build only, so a plain `cargo build` /
//! `cargo check` and all 20 consumer crates are unaffected.
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
}
