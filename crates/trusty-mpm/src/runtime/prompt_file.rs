//! The PM system-prompt file every spawn path carries, and the compiled-prompt
//! refresh that rides with it (#2125, #4752, #7568).
//!
//! Why: `runtime::claude_code` is at the 500-SLOC production cap, and #7568 adds
//! a named-root seam beside the ambient entry point. The pair is cohesive — one
//! composes the prompt and records the fold, the other only resolves the
//! operator's framework root for it — so they move out together.
//! What: [`build_prompt_file`] (ambient framework root, the production spawn
//! path) and [`build_prompt_file_in`] (named root, what the tests drive).
//! Test: `build_prompt_file_writes_resolved_prompt_for_project`,
//! `build_prompt_file_refreshes_the_compiled_prompt`,
//! `build_prompt_file_compiled_write_failure_does_not_block_the_spawn`,
//! `build_prompt_file_records_under_the_named_framework_root`,
//! `an_ambient_build_prompt_file_records_under_whatever_home_it_inherits` in
//! `runtime::claude_code::tests`.

use std::path::{Path, PathBuf};

use super::RuntimeError;
use crate::core::session_profile::SessionProfile;

/// Build and write the PM system-prompt file for `project_dir`, for injection
/// into the daemon managed-spawn command via `--append-system-prompt-file`
/// (issue #2125 item 3 — the daemon-adapter carrier).
///
/// Why: `spawn` never actually passed `--append-system-prompt-file`
/// at all, so the one thing that turns a bare `claude` process into a
/// trusty-mpm PM never reached the daemon's default managed-spawn path,
/// leaving every bare-`tm` session running vanilla Claude Code (#2125). This
/// reuses the exact same seam
/// ([`crate::core::session_launch::build_system_prompt_for_with_style_and_native`])
/// the CLI `tm launch` / client `/connect` paths already use, so all three
/// drivers build byte-identical prompts for the same project.
/// What: resolves live native-output-style support (fail-safe to injection via
/// [`crate::core::output_style::claude_supports_native_output_style`]), builds
/// the override-resolved + style-injected prompt for `project_dir`, and writes
/// it to a fresh file under `prompt_dir` via
/// [`crate::core::model_inject::write_prompt_file_in`].
/// #8286: a write failure is `Err` ([`RuntimeError::Spawn`] naming the file and
/// the I/O cause), and every caller refuses the launch on it. Each caller
/// launches a PM or supervisor session, whose role IS this prompt, and there
/// is no other carrier: #2173 forbids falling back to the project's CLAUDE.md.
///
/// #4752/#4832: this is also where the session's compiled prompt
/// (`<harness-root>/.trusty-mpm/sessions/<id>/INSTRUCTIONS-COMPILED.md`) is
/// refreshed from the exact bytes handed to `--append-system-prompt-file`.
/// `session_id` selects that directory: `spawn`/`spawn_resume` pass the managed
/// id they already hold, and the in-place relaunch passes `None` so
/// [`crate::core::harness_root::session_scope`] falls back to the pane's
/// `TM_MANAGED_SESSION_ID`. Passing `None` where an id IS known would refresh a
/// different file from the one preparation wrote.
///
/// WHY THIS ONE IS BEST-EFFORT while the same write is FATAL in the
/// provisioning paths — the asymmetry is deliberate: those are PROVISIONING
/// steps, whose job is to establish the session's on-disk state before anything
/// spawns; refusing there is coherent and is what the owner ruled. This call
/// sits INSIDE the spawn, past that gate.
///
/// That holds only because ALL THREE entry points that reach this function have
/// already had a fatal compiled write succeed:
///   * fresh start → `session_launch::prepare_session_inner`
///   * daemon resume → `managed_routes::lifecycle::resume_managed`
///   * bare-`tm` in-place relaunch → `guided_inplace::run_inplace_relaunch`,
///     via `instruction_pipeline::refresh_compiled_prompt`
///
/// The third was missing until round 4 of #4752, which made the earlier version
/// of this comment false: `run_inplace_relaunch` calls neither of the other two,
/// so this best-effort write was its ONLY compiled write. Adding a call here
/// without that fatal upstream write would resurrect the same hole — check the
/// list above before adding a fourth spawn path.
///
/// The prompt file below is the write the launch depends on, and since #8286
/// it is the fatal one; the compiled copy is only for inspection.
/// Test: `build_prompt_file_writes_resolved_prompt_for_project`,
/// `build_prompt_file_refreshes_the_compiled_prompt`,
/// `build_prompt_file_compiled_write_failure_does_not_block_the_spawn`.
// #8545: the last production caller now names its root; only tests use this.
#[cfg(test)]
pub(super) fn build_prompt_file(
    project_dir: &Path,
    session_id: Option<&str>,
) -> (Result<PathBuf, RuntimeError>, SessionProfile) {
    // #7514: a real spawn, so the ambient framework root is the right ledger —
    // read once, here, and passed down rather than resolved inside the writer.
    let framework_root = crate::core::paths::FrameworkPaths::default().root;
    build_prompt_file_in(
        &framework_root,
        &std::env::temp_dir(),
        project_dir,
        session_id,
    )
}

/// [`build_prompt_file`] against a caller-named framework root.
///
/// Why (#7568): the entry point above resolves its ledger from the process home
/// directory, and this is the LAST savings producer that did. Every in-process
/// test that drives a spawn therefore had to redirect `$HOME` — a
/// process-global write, `#[serial]`-guarded, of the class #5544 bans outright
/// in the `tm` binary — just to keep a fixture's row and its `no-fold-warned`
/// marker out of the operator's own `~/.trusty-mpm/usage/`. Naming the root is
/// what lets a test assert the destination instead of guarding the process.
/// The ambient form stays the production path: a real spawn's ledger IS the
/// operator's.
/// What: the body of [`build_prompt_file`] — composes the project's prompt,
/// refreshes the compiled copy under `framework_root` (best-effort; see the
/// entry point for why this write is not fatal here), and writes the
/// `--append-system-prompt-file` payload to a file under `prompt_dir`
/// (production: the process temp dir; #8286 tests name an unwritable one).
/// Test: `build_prompt_file_records_under_the_named_framework_root`,
/// `an_ambient_build_prompt_file_records_under_whatever_home_it_inherits`,
/// `spawn_refuses_when_the_prompt_file_cannot_be_written`.
pub(super) fn build_prompt_file_in(
    framework_root: &Path,
    prompt_dir: &Path,
    project_dir: &Path,
    session_id: Option<&str>,
) -> (Result<PathBuf, RuntimeError>, SessionProfile) {
    // #8453: resolved once, against the named root's user config; the prompt
    // is composed for it and the caller stamps the same value.
    let profile = crate::core::session_profile::resolve(
        project_dir,
        &crate::core::config::MpmConfig::load(framework_root),
    );
    // #9012: the PM instructions are runtime content; none refuses the spawn.
    let content = match crate::core::content_source::framework_content_for(project_dir) {
        Ok(content) => content,
        Err(err) => {
            return (
                Err(RuntimeError::Spawn(format!(
                    "cannot compose the PM instructions for {}: {err}",
                    project_dir.display()
                ))),
                profile,
            );
        }
    };
    let native = crate::core::output_style::claude_supports_native_output_style();
    let prompt = crate::core::session_launch::build_system_prompt_for_profile(
        &content,
        project_dir,
        None,
        native,
        profile,
    );

    // #4752: refresh this PROJECT's compiled prompt from the very string about
    // to be passed to `--append-system-prompt-file`. Best-effort by design —
    // see this function's doc comment for why the same write is fatal in the
    // two provisioning steps and not here.
    let scope = crate::core::harness_root::session_scope(session_id);
    let compiled = crate::core::instruction_pipeline::compiled_prompt_path(project_dir, &scope);
    if let Err(err) = crate::core::instruction_pipeline::write_compiled_prompt_recording_in(
        &content,
        framework_root,
        &compiled,
        &prompt,
    ) {
        tracing::warn!(
            project = %project_dir.display(),
            path = %compiled.display(),
            "failed to refresh the compiled PM prompt (non-fatal; the session still \
             launches with the prompt file below): {err}"
        );
    }

    // #8286: no flag-less fallback; the caller refuses the launch on `Err`.
    let file =
        crate::core::model_inject::write_prompt_file_in(prompt_dir, &prompt).map_err(|err| {
            RuntimeError::Spawn(format!(
                "{err}; refusing to launch {} without its PM instructions (#8286)",
                project_dir.display()
            ))
        });
    (file, profile)
}
