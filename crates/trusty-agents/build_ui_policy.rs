// Shared build-script policy, `include!`d by `build.rs` and by
// `tests/build_ui_policy.rs` (#8094).
//
// Why: cargo never compiles a build script's own `#[cfg(test)]` module, so the
// decision that used to hide a failed `pnpm build` behind a placeholder page
// had no way to be tested. Splicing the same two pure functions into both the
// build script and a test target gives that policy real coverage without
// inventing a build-support crate.
// What: `placeholder_reason` (when embedding the stub page is legitimate) and
// `classify_pnpm_step` (what one pnpm step's result means for the cargo
// build). Both are pure. Neither this file nor anything it adds may carry a
// `use` item or an inner `//!` doc comment: `include!` splices it into two
// different crate roots, where a `use` would collide with `build.rs`'s own
// imports and an inner attribute is not in a legal position.
// Test: `crates/trusty-agents/tests/build_ui_policy.rs`.

/// Why the placeholder `ui/dist/index.html` may stand in for a real Svelte
/// build, as `cargo:warning=` text; `None` means the UI must actually build.
///
/// Why (#8094): a `cargo install` that ships `<p>trusty-agents: UI not
/// built.</p>` must be something the operator asked for, never something a
/// broken `pnpm build` decided on their behalf. The two legitimate reasons are
/// the explicit `SKIP_UI_BUILD=1` opt-out and a host with no JS toolchain at
/// all (#112) — both are checked BEFORE pnpm is ever run, so every pnpm
/// failure that reaches [`classify_pnpm_step`] is a real build failure.
/// What: returns the warning text naming the reason, so the operator can tell
/// which opt-out produced the placeholder.
/// Test: `skip_ui_build_names_the_opt_out`, `a_missing_pnpm_names_pnpm`,
/// `a_usable_toolchain_has_no_placeholder_reason`.
fn placeholder_reason(skip_ui_build: bool, pnpm_available: bool) -> Option<String> {
    if skip_ui_build {
        return Some("SKIP_UI_BUILD=1 — web UI build skipped, embedding placeholder UI".to_string());
    }
    if !pnpm_available {
        return Some(
            "pnpm not found — web UI cannot be built, embedding placeholder UI \
             (set SKIP_UI_BUILD=1 to silence this)"
                .to_string(),
        );
    }
    None
}

/// What one pnpm step's exit says about the cargo build: `Ok(())` to carry on,
/// `Err(reason)` to abort with.
///
/// Why (#8094): the previous code turned every failure here into
/// `ensure_placeholder`, so a stale `ui/node_modules` made `cargo install`
/// report success while embedding the stub page. A failure past
/// [`placeholder_reason`] means the operator asked for the real UI and pnpm
/// could not produce it, which is a build error.
/// What: a zero exit is `Ok`. A non-zero exit and a spawn error both become
/// the abort message, which carries the step, the failure, and the
/// `SKIP_UI_BUILD=1` escape hatch. pnpm's own diagnostics reach the terminal
/// on the build script's inherited stderr, which cargo prints when the script
/// fails.
/// Test: `a_failing_pnpm_build_aborts_the_cargo_build`,
/// `an_unspawnable_pnpm_step_aborts_the_cargo_build`,
/// `a_successful_pnpm_step_carries_on`.
fn classify_pnpm_step(
    step: &str,
    result: std::io::Result<std::process::ExitStatus>,
) -> Result<(), String> {
    let failure = match result {
        Ok(status) if status.success() => return Ok(()),
        Ok(status) => format!("{step} failed ({status})"),
        Err(e) => format!("{step} could not be spawned ({e})"),
    };
    Err(format!(
        "{failure} — refusing to embed a placeholder web UI in place of the real one. \
         Fix the UI build, or set SKIP_UI_BUILD=1 to build without the web UI."
    ))
}
