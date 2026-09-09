//! Tests for the instruction/language-compression savings producer (#6958).
//!
//! Why: the producer's whole contract is "write a row only when the fold
//! genuinely removed something". A permissive implementation — one that writes a
//! row on every launch, or that substitutes a guessed price for an unknown
//! model — would put a fabricated figure on the operator's status bar.
//! What: the row builder is driven directly with hand-supplied byte counts and
//! an injected price, so every decline branch is asserted without a filesystem
//! or a resolved model.
//! Test: this file.

use super::*;

/// A price stand-in: Sonnet's published input rate, as the shared table carries
/// it. Used so the arithmetic below is hand-checkable.
fn sonnet_price() -> Option<(String, f64)> {
    Some(("claude-sonnet-4-6".to_string(), 3.0))
}

/// Why (#6958, required acceptance): the common case is a project that
/// overrides nothing, where the composer ADDS generated context and the
/// delivered prompt is larger than its sources. Writing a row there would put a
/// number on the status bar that no fold produced.
/// Test: itself.
#[test]
fn no_row_when_the_compiled_prompt_is_not_smaller() {
    assert!(
        instruction_compression_row("sess-a", 26_617, 30_721, sonnet_price).is_none(),
        "a larger compiled prompt must write no row"
    );
    assert!(
        instruction_compression_row("sess-a", 26_617, 26_617, sonnet_price).is_none(),
        "an equal compiled prompt must write no row"
    );
    // A delta under one whole token is also nothing to report.
    assert!(
        instruction_compression_row("sess-a", 1_003, 1_000, sonnet_price).is_none(),
        "a sub-token delta must write no row"
    );
}

/// Why: the positive branch, with figures a reader can check by hand —
/// 40,000 bytes folded away is 10,000 tokens at four bytes each, which at
/// Sonnet's $3/Mtok input rate is $0.03.
/// Test: itself.
#[test]
fn a_folded_source_set_produces_a_row() {
    let row = instruction_compression_row("sess-a", 60_000, 20_000, sonnet_price).expect("a row");
    assert_eq!(row.session_id, "sess-a");
    assert_eq!(row.tokens_saved, 10_000);
    // #7179: the percent denominator is the folded source set's own token
    // count — 60,000 bytes at four bytes per token is 15,000 tokens.
    assert_eq!(row.tokens_before, 15_000);
    assert!((row.cost_saved_usd - 0.03).abs() < 1e-9);
    assert!(
        row.basis.contains("60000") && row.basis.contains("20000"),
        "the basis must carry both byte counts: {}",
        row.basis
    );
}

/// Why: the fold divisor is shared across producers so their rows are
/// comparable; a producer that picked its own would report an incomparable
/// number under the same total.
/// Test: itself.
#[test]
fn instruction_compression_tokens_use_the_shared_divisor() {
    let row = instruction_compression_row("sess-a", 8_000, 4_000, sonnet_price).expect("a row");
    assert_eq!(row.tokens_saved, (4_000.0 / BYTES_PER_TOKEN) as i64);
    assert_eq!(row.tokens_saved, 1_000);
}

/// Why: the fold reader filters on `technique` for a per-technique breakdown,
/// so the name the producer writes is part of the contract.
/// Test: itself.
#[test]
fn instruction_compression_row_carries_the_named_technique() {
    let row = instruction_compression_row("sess-a", 60_000, 20_000, sonnet_price).expect("a row");
    assert_eq!(row.technique, TECHNIQUE_INSTRUCTION_COMPRESSION);
    assert_eq!(row.technique, "instruction-compression");
}

/// Why (#6972): this producer runs at session launch, off the very chain that
/// produced the session's `--model` flag, so its config lookup is authoritative
/// rather than the divert producer's last-resort guess. The row says which,
/// so an operator reading the ledger does not have to know that.
/// Test: itself.
#[test]
fn instruction_compression_row_names_its_model_source() {
    let row = instruction_compression_row("sess-a", 60_000, 20_000, sonnet_price).expect("a row");
    assert_eq!(
        row.model_source,
        crate::core::session_model::MODEL_SOURCE_LAUNCH_CONFIG
    );
    assert_eq!(row.model_source, "launch-config");
    assert_ne!(
        row.model_source,
        crate::core::session_model::MODEL_SOURCE_CONFIG_FALLBACK,
        "a launch-time config read must not be labelled a fallback"
    );
}

/// Why (#6958): a model the shared price table does not know must decline the
/// row, never substitute a guessed rate. A fabricated price is worse than no
/// figure — it is a figure the operator would act on.
/// Test: itself.
#[test]
fn no_row_when_the_model_cannot_be_priced() {
    assert!(
        instruction_compression_row("sess-a", 60_000, 20_000, || None).is_none(),
        "an unpriceable model must write no row"
    );
    assert!(
        instruction_compression_row("sess-a", 60_000, 20_000, || Some(("m".into(), 0.0))).is_none(),
        "a zero rate must write no row"
    );
}

/// Why: the producer's only inputs are a destination path and a prompt, so the
/// session id and the harness root have to come out of that path correctly or
/// every row is attributed to the wrong session.
/// Test: itself.
#[test]
fn session_and_root_reads_the_compiled_prompt_path() {
    let dest = std::path::Path::new(
        "/repos/trusty-tools/.trusty-mpm/sessions/sess-42/INSTRUCTIONS-COMPILED.md",
    );
    let (session_id, root) = session_and_root(dest).expect("resolved");
    assert_eq!(session_id, "sess-42");
    assert_eq!(root, std::path::Path::new("/repos/trusty-tools"));
}

/// Why: a path that is not of the compiled-prompt shape must decline rather
/// than attribute a row to a directory name that is not a session.
/// Test: itself.
#[test]
fn session_and_root_rejects_a_short_path() {
    assert!(session_and_root(std::path::Path::new("INSTRUCTIONS-COMPILED.md")).is_none());
    assert!(session_and_root(std::path::Path::new("/a/b/INSTRUCTIONS-COMPILED.md")).is_none());
}

/// Why: the bundled sections are the floor of the source set, and a count that
/// silently dropped them would make every fold look like a saving.
/// Test: itself.
#[test]
fn folded_source_bytes_counts_the_bundled_sections() {
    let dir = tempfile::tempdir().expect("temp dir");
    let bundled: usize = crate::core::instruction_pipeline::SECTION_SOURCES
        .iter()
        .map(|(_, body)| body.len())
        .sum();
    assert!(bundled > 0, "the bundled corpus must not be empty");
    assert_eq!(
        folded_source_bytes(dir.path()),
        bundled,
        "a project with no CLAUDE.md contributes only the bundled sections"
    );
}

/// Why: an override body is a source the composer READ and partly discarded, so
/// it belongs on the source side. Leaving it out would make every overriding
/// project look like it folded nothing.
/// Test: itself.
#[test]
fn folded_source_bytes_adds_an_override_body() {
    let dir = tempfile::tempdir().expect("temp dir");
    let baseline = folded_source_bytes(dir.path());
    let body = "Ship it. Skip the ceremony.";
    std::fs::write(
        dir.path().join("CLAUDE.md"),
        format!(
            "<!-- TRUSTY-MPM: WORKFLOW START v=1 -->\n{body}\n<!-- TRUSTY-MPM: WORKFLOW END -->\n"
        ),
    )
    .expect("write CLAUDE.md");

    let with_override = folded_source_bytes(dir.path());
    assert_eq!(
        with_override,
        baseline + body.len(),
        "the override body must be counted on the source side"
    );
}

/// Why: the price the producer uses must come from the shared table, not a
/// fourth copy. Asserting the resolved rate against a direct table lookup is
/// what pins that, without depending on which model this machine is configured
/// for.
/// Test: itself.
#[test]
fn resolve_pm_price_agrees_with_the_shared_table() {
    // A machine configured for a model outside the table resolves to `None`,
    // which declines the row — the documented behaviour, not a test failure.
    if let Some((model, rate)) = resolve_pm_price() {
        let table = trusty_common::inference::pricing(&model)
            .expect("the resolved model must be one the table knows");
        assert_eq!(rate, table.input, "the rate must be the table's input rate");
        assert!(rate > 0.0, "a priced model must have a positive input rate");
    }
}

/// Build a compiled-prompt destination under a fresh harness root.
///
/// Why: every keying test needs the same
/// `<root>/.trusty-mpm/sessions/<scope>/INSTRUCTIONS-COMPILED.md` shape, and the
/// producer reads the root as a project directory, so it must exist on disk.
/// What: creates the session directory and returns the destination path.
/// Test: used by `the_row_is_keyed_by_the_claude_session_id`.
fn compiled_prompt_dest(root: &std::path::Path, scope: &str) -> std::path::PathBuf {
    let dir = root.join(".trusty-mpm").join("sessions").join(scope);
    std::fs::create_dir_all(&dir).expect("session dir");
    dir.join("INSTRUCTIONS-COMPILED.md")
}

/// Why (#7209): the `💸` segment folds this ledger by the session id Claude Code
/// sends the statusline on stdin, while the compiled prompt lives under the
/// managed scope (`local` for an unmanaged launch). A row keyed by that
/// directory name is a row the segment can never match, so the segment renders
/// nothing at all — the owner-reported symptom.
/// Test: itself.
#[test]
fn the_row_is_keyed_by_the_claude_session_id() {
    let project = tempfile::tempdir().expect("temp project");
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let dest = compiled_prompt_dest(project.path(), "local");
    let ledger = crate::core::savings::savings_log_in(framework_root.path());

    record_instruction_compression_to(
        framework_root.path(),
        &dest,
        "a compiled prompt far smaller than its sources",
        Some("claude-abc-123".to_string()),
        sonnet_price,
    );

    let written = std::fs::read_to_string(&ledger).expect("the ledger must exist");
    assert!(
        written.contains("\"session_id\":\"claude-abc-123\""),
        "the row must be keyed by the Claude Code session id: {written}"
    );

    let matched = crate::core::savings::fold_session(&ledger, "claude-abc-123");
    assert!(
        !matched.is_zero(),
        "the statusline fold under the Claude Code id must find the row: {matched:?}"
    );
    assert!(
        crate::core::savings::fold_session(&ledger, "local").is_zero(),
        "nothing may be attributed to the compiled-prompt directory name"
    );
}

/// Why (#7245): this producer ALWAYS lacks the Claude Code id — it runs before
/// `claude` is spawned — so #7209's fallback to the directory name was not an
/// edge case but every row, and the statusline folds by no such key. Writing one
/// anyway put a row on the ledger that no surface could ever attribute. It is
/// staged for the hook instead, and the ledger stays untouched until that hook
/// can key it.
/// Test: itself.
#[test]
fn no_claude_id_stages_the_row_instead_of_writing_an_unfoldable_one() {
    let project = tempfile::tempdir().expect("temp project");
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let dest = compiled_prompt_dest(project.path(), "sess-42");

    record_instruction_compression_to(framework_root.path(), &dest, "tiny", None, sonnet_price);

    let ledger = crate::core::savings::savings_log_in(framework_root.path());
    assert!(
        !ledger.exists(),
        "a row nothing can fold must not reach the ledger"
    );
    let staged = crate::core::savings_sidecar::pending_row_path_in(framework_root.path(), &dest);
    let text = std::fs::read_to_string(&staged).expect("the row must be staged for the hook");
    assert!(
        text.contains("\"session_id\":\"sess-42\""),
        "the staged row keeps the compile-time id until the hook re-keys it: {text}"
    );
}

/// Why (#7245, required acceptance): the closure condition — a session that never
/// exports `CLAUDE_CODE_SESSION_ID` to the compiling `tm` process still produces
/// a row the statusline can fold. This drives the real producer and the real
/// hook-side claim end to end, including the second invocation that must not
/// duplicate the row.
/// Test: itself.
#[test]
fn a_staged_row_becomes_foldable_at_the_first_hook_invocation() {
    let project = tempfile::tempdir().expect("temp project");
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let dest = compiled_prompt_dest(project.path(), "m1");
    let ledger = crate::core::savings::savings_log_in(framework_root.path());

    record_instruction_compression_to(framework_root.path(), &dest, "tiny", None, sonnet_price);
    assert!(
        crate::core::savings::fold_session(&ledger, "c1").is_zero(),
        "before the hook there is nothing for the statusline to fold"
    );

    for _ in 0..2 {
        crate::core::savings_sidecar::emit_staged_row(&ledger, framework_root.path(), &dest, "c1");
    }

    let folded = crate::core::savings::fold_session(&ledger, "c1");
    assert!(
        !folded.is_zero(),
        "the statusline fold for the launched Claude session must find the row: {folded:?}"
    );
    assert_eq!(
        folded.rows, 1,
        "two hook invocations must leave exactly one row for the pair: {folded:?}"
    );
    assert!(
        crate::core::savings::fold_session(&ledger, "m1").is_zero(),
        "nothing may stay attributed to the managed session id"
    );
}

/// Why (#7245): for a project that overrides no instruction section the composer
/// only ADDS context, so this decline is permanent and no row can ever be
/// written. At `debug!` that left the segment's absence unexplained. The
/// producer must state it — once — and still write nothing.
/// Test: itself.
#[test]
fn a_prompt_that_folds_nothing_warns_once_and_writes_no_row() {
    let project = tempfile::tempdir().expect("temp project");
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let dest = compiled_prompt_dest(project.path(), "m1");
    let bulky = "x".repeat(folded_source_bytes(project.path()) + 1);

    record_instruction_compression_to(framework_root.path(), &dest, &bulky, None, sonnet_price);

    assert!(
        !crate::core::savings::savings_log_in(framework_root.path()).exists(),
        "a prompt that folded nothing must write no row"
    );
    assert!(
        !crate::core::savings_sidecar::pending_row_path_in(framework_root.path(), &dest).exists(),
        "a prompt that folded nothing must stage nothing either"
    );
    assert!(
        !crate::core::savings_sidecar::warn_no_fold_once(
            framework_root.path(),
            project.path(),
            folded_source_bytes(project.path()),
            bulky.len(),
        ),
        "the producer must already have warned for this project and byte pair"
    );
}

/// Why (#7209): the producers must read one variable name. A rename on one side
/// only would silently unmatch every row the other writes.
/// Test: itself.
#[test]
fn claude_code_session_id_names_the_harness_variable() {
    assert_eq!(
        crate::core::savings::CLAUDE_CODE_SESSION_ID_ENV,
        "CLAUDE_CODE_SESSION_ID"
    );
}
