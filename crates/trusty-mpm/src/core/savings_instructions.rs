//! The instruction/language-compression savings producer (#6958).
//!
//! Why: the owner asked to see what "language compression" is worth. The one
//! place trusty-mpm folds instruction prose is the composer that writes
//! `INSTRUCTIONS-COMPILED.md`, so that is where the number is available: it
//! knows every candidate instruction body it read and the exact bytes it
//! delivered. Measuring there — rather than against a dated constant recorded
//! by hand — means the figure re-derives itself when the instruction corpus
//! changes, and it goes to zero honestly when nothing was folded.
//!
//! What: [`record_instruction_compression_in`], called from
//! [`crate::core::instruction_pipeline::write_compiled_prompt_recording_in`].
//! #7514 split that off the bare write, which now records nothing, and made the
//! ledger's root a parameter rather than a read of the process home directory —
//! each launch path resolves the ambient root once, in its own named wrapper.
//! It compares the folded source set against the
//! delivered prompt and appends one [`crate::core::savings::SavingsRow`] per
//! session launch when, and only when, the delivered prompt is smaller.
//!
//! **What counts as "folded".** The source set is every instruction body the
//! composer READ for this session: the nine bundled section sources, plus each
//! named-section override body it read from the project's `CLAUDE.md`. Both are
//! candidates; only one of each overridden pair reaches the output, so the
//! delta is what the override mechanism folded away. Generated context the
//! composer ADDS — the live agent roster, the detected stack profile — has no
//! source file behind it and appears only on the delivered side, which is why a
//! project that overrides nothing produces a delivered prompt LARGER than its
//! sources and correctly writes no row. A savings figure has to be able to come
//! out zero, or it is not a measurement.
//!
//! Everything here is best-effort: an unresolvable session id, an unpriceable
//! model, or an unwritable ledger each skip the row. A missing savings row must
//! never cost a session its launch.
//!
//! **Which session a row is keyed by (#7209, corrected by #7245).** The
//! compiled prompt lives at
//! `<harness-root>/.trusty-mpm/sessions/<scope>/INSTRUCTIONS-COMPILED.md`,
//! where `<scope>` is [`crate::core::harness_root::session_scope`] — the
//! managed session id, or `local`. The `💸` statusline segment folds this
//! ledger by the id Claude Code sends it on stdin, which is
//! [`crate::core::savings::CLAUDE_CODE_SESSION_ID_ENV`] and is a different
//! value.
//!
//! #7209 keyed the row by that variable when it is set. It never is here: this
//! producer runs in the `tm` process that compiles the prompt, before `claude`
//! is spawned, and Claude Code exports the variable into its own children only.
//! Every row therefore fell back to the directory name, which is the one key the
//! statusline cannot match. So since #7245 the fallback does not write a row at
//! all — it STAGES it through [`crate::core::savings_sidecar::stage_row`], and
//! the first `tm hook` invocation (which runs inside Claude Code and knows the
//! id) appends it. The directory location is unchanged: one compiled prompt per
//! managed session is what that path is for.
//!
//! **The decline that hides the segment (#7245).** When the compiled prompt is
//! not smaller than its sources there is nothing to record, and for a project
//! that overrides no instruction section that is permanently true — the composer
//! ADDS generated context and folds nothing away. That decline logged at
//! `debug!`, so the segment's absence had no explanation anywhere an operator
//! would look. It now warns once per project through
//! [`crate::core::savings_sidecar::warn_no_fold_once`], naming both byte counts.
//!
//! Test: the inline suite in `savings_instructions_tests.rs` —
//! `no_row_when_the_compiled_prompt_is_not_smaller`,
//! `a_folded_source_set_produces_a_row`, `no_row_when_the_model_cannot_be_priced`,
//! `folded_source_bytes_adds_an_override_body`,
//! `the_row_is_keyed_by_the_claude_session_id`,
//! `no_claude_id_stages_the_row_instead_of_writing_an_unfoldable_one`,
//! `a_staged_row_becomes_foldable_at_the_first_hook_invocation`,
//! `a_prompt_that_folds_nothing_warns_once_and_writes_no_row`,
//! `the_row_reports_the_compiled_prompts_own_size`,
//! `a_stub_compiled_prompt_writes_no_row`.
//!
//! **The implausibly small compiled prompt (#7491).** `compiled < sources` was
//! the producer's only plausibility test, and a stub, a truncated write, or a
//! stale file at the compiled-prompt path passes it trivially — the owner's
//! ledger carried `sources 22559 B - compiled 13 B`, a claimed ~100% reduction
//! that inflated the `💸` percentage for as long as the append-only ledger kept
//! it. [`min_plausible_compiled_bytes`] is now the floor, and a file below it
//! declines the row with a `warn!` naming the path and both byte counts.

use std::path::Path;

use crate::core::harness_root::{HARNESS_DIR, SESSIONS_DIR};
use crate::core::instruction_pipeline::COMPILED_PROMPT_FILE;
use crate::core::savings::{
    BYTES_PER_TOKEN, SavingsRow, TECHNIQUE_INSTRUCTION_COMPRESSION, append_row,
    claude_code_session_id, now_ts, savings_log_in,
};
use crate::core::savings_sidecar::{stage_row, warn_no_fold_once};

/// Append one `instruction-compression` row for a session whose compiled prompt
/// came out smaller than the sources that fed it.
///
/// Why: called from the compiled-prompt writer's recording entry points so every
/// launch path — fresh start, daemon resume, in-place relaunch — records the same
/// way without each one growing its own call. #7514 removed the ambient
/// `record_instruction_compression` that resolved `framework_root` from the
/// process home directory: every caller reachable from a test wrote into the
/// operator's own ledger through it. Each launch path now resolves the ambient
/// root once, in its own named wrapper, and passes it down.
/// What: reads the Claude Code session id the statusline folds by, then defers to
/// [`record_instruction_compression_to`] against `framework_root`.
/// Test: `a_bare_compiled_write_records_no_savings_row`,
/// `a_recording_compiled_write_reaches_the_named_framework_root`.
pub(crate) fn record_instruction_compression_in(framework_root: &Path, dest: &Path, prompt: &str) {
    // #7209: the row's key is the Claude Code session id, not the directory name.
    record_instruction_compression_to(
        framework_root,
        dest,
        prompt,
        claude_code_session_id(),
        resolve_pm_price,
    );
}

/// [`record_instruction_compression_in`] against an explicit framework root,
/// session id and price.
///
/// Why: the three ambient reads the entry point makes — the framework root under
/// the operator's home, the harness's session-id variable, and the configured
/// PM model — are exactly what makes the keying decision untestable in place.
/// Passing all three in keeps the test on a tempdir root with no process-env
/// mutation, which the `tm` binary's env ratchet (#5544) and parallel test
/// binaries both require. `framework_root` rather than a ledger path because
/// #7245 gave this function a second destination under that root: the staging
/// file, which has to land beside the ledger the hook will later append to.
/// What: derives the compile-time session id and the harness root from `dest`
/// (which is always
/// `<harness-root>/.trusty-mpm/sessions/<id>/INSTRUCTIONS-COMPILED.md`),
/// measures the fold, prices the token delta, and then either appends the row —
/// when `claude_session_id` gives it the key the statusline folds by — or stages
/// it for the first `tm hook` invocation, which is the first process that knows
/// that key. A prompt that folded nothing warns once per project and writes
/// nothing. Silent no-op on every other failure.
/// Test: `the_row_is_keyed_by_the_claude_session_id`,
/// `no_claude_id_stages_the_row_instead_of_writing_an_unfoldable_one`,
/// `a_prompt_that_folds_nothing_warns_once_and_writes_no_row`.
fn record_instruction_compression_to(
    framework_root: &Path,
    dest: &Path,
    prompt: &str,
    claude_session_id: Option<String>,
    price: impl FnOnce() -> Option<(String, f64)>,
) {
    let Some((compiled_prompt_id, harness_root)) = session_and_root(dest) else {
        // #7514: a destination that is not a compiled-prompt path has no session
        // and no harness root to measure against, so there is nothing to record.
        tracing::warn!(
            compiled_prompt = %dest.display(),
            "not a <root>/.trusty-mpm/sessions/<id>/INSTRUCTIONS-COMPILED.md path, so \
             neither the session nor the source set it would be measured against is \
             known; writing no instruction-compression savings row"
        );
        return;
    };
    let source_bytes = folded_source_bytes(&harness_root);
    let compiled_bytes = prompt.len();
    // #7491: a compiled prompt this small is not a fold, it is a stub,
    // a truncated write, or a stale file at the compiled-prompt path.
    if compiled_bytes < min_plausible_compiled_bytes() {
        tracing::warn!(
            compiled_prompt = %dest.display(),
            compiled_bytes,
            source_bytes,
            floor = min_plausible_compiled_bytes(),
            "the compiled prompt is smaller than the smallest instruction section \
             it is assembled from, so it cannot be a real compiled prompt; writing \
             no instruction-compression savings row rather than a near-100% one"
        );
        return;
    }
    // #7245: checked here, where the project root is in hand, so the decline that
    // makes the 💸 segment structurally absent for an override-free project is
    // stated once instead of hidden at `debug!` inside the row builder.
    if compiled_bytes >= source_bytes {
        warn_no_fold_once(framework_root, &harness_root, source_bytes, compiled_bytes);
        return;
    }
    // #7209: the statusline folds by the id Claude Code sends it, so a row keyed
    // by the compiled-prompt directory name never matches for a managed session.
    let session_id = claude_session_id
        .clone()
        .unwrap_or_else(|| compiled_prompt_id.clone());
    let Some(row) = instruction_compression_row(&session_id, source_bytes, compiled_bytes, price)
    else {
        return;
    };
    // #7245: this process is the compiler, not a child of Claude Code, so an
    // absent id here is the NORMAL case rather than an edge one. Staging the row
    // hands it to the first hook, which can key it correctly.
    if claude_session_id.is_none() {
        stage_row(framework_root, dest, &row);
        return;
    }
    let ledger = savings_log_in(framework_root);
    if let Err(source) = append_row(&ledger, &row) {
        tracing::warn!(
            ledger = %ledger.display(),
            %source,
            "could not append the instruction-compression savings row"
        );
    }
}

/// Re-measure the fold from a compiled prompt already on disk and append the
/// row under a known Claude session id.
///
/// Why (#7411): staging is the only way this producer's row reaches the ledger
/// for a normal launch, so a staged file that is lost — an unwritable
/// `pending-savings/`, a compile whose row was discarded, a `tm` upgraded
/// between the compile and the hook — leaves the session with no row at all and
/// the `💸` segment blank for the rest of it. The compiled prompt itself
/// survives on disk, and it is the same input the producer measured, so the
/// hook can redo the measurement instead of inventing a second staging
/// mechanism to protect the first.
/// What: reads `compiled_prompt`, then runs the ordinary producer against it
/// with `claude_session_id` supplied, which makes it append rather than stage.
/// Reports whether a row landed, read back off the ledger — the producer
/// declines silently for an unpriceable model or a prompt that folded nothing,
/// and a caller must not report those as a row. Callers guard against the
/// double-append themselves with [`crate::core::savings::has_row`].
/// Test: `rederiving_from_the_compiled_prompt_appends_under_the_session_id`,
/// `rederiving_a_prompt_that_folds_nothing_appends_nothing`.
pub(crate) fn rederive_from_compiled_prompt(
    framework_root: &Path,
    compiled_prompt: &Path,
    claude_session_id: &str,
) -> bool {
    let Ok(prompt) = std::fs::read_to_string(compiled_prompt) else {
        return false;
    };
    let ledger = savings_log_in(framework_root);
    record_instruction_compression_to(
        framework_root,
        compiled_prompt,
        &prompt,
        Some(claude_session_id.to_string()),
        resolve_pm_price,
    );
    crate::core::savings::has_row(
        &ledger,
        claude_session_id,
        TECHNIQUE_INSTRUCTION_COMPRESSION,
    )
}

/// Split `<harness-root>/.trusty-mpm/sessions/<id>/INSTRUCTIONS-COMPILED.md`
/// into its session id and its harness root.
///
/// Why: the writer takes only a destination path, and the harness root the
/// source set is measured against is encoded in it. Deriving it here rather
/// than widening the writer's signature keeps the three launch call sites
/// unchanged.
/// What: the session id is the parent directory's name — the FALLBACK key since
/// #7209, used only when the harness exported no Claude Code session id; the
/// harness root is three directories above that (`sessions/`, `.trusty-mpm/`,
/// the root).
/// Returns `None` for any path not of that shape.
///
/// #7514: it now CHECKS that shape. It used to take the parent as a session id
/// and count three directories up for the root without looking at what those
/// directories were called, so `<tmp>/a/b/INSTRUCTIONS-COMPILED.md` resolved to
/// session `b` under root `<tmp>` — which is exactly the
/// `session_id":"b" … compiled 13 B` row the owner's ledger carried, produced by
/// this crate's own `write_compiled_prompt_to_creates_parent_dirs` fixture. A
/// path that is not a compiled prompt now declines, so no row can be attributed
/// to a directory that is not a session.
/// Test: `session_and_root_reads_the_compiled_prompt_path`,
/// `session_and_root_rejects_a_short_path`,
/// `session_and_root_rejects_a_path_that_is_not_under_sessions`,
/// `a_malformed_compiled_prompt_path_records_no_row`.
fn session_and_root(dest: &Path) -> Option<(String, std::path::PathBuf)> {
    if dest.file_name()? != std::ffi::OsStr::new(COMPILED_PROMPT_FILE) {
        return None;
    }
    let session_dir = dest.parent()?;
    let session_id = session_dir.file_name()?.to_str()?.to_string();
    let sessions_dir = session_dir.parent()?;
    if sessions_dir.file_name()? != std::ffi::OsStr::new(SESSIONS_DIR) {
        return None;
    }
    let harness_dir = sessions_dir.parent()?;
    if harness_dir.file_name()? != std::ffi::OsStr::new(HARNESS_DIR) {
        return None;
    }
    Some((session_id, harness_dir.parent()?.to_path_buf()))
}

/// Total bytes of every instruction body the composer read for this project.
///
/// Why: see the module header — this is the "before" half of the fold, and it
/// has to include the override bodies as well as the bundled sections, because
/// an override is a source the composer read and (partly) discarded.
/// What: the nine bundled section sources plus every accepted named-section
/// override body found in the project's `CLAUDE.md`. Rejected override blocks
/// are excluded: the composer did not fold them, it declined them.
/// Test: `folded_source_bytes_counts_the_bundled_sections`,
/// `folded_source_bytes_adds_an_override_body`.
fn folded_source_bytes(project_dir: &Path) -> usize {
    let bundled: usize = crate::core::instruction_pipeline::SECTION_SOURCES
        .iter()
        .map(|(_, body)| body.len())
        .sum();
    let overrides: usize = crate::core::claude_md_sections::scan_project(project_dir)
        .overrides
        .iter()
        .map(|applied| applied.body.len())
        .sum();
    bundled + overrides
}

/// The smallest byte count a real compiled PM prompt can have.
///
/// Why (#7491): the ledger recorded `sources 22559 B - compiled 13 B` — a
/// claimed ~100% reduction — because the producer's only plausibility test was
/// `compiled < sources`, which a stub, a truncated write, or a stale file at the
/// compiled-prompt path passes trivially. The ledger is append-only, so one such
/// row inflates the `💸` percentage for as long as it is folded.
/// What: the smallest single bundled instruction section. The compiled prompt is
/// an assembly of those sections (with overrides substituted for some of them),
/// so anything below one whole section cannot be such an assembly. Derived from
/// the corpus rather than spelled as a constant, so it re-derives when the
/// sections change.
/// Test: `a_stub_compiled_prompt_writes_no_row`,
/// `the_row_reports_the_compiled_prompts_own_size`.
pub(crate) fn min_plausible_compiled_bytes() -> usize {
    crate::core::instruction_pipeline::SECTION_SOURCES
        .iter()
        .map(|(_, body)| body.len())
        .min()
        .unwrap_or(0)
}

/// Build the row, or decline to.
///
/// Why: separating the arithmetic and every decline condition from the IO is
/// what makes "writes no row when the compiled output is not smaller" a unit
/// test rather than a filesystem assertion.
/// What: returns `None` when the compiled output is not strictly smaller, when
/// the byte delta rounds to no whole token, when `price` cannot price the
/// session's model, or when the resulting cost is not strictly positive.
/// `price` resolves the session's model and its USD-per-million input rate.
/// Test: `no_row_when_the_compiled_prompt_is_not_smaller`,
/// `no_row_when_the_model_cannot_be_priced`,
/// `instruction_compression_tokens_use_the_shared_divisor`.
fn instruction_compression_row(
    session_id: &str,
    source_bytes: usize,
    compiled_bytes: usize,
    price: impl FnOnce() -> Option<(String, f64)>,
) -> Option<SavingsRow> {
    if compiled_bytes >= source_bytes {
        // #7245: the caller checks this first and warns once per project, so
        // reaching here means a direct unit-test call. Kept as the builder's own
        // guard — the arithmetic below is only meaningful on a real fold.
        tracing::debug!(
            session_id,
            source_bytes,
            compiled_bytes,
            "instruction composition folded nothing away; writing no savings row"
        );
        return None;
    }
    let saved_bytes = source_bytes - compiled_bytes;
    let tokens_saved = (saved_bytes as f64 / BYTES_PER_TOKEN).floor() as i64;
    if tokens_saved <= 0 {
        return None;
    }
    let (model, input_per_million) = price()?;
    let cost_saved_usd = (tokens_saved as f64 / 1_000_000.0) * input_per_million;
    if !cost_saved_usd.is_finite() || cost_saved_usd <= 0.0 {
        return None;
    }
    Some(SavingsRow {
        ts: now_ts(),
        session_id: session_id.to_string(),
        technique: TECHNIQUE_INSTRUCTION_COMPRESSION.to_string(),
        tokens_saved,
        // #7179: the percent segment's denominator — the folded source set's
        // own token count. Floor of a non-negative byte count: never negative.
        tokens_before: (source_bytes as f64 / BYTES_PER_TOKEN).floor() as u64,
        cost_saved_usd,
        basis: format!(
            "sources {source_bytes} B - compiled {compiled_bytes} B, \
             at {BYTES_PER_TOKEN} B/token, priced at {model} input \
             ${input_per_million}/Mtok"
        ),
        // #6972: authoritative here, unlike in the divert producer — this runs at
        // launch, off the chain that produced the session's own `--model` flag.
        model_source: crate::core::session_model::MODEL_SOURCE_LAUNCH_CONFIG.to_string(),
    })
}

/// Resolve the session's PM model and its input price.
///
/// Why: the price table is `trusty_common::inference::pricing` — the
/// most complete of the workspace's three and the one the epic already names as
/// the consolidation target. This feature adds no fourth table, and it prices
/// nothing it cannot look up: an unrecognised model slug declines the row
/// rather than substituting a guessed rate.
/// What: resolves the PM model through the same `resolve_pm_model` chain the
/// launcher uses, then returns `(slug, input USD per million tokens)`, or `None`
/// when the table does not know the family.
/// Test: `resolve_pm_price_agrees_with_the_shared_table`.
fn resolve_pm_price() -> Option<(String, f64)> {
    let config = crate::core::config::MpmConfig::load_default();
    let model = crate::core::model_inject::resolve_pm_model(&config, None);
    let pricing = trusty_common::inference::pricing(&model)?;
    Some((model, pricing.input))
}

#[cfg(test)]
#[path = "savings_instructions_tests.rs"]
mod tests;
