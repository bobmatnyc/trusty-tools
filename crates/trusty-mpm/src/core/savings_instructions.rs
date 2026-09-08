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
//! What: [`record_instruction_compression`], called from
//! [`crate::core::instruction_pipeline::write_compiled_prompt_to`] — the single
//! writer of the compiled prompt. It compares the folded source set against the
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
//! **Which session a row is keyed by (#7209).** The compiled prompt lives at
//! `<harness-root>/.trusty-mpm/sessions/<scope>/INSTRUCTIONS-COMPILED.md`,
//! where `<scope>` is [`crate::core::harness_root::session_scope`] — the
//! managed session id, or `local`. The `💸` statusline segment folds this
//! ledger by the id Claude Code sends it on stdin, which is
//! [`crate::core::savings::CLAUDE_CODE_SESSION_ID_ENV`] and is a different
//! value. So the row is keyed by the Claude Code id when the harness exported
//! one, and by the directory name only when it did not. The directory location
//! is unchanged: one compiled prompt per managed session is what that path is
//! for.
//!
//! Test: the inline suite in `savings_instructions_tests.rs` —
//! `no_row_when_the_compiled_prompt_is_not_smaller`,
//! `a_folded_source_set_produces_a_row`, `no_row_when_the_model_cannot_be_priced`,
//! `folded_source_bytes_adds_an_override_body`,
//! `the_row_is_keyed_by_the_claude_session_id`,
//! `the_row_falls_back_to_the_compiled_prompt_directory_id`.

use std::path::Path;

use crate::core::savings::{
    BYTES_PER_TOKEN, SavingsRow, TECHNIQUE_INSTRUCTION_COMPRESSION, append_row,
    claude_code_session_id, default_savings_log, now_ts,
};

/// Append one `instruction-compression` row for a session whose compiled prompt
/// came out smaller than the sources that fed it.
///
/// Why: called from the compiled-prompt writer so every launch path — fresh
/// start, daemon resume, in-place relaunch — records the same way without each
/// one growing its own call.
/// What: reads the Claude Code session id the statusline folds by, then defers
/// to [`record_instruction_compression_to`] against the default ledger.
/// Test: `the_row_is_keyed_by_the_claude_session_id`,
/// `the_row_falls_back_to_the_compiled_prompt_directory_id`.
pub fn record_instruction_compression(dest: &Path, prompt: &str) {
    // #7209: the row's key is the Claude Code session id, not the directory name.
    record_instruction_compression_to(
        &default_savings_log(),
        dest,
        prompt,
        claude_code_session_id(),
        resolve_pm_price,
    );
}

/// [`record_instruction_compression`] against an explicit ledger, session id
/// and price.
///
/// Why: the three ambient reads the entry point makes — the ledger path under
/// the operator's home, the harness's session-id variable, and the configured
/// PM model — are exactly what makes the keying decision untestable in place.
/// Passing all three in keeps the test on a tempdir ledger with no process-env
/// mutation, which the `tm` binary's env ratchet (#5544) and parallel test
/// binaries both require.
/// What: derives the fallback session id and the harness root from `dest`
/// (which is always
/// `<harness-root>/.trusty-mpm/sessions/<id>/INSTRUCTIONS-COMPILED.md`), keys
/// the row by `claude_session_id` when the harness supplied one, measures the
/// fold, prices the token delta, and appends the row. Silent no-op on every
/// failure.
/// Test: `the_row_is_keyed_by_the_claude_session_id`,
/// `the_row_falls_back_to_the_compiled_prompt_directory_id`.
fn record_instruction_compression_to(
    ledger: &Path,
    dest: &Path,
    prompt: &str,
    claude_session_id: Option<String>,
    price: impl FnOnce() -> Option<(String, f64)>,
) {
    let Some((compiled_prompt_id, harness_root)) = session_and_root(dest) else {
        return;
    };
    // #7209: the statusline folds by the id Claude Code sends it, so a row keyed
    // by the compiled-prompt directory name never matches for a managed session.
    let session_id = claude_session_id.unwrap_or(compiled_prompt_id);
    let source_bytes = folded_source_bytes(&harness_root);
    let Some(row) = instruction_compression_row(&session_id, source_bytes, prompt.len(), price)
    else {
        return;
    };
    if let Err(source) = append_row(ledger, &row) {
        tracing::warn!(
            ledger = %ledger.display(),
            %source,
            "could not append the instruction-compression savings row"
        );
    }
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
/// Test: `session_and_root_reads_the_compiled_prompt_path`,
/// `session_and_root_rejects_a_short_path`.
fn session_and_root(dest: &Path) -> Option<(String, std::path::PathBuf)> {
    let session_dir = dest.parent()?;
    let session_id = session_dir.file_name()?.to_str()?.to_string();
    if session_id.is_empty() {
        return None;
    }
    let harness_root = session_dir.parent()?.parent()?.parent()?.to_path_buf();
    Some((session_id, harness_root))
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
